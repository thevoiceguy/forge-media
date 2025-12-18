//! Health check endpoint

use axum::routing::get;
use axum::response::IntoResponse;
use axum::Router;
use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::sessions::AppState;

/// Health check response
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
}

/// Health check handler
///
/// Returns the health status of the API server
async fn health_check(State(state): State<Arc<AppState>>) -> axum::response::Response {
    // Default to OK; override when HA indicates standby/degraded
    let status = {
        #[cfg(feature = "ha")]
        {
            if let Some(ha) = &state.ha_manager {
                if ha.is_primary().await && ha.is_healthy().await {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            } else {
                StatusCode::OK
            }
        }

        #[cfg(not(feature = "ha"))]
        {
            StatusCode::OK
        }
    };

    let body = HealthResponse {
        status: if status == StatusCode::OK {
            "healthy".to_string()
        } else {
            "standby".to_string()
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: 0, // TODO: Track actual uptime
    };

    (status, axum::Json(body)).into_response()
}

/// Create health check routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/health", get(health_check))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt as _;

    #[tokio::test]
    async fn test_health_check() {
        let state = Arc::new(AppState::new(
            forge_engine::SessionManager::new(Default::default(), None),
            Arc::new(crate::routes::prometheus::MetricsHandle::init()),
            Arc::new(forge_conference_processor::ConferenceBridge::default()),
            std::env::temp_dir().join("forge-test-recordings"),
            std::env::temp_dir().join("forge-test-prompts"),
            forge_core::config::default_ai_allowed_endpoints(),
            Arc::new(forge_core::EventBus::new()),
        ));
        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(health.status, "healthy");
        assert_eq!(health.version, env!("CARGO_PKG_VERSION"));
    }
}
