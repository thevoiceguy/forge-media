//! Authentication middleware for the Forge API

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use std::collections::HashSet;
use std::sync::Arc;

/// Static bearer-token authentication configuration
#[derive(Clone, Debug)]
pub struct AuthConfig {
    tokens: Arc<HashSet<String>>,
}

impl AuthConfig {
    /// Create a new auth configuration from a list of tokens
    pub fn new<T: Into<String>>(tokens: impl IntoIterator<Item = T>) -> Self {
        let set = tokens.into_iter().map(Into::into).collect();
        Self {
            tokens: Arc::new(set),
        }
    }

    /// Check if authentication is enabled (any tokens configured)
    pub fn is_enabled(&self) -> bool {
        !self.tokens.is_empty()
    }

    /// Validate a presented bearer token
    fn is_valid(&self, token: &str) -> bool {
        self.tokens.contains(token)
    }
}

/// Public endpoints that don't require authentication
const PUBLIC_ENDPOINTS: &[&str] = &["/health", "/ha/health"];

/// Simple bearer-token authorization middleware
pub async fn auth_middleware(
    request: Request<Body>,
    next: Next,
) -> Result<axum::response::Response, impl IntoResponse> {
    // Skip authentication for public endpoints (health checks, metrics)
    let path = request.uri().path();
    if PUBLIC_ENDPOINTS.contains(&path) {
        return Ok(next.run(request).await);
    }

    // Tokens are provided via extensions from the server builder
    let auth_config = request.extensions().get::<AuthConfig>().cloned();

    if let Some(config) = auth_config {
        if !config.is_enabled() {
            return Ok(next.run(request).await);
        }

        // Extract bearer token
        let token = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value: &HeaderValue| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim);

        if let Some(token) = token {
            if config.is_valid(token) {
                return Ok(next.run(request).await);
            }
        }

        Err((StatusCode::UNAUTHORIZED, "Missing or invalid bearer token"))
    } else {
        // If no config is present, allow by default (should not happen in normal operation)
        Ok(next.run(request).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_config_is_enabled() {
        let config_empty = AuthConfig::new(Vec::<String>::new());
        assert!(
            !config_empty.is_enabled(),
            "Empty config should not be enabled"
        );

        let config_with_token = AuthConfig::new(vec!["token"]);
        assert!(
            config_with_token.is_enabled(),
            "Config with token should be enabled"
        );
    }

    #[test]
    fn test_auth_config_validates_tokens() {
        let config = AuthConfig::new(vec!["valid-token-1", "valid-token-2"]);

        assert!(
            config.is_valid("valid-token-1"),
            "Should accept valid token 1"
        );
        assert!(
            config.is_valid("valid-token-2"),
            "Should accept valid token 2"
        );
        assert!(
            !config.is_valid("invalid-token"),
            "Should reject invalid token"
        );
        assert!(!config.is_valid(""), "Should reject empty token");
    }

    #[test]
    fn test_public_endpoints_list() {
        assert!(
            PUBLIC_ENDPOINTS.contains(&"/health"),
            "Health endpoint should be public"
        );
        assert!(
            PUBLIC_ENDPOINTS.contains(&"/ha/health"),
            "HA health endpoint should be public"
        );
        assert!(
            !PUBLIC_ENDPOINTS.contains(&"/v1/sessions"),
            "Sessions endpoint should not be public"
        );
    }

    #[test]
    fn test_auth_config_from_multiple_types() {
        // Test with Vec<String>
        let config1 = AuthConfig::new(vec!["token1".to_string(), "token2".to_string()]);
        assert!(config1.is_valid("token1"));

        // Test with Vec<&str>
        let config2 = AuthConfig::new(vec!["token1", "token2"]);
        assert!(config2.is_valid("token1"));

        // Test with empty
        let config3 = AuthConfig::new(Vec::<&str>::new());
        assert!(!config3.is_enabled());
    }
}
