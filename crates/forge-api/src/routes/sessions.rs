//! Session management endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forge_core::CallId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::response::{created, no_content, success, ApiSuccess};

/// Request to create a new session
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub call_id: Option<String>,
    pub sdp: Option<String>,
    pub from_tag: Option<String>,
    pub to_tag: Option<String>,
}

/// Session information response
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    pub call_id: String,
    pub state: String,
    pub created_at: String,
    pub local_addr: Option<String>,
    pub remote_addr: Option<String>,
}

/// List of sessions response
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionResponse>,
    pub count: usize,
}

/// Application state (placeholder - will be replaced with actual engine)
#[derive(Clone)]
pub struct AppState {
    // TODO: Add ForgeEngine reference
}

impl AppState {
    pub fn new() -> Self {
        Self {}
    }
}

/// Create a new session
///
/// POST /v1/sessions
async fn create_session(
    State(_state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("Creating session: {:?}", request);

    // TODO: Implement actual session creation with ForgeEngine
    let call_id = request
        .call_id
        .unwrap_or_else(|| CallId::generate().to_string());

    let response = SessionResponse {
        call_id: call_id.clone(),
        state: "creating".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        local_addr: Some("0.0.0.0:30000".to_string()),
        remote_addr: None,
    };

    tracing::info!("Session created: {}", call_id);

    Ok(created(response))
}

/// Get session information
///
/// GET /v1/sessions/:id
async fn get_session(
    State(_state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<ApiSuccess<SessionResponse>> {
    tracing::info!("Getting session: {}", call_id);

    // TODO: Implement actual session lookup
    // For now, return a stub response
    let response = SessionResponse {
        call_id: call_id.clone(),
        state: "active".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        local_addr: Some("0.0.0.0:30000".to_string()),
        remote_addr: Some("192.168.1.100:5060".to_string()),
    };

    Ok(success(response))
}

/// Delete a session
///
/// DELETE /v1/sessions/:id
async fn delete_session(
    State(_state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("Deleting session: {}", call_id);

    // TODO: Implement actual session deletion
    // For now, just return success

    tracing::info!("Session deleted: {}", call_id);

    Ok(no_content())
}

/// List all sessions
///
/// GET /v1/sessions
async fn list_sessions(
    State(_state): State<Arc<AppState>>,
) -> ApiResult<ApiSuccess<SessionListResponse>> {
    tracing::info!("Listing sessions");

    // TODO: Implement actual session listing
    let response = SessionListResponse {
        sessions: vec![],
        count: 0,
    };

    Ok(success(response))
}

/// Create session routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/sessions", post(create_session))
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/:id", get(get_session))
        .route("/v1/sessions/:id", delete(delete_session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt as _;

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::new())
    }

    #[tokio::test]
    async fn test_create_session() {
        let app = routes().with_state(test_state());

        let request_body = serde_json::json!({
            "call_id": "test-123"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let app = routes().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_session() {
        let app = routes().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions/test-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let app = routes().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/sessions/test-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
