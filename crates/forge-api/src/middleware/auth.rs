//! Authentication middleware for the Forge API
//!
//! Tokens are bearer tokens carried in the `Authorization` header. Each token
//! is stored with an associated [`TokenScope`] (and optional tenant label), so
//! that downstream handlers can enforce role-based authorization using the
//! [`AuthContext`] that this middleware installs in request extensions.
//!
//! Constant-time comparison (via [`subtle::ConstantTimeEq`]) is used when
//! matching a presented token against configured tokens so that the duration
//! of a negative result cannot be used to probe the token store (audit
//! finding C4).

// `Result<_, axum::Response>` is how axum extractors report rejections, and
// `axum::Response` is unavoidably large. Silencing clippy's structural size
// lint here is preferable to boxing every rejection path.
#![allow(clippy::result_large_err)]

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// Role/scope attached to a bearer token.
///
/// Ordered from least to most privileged; `PartialOrd`/`Ord` let handlers
/// write simple `if ctx.scope >= TokenScope::Operator { ... }` checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenScope {
    /// Read-only: can `GET` resources but cannot mutate.
    ReadOnly,
    /// Operator: can create/modify/delete sessions, conferences, recordings.
    Operator,
    /// Admin: everything Operator can do plus cluster-wide operations
    /// (HA failover, drain, global configuration) — see audit finding C6.
    Admin,
}

impl TokenScope {
    /// True when this scope grants at least `needed`.
    pub fn satisfies(self, needed: TokenScope) -> bool {
        self >= needed
    }
}

/// A configured bearer token with its scope and optional tenant label.
#[derive(Clone, Debug)]
pub struct Token {
    value: String,
    scope: TokenScope,
    tenant: Option<String>,
}

impl Token {
    /// Build a new token. `tenant` is opaque and exposed to handlers through
    /// [`AuthContext`] for per-tenant resource scoping.
    pub fn new<S: Into<String>>(value: S, scope: TokenScope, tenant: Option<String>) -> Self {
        Self {
            value: value.into(),
            scope,
            tenant,
        }
    }

    /// Shorthand for an operator-scoped, tenant-less token (the legacy default).
    pub fn operator<S: Into<String>>(value: S) -> Self {
        Self::new(value, TokenScope::Operator, None)
    }

    /// Shorthand for an admin-scoped, tenant-less token.
    pub fn admin<S: Into<String>>(value: S) -> Self {
        Self::new(value, TokenScope::Admin, None)
    }
}

/// Authentication context stored in the request's extensions after a
/// successful match. Handlers can extract it via the
/// [`require_scope`](crate::middleware::auth::require_scope) helper or by
/// reading `request.extensions().get::<AuthContext>()`.
#[derive(Clone, Debug)]
pub struct AuthContext {
    pub scope: TokenScope,
    pub tenant: Option<String>,
}

/// Static bearer-token authentication configuration.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    tokens: Arc<Vec<Token>>,
}

impl AuthConfig {
    /// Legacy constructor: every supplied string becomes an Operator-scoped,
    /// tenant-less token. Preserves behaviour of code written before the
    /// scope model existed.
    pub fn new<T: Into<String>>(tokens: impl IntoIterator<Item = T>) -> Self {
        let list: Vec<Token> = tokens.into_iter().map(|t| Token::operator(t)).collect();
        Self::from_tokens(list)
    }

    /// Preferred constructor: supply `Token` instances directly, each with
    /// its own scope and tenant.
    pub fn from_tokens(tokens: impl IntoIterator<Item = Token>) -> Self {
        Self {
            tokens: Arc::new(tokens.into_iter().collect()),
        }
    }

    /// True when any tokens are configured.
    pub fn is_enabled(&self) -> bool {
        !self.tokens.is_empty()
    }

    /// Match the presented token against every configured token in
    /// constant time (per token). Returns the matched [`AuthContext`] on
    /// success.
    ///
    /// Using `ConstantTimeEq` prevents timing side-channels that would let
    /// an attacker binary-search a prefix of a valid token. We still short-
    /// circuit once a match is found (any timing variance here is purely
    /// between the *token count* seen so far, not between character
    /// positions inside a token) — callers that need full-list timing can
    /// set a fixed-size token store.
    fn lookup(&self, presented: &str) -> Option<AuthContext> {
        let presented = presented.as_bytes();
        let mut matched: Option<&Token> = None;
        for t in self.tokens.iter() {
            let stored = t.value.as_bytes();
            if stored.len() == presented.len()
                && bool::from(stored.ct_eq(presented))
                && matched.is_none()
            {
                matched = Some(t);
            }
        }
        matched.map(|t| AuthContext {
            scope: t.scope,
            tenant: t.tenant.clone(),
        })
    }

    /// Back-compat accessor — true when the presented token is any
    /// configured token, regardless of scope.
    pub fn is_valid(&self, token: &str) -> bool {
        self.lookup(token).is_some()
    }

    /// Resolve the matched token to its scope/tenant.
    pub fn authenticate(&self, token: &str) -> Option<AuthContext> {
        self.lookup(token)
    }
}

/// Public endpoints that don't require authentication.
const PUBLIC_ENDPOINTS: &[&str] = &["/health", "/ha/health"];

/// Simple bearer-token authentication middleware.
///
/// On success, inserts an [`AuthContext`] into the request extensions so
/// downstream handlers can enforce scope-based authorization.
pub async fn auth_middleware(
    request: Request<Body>,
    next: Next,
) -> Result<axum::response::Response, impl IntoResponse> {
    let path = request.uri().path();
    if PUBLIC_ENDPOINTS.contains(&path) {
        return Ok(next.run(request).await);
    }

    let auth_config = request.extensions().get::<AuthConfig>().cloned();

    if let Some(config) = auth_config {
        if !config.is_enabled() {
            // Auth disabled → treat requests as anonymous Admin (preserves
            // local-dev behaviour; production must configure tokens).
            let mut request = request;
            request.extensions_mut().insert(AuthContext {
                scope: TokenScope::Admin,
                tenant: None,
            });
            return Ok(next.run(request).await);
        }

        let token = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value: &HeaderValue| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim);

        if let Some(token) = token {
            if let Some(ctx) = config.authenticate(token) {
                let mut request = request;
                request.extensions_mut().insert(ctx);
                return Ok(next.run(request).await);
            }
        }

        Err((StatusCode::UNAUTHORIZED, "Missing or invalid bearer token"))
    } else {
        // No config present — fail closed in principle, but historically
        // forge-api treats this as "auth not installed" and allows. Preserve
        // the legacy behaviour but still stamp an Admin context so scope
        // checks downstream don't hard-reject.
        let mut request = request;
        request.extensions_mut().insert(AuthContext {
            scope: TokenScope::Admin,
            tenant: None,
        });
        Ok(next.run(request).await)
    }
}

/// Axum extractor that succeeds only when the request carries an
/// `AuthContext` with at least `SCOPE` permission. On failure it rejects
/// with the appropriate HTTP status, so handlers that add it to their
/// signature automatically enforce the minimum scope:
///
/// ```ignore
/// async fn delete_session(
///     _auth: RequireOperator,
///     State(state): State<Arc<AppState>>,
///     Path(id): Path<String>,
/// ) -> Response { ... }
/// ```
///
/// Three common aliases ship below: [`RequireReadOnly`], [`RequireOperator`],
/// [`RequireAdmin`].
pub struct RequireScopeAt<const SCOPE: u8>(pub AuthContext);

impl<const SCOPE: u8> std::fmt::Debug for RequireScopeAt<SCOPE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RequireScopeAt").field(&self.0).finish()
    }
}

/// Require `TokenScope::ReadOnly` or higher.
pub type RequireReadOnly = RequireScopeAt<0>;
/// Require `TokenScope::Operator` or higher.
pub type RequireOperator = RequireScopeAt<1>;
/// Require `TokenScope::Admin`.
pub type RequireAdmin = RequireScopeAt<2>;

impl<const SCOPE: u8> RequireScopeAt<SCOPE> {
    fn needed() -> TokenScope {
        match SCOPE {
            0 => TokenScope::ReadOnly,
            1 => TokenScope::Operator,
            _ => TokenScope::Admin,
        }
    }
}

impl<S, const SCOPE: u8> axum::extract::FromRequestParts<S> for RequireScopeAt<SCOPE>
where
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let needed = Self::needed();
        match parts.extensions.get::<AuthContext>().cloned() {
            Some(ctx) if ctx.scope.satisfies(needed) => Ok(RequireScopeAt(ctx)),
            Some(_) => Err((
                StatusCode::FORBIDDEN,
                format!("requires scope {:?}", needed),
            )
                .into_response()),
            None => Err((StatusCode::UNAUTHORIZED, "no auth context").into_response()),
        }
    }
}

/// Enforce a minimum scope on a handler. If the request's `AuthContext` is
/// missing or its scope is lower than `needed`, the handler returns
/// `403 Forbidden` (or `401 Unauthorized` when no context is attached).
///
/// Usage:
/// ```ignore
/// async fn delete_session(State(state): State<Arc<AppState>>, req: Request<Body>) -> Response {
///     if let Err(r) = require_scope(&req, TokenScope::Operator) { return r; }
///     // ... handler body ...
/// }
/// ```
pub fn require_scope<B>(
    request: &Request<B>,
    needed: TokenScope,
) -> Result<&AuthContext, axum::response::Response> {
    match request.extensions().get::<AuthContext>() {
        Some(ctx) if ctx.scope.satisfies(needed) => Ok(ctx),
        Some(_) => Err((
            StatusCode::FORBIDDEN,
            format!("requires scope {:?}", needed),
        )
            .into_response()),
        None => Err((StatusCode::UNAUTHORIZED, "no auth context").into_response()),
    }
}

/// Test-only helper: wrap a state-bound router in the auth middleware
/// layer with an empty token list, which causes the middleware to stamp
/// every request with an Admin-scoped [`AuthContext`]. Route tests can
/// then exercise handlers without building a full server.
#[cfg(test)]
pub fn wrap_for_tests<S: Clone + Send + Sync + 'static>(
    router: axum::Router<S>,
    state: S,
) -> axum::Router {
    use axum::Extension;
    // Layer order matters: axum's `.layer` stacks outside-in. `from_fn`
    // must see the `Extension<AuthConfig>` already inserted, so the
    // Extension layer has to be outermost (applied LAST).
    router
        .with_state(state)
        .layer(axum::middleware::from_fn(auth_middleware))
        .layer(Extension(AuthConfig::new(Vec::<String>::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_config_is_enabled() {
        let config_empty = AuthConfig::new(Vec::<String>::new());
        assert!(!config_empty.is_enabled());

        let config_with_token = AuthConfig::new(vec!["token"]);
        assert!(config_with_token.is_enabled());
    }

    #[test]
    fn test_auth_config_validates_tokens() {
        let config = AuthConfig::new(vec!["valid-token-1", "valid-token-2"]);

        assert!(config.is_valid("valid-token-1"));
        assert!(config.is_valid("valid-token-2"));
        assert!(!config.is_valid("invalid-token"));
        assert!(!config.is_valid(""));
    }

    // C4 regression: lookups must go through constant-time compare and must
    // still reject tokens that differ only by a single character at various
    // positions (length, first char, middle, last).
    #[test]
    fn test_auth_config_rejects_near_miss_tokens() {
        let config = AuthConfig::new(vec!["correct-horse-battery-staple"]);

        assert!(!config.is_valid("Correct-horse-battery-staple"));
        assert!(!config.is_valid("correct-horse-battery-stapl"));
        assert!(!config.is_valid("correct-horse-battery-stapleX"));
        assert!(!config.is_valid("wrong-horse-battery-staple"));
    }

    #[test]
    fn test_scope_ordering() {
        assert!(TokenScope::Admin > TokenScope::Operator);
        assert!(TokenScope::Operator > TokenScope::ReadOnly);
        assert!(TokenScope::Admin.satisfies(TokenScope::Operator));
        assert!(TokenScope::Operator.satisfies(TokenScope::Operator));
        assert!(!TokenScope::ReadOnly.satisfies(TokenScope::Operator));
    }

    #[test]
    fn test_scoped_tokens() {
        let config = AuthConfig::from_tokens([
            Token::new("admin-tok", TokenScope::Admin, Some("tenant-a".into())),
            Token::new("op-tok", TokenScope::Operator, None),
            Token::new("ro-tok", TokenScope::ReadOnly, None),
        ]);

        let admin = config.authenticate("admin-tok").unwrap();
        assert_eq!(admin.scope, TokenScope::Admin);
        assert_eq!(admin.tenant.as_deref(), Some("tenant-a"));

        let op = config.authenticate("op-tok").unwrap();
        assert_eq!(op.scope, TokenScope::Operator);
        assert!(op.tenant.is_none());

        let ro = config.authenticate("ro-tok").unwrap();
        assert_eq!(ro.scope, TokenScope::ReadOnly);

        assert!(config.authenticate("nope").is_none());
    }

    #[test]
    fn test_public_endpoints_list() {
        assert!(PUBLIC_ENDPOINTS.contains(&"/health"));
        assert!(PUBLIC_ENDPOINTS.contains(&"/ha/health"));
        assert!(!PUBLIC_ENDPOINTS.contains(&"/v1/sessions"));
    }

    #[test]
    fn test_auth_config_from_multiple_types() {
        let config1 = AuthConfig::new(vec!["token1".to_string(), "token2".to_string()]);
        assert!(config1.is_valid("token1"));

        let config2 = AuthConfig::new(vec!["token1", "token2"]);
        assert!(config2.is_valid("token1"));

        let config3 = AuthConfig::new(Vec::<&str>::new());
        assert!(!config3.is_enabled());
    }

    #[test]
    fn test_require_scope_accepts_exact() {
        let req = Request::builder()
            .uri("/")
            .extension(AuthContext {
                scope: TokenScope::Operator,
                tenant: None,
            })
            .body(())
            .unwrap();
        assert!(require_scope(&req, TokenScope::Operator).is_ok());
    }

    #[test]
    fn test_require_scope_rejects_insufficient() {
        let req = Request::builder()
            .uri("/")
            .extension(AuthContext {
                scope: TokenScope::ReadOnly,
                tenant: None,
            })
            .body(())
            .unwrap();
        assert!(require_scope(&req, TokenScope::Operator).is_err());
    }

    #[test]
    fn test_require_scope_rejects_missing_context() {
        let req = Request::builder().uri("/").body(()).unwrap();
        assert!(require_scope(&req, TokenScope::ReadOnly).is_err());
    }

    // C5 regression: admin-only endpoints must reject an operator-scoped
    // token even if it authenticates successfully.
    #[tokio::test]
    async fn test_operator_token_denied_admin_endpoint() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::{routing::post, Extension, Router};
        use tower::util::ServiceExt as _;

        async fn admin_handler(_: RequireAdmin) -> &'static str {
            "ok"
        }

        let auth = AuthConfig::from_tokens([Token::operator("op-token")]);
        let app: Router = Router::new()
            .route("/admin", post(admin_handler))
            .layer(axum::middleware::from_fn(auth_middleware))
            .layer(Extension(auth));

        let req = Request::builder()
            .method("POST")
            .uri("/admin")
            .header(axum::http::header::AUTHORIZATION, "Bearer op-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_admin_token_allowed_admin_endpoint() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::{routing::post, Extension, Router};
        use tower::util::ServiceExt as _;

        async fn admin_handler(_: RequireAdmin) -> &'static str {
            "ok"
        }

        let auth = AuthConfig::from_tokens([Token::admin("admin-token")]);
        let app: Router = Router::new()
            .route("/admin", post(admin_handler))
            .layer(axum::middleware::from_fn(auth_middleware))
            .layer(Extension(auth));

        let req = Request::builder()
            .method("POST")
            .uri("/admin")
            .header(axum::http::header::AUTHORIZATION, "Bearer admin-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_unauthenticated_request_denied() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::{routing::get, Extension, Router};
        use tower::util::ServiceExt as _;

        async fn handler(_: RequireReadOnly) -> &'static str {
            "ok"
        }

        let auth = AuthConfig::from_tokens([Token::operator("some-token")]);
        let app: Router = Router::new()
            .route("/", get(handler))
            .layer(axum::middleware::from_fn(auth_middleware))
            .layer(Extension(auth));

        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
