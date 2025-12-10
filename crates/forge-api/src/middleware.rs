//! API middleware

pub mod auth;
pub mod rate_limit;

use axum::http::HeaderValue;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

pub use rate_limit::RateLimiter;

/// Create CORS layer with default configuration
///
/// Restricts origins to the provided list. If the list contains "*",
/// a permissive configuration is used (not recommended).
pub fn cors_layer(origins: &[String]) -> CorsLayer {
    // Allow any origin if explicitly configured
    if origins.iter().any(|o| o == "*") {
        tracing::warn!(
            "CORS configured with wildcard origin; this is not recommended for production"
        );
        return CorsLayer::permissive();
    }

    let mut allowed: Vec<HeaderValue> = Vec::new();
    for origin in origins {
        match HeaderValue::from_str(origin) {
            Ok(value) => allowed.push(value),
            Err(e) => tracing::warn!("Invalid CORS origin '{}': {}", origin, e),
        }
    }

    // If no valid origins are provided, fall back to denying cross-origin by not adding any
    // Access-Control-Allow-Origin header.
    if allowed.is_empty() {
        tracing::info!("CORS enabled but no valid origins configured; cross-origin requests will be blocked by browsers");
        CorsLayer::new().allow_methods(Any).allow_headers(Any)
    } else {
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(allowed))
            .allow_methods(Any)
            .allow_headers(Any)
    }
}

/// Create tracing/logging layer for request logging
pub fn tracing_layer(
) -> TraceLayer<tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>>
{
    TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO))
}
