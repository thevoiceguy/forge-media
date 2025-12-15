//! WebRTC connection management endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use forge_ice::IceCandidate;
use forge_webrtc::PeerConnection;
use metrics::{counter, gauge, histogram};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::response::{created, no_content, success, ApiSuccess};
use crate::routes::sessions::AppState;

/// WebRTC connection manager
#[derive(Clone)]
pub struct WebRtcManager {
    connections: Arc<DashMap<String, Arc<tokio::sync::Mutex<PeerConnection>>>>,
}

impl Default for WebRtcManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WebRtcManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
        }
    }

    pub fn add_connection(&self, id: String, conn: Arc<tokio::sync::Mutex<PeerConnection>>) {
        self.connections.insert(id, conn);
    }

    pub fn get_connection(
        &self,
        id: &str,
    ) -> Option<Arc<tokio::sync::Mutex<PeerConnection>>> {
        self.connections.get(id).map(|entry| entry.value().clone())
    }

    pub fn remove_connection(&self, id: &str) -> Option<Arc<tokio::sync::Mutex<PeerConnection>>> {
        self.connections.remove(id).map(|(_, conn)| conn)
    }

    pub fn list_connections(&self) -> Vec<String> {
        self.connections.iter().map(|entry| entry.key().clone()).collect()
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

/// Request to create a new WebRTC connection
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct CreateConnectionRequest {
    /// STUN servers for server-reflexive candidate discovery
    /// Format: ["stun:stun.l.google.com:19302"]
    #[validate(length(max = 10))]
    pub stun_servers: Option<Vec<String>>,
}

/// WebRTC connection information response
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionResponse {
    pub connection_id: String,
    pub state: String,
    pub sdp_offer: String,
    pub dtls_fingerprint: String,
}

/// List of WebRTC connections response
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionListResponse {
    pub connections: Vec<ConnectionStatusResponse>,
    pub count: usize,
}

/// Connection status response
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionStatusResponse {
    pub connection_id: String,
    pub state: String,
}

/// Request to set remote SDP answer
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct SetAnswerRequest {
    #[validate(length(min = 1, max = 65536))]
    pub sdp_answer: String,
}

/// ICE candidate DTO
#[derive(Debug, Serialize, Deserialize)]
pub struct IceCandidateDto {
    pub foundation: String,
    pub component: u16,
    pub protocol: String,
    pub priority: u32,
    pub ip: String,
    pub port: u16,
    pub typ: String,
    pub rel_addr: Option<String>,
    pub rel_port: Option<u16>,
}

impl From<&IceCandidate> for IceCandidateDto {
    fn from(candidate: &IceCandidate) -> Self {
        Self {
            foundation: candidate.foundation.clone(),
            component: candidate.component,
            protocol: format!("{:?}", candidate.protocol),
            priority: candidate.priority,
            ip: candidate.ip.to_string(),
            port: candidate.port,
            typ: format!("{:?}", candidate.typ),
            rel_addr: candidate.rel_addr.map(|addr| addr.to_string()),
            rel_port: candidate.rel_port,
        }
    }
}

/// Request to add an ICE candidate
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct AddIceCandidateRequest {
    #[validate(length(min = 1, max = 1024))]
    pub candidate: String,
}

/// Create a new WebRTC connection
///
/// POST /v1/webrtc/connections
#[tracing::instrument(skip(state, request))]
async fn create_connection(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateConnectionRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to create WebRTC connection");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    counter!("webrtc_connections_created_total", 1);

    // Use provided STUN servers or default
    let stun_servers = request
        .stun_servers
        .unwrap_or_else(|| vec!["stun:stun.l.google.com:19302".to_string()]);

    // Create WebRTC peer connection
    let mut peer = PeerConnection::new(stun_servers)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create peer connection: {}", e)))?;

    // Generate SDP offer
    let sdp_offer = peer
        .create_offer()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create SDP offer: {}", e)))?;

    let connection_id = peer.connection_id().to_string();
    let dtls_fingerprint = peer.dtls_fingerprint().to_string();
    let state_str = format!("{:?}", peer.get_state());

    // Update ICE candidate metric
    let candidate_count = peer.local_candidate_count().await;
    gauge!("forge_webrtc_ice_candidates_gathered", candidate_count as f64);

    // Store connection in manager
    let peer_arc = Arc::new(tokio::sync::Mutex::new(peer));
    state.webrtc_manager.add_connection(connection_id.clone(), peer_arc);

    let response = ConnectionResponse {
        connection_id: connection_id.clone(),
        state: state_str,
        sdp_offer,
        dtls_fingerprint,
    };

    tracing::info!("WebRTC connection created: {}", connection_id);

    // Update metrics
    let active_count = state.webrtc_manager.connection_count();
    gauge!("forge_webrtc_connections_active", active_count as f64);

    Ok(created(response))
}

/// Get WebRTC connection information
///
/// GET /v1/webrtc/connections/:id
#[tracing::instrument(skip(state), fields(connection_id = %connection_id))]
async fn get_connection(
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<String>,
) -> ApiResult<ApiSuccess<ConnectionStatusResponse>> {
    tracing::debug!("API request to get WebRTC connection info");

    let peer = state
        .webrtc_manager
        .get_connection(&connection_id)
        .ok_or_else(|| ApiError::ConnectionNotFound(connection_id.clone()))?;

    let peer_lock = peer.lock().await;
    let state_str = format!("{:?}", peer_lock.get_state());

    let response = ConnectionStatusResponse {
        connection_id,
        state: state_str,
    };

    Ok(success(response))
}

/// Delete a WebRTC connection
///
/// DELETE /v1/webrtc/connections/:id
#[tracing::instrument(skip(state), fields(connection_id = %connection_id))]
async fn delete_connection(
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to delete WebRTC connection");

    // Remove from WebRtcManager
    state
        .webrtc_manager
        .remove_connection(&connection_id)
        .ok_or_else(|| ApiError::ConnectionNotFound(connection_id.clone()))?;

    counter!("webrtc_connections_deleted_total", 1);

    // Update active connections gauge
    let active_count = state.webrtc_manager.connection_count();
    gauge!("forge_webrtc_connections_active", active_count as f64);

    tracing::info!("WebRTC connection deleted: {}", connection_id);

    Ok(no_content())
}

/// Set remote SDP answer
///
/// POST /v1/webrtc/connections/:id/answer
#[tracing::instrument(skip(state, request), fields(connection_id = %connection_id))]
async fn set_answer(
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<String>,
    Json(request): Json<SetAnswerRequest>,
) -> ApiResult<ApiSuccess<ConnectionStatusResponse>> {
    tracing::info!("API request to set remote SDP answer");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    let peer = state
        .webrtc_manager
        .get_connection(&connection_id)
        .ok_or_else(|| ApiError::ConnectionNotFound(connection_id.clone()))?;

    // Measure time for set_remote_answer (includes ICE checks and DTLS handshake)
    let start = std::time::Instant::now();

    let mut peer_lock = peer.lock().await;
    peer_lock
        .set_remote_answer(&request.sdp_answer)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to set remote answer: {}", e)))?;

    let duration = start.elapsed();
    histogram!("forge_webrtc_connection_establishment_duration_seconds", duration.as_secs_f64());

    let state_str = format!("{:?}", peer_lock.get_state());

    let response = ConnectionStatusResponse {
        connection_id,
        state: state_str,
    };

    Ok(success(response))
}

/// Add ICE candidate
///
/// POST /v1/webrtc/connections/:id/ice-candidate
#[tracing::instrument(skip(state, request), fields(connection_id = %connection_id))]
async fn add_ice_candidate(
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<String>,
    Json(request): Json<AddIceCandidateRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to add ICE candidate");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    let peer = state
        .webrtc_manager
        .get_connection(&connection_id)
        .ok_or_else(|| ApiError::ConnectionNotFound(connection_id.clone()))?;

    // Parse ICE candidate from SDP attribute string
    let candidate = IceCandidate::from_sdp_attribute(&request.candidate)
        .map_err(|e| ApiError::InvalidRequest(format!("Failed to parse ICE candidate: {}", e)))?;

    let mut peer_lock = peer.lock().await;
    peer_lock
        .add_ice_candidate(candidate)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to add ICE candidate: {}", e)))?;

    counter!("forge_webrtc_ice_candidates_added_total", 1);

    tracing::info!("ICE candidate added to connection: {}", connection_id);

    Ok(no_content())
}

/// Get connection status
///
/// GET /v1/webrtc/connections/:id/status
#[tracing::instrument(skip(state), fields(connection_id = %connection_id))]
async fn get_status(
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<String>,
) -> ApiResult<ApiSuccess<ConnectionStatusResponse>> {
    tracing::debug!("API request to get WebRTC connection status");

    let peer = state
        .webrtc_manager
        .get_connection(&connection_id)
        .ok_or_else(|| ApiError::ConnectionNotFound(connection_id.clone()))?;

    let peer_lock = peer.lock().await;
    let state_str = format!("{:?}", peer_lock.get_state());

    let response = ConnectionStatusResponse {
        connection_id,
        state: state_str,
    };

    Ok(success(response))
}

/// List all WebRTC connections
///
/// GET /v1/webrtc/connections
async fn list_connections(
    State(state): State<Arc<AppState>>,
) -> ApiResult<ApiSuccess<ConnectionListResponse>> {
    tracing::info!("Listing WebRTC connections");

    let connection_ids = state.webrtc_manager.list_connections();
    let mut connections = Vec::new();

    for connection_id in connection_ids {
        if let Some(peer) = state.webrtc_manager.get_connection(&connection_id) {
            let peer_lock = peer.lock().await;
            connections.push(ConnectionStatusResponse {
                connection_id,
                state: format!("{:?}", peer_lock.get_state()),
            });
        }
    }

    let count = connections.len();
    let response = ConnectionListResponse { connections, count };

    Ok(success(response))
}

/// Create WebRTC routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/webrtc/connections", post(create_connection))
        .route("/v1/webrtc/connections", get(list_connections))
        .route("/v1/webrtc/connections/:id", get(get_connection))
        .route("/v1/webrtc/connections/:id", delete(delete_connection))
        .route("/v1/webrtc/connections/:id/answer", post(set_answer))
        .route(
            "/v1/webrtc/connections/:id/ice-candidate",
            post(add_ice_candidate),
        )
        .route("/v1/webrtc/connections/:id/status", get(get_status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use forge_engine::SessionManager;
    use tower::util::ServiceExt as _;

    fn test_state() -> Arc<AppState> {
        let base = 50000 + ((std::process::id() % 5000) * 2) as u16;
        let port_pool_config = forge_rtp::PortPoolConfig::new(base, base + 1000).unwrap();
        let session_manager_config = forge_engine::SessionManagerConfig {
            port_pool_config,
            ..Default::default()
        };
        let session_manager = SessionManager::new(session_manager_config, None);
        let metrics_handle = Arc::new(crate::routes::prometheus::MetricsHandle::init());
        let conference_bridge = Arc::new(forge_conference_processor::ConferenceBridge::default());
        Arc::new(AppState::new(
            session_manager,
            metrics_handle,
            conference_bridge,
            std::env::temp_dir().join("forge-test-recordings"),
            std::env::temp_dir().join("forge-test-prompts"),
        ))
    }

    #[tokio::test]
    async fn test_create_connection() {
        let app = routes().with_state(test_state());

        let request_body = serde_json::json!({
            "stun_servers": ["stun:stun.l.google.com:19302"]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/webrtc/connections")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_list_connections() {
        let app = routes().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/webrtc/connections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
