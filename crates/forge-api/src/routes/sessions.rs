//! Session management endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forge_core::{AudioCodec, CallId, ParticipantId};
use forge_engine::{
    ParticipantCodecConfig, ParticipantLabel, ParticipantMediaState, ParticipantMediaUpdate,
    SessionManager,
};
use metrics::{counter, histogram};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::middleware::auth::{RequireOperator, RequireReadOnly};
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

/// Request to update runtime media configuration for a participant leg.
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct UpdateParticipantMediaRequest {
    /// Remote RTP endpoint for this leg (e.g. "203.0.113.10:4000")
    #[validate(length(min = 1, max = 64))]
    pub remote_rtp_addr: Option<String>,
    /// Clear the currently configured/learned remote RTP endpoint.
    pub clear_remote_rtp_addr: Option<bool>,
    /// RTP payload type for the negotiated codec.
    #[validate(range(max = 127))]
    pub payload_type: Option<u8>,
    /// Negotiated codec name (e.g. "pcmu", "pcma", "opus")
    #[validate(length(min = 1, max = 32))]
    pub codec: Option<String>,
    /// Codec clock rate (Hz), e.g. 8000 or 48000.
    #[validate(range(min = 1, max = 192000))]
    pub clock_rate: Option<u32>,
    /// Negotiated telephone-event payload type for RFC 2833 DTMF.
    #[validate(range(max = 127))]
    pub telephone_event_payload_type: Option<u8>,
    /// Restrict symmetric RTP latching to the provided source IPs.
    pub latch_allowed_ips: Option<Vec<String>>,
    /// Clear any configured source-IP latch allowlist.
    pub clear_latch_allowed_ips: Option<bool>,
}

/// Map an arbitrary codec name (from the peer's SDP `a=rtpmap`) onto a
/// fixed allowlist of labels for Prometheus counters. Anything outside the
/// list becomes `"other"`. This is the defense for audit finding C7: the
/// counter label space is now bounded and cannot be poisoned by a crafted
/// SDP offer.
///
/// The allowlist matches the codec families the engine actually implements
/// (see `forge_codecs`). Comparison is case-insensitive because SDP
/// `rtpmap` casing varies between stacks.
pub fn canonical_codec_label(raw: &str) -> &'static str {
    match raw.to_ascii_uppercase().as_str() {
        "OPUS" => "opus",
        "PCMU" | "G711U" | "G711MU" | "MULAW" => "pcmu",
        "PCMA" | "G711A" | "ALAW" => "pcma",
        "G722" => "g722",
        "G729" | "G729A" | "G729B" => "g729",
        "ILBC" => "ilbc",
        "CN" | "COMFORTNOISE" => "cn",
        "TELEPHONE-EVENT" => "telephone-event",
        "RED" => "red",
        "VP8" => "vp8",
        "VP9" => "vp9",
        "H264" => "h264",
        "AV1" => "av1",
        _ => "other",
    }
}

fn parse_participant_leg(raw: &str) -> Result<ParticipantLabel, ApiError> {
    raw.parse().map_err(ApiError::InvalidRequest)
}

fn parse_audio_codec(raw: &str) -> Result<AudioCodec, ApiError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pcmu" | "g711u" | "mulaw" => Ok(AudioCodec::PCMU),
        "pcma" | "g711a" | "alaw" => Ok(AudioCodec::PCMA),
        "g722" => Ok(AudioCodec::G722),
        "g729" | "g729a" | "g729b" => Ok(AudioCodec::G729),
        "opus" => Ok(AudioCodec::Opus),
        "speex" => Ok(AudioCodec::Speex),
        "ilbc" => Ok(AudioCodec::ILBC),
        "amr" => Ok(AudioCodec::AMR),
        "amrwb" | "amr-wb" => Ok(AudioCodec::AMRWB),
        "pcm" | "pcm16" => Ok(AudioCodec::PCM),
        _ => Err(ApiError::InvalidRequest(format!(
            "Unsupported codec '{}'",
            raw
        ))),
    }
}

fn build_participant_media_update(
    request: UpdateParticipantMediaRequest,
) -> Result<ParticipantMediaUpdate, ApiError> {
    let remote_addr =
        match (
            request.remote_rtp_addr.as_ref(),
            request.clear_remote_rtp_addr.unwrap_or(false),
        ) {
            (Some(_), true) => {
                return Err(ApiError::InvalidRequest(
                    "remote_rtp_addr and clear_remote_rtp_addr cannot both be set".to_string(),
                ))
            }
            (Some(addr), false) => Some(Some(addr.parse::<SocketAddr>().map_err(|e| {
                ApiError::InvalidRequest(format!("Invalid remote_rtp_addr: {}", e))
            })?)),
            (None, true) => Some(None),
            (None, false) => None,
        };

    let codec_fields_set = [
        request.payload_type.is_some(),
        request.codec.is_some(),
        request.clock_rate.is_some(),
    ]
    .into_iter()
    .filter(|is_set| *is_set)
    .count();

    let codec_config = if codec_fields_set == 0 {
        None
    } else if codec_fields_set != 3 {
        return Err(ApiError::InvalidRequest(
            "payload_type, codec, and clock_rate must be provided together".to_string(),
        ));
    } else {
        Some(ParticipantCodecConfig {
            payload_type: request.payload_type.unwrap(),
            codec: parse_audio_codec(request.codec.as_deref().unwrap())?,
            clock_rate: request.clock_rate.unwrap(),
        })
    };

    let latch_allowed_ips = match (
        request.latch_allowed_ips.as_ref(),
        request.clear_latch_allowed_ips.unwrap_or(false),
    ) {
        (Some(_), true) => {
            return Err(ApiError::InvalidRequest(
                "latch_allowed_ips and clear_latch_allowed_ips cannot both be set".to_string(),
            ))
        }
        (Some(ips), false) => {
            let mut parsed = HashSet::with_capacity(ips.len());
            for ip in ips {
                parsed.insert(ip.parse::<IpAddr>().map_err(|e| {
                    ApiError::InvalidRequest(format!(
                        "Invalid latch_allowed_ips entry '{}': {}",
                        ip, e
                    ))
                })?);
            }
            Some(Some(parsed))
        }
        (None, true) => Some(None),
        (None, false) => None,
    };

    Ok(ParticipantMediaUpdate {
        remote_addr,
        codec_config,
        telephone_event_payload_type: request.telephone_event_payload_type,
        latch_allowed_ips,
    })
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
    pub media_bridge_manager: Arc<forge_engine::MediaBridgeManager>,
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
        let media_bridge_manager = Arc::new(forge_engine::MediaBridgeManager::new());

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
            media_bridge_manager,
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
    _auth: RequireOperator,
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
                // Record metrics for each negotiated codec.
                //
                // Audit finding C7: the counter label was the raw codec name
                // from the peer's SDP. That's attacker-controlled high-
                // cardinality input — an adversary can emit SDPs with
                // unbounded unique `a=rtpmap` names and bloat the Prometheus
                // registry until the process runs out of memory. Map every
                // codec name through a closed enum of known values and send
                // anything unexpected to a fixed "other" bucket.
                for codec_name in &codec_names {
                    counter!(
                        "sdp_codecs_negotiated_total",
                        1,
                        "codec" => canonical_codec_label(codec_name)
                    );
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
    _auth: RequireReadOnly,
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
    _auth: RequireOperator,
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

/// Get runtime media configuration for a participant leg.
///
/// GET /v1/sessions/{id}/participants/{leg}/media
#[tracing::instrument(skip(state), fields(call_id = %call_id, leg = %leg))]
async fn get_participant_media(
    _auth: RequireReadOnly,
    State(state): State<Arc<AppState>>,
    Path((call_id, leg)): Path<(String, String)>,
) -> ApiResult<ApiSuccess<ParticipantMediaState>> {
    let call_id = CallId(call_id);
    let leg = parse_participant_leg(&leg)?;

    let participant = state
        .session_manager
        .participant_media_state(&call_id, leg)
        .await
        .map_err(ApiError::from)?;

    Ok(success(participant))
}

/// Update runtime media configuration for a participant leg.
///
/// PUT /v1/sessions/{id}/participants/{leg}/media
#[tracing::instrument(skip(state, request), fields(call_id = %call_id, leg = %leg))]
async fn update_participant_media(
    _auth: RequireOperator,
    State(state): State<Arc<AppState>>,
    Path((call_id, leg)): Path<(String, String)>,
    Json(request): Json<UpdateParticipantMediaRequest>,
) -> ApiResult<ApiSuccess<ParticipantMediaState>> {
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    let call_id = CallId(call_id);
    let leg = parse_participant_leg(&leg)?;
    let update = build_participant_media_update(request)?;

    let participant = state
        .session_manager
        .update_participant_media(&call_id, leg, update)
        .await
        .map_err(ApiError::from)?;

    Ok(success(participant))
}

/// List all sessions
///
/// GET /v1/sessions
async fn list_sessions(
    _auth: RequireReadOnly,
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
    _auth: RequireOperator,
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
        .route(
            "/v1/sessions/{id}/participants/{leg}/media",
            get(get_participant_media).put(update_participant_media),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt as _;

    // C7 regression: the Prometheus `codec` label must map to a fixed,
    // bounded set of strings regardless of what arrives on the wire.
    #[test]
    fn test_canonical_codec_label_known_values() {
        assert_eq!(canonical_codec_label("opus"), "opus");
        assert_eq!(canonical_codec_label("Opus"), "opus");
        assert_eq!(canonical_codec_label("OPUS"), "opus");
        assert_eq!(canonical_codec_label("PCMU"), "pcmu");
        assert_eq!(canonical_codec_label("mulaw"), "pcmu");
        assert_eq!(canonical_codec_label("PCMA"), "pcma");
        assert_eq!(canonical_codec_label("G722"), "g722");
        assert_eq!(canonical_codec_label("G729"), "g729");
        assert_eq!(canonical_codec_label("G729A"), "g729");
        assert_eq!(canonical_codec_label("telephone-event"), "telephone-event");
    }

    #[test]
    fn test_canonical_codec_label_rejects_attacker_input() {
        // Long / random / punctuation strings must collapse to "other",
        // preventing label-cardinality DoS.
        for crafted in [
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "my-custom-codec-name",
            "!@#$%^&*()",
            "\u{1F4A3}", // emoji
            "opus2",
            "opus/48000/2", // full rtpmap value
            "",
        ] {
            assert_eq!(
                canonical_codec_label(crafted),
                "other",
                "crafted input `{}` must not reach Prometheus verbatim",
                crafted
            );
        }
    }

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
            #[cfg(feature = "ha")]
            None,
        ))
    }

    fn test_state() -> Arc<AppState> {
        // Use a random port range to avoid conflicts
        let base = 20000 + (std::process::id() % 10000) as u16;
        test_state_with_ports(base, base + 1000)
    }

    /// Wrap a stateful router with the auth layer stack configured with
    /// an empty token list. The middleware then treats each request as
    /// auth-disabled and stamps an Admin-scoped `AuthContext`, so the
    /// scope extractors let the handlers execute.
    fn with_auth(router: axum::Router<Arc<AppState>>, state: Arc<AppState>) -> axum::Router {
        let auth_config = crate::middleware::auth::AuthConfig::new(Vec::<String>::new());
        router
            .with_state(state)
            .layer(axum::Extension(auth_config))
            .layer(axum::middleware::from_fn(
                crate::middleware::auth::auth_middleware,
            ))
    }

    #[tokio::test]
    async fn test_create_session() {
        let state = test_state_with_ports(40000, 41000);
        let app = with_auth(routes(), state);

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
        let state = test_state_with_ports(41000, 42000);
        let app = with_auth(routes(), state);

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

        let app = with_auth(routes(), state);

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

        let app = with_auth(routes(), state);

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

    #[tokio::test]
    async fn test_update_participant_media_route() {
        let state = test_state_with_ports(44000, 45000);

        state
            .session_manager
            .create_session(
                CallId("test-media-123".to_string()),
                ParticipantId::new("caller"),
                ParticipantId::new("callee"),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let app = with_auth(routes(), state);
        let request_body = serde_json::json!({
            "remote_rtp_addr": "203.0.113.20:5000",
            "payload_type": 8,
            "codec": "pcma",
            "clock_rate": 8000,
            "telephone_event_payload_type": 101,
            "latch_allowed_ips": ["203.0.113.20"]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/sessions/test-media-123/participants/b/media")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let response: crate::response::ApiSuccess<ParticipantMediaState> =
            serde_json::from_slice(&body).unwrap();

        assert_eq!(response.data.leg, ParticipantLabel::B);
        assert_eq!(response.data.participant_id, "callee");
        assert_eq!(
            response.data.remote_rtp_addr,
            Some("203.0.113.20:5000".parse::<SocketAddr>().unwrap())
        );
        assert_eq!(response.data.payload_type, 8);
        assert_eq!(response.data.codec, AudioCodec::PCMA);
        assert_eq!(response.data.clock_rate, 8000);
        assert_eq!(response.data.telephone_event_payload_type, 101);
        assert_eq!(
            response.data.latch_allowed_ips,
            Some(vec!["203.0.113.20".parse::<IpAddr>().unwrap()])
        );
    }

    #[tokio::test]
    async fn test_update_participant_media_route_rejects_partial_codec_tuple() {
        let state = test_state_with_ports(45000, 46000);

        state
            .session_manager
            .create_session(
                CallId("test-media-invalid-123".to_string()),
                ParticipantId::generate(),
                ParticipantId::generate(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let app = with_auth(routes(), state);
        let request_body = serde_json::json!({
            "codec": "opus"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/sessions/test-media-invalid-123/participants/a/media")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
