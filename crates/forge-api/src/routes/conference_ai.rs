//! Conference AI API endpoints
//!
//! Provides REST API for attaching/detaching AI to conference rooms

use crate::response::{created, no_content};
use crate::routes::ai::validate_ai_endpoint;
use crate::routes::sessions::AppState;
use crate::{ApiError, ApiResult};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use forge_conference_processor::{AudioMode, ConferenceAIConfig, ConferenceAIManager};
use forge_engine::ai_integration::AISessionConfig;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;
use validator::Validate;
use forge_core::{CallId, SecureString};

/// Create conference AI routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/conferences/:room_id/ai", post(attach_ai))
        .route("/v1/conferences/:room_id/ai", get(get_ai_status))
        .route("/v1/conferences/:room_id/ai", delete(detach_ai))
}

/// Request to attach AI to a conference room
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct AttachConferenceAIRequest {
    /// OpenAI API key
    #[validate(length(min = 1))]
    pub api_key: String,

    /// OpenAI model (default: gpt-4o-realtime-preview-2024-12-17)
    pub model: Option<String>,

    /// Voice personality (alloy, shimmer, echo, etc.)
    pub voice: Option<String>,

    /// Optional custom endpoint (must be https/wss and in allowlist)
    #[validate(length(min = 1, max = 512))]
    pub endpoint: Option<String>,

    /// System instructions for the AI
    pub instructions: Option<String>,

    /// Temperature (0.0-1.0, default: 0.8)
    #[validate(range(min = 0.0, max = 1.0))]
    pub temperature: Option<f32>,

    /// Audio mode: "mixed" or "individual"
    pub audio_mode: Option<String>,

    /// Enable transcription
    pub enable_transcription: Option<bool>,
}

/// Response for AI status
#[derive(Debug, Serialize, Deserialize)]
pub struct ConferenceAIResponse {
    pub room_id: String,
    pub state: String,
    pub model: String,
    pub voice: Option<String>,
    pub audio_mode: String,
    pub enable_transcription: bool,
    pub participants_heard: Vec<String>,
}

/// Attach AI to a conference room
///
/// POST /v1/conferences/:room_id/ai
async fn attach_ai(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<AttachConferenceAIRequest>,
) -> ApiResult<axum::response::Response> {
    info!("Attaching AI to conference room: {}", room_id);

    // Validate request
    request.validate().map_err(|e| {
        ApiError::InvalidRequest(format!("Invalid request: {}", e))
    })?;

    if let Some(endpoint) = request.endpoint.as_ref() {
        validate_ai_endpoint(endpoint, &state.ai_allowed_endpoints)?;
    }

    // Get conference room
    let room = state.conference_bridge.get_room(&room_id).map_err(|e| {
        ApiError::NotFound(format!("Conference room not found: {}", e))
    })?;

    // Check if AI already attached
    if room.has_ai() {
        return Err(ApiError::Conflict(format!(
            "AI already attached to room {}",
            room_id
        )));
    }

    // Parse audio mode
    let audio_mode = match request.audio_mode.as_deref() {
        Some("mixed") | None => AudioMode::Mixed,
        Some("individual") => AudioMode::Individual,
        Some(mode) => {
            return Err(ApiError::InvalidRequest(format!(
                "Invalid audio_mode: {}. Must be 'mixed' or 'individual'",
                mode
            )))
        }
    };

    // Create AI session config
    let ai_config = AISessionConfig {
        connector_type: forge_ai_stream::AIConnectorType::OpenAI,
        api_key: SecureString::new(request.api_key),
        endpoint: request.endpoint.clone(),
        model: request
            .model
            .unwrap_or_else(|| "gpt-4o-realtime-preview-2024-12-17".to_string()),
        voice: request.voice,
        temperature: request.temperature,
        instructions: request.instructions,
        enable_vad: true,
        enable_barge_in: true,
        sample_rate: 16000, // AI sessions default to 16kHz
    };

    // Create call ID for the conference AI session
    let call_id = CallId::from(format!("conference-{}", room_id));

    // Attach AI session via manager (no EventBus needed for conference)
    state
        .ai_session_manager
        .attach_ai(call_id.clone(), ai_config.clone(), None)
        .await
        .map_err(|e| {
            ApiError::Internal(format!("Failed to attach AI session: {}", e))
        })?;

    // Create conference AI config
    let conference_ai_config = ConferenceAIConfig::new(room.format())
        .with_audio_mode(audio_mode)
        .with_transcription(request.enable_transcription.unwrap_or(false));

    // Create conference AI manager with session manager reference
    let ai_manager = ConferenceAIManager::new(
        room_id.clone(),
        call_id,
        Arc::clone(&state.ai_session_manager),
        conference_ai_config,
    )
    .map_err(|e| {
        ApiError::Internal(format!("Failed to create AI manager: {}", e))
    })?;

    // Attach AI to room with event bus for DTMF forwarding
    room.attach_ai(ai_manager, Some(Arc::clone(&state.core_event_bus)))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to attach AI: {}", e)))?;

    info!("Successfully attached AI to conference room {}", room_id);

    // Build response
    let response = ConferenceAIResponse {
        room_id: room_id.clone(),
        state: format!("{:?}", room.ai_state().unwrap_or(forge_conference_processor::ConferenceAIState::Connecting)),
        model: ai_config.model,
        voice: ai_config.voice,
        audio_mode: format!("{:?}", audio_mode),
        enable_transcription: request.enable_transcription.unwrap_or(false),
        participants_heard: room.participants(),
    };

    Ok(created(response))
}

/// Get AI status for a conference room
///
/// GET /v1/conferences/:room_id/ai
async fn get_ai_status(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    // Get conference room
    let room = state.conference_bridge.get_room(&room_id).map_err(|e| {
        ApiError::NotFound(format!("Conference room not found: {}", e))
    })?;

    // Check if AI is attached
    if !room.has_ai() {
        return Err(ApiError::NotFound(format!(
            "No AI attached to room {}",
            room_id
        )));
    }

    let ai_state = room.ai_state().unwrap_or(forge_conference_processor::ConferenceAIState::Terminated);

    // Build response (simplified for now - would need to store more info)
    let response = ConferenceAIResponse {
        room_id: room_id.clone(),
        state: format!("{:?}", ai_state),
        model: "unknown".to_string(), // Would need to store this
        voice: None,
        audio_mode: "Mixed".to_string(), // Would need to store this
        enable_transcription: false,
        participants_heard: room.participants(),
    };

    Ok(Json(response).into_response())
}

/// Detach AI from a conference room
///
/// DELETE /v1/conferences/:room_id/ai
async fn detach_ai(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to detach AI from conference room");

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Check if AI is attached
    if !room.has_ai() {
        return Err(ApiError::NotFound("No AI attached to room".to_string()));
    }

    // Detach AI in a background task to avoid Handler trait issues
    // (There's a known issue with awaiting ConferenceAIManager::stop() in handlers)
    let room_clone = Arc::clone(&room);
    tokio::spawn(async move {
        if let Err(e) = room_clone.detach_ai().await {
            tracing::error!("Failed to detach AI in background task: {}", e);
        }
    });

    Ok(no_content())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attach_request_validation() {
        // Valid request
        let valid = AttachConferenceAIRequest {
            api_key: "sk-test".to_string(),
            model: None,
            voice: None,
            endpoint: None,
            instructions: None,
            temperature: Some(0.8),
            audio_mode: None,
            enable_transcription: None,
        };
        assert!(valid.validate().is_ok());

        // Invalid temperature
        let invalid = AttachConferenceAIRequest {
            api_key: "sk-test".to_string(),
            model: None,
            voice: None,
            endpoint: None,
            instructions: None,
            temperature: Some(1.5), // Out of range
            audio_mode: None,
            enable_transcription: None,
        };
        assert!(invalid.validate().is_err());

        // Empty API key
        let invalid = AttachConferenceAIRequest {
            api_key: "".to_string(),
            model: None,
            voice: None,
            endpoint: None,
            instructions: None,
            temperature: None,
            audio_mode: None,
            enable_transcription: None,
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn test_conference_ai_response_serialization() {
        let response = ConferenceAIResponse {
            room_id: "test-room".to_string(),
            state: "Active".to_string(),
            model: "gpt-4o-realtime-preview-2024-12-17".to_string(),
            voice: Some("alloy".to_string()),
            audio_mode: "Mixed".to_string(),
            enable_transcription: false,
            participants_heard: vec!["alice".to_string(), "bob".to_string()],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("test-room"));
        assert!(json.contains("Active"));
    }
}
