//! Session management endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forge_core::{CallId, ParticipantId};
use forge_engine::SessionManager;
use metrics::{counter, histogram};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::response::{created, no_content, success, ApiSuccess};

/// Request to create a new session
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct CreateSessionRequest {
    #[validate(length(min = 1, max = 256))]
    pub call_id: Option<String>,
    #[validate(length(min = 1, max = 256))]
    pub participant_a: Option<String>,
    #[validate(length(min = 1, max = 256))]
    pub participant_b: Option<String>,
    /// Legacy SDP field (for backward compatibility)
    #[validate(length(max = 65536))]
    pub sdp: Option<String>,
    /// SDP offer from remote endpoint (for negotiation)
    #[validate(length(max = 65536))]
    pub sdp_offer: Option<String>,
    /// Local IP address to use in SDP answer (required if sdp_offer is provided)
    #[validate(length(min = 7, max = 45))] // IPv4 min: "1.1.1.1", IPv6 max
    pub local_address: Option<String>,
    /// SDP profile to use for capabilities (defaults to "audio-only")
    /// Valid values: "audio-only", "audio-opus", "audio-all"
    #[validate(length(min = 1, max = 64))]
    pub sdp_profile: Option<String>,
    #[validate(length(min = 1, max = 256))]
    pub from_tag: Option<String>,
    #[validate(length(min = 1, max = 256))]
    pub to_tag: Option<String>,
    /// TOS/DSCP value for QoS marking (0-255)
    /// Common values:
    /// - 0xB8 (184) = EF (Expedited Forwarding) - for voice (default)
    /// - 0xA0 (160) = AF41 - for video
    /// - 0x00 (0) = Best effort
    /// If not specified, uses the global default from config
    #[validate(range(max = 255))]
    pub tos: Option<u8>,
}

/// Session information response
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    pub call_id: String,
    pub state: String,
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub sdp: Option<String>,
    /// Negotiated SDP answer (if sdp_offer was provided in request)
    pub sdp_answer: Option<String>,
    /// Negotiated codecs by media type (e.g., {"audio": ["PCMU", "PCMA"]})
    pub negotiated_codecs: Option<std::collections::HashMap<String, Vec<String>>>,
    pub from_tag: Option<String>,
    pub to_tag: Option<String>,
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
    pub metrics_handle: Arc<super::prometheus::MetricsHandle>,
    pub conference_bridge: Arc<forge_conference::ConferenceBridge>,
    pub storage_manager: Arc<tokio::sync::Mutex<forge_storage::StorageManager>>,
    pub recording_base_dir: std::path::PathBuf,
    pub prompts_base_dir: std::path::PathBuf,
    pub ai_allowed_endpoints: Vec<String>,
    pub event_bus: crate::EventBus,
    pub core_event_bus: Arc<forge_core::EventBus>,
    pub webrtc_manager: Arc<super::webrtc::WebRtcManager>,
    pub ai_session_manager: Arc<forge_engine::AISessionManager>,
    /// HA manager for cluster coordination (optional, feature-gated)
    #[cfg(feature = "ha")]
    pub ha_manager: Option<Arc<crate::ha::HAManager>>,
}

impl AppState {
    pub fn new(
        session_manager: Arc<SessionManager>,
        metrics_handle: Arc<super::prometheus::MetricsHandle>,
        conference_bridge: Arc<forge_conference::ConferenceBridge>,
        recording_base_dir: std::path::PathBuf,
        prompts_base_dir: std::path::PathBuf,
        ai_allowed_endpoints: Vec<String>,
        core_event_bus: Arc<forge_core::EventBus>,
        #[cfg(feature = "ha")] ha_manager: Option<Arc<crate::ha::HAManager>>,
    ) -> Self {
        // Create default storage manager
        let storage_manager =
            Arc::new(tokio::sync::Mutex::new(forge_storage::StorageManager::new(
                &recording_base_dir,
                std::time::Duration::from_secs(7 * 24 * 3600),
                0,
            )));

        // Create event bus for real-time events
        let event_bus = crate::EventBus::default();

        // Create WebRTC manager
        let webrtc_manager = Arc::new(super::webrtc::WebRtcManager::new());

        // Create AI session manager
        let ai_session_manager = Arc::new(forge_engine::AISessionManager::new());

        Self {
            session_manager,
            metrics_handle,
            conference_bridge,
            storage_manager,
            recording_base_dir,
            prompts_base_dir,
            ai_allowed_endpoints,
            event_bus,
            core_event_bus,
            webrtc_manager,
            ai_session_manager,
            #[cfg(feature = "ha")]
            ha_manager,
        }
    }
}

/// Create a new session
///
/// POST /v1/sessions
#[tracing::instrument(skip(state, request), fields(call_id = ?request.call_id))]
async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> ApiResult<axum::response::Response> {
    let sdp_len = request.sdp.as_ref().map(|s| s.len());
    tracing::info!(
        "API request to create session (from_tag={:?}, to_tag={:?}, sdp_len={:?})",
        request.from_tag,
        request.to_tag,
        sdp_len
    );

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // SDP negotiation (if sdp_offer is provided)
    let sdp_negotiation_result = if let Some(ref offer_text) = request.sdp_offer {
        // Start timing SDP negotiation
        let negotiation_start = std::time::Instant::now();
        counter!("sdp_negotiation_total", 1);

        // Validate that local_address is provided
        let local_addr = request.local_address.as_ref().ok_or_else(|| {
            counter!("sdp_negotiation_failures_total", 1, "reason" => "missing_local_address");
            ApiError::InvalidRequest(
                "local_address is required when sdp_offer is provided".to_string(),
            )
        })?;

        // Load SDP profile
        let profile_name = request.sdp_profile.as_deref().unwrap_or("audio-only");
        let profile = match profile_name {
            "audio-only" => forge_sdp::profiles::SdpProfile::audio_only(),
            "audio-opus" => forge_sdp::profiles::SdpProfile::audio_opus(),
            "audio-all" => forge_sdp::profiles::SdpProfile::audio_all(),
            _ => {
                counter!("sdp_negotiation_failures_total", 1, "reason" => "invalid_profile");
                return Err(ApiError::InvalidRequest(format!(
                    "Unknown SDP profile: {}. Valid values: audio-only, audio-opus, audio-all",
                    profile_name
                )));
            }
        };

        tracing::debug!("Using SDP profile: {}", profile.name);

        // Parse SDP offer
        use forge_sdp::SessionDescriptionExt;
        let offer = forge_sdp::SessionDescription::from_str(offer_text).map_err(|e| {
            counter!("sdp_negotiation_failures_total", 1, "reason" => "parse_error");
            ApiError::InvalidRequest(format!("Invalid SDP offer: {}", e))
        })?;

        // Generate local capabilities (use a placeholder port, will be updated after session creation)
        let local_caps = profile.with_local_addr(local_addr, 10000);

        // Negotiate answer
        let answer = forge_sdp::SessionDescription::negotiate_answer(
            &offer,
            &local_caps,
            local_addr,
        )
        .map_err(|e| match e {
            forge_sdp::SdpError::NoCommonCodec => {
                counter!("sdp_negotiation_failures_total", 1, "reason" => "no_common_codec");
                ApiError::NotAcceptable(
                    "No common codec found between offer and local capabilities".to_string(),
                )
            }
            _ => {
                counter!("sdp_negotiation_failures_total", 1, "reason" => "negotiation_error");
                ApiError::InvalidRequest(format!("SDP negotiation failed: {}", e))
            }
        })?;

        // Extract negotiated codecs and build ParticipantCodecConfig
        let mut negotiated_codecs = std::collections::HashMap::new();
        let codec_config = if let Some(audio_media) = answer.find_media(forge_sdp::MediaType::Audio)
        {
            let codecs = forge_sdp::helpers::extract_codecs(audio_media);
            let codec_names: Vec<String> = codecs
                .iter()
                .filter(|c| !c.is_dtmf()) // Exclude DTMF from list
                .map(|c| c.encoding_name.clone())
                .collect();
            if !codec_names.is_empty() {
                negotiated_codecs.insert("audio".to_string(), codec_names.clone());
                // Record metrics for each negotiated codec
                for codec_name in &codec_names {
                    counter!("sdp_codecs_negotiated_total", 1, "codec" => codec_name.clone());
                }
            }

            // Get primary codec for session configuration
            if let Some(primary) = forge_sdp::helpers::extract_primary_codec(audio_media) {
                if let Some(audio_codec) = primary.to_audio_codec() {
                    Some(forge_engine::ParticipantCodecConfig {
                        payload_type: primary.payload_type,
                        codec: audio_codec,
                        clock_rate: primary.clock_rate,
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Serialize answer
        let answer_text = forge_sdp::serialize::serialize_sdp(&answer);

        // Record successful negotiation duration
        let negotiation_duration = negotiation_start.elapsed();
        histogram!(
            "sdp_negotiation_duration_seconds",
            negotiation_duration.as_secs_f64()
        );

        tracing::info!(
            "SDP negotiation successful: negotiated {:?} in {:?}",
            negotiated_codecs,
            negotiation_duration
        );

        Some((answer_text, negotiated_codecs, codec_config))
    } else {
        None
    };

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

    // Prepare custom session config if TOS is specified
    let custom_config = if let Some(tos) = request.tos {
        let mut config = forge_engine::MediaSessionConfig::default();
        config.socket_config.tos = tos;
        tracing::debug!(
            "Creating session with custom TOS: 0x{:02X} (DSCP=0x{:02X})",
            tos,
            tos >> 2
        );
        Some(config)
    } else {
        None
    };

    // Create session (with codecs if negotiated)
    let session = if let Some((_, _, Some(codec_config))) = &sdp_negotiation_result {
        // Create session with negotiated codec configuration
        // Both participants use the same negotiated codec in a 2-party call
        state
            .session_manager
            .create_session_with_codecs(
                call_id.clone(),
                participant_a,
                participant_b,
                codec_config.clone(),
                codec_config.clone(),
                request.sdp.clone(),
                request.from_tag.clone(),
                request.to_tag.clone(),
                custom_config,
            )
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to create session: {}", e)))?
    } else {
        // Create session with default codec (PCMU)
        state
            .session_manager
            .create_session_with_config(
                call_id.clone(),
                participant_a,
                participant_b,
                request.sdp.clone(),
                request.from_tag.clone(),
                request.to_tag.clone(),
                custom_config,
            )
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to create session: {}", e)))?
    };

    let ports = session.ports();
    let session_state = session.state().await;

    // Extract SDP answer and negotiated codecs from negotiation result
    // Update SDP answer with actual allocated ports
    let (sdp_answer, negotiated_codecs) = if let Some((answer_text, codecs, _)) =
        sdp_negotiation_result
    {
        // Parse the answer to update ports
        use forge_sdp::SessionDescriptionExt;
        let mut answer = forge_sdp::SessionDescription::from_str(&answer_text).map_err(|e| {
            ApiError::Internal(format!("Failed to parse generated SDP answer: {}", e))
        })?;

        // Update connection and media port with actual allocated port
        if let Some(audio_media) = answer.find_media_mut(forge_sdp::MediaType::Audio) {
            audio_media.port = ports.rtp_port;
        }

        // Connection address is already set from negotiation, port is in media line

        // Re-serialize with updated ports
        let updated_answer = forge_sdp::serialize::serialize_sdp(&answer);
        (Some(updated_answer), Some(codecs))
    } else {
        (None, None)
    };

    let response = SessionResponse {
        call_id: call_id.0,
        state: format!("{:?}", session_state),
        rtp_port: ports.rtp_port,
        rtcp_port: ports.rtcp_port,
        sdp: session.sdp().map(|s| s.to_string()),
        sdp_answer,
        negotiated_codecs,
        from_tag: session.from_tag().map(|t| t.to_string()),
        to_tag: session.to_tag().map(|t| t.to_string()),
        participant_a: None,
        participant_b: None,
    };

    tracing::info!(
        "Session created: {} on ports {}/{}",
        response.call_id,
        ports.rtp_port,
        ports.rtcp_port
    );

    Ok(created(response))
}

/// Get session information
///
/// GET /v1/sessions/{id}
#[tracing::instrument(skip(state), fields(call_id = %call_id))]
async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<ApiSuccess<SessionResponse>> {
    tracing::debug!("API request to get session info");

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
        sdp: session.sdp().map(|s| s.to_string()),
        sdp_answer: None,
        negotiated_codecs: None,
        from_tag: session.from_tag().map(|t| t.to_string()),
        to_tag: session.to_tag().map(|t| t.to_string()),
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
/// DELETE /v1/sessions/{id}
#[tracing::instrument(skip(state), fields(call_id = %call_id))]
async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to delete session");

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
            sdp: session.sdp().map(|s| s.to_string()),
            sdp_answer: None,
            negotiated_codecs: None,
            from_tag: session.from_tag().map(|t| t.to_string()),
            to_tag: session.to_tag().map(|t| t.to_string()),
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
/// POST /v1/sessions/{id}/start
#[tracing::instrument(skip(state), fields(call_id = %call_id))]
async fn start_session(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<ApiSuccess<SessionResponse>> {
    tracing::info!("API request to start session forwarding");

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
        sdp: session.sdp().map(|s| s.to_string()),
        sdp_answer: None,        // Will be populated in Sprint 2.2
        negotiated_codecs: None, // Will be populated in Sprint 2.2
        from_tag: session.from_tag().map(|t| t.to_string()),
        to_tag: session.to_tag().map(|t| t.to_string()),
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
        .route("/v1/sessions/{id}", get(get_session))
        .route("/v1/sessions/{id}", delete(delete_session))
        .route("/v1/sessions/{id}/start", post(start_session))
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
        let session_manager = SessionManager::new(session_manager_config, None);
        let metrics_handle = Arc::new(crate::routes::prometheus::MetricsHandle::init());
        let conference_bridge = Arc::new(forge_conference::ConferenceBridge::default());
        Arc::new(AppState::new(
            session_manager,
            metrics_handle,
            conference_bridge,
            std::env::temp_dir().join("forge-test-recordings"),
            std::env::temp_dir().join("forge-test-prompts"),
            forge_core::config::default_ai_allowed_endpoints(),
            Arc::new(forge_core::EventBus::new()),
        ))
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
        state
            .session_manager
            .create_session(
                call_id.clone(),
                ParticipantId::generate(),
                ParticipantId::generate(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

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
        state
            .session_manager
            .create_session(
                call_id.clone(),
                ParticipantId::generate(),
                ParticipantId::generate(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

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
