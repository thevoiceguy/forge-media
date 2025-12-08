//! Session management endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forge_core::{CallId, ParticipantId};
use forge_engine::{SessionManager, SessionState};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::response::{created, no_content, success, ApiSuccess};

/// Request to create a new session
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub call_id: Option<String>,
    pub participant_a: Option<String>,
    pub participant_b: Option<String>,
    pub sdp: Option<String>,
    pub from_tag: Option<String>,
    pub to_tag: Option<String>,
}

/// Session information response
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    pub call_id: String,
    pub state: String,
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub participant_a: Option<ParticipantStats>,
    pub participant_b: Option<ParticipantStats>,
}

/// Participant statistics in response
#[derive(Debug, Serialize, Deserialize)]
pub struct ParticipantStats {
    pub id: String,
    pub packets_received: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub bytes_sent: u64,
}

/// List of sessions response
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionResponse>,
    pub count: usize,
}

/// Application state with session manager
#[derive(Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
}

impl AppState {
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
    }
}

/// Create a new session
///
/// POST /v1/sessions
async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("Creating session: {:?}", request);

    // Parse or generate IDs
    let call_id = if let Some(id) = request.call_id {
        CallId(id)
    } else {
        CallId::generate()
    };

    let participant_a = if let Some(id) = request.participant_a {
        ParticipantId(id)
    } else {
        ParticipantId::generate()
    };

    let participant_b = if let Some(id) = request.participant_b {
        ParticipantId(id)
    } else {
        ParticipantId::generate()
    };

    // Create session
    let session = state
        .session_manager
        .create_session(call_id.clone(), participant_a, participant_b)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create session: {}", e)))?;

    let ports = session.ports();
    let session_state = session.state().await;

    let response = SessionResponse {
        call_id: call_id.0,
        state: format!("{:?}", session_state),
        rtp_port: ports.rtp_port,
        rtcp_port: ports.rtcp_port,
        participant_a: None,
        participant_b: None,
    };

    tracing::info!("Session created: {} on ports {}/{}", response.call_id, ports.rtp_port, ports.rtcp_port);

    Ok(created(response))
}

/// Get session information
///
/// GET /v1/sessions/:id
async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<ApiSuccess<SessionResponse>> {
    tracing::info!("Getting session: {}", call_id);

    let call_id = CallId(call_id);
    let session = state
        .session_manager
        .get_session(&call_id)
        .ok_or_else(|| ApiError::SessionNotFound(call_id.0.clone()))?;

    let ports = session.ports();
    let session_state = session.state().await;
    let stats_a = session.participant_a_stats().await;
    let stats_b = session.participant_b_stats().await;

    let response = SessionResponse {
        call_id: call_id.0,
        state: format!("{:?}", session_state),
        rtp_port: ports.rtp_port,
        rtcp_port: ports.rtcp_port,
        participant_a: Some(ParticipantStats {
            id: "A".to_string(),
            packets_received: stats_a.packets_received,
            bytes_received: stats_a.bytes_received,
            packets_sent: stats_a.packets_sent,
            bytes_sent: stats_a.bytes_sent,
        }),
        participant_b: Some(ParticipantStats {
            id: "B".to_string(),
            packets_received: stats_b.packets_received,
            bytes_received: stats_b.bytes_received,
            packets_sent: stats_b.packets_sent,
            bytes_sent: stats_b.bytes_sent,
        }),
    };

    Ok(success(response))
}

/// Delete a session
///
/// DELETE /v1/sessions/:id
async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("Deleting session: {}", call_id);

    let call_id = CallId(call_id.clone());
    state
        .session_manager
        .stop_session(&call_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to stop session: {}", e)))?;

    tracing::info!("Session deleted: {}", call_id.0);

    Ok(no_content())
}

/// List all sessions
///
/// GET /v1/sessions
async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> ApiResult<ApiSuccess<SessionListResponse>> {
    tracing::info!("Listing sessions");

    let sessions = state.session_manager.list_sessions();
    let mut session_responses = Vec::new();

    for session in sessions {
        let ports = session.ports();
        let session_state = session.state().await;

        session_responses.push(SessionResponse {
            call_id: session.call_id().0.clone(),
            state: format!("{:?}", session_state),
            rtp_port: ports.rtp_port,
            rtcp_port: ports.rtcp_port,
            participant_a: None,
            participant_b: None,
        });
    }

    let count = session_responses.len();
    let response = SessionListResponse {
        sessions: session_responses,
        count,
    };

    Ok(success(response))
}

/// Start/activate a session (begin forwarding)
///
/// POST /v1/sessions/:id/start
async fn start_session(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<ApiSuccess<SessionResponse>> {
    tracing::info!("Starting session: {}", call_id);

    let call_id = CallId(call_id);
    state
        .session_manager
        .start_session(&call_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to start session: {}", e)))?;

    // Get updated session info
    let session = state
        .session_manager
        .get_session(&call_id)
        .ok_or_else(|| ApiError::SessionNotFound(call_id.0.clone()))?;

    let ports = session.ports();
    let session_state = session.state().await;

    let response = SessionResponse {
        call_id: call_id.0,
        state: format!("{:?}", session_state),
        rtp_port: ports.rtp_port,
        rtcp_port: ports.rtcp_port,
        participant_a: None,
        participant_b: None,
    };

    tracing::info!("Session started: {}", response.call_id);

    Ok(success(response))
}

/// Create session routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/sessions", post(create_session))
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/:id", get(get_session))
        .route("/v1/sessions/:id", delete(delete_session))
        .route("/v1/sessions/:id/start", post(start_session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt as _;

    fn test_state_with_ports(min_port: u16, max_port: u16) -> Arc<AppState> {
        let port_pool_config = forge_rtp::PortPoolConfig::new(min_port, max_port).unwrap();
        let session_manager_config = forge_engine::SessionManagerConfig {
            port_pool_config,
            ..Default::default()
        };
        let session_manager = Arc::new(SessionManager::new(session_manager_config, None));
        Arc::new(AppState::new(session_manager))
    }

    fn test_state() -> Arc<AppState> {
        // Use a random port range to avoid conflicts
        let base = 20000 + (std::process::id() % 10000) as u16;
        test_state_with_ports(base, base + 1000)
    }

    #[tokio::test]
    async fn test_create_session() {
        let app = routes().with_state(test_state_with_ports(40000, 41000));

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
        let app = routes().with_state(test_state_with_ports(41000, 42000));

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
        let state = test_state_with_ports(42000, 43000);

        // First create a session
        let call_id = CallId("test-get-123".to_string());
        state.session_manager.create_session(
            call_id.clone(),
            ParticipantId::generate(),
            ParticipantId::generate(),
        ).await.unwrap();

        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions/test-get-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let state = test_state_with_ports(43000, 44000);

        // First create a session
        let call_id = CallId("test-delete-123".to_string());
        state.session_manager.create_session(
            call_id.clone(),
            ParticipantId::generate(),
            ParticipantId::generate(),
        ).await.unwrap();

        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/sessions/test-delete-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
