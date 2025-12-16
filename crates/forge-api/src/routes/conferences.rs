//! Conference and recording endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use validator::Validate;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::response::{created, no_content, success, ApiSuccess};
use crate::routes::sessions::AppState;

/// Request to create a new conference room
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct CreateRoomRequest {
    #[validate(length(min = 1, max = 256))]
    pub room_id: String,
}

/// Request to add a participant to a room
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct AddParticipantRequest {
    #[validate(length(min = 1, max = 256))]
    pub participant_id: String,
    /// Whether this participant should join as a host/moderator
    #[serde(default)]
    pub is_host: bool,
}

/// Request to start recording
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct StartRecordingRequest {
    #[validate(length(min = 1, max = 1024))]
    pub output_path: String,
    /// Audio codec for recording (optional, defaults to "pcm" for WAV)
    /// Supported values: "pcm" (WAV), "opus" (requires opus feature)
    pub codec: Option<String>,
}

/// Request to start participant recording
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct StartParticipantRecordingRequest {
    #[validate(length(min = 1, max = 256))]
    pub participant_id: String,
    #[validate(length(min = 1, max = 1024))]
    pub output_path: String,
    // Note: Participant recordings use the room's audio format
}

/// Request to play an announcement into a conference
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct PlayAnnouncementRequest {
    #[validate(length(min = 1, max = 1024))]
    pub prompt: String,
}

/// Request to update participant state
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct UpdateParticipantStateRequest {
    /// New state: "active", "muted", or "on_hold"
    #[validate(length(min = 1, max = 32))]
    pub state: String,
}

/// Request to configure a room
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigureRoomRequest {
    /// Room-specific guest PIN (overrides global)
    pub guest_pin: Option<String>,
    /// Room-specific host PIN (overrides global)
    pub host_pin: Option<String>,
    /// Require guest PIN for this room (overrides global)
    pub require_guest_pin: Option<bool>,
    /// Maximum PIN attempts for this room (overrides global)
    pub max_pin_attempts: Option<u32>,
    /// Lock this room by default (overrides global)
    pub default_locked: Option<bool>,
    /// Enable DTMF commands for this room (overrides global)
    pub enable_dtmf: Option<bool>,
    /// Maximum channels for this room (overrides global)
    pub max_channels: Option<usize>,
    /// Wait for moderator for this room (overrides global)
    pub wait_for_moderator: Option<bool>,
    /// Minimum users for this room (overrides global)
    pub min_users: Option<usize>,
    /// Minimum recording participants for this room (overrides global)
    pub min_recording_participants: Option<usize>,
}

/// Response with room configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct RoomConfigResponse {
    pub room_id: String,
    pub require_guest_pin: bool,
    pub has_guest_pin: bool,
    pub has_host_pin: bool,
    pub max_pin_attempts: u32,
    pub default_locked: bool,
    pub enable_dtmf: bool,
    pub max_channels: usize,
    pub wait_for_moderator: bool,
    pub min_users: usize,
    pub min_recording_participants: usize,
}

/// Response with participant details including host status
#[derive(Debug, Serialize, Deserialize)]
pub struct ParticipantDetailResponse {
    pub id: String,
    pub is_host: bool,
}

/// List of participants with details
#[derive(Debug, Serialize, Deserialize)]
pub struct ParticipantDetailListResponse {
    pub participants: Vec<ParticipantDetailResponse>,
    pub count: usize,
}

/// Request to promote a participant to host
#[derive(Debug, Serialize, Deserialize)]
pub struct PromoteParticipantRequest {
    /// PIN for host authentication (required)
    pub host_pin: String,
}

/// Participant metadata response
#[derive(Debug, Serialize, Deserialize)]
pub struct ParticipantMetadataResponse {
    pub id: String,
    pub join_time_ms: u128,
    pub state: String,
    pub gain: f32,
    pub is_recording: bool,
    pub packets_received: u64,
    pub is_speaking: bool,
    pub last_speech_ms: Option<u128>,
}

/// List of participant metadata response
#[derive(Debug, Serialize, Deserialize)]
pub struct ParticipantMetadataListResponse {
    pub participants: Vec<ParticipantMetadataResponse>,
    pub count: usize,
}

/// Conference room information response
#[derive(Debug, Serialize, Deserialize)]
pub struct RoomResponse {
    pub room_id: String,
    pub participant_count: usize,
    pub participants: Vec<String>,
    pub is_recording: bool,
}

/// List of conference rooms response
#[derive(Debug, Serialize, Deserialize)]
pub struct RoomListResponse {
    pub rooms: Vec<RoomResponse>,
    pub count: usize,
}

/// Recording information response
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingResponse {
    pub id: String,
    pub room_id: String,
    pub participant_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub size_bytes: u64,
    pub duration_secs: f64,
}

/// List of recordings response
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingListResponse {
    pub recordings: Vec<RecordingResponse>,
    pub count: usize,
}

/// Create routes for conference and recording management
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/conferences", get(list_rooms).post(create_room))
        .route(
            "/v1/conferences/:room_id",
            get(get_room).delete(delete_room),
        )
        .route(
            "/v1/conferences/:room_id/configure",
            post(configure_room),
        )
        .route(
            "/v1/conferences/:room_id/config",
            get(get_room_config),
        )
        .route(
            "/v1/conferences/:room_id/participants",
            post(add_participant).get(list_participants_with_status),
        )
        .route(
            "/v1/conferences/:room_id/participants/metadata",
            get(get_all_participants_metadata),
        )
        .route(
            "/v1/conferences/:room_id/participants/:participant_id",
            delete(remove_participant),
        )
        .route(
            "/v1/conferences/:room_id/participants/:participant_id/metadata",
            get(get_participant_metadata),
        )
        .route(
            "/v1/conferences/:room_id/participants/:participant_id/state",
            axum::routing::put(update_participant_state),
        )
        .route(
            "/v1/conferences/:room_id/participants/:participant_id/promote",
            post(promote_participant),
        )
        .route(
            "/v1/conferences/:room_id/waiting",
            get(list_waiting_participants),
        )
        .route(
            "/v1/conferences/:room_id/recording",
            post(start_recording).delete(stop_recording),
        )
        .route(
            "/v1/conferences/:room_id/participant-recording",
            post(start_participant_rec).delete(stop_participant_rec),
        )
        .route(
            "/v1/conferences/:room_id/announcement",
            post(play_announcement),
        )
        .route("/v1/recordings", get(list_recordings))
        .route(
            "/v1/recordings/:id",
            get(get_recording).delete(delete_recording),
        )
}

/// List all conference rooms
///
/// GET /v1/conferences
#[tracing::instrument(skip(state))]
async fn list_rooms(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<ApiSuccess<RoomListResponse>>> {
    tracing::info!("API request to list conference rooms");

    let room_ids = state.conference_bridge.list_rooms();
    let mut rooms = Vec::new();

    for room_id in room_ids {
        match state.conference_bridge.get_room(&room_id) {
            Ok(room) => {
                rooms.push(RoomResponse {
                    room_id: room.id().to_string(),
                    participant_count: room.participant_count(),
                    participants: room.participants(),
                    is_recording: room.is_recording(),
                });
            }
            Err(e) => {
                tracing::warn!("Failed to get room {}: {}", room_id, e);
            }
        }
    }

    let response = RoomListResponse {
        count: rooms.len(),
        rooms,
    };

    Ok(Json(success(response)))
}

/// Create a new conference room
///
/// POST /v1/conferences
#[tracing::instrument(skip(state, request), fields(room_id = ?request.room_id))]
async fn create_room(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateRoomRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to create conference room");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Create room
    let room = state
        .conference_bridge
        .create_room(&request.room_id, None)
        .map_err(|e| ApiError::Internal(format!("Failed to create room: {}", e)))?;

    let response = RoomResponse {
        room_id: room.id().to_string(),
        participant_count: 0,
        participants: vec![],
        is_recording: false,
    };

    Ok(created(response))
}

/// Get conference room information
///
/// GET /v1/conferences/:room_id
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn get_room(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<Json<ApiSuccess<RoomResponse>>> {
    tracing::info!("API request to get conference room");

    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    let response = RoomResponse {
        room_id: room.id().to_string(),
        participant_count: room.participant_count(),
        participants: room.participants(),
        is_recording: room.is_recording(),
    };

    Ok(Json(success(response)))
}

/// Delete a conference room
///
/// DELETE /v1/conferences/:room_id
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn delete_room(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to delete conference room");

    state
        .conference_bridge
        .delete_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    Ok(no_content())
}

/// Add a participant to a conference room
///
/// POST /v1/conferences/:room_id/participants
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, participant_id = ?request.participant_id))]
async fn add_participant(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<AddParticipantRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to add participant to conference room");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Add participant
    state
        .conference_bridge
        .add_participant_to_room(&room_id, &request.participant_id, request.is_host)
        .map_err(|e| ApiError::Internal(format!("Failed to add participant: {}", e)))?;

    // Publish ParticipantJoined event
    let event = crate::ConferenceEvent::ParticipantJoined {
        room_id: room_id.clone(),
        participant_id: request.participant_id.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// Play an announcement prompt into a conference room
///
/// POST /v1/conferences/:room_id/announcement
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, prompt = ?request.prompt))]
async fn play_announcement(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<PlayAnnouncementRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to play announcement into conference room");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Resolve prompt path inside configured prompts directory
    let prompt_path = resolve_prompt_path(&state.prompts_base_dir, &request.prompt)
        .map_err(ApiError::InvalidRequest)?;

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Play announcement
    room.play_announcement(prompt_path)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to play announcement: {}", e)))?;

    Ok(no_content())
}

/// Remove a participant from a conference room
///
/// DELETE /v1/conferences/:room_id/participants/:participant_id
#[tracing::instrument(skip(state), fields(room_id = %room_id, participant_id = %participant_id))]
async fn remove_participant(
    State(state): State<Arc<AppState>>,
    Path((room_id, participant_id)): Path<(String, String)>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to remove participant from conference room");

    state
        .conference_bridge
        .remove_participant_from_room(&room_id, &participant_id)
        .map_err(|e| ApiError::Internal(format!("Failed to remove participant: {}", e)))?;

    // Publish ParticipantLeft event
    let event = crate::ConferenceEvent::ParticipantLeft {
        room_id: room_id.clone(),
        participant_id: participant_id.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// Start recording a conference room
///
/// POST /v1/conferences/:room_id/recording
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, output_path = ?request.output_path, codec = ?request.codec))]
async fn start_recording(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<StartRecordingRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to start recording conference room");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Parse codec if specified
    let format = if let Some(codec_str) = &request.codec {
        let codec = forge_media_processor::AudioCodec::from_str(codec_str)
            .ok_or_else(|| ApiError::InvalidRequest(format!("Invalid codec: {}", codec_str)))?;

        // Get room's current format and override codec
        let room_format = state
            .conference_bridge
            .get_room(&room_id)
            .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?
            .format();

        Some(forge_media_processor::AudioFormat {
            sample_rate: room_format.sample_rate,
            channels: room_format.channels,
            codec,
        })
    } else {
        None
    };

    // Start recording
    let output_path = resolve_recording_path(&state.recording_base_dir, &request.output_path)
        .map_err(|e| ApiError::InvalidRequest(e))?;

    room.start_recording(&output_path, format)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to start recording: {}", e)))?;

    // Register recording metadata
    let recording_id = Uuid::new_v4().to_string();
    state
        .storage_manager
        .lock()
        .await
        .register_recording(forge_storage::RecordingInfo::new(
            recording_id,
            output_path,
            room_id.clone(),
            None,
        ));

    // Publish RecordingStarted event
    let event = crate::ConferenceEvent::RecordingStarted {
        room_id: room_id.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// Stop recording a conference room
///
/// DELETE /v1/conferences/:room_id/recording
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn stop_recording(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to stop recording conference room");

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Stop recording
    room.stop_recording()
        .map_err(|e| ApiError::Internal(format!("Failed to stop recording: {}", e)))?;

    // Finalize recording metadata
    state
        .storage_manager
        .lock()
        .await
        .finalize_room_recording(&room_id)
        .await
        .map_err(|e| ApiError::RecordingNotFound(format!("Recording not found: {}", e)))?;

    // Publish RecordingStopped event
    let event = crate::ConferenceEvent::RecordingStopped {
        room_id: room_id.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// Start recording for a specific participant
///
/// POST /v1/conferences/:room_id/participant-recording
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, participant_id = %request.participant_id))]
async fn start_participant_rec(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<StartParticipantRecordingRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!(
        "API request to start participant recording for {} in {}",
        request.participant_id,
        room_id
    );

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation error: {}", e)))?;

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Verify participant exists in room
    if !room.participants().contains(&request.participant_id) {
        return Err(ApiError::InvalidRequest(format!(
            "Participant {} not found in room {}",
            request.participant_id, room_id
        )));
    }

    // Resolve output path with security checks
    let output_path = resolve_recording_path(&state.recording_base_dir, &request.output_path)
        .map_err(|e| ApiError::InvalidRequest(e))?;

    // Start participant recording
    room.start_participant_recording(&request.participant_id, &output_path)
        .await
        .map_err(|e| {
            ApiError::Internal(format!(
                "Failed to start participant recording: {}",
                e
            ))
        })?;

    // Register recording metadata
    let recording_id = Uuid::new_v4().to_string();
    state
        .storage_manager
        .lock()
        .await
        .register_recording(forge_storage::RecordingInfo::new(
            recording_id,
            output_path,
            room_id.clone(),
            Some(request.participant_id.clone()),
        ));

    // Publish ParticipantRecordingStarted event
    let event = crate::ConferenceEvent::ParticipantRecordingStarted {
        room_id: room_id.clone(),
        participant_id: request.participant_id.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// Stop recording for a specific participant
///
/// DELETE /v1/conferences/:room_id/participant-recording
#[tracing::instrument(skip(state, request), fields(room_id = %room_id))]
async fn stop_participant_rec(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<serde_json::Value>,
) -> ApiResult<axum::response::Response> {
    // Extract participant_id from JSON body
    let participant_id = request
        .get("participant_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::InvalidRequest("participant_id is required".to_string()))?;

    tracing::info!(
        "API request to stop participant recording for {} in {}",
        participant_id,
        room_id
    );

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Stop participant recording
    room.stop_participant_recording(participant_id)
        .map_err(|e| {
            // Map "not found" errors to 404 instead of 500
            let err_msg = e.to_string();
            if err_msg.contains("not found") || err_msg.contains("Participant") {
                ApiError::InvalidRequest(format!(
                    "Participant {} not found in room {}",
                    participant_id, room_id
                ))
            } else {
                ApiError::Internal(format!(
                    "Failed to stop participant recording: {}",
                    e
                ))
            }
        })?;

    // Finalize recording metadata
    state
        .storage_manager
        .lock()
        .await
        .finalize_participant_recording(&room_id, participant_id)
        .await
        .map_err(|e| ApiError::RecordingNotFound(format!("Recording not found: {}", e)))?;

    // Publish ParticipantRecordingStopped event
    let event = crate::ConferenceEvent::ParticipantRecordingStopped {
        room_id: room_id.clone(),
        participant_id: participant_id.to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// List all recordings
///
/// GET /v1/recordings
#[tracing::instrument(skip(state))]
async fn list_recordings(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<ApiSuccess<RecordingListResponse>>> {
    tracing::info!("API request to list recordings");

    let storage = state.storage_manager.lock().await;
    let recordings: Vec<RecordingResponse> = storage
        .list_recordings()
        .iter()
        .map(|r| recording_info_to_response(r))
        .collect();

    let response = RecordingListResponse {
        count: recordings.len(),
        recordings,
    };

    Ok(Json(success(response)))
}

/// Get recording information
///
/// GET /v1/recordings/:id
#[tracing::instrument(skip(state), fields(recording_id = %recording_id))]
async fn get_recording(
    State(state): State<Arc<AppState>>,
    Path(recording_id): Path<String>,
) -> ApiResult<Json<ApiSuccess<RecordingResponse>>> {
    tracing::info!("API request to get recording info");

    let storage = state.storage_manager.lock().await;
    let recording = storage.get_recording(&recording_id).ok_or_else(|| {
        ApiError::RecordingNotFound(format!("Recording {} not found", recording_id))
    })?;

    let response = recording_info_to_response(recording);

    Ok(Json(success(response)))
}

/// Delete a recording
///
/// DELETE /v1/recordings/:id
#[tracing::instrument(skip(state), fields(recording_id = %recording_id))]
async fn delete_recording(
    State(state): State<Arc<AppState>>,
    Path(recording_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to delete recording");

    state
        .storage_manager
        .lock()
        .await
        .delete_recording(&recording_id)
        .await
        .map_err(|e| ApiError::RecordingNotFound(format!("Recording not found: {}", e)))?;

    Ok(no_content())
}

/// Convert RecordingInfo to RecordingResponse
fn recording_info_to_response(info: &forge_storage::RecordingInfo) -> RecordingResponse {
    RecordingResponse {
        id: info.id.clone(),
        room_id: info.room_id.clone(),
        participant_id: info.participant_id.clone(),
        started_at: info
            .started_at
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "unknown".to_string()),
        ended_at: info.ended_at.and_then(|t| {
            t.duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs().to_string())
        }),
        size_bytes: info.size_bytes,
        duration_secs: info.duration_secs,
    }
}

/// Resolve a user-supplied recording path against the configured base directory.
/// Rejects absolute paths, parent-directory traversal, and overwriting existing files.
fn resolve_recording_path(base_dir: &FsPath, requested: &str) -> Result<PathBuf, String> {
    use std::path::Component;

    if requested.trim().is_empty() {
        return Err("output_path cannot be empty".to_string());
    }

    let mut sanitized = PathBuf::from(base_dir);
    for comp in FsPath::new(requested).components() {
        match comp {
            Component::Normal(c) => sanitized.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(
                    "output_path cannot traverse outside the recording base directory".to_string(),
                );
            }
            _ => {
                return Err("absolute paths are not allowed for output_path".to_string());
            }
        }
    }

    // Prevent overwriting existing files
    if sanitized.exists() {
        return Err(format!("recording already exists at {:?}", sanitized));
    }

    // Ensure parent directory exists
    if let Some(parent) = sanitized.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create recording directory: {}", e))?;
    }

    Ok(sanitized)
}

/// Resolve a prompt path against the prompts directory.
/// Rejects traversal and requires the file to exist.
fn resolve_prompt_path(base_dir: &FsPath, requested: &str) -> Result<PathBuf, String> {
    use std::path::Component;

    if requested.trim().is_empty() {
        return Err("prompt path cannot be empty".to_string());
    }

    let mut sanitized = PathBuf::from(base_dir);
    for comp in FsPath::new(requested).components() {
        match comp {
            Component::Normal(c) => sanitized.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("prompt path cannot traverse outside the prompts directory".to_string());
            }
            _ => return Err("absolute paths are not allowed for prompt path".to_string()),
        }
    }

    if !sanitized.exists() {
        return Err(format!("prompt file not found at {:?}", sanitized));
    }

    Ok(sanitized)
}

/// Get metadata for a specific participant
///
/// GET /v1/conferences/:room_id/participants/:participant_id/metadata
#[tracing::instrument(skip(state), fields(room_id = %room_id, participant_id = %participant_id))]
async fn get_participant_metadata(
    State(state): State<Arc<AppState>>,
    Path((room_id, participant_id)): Path<(String, String)>,
) -> ApiResult<Json<ApiSuccess<ParticipantMetadataResponse>>> {
    tracing::info!(
        "API request to get metadata for participant {} in room {}",
        participant_id,
        room_id
    );

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Get participant metadata
    let metadata = room
        .get_participant_metadata(&participant_id)
        .map_err(|e| {
            let err_msg = e.to_string();
            if err_msg.contains("not found") || err_msg.contains("Participant") {
                ApiError::InvalidRequest(format!(
                    "Participant {} not found in room {}",
                    participant_id, room_id
                ))
            } else {
                ApiError::Internal(format!("Failed to get participant metadata: {}", e))
            }
        })?;

    // Convert to response
    let state_str = match metadata.state {
        forge_conference_processor::ParticipantState::Active => "active",
        forge_conference_processor::ParticipantState::Muted => "muted",
        forge_conference_processor::ParticipantState::OnHold => "on_hold",
    };

    let response = ParticipantMetadataResponse {
        id: metadata.id,
        join_time_ms: metadata.join_time.elapsed().as_millis(),
        state: state_str.to_string(),
        gain: metadata.gain,
        is_recording: metadata.is_recording,
        packets_received: metadata.packets_received,
        is_speaking: metadata.is_speaking,
        last_speech_ms: metadata.last_speech_detected.map(|t| t.elapsed().as_millis()),
    };

    Ok(Json(success(response)))
}

/// Get metadata for all participants in a room
///
/// GET /v1/conferences/:room_id/participants/metadata
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn get_all_participants_metadata(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<Json<ApiSuccess<ParticipantMetadataListResponse>>> {
    tracing::info!(
        "API request to get metadata for all participants in room {}",
        room_id
    );

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Get all participant metadata
    let metadata_list = room.get_all_participant_metadata();

    // Convert to response
    let participants: Vec<ParticipantMetadataResponse> = metadata_list
        .into_iter()
        .map(|metadata| {
            let state_str = match metadata.state {
                forge_conference_processor::ParticipantState::Active => "active",
                forge_conference_processor::ParticipantState::Muted => "muted",
                forge_conference_processor::ParticipantState::OnHold => "on_hold",
            };

            ParticipantMetadataResponse {
                id: metadata.id,
                join_time_ms: metadata.join_time.elapsed().as_millis(),
                state: state_str.to_string(),
                gain: metadata.gain,
                is_recording: metadata.is_recording,
                packets_received: metadata.packets_received,
                is_speaking: metadata.is_speaking,
                last_speech_ms: metadata.last_speech_detected.map(|t| t.elapsed().as_millis()),
            }
        })
        .collect();

    let count = participants.len();
    let response = ParticipantMetadataListResponse {
        participants,
        count,
    };

    Ok(Json(success(response)))
}

/// Update participant state
///
/// PUT /v1/conferences/:room_id/participants/:participant_id/state
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, participant_id = %participant_id))]
async fn update_participant_state(
    State(state): State<Arc<AppState>>,
    Path((room_id, participant_id)): Path<(String, String)>,
    Json(request): Json<UpdateParticipantStateRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!(
        "API request to update state for participant {} in room {} to {}",
        participant_id,
        room_id,
        request.state
    );

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation error: {}", e)))?;

    // Parse state
    let state_enum = match request.state.to_lowercase().as_str() {
        "active" => forge_conference_processor::ParticipantState::Active,
        "muted" => forge_conference_processor::ParticipantState::Muted,
        "on_hold" | "onhold" => forge_conference_processor::ParticipantState::OnHold,
        _ => {
            return Err(ApiError::InvalidRequest(format!(
                "Invalid state '{}'. Must be 'active', 'muted', or 'on_hold'",
                request.state
            )))
        }
    };

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Update participant state
    room.set_participant_state(&participant_id, state_enum)
        .map_err(|e| {
            let err_msg = e.to_string();
            if err_msg.contains("not found") || err_msg.contains("Participant") {
                ApiError::InvalidRequest(format!(
                    "Participant {} not found in room {}",
                    participant_id, room_id
                ))
            } else {
                ApiError::Internal(format!("Failed to update participant state: {}", e))
            }
        })?;

    // Publish ParticipantStateChanged event
    let event = crate::ConferenceEvent::ParticipantStateChanged {
        room_id: room_id.clone(),
        participant_id: participant_id.clone(),
        state: request.state.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };
    state.event_bus.publish(event).await;

    Ok(no_content())
}

/// Configure a room with room-specific settings
///
/// POST /v1/conferences/:room_id/configure
#[tracing::instrument(skip(state, request), fields(room_id = %room_id))]
async fn configure_room(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<ConfigureRoomRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to configure room {}", room_id);

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Build RoomConfig from request
    let room_config = forge_conference_processor::RoomConfig {
        guest_pin: request.guest_pin,
        host_pin: request.host_pin,
        require_guest_pin: request.require_guest_pin,
        max_pin_attempts: request.max_pin_attempts,
        default_locked: request.default_locked,
        enable_dtmf: request.enable_dtmf,
        dtmf_commands: None, // DTMF command bindings not exposed via API yet
        max_channels: request.max_channels,
        wait_for_moderator: request.wait_for_moderator,
        min_users: request.min_users,
        min_recording_participants: request.min_recording_participants,
        // Audio feedback sounds not exposed via API (use config file)
        join_sound: None,
        exit_sound: None,
        alone_sound: None,
        invalid_pin_sound: None,
        locked_sound: None,
        unlocked_sound: None,
        kicked_sound: None,
        max_members_sound: None,
        moh_sound: None,
        pin_prompt_sound: None,
        recording_started_sound: None,
        recording_stopped_sound: None,
    };

    // Load global config (this should come from AppState in a real implementation)
    // For now, use default global config
    let global_config = forge_conference_processor::ConferenceConfig::default();

    // Configure room
    room.configure(room_config, &global_config)
        .map_err(|e| ApiError::Internal(format!("Failed to configure room: {}", e)))?;

    Ok(no_content())
}

/// Get room configuration
///
/// GET /v1/conferences/:room_id/config
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn get_room_config(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<Json<ApiSuccess<RoomConfigResponse>>> {
    tracing::info!("API request to get config for room {}", room_id);

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Get effective config
    let config = room
        .get_effective_config()
        .ok_or_else(|| ApiError::Internal("Room not configured".to_string()))?;

    let response = RoomConfigResponse {
        room_id: room_id.clone(),
        require_guest_pin: config.security.require_guest_pin,
        has_guest_pin: config.security.guest_pin.is_some(),
        has_host_pin: config.security.host_pin.is_some(),
        max_pin_attempts: config.security.max_pin_attempts,
        default_locked: config.security.default_locked,
        enable_dtmf: config.dtmf_enabled,
        max_channels: config.max_channels,
        wait_for_moderator: config.wait_for_moderator,
        min_users: config.min_users,
        min_recording_participants: config.min_recording_participants,
    };

    Ok(Json(success(response)))
}

/// List participants with host status
///
/// GET /v1/conferences/:room_id/participants
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn list_participants_with_status(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<Json<ApiSuccess<ParticipantDetailListResponse>>> {
    tracing::info!("API request to list participants with status for room {}", room_id);

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Get all participants
    let participant_ids = room.participants();
    let mut participants = Vec::new();

    for participant_id in participant_ids {
        participants.push(ParticipantDetailResponse {
            id: participant_id.clone(),
            is_host: room.is_host(&participant_id),
        });
    }

    let count = participants.len();
    let response = ParticipantDetailListResponse {
        participants,
        count,
    };

    Ok(Json(success(response)))
}

/// List waiting participants
///
/// GET /v1/conferences/:room_id/waiting
#[tracing::instrument(skip(state), fields(room_id = %room_id))]
async fn list_waiting_participants(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
) -> ApiResult<Json<ApiSuccess<ParticipantDetailListResponse>>> {
    tracing::info!("API request to list waiting participants for room {}", room_id);

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Get waiting participants
    let waiting_ids = room.waiting_participants();
    let participants: Vec<ParticipantDetailResponse> = waiting_ids
        .into_iter()
        .map(|id| ParticipantDetailResponse {
            id,
            is_host: false, // Waiting participants are not hosts
        })
        .collect();

    let count = participants.len();
    let response = ParticipantDetailListResponse {
        participants,
        count,
    };

    Ok(Json(success(response)))
}

/// Promote a participant to host
///
/// POST /v1/conferences/:room_id/participants/:participant_id/promote
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, participant_id = %participant_id))]
async fn promote_participant(
    State(state): State<Arc<AppState>>,
    Path((room_id, participant_id)): Path<(String, String)>,
    Json(request): Json<PromoteParticipantRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!(
        "API request to promote participant {} to host in room {}",
        participant_id,
        room_id
    );

    // Get room
    let room = state
        .conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Authenticate with host PIN
    let auth_result = room.authenticate_participant(&participant_id, &request.host_pin);

    match auth_result {
        forge_conference_processor::PinAuthResult::HostAuthenticated => {
            // Promote participant to host
            room.promote_to_host(&participant_id)
                .map_err(|e| ApiError::Internal(format!("Failed to promote participant: {}", e)))?;

            tracing::info!("Promoted participant {} to host in room {}", participant_id, room_id);
            Ok(no_content())
        }
        _ => {
            tracing::warn!(
                "Failed to promote participant {} in room {}: invalid host PIN",
                participant_id,
                room_id
            );
            Err(ApiError::Unauthorized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn create_test_state() -> Arc<AppState> {
        let bridge = Arc::new(
            forge_conference_processor::ConferenceBridge::new(
                forge_media_processor::AudioFormat::pcm_mono(),
                480,
            )
            .unwrap(),
        );
        let session_manager = forge_engine::SessionManager::new(Default::default(), None);
        let metrics_handle = Arc::new(crate::routes::prometheus::MetricsHandle::init());
        Arc::new(AppState::new(
            session_manager,
            metrics_handle,
            bridge,
            std::env::temp_dir().join("forge-test-recordings"),
            std::env::temp_dir().join("forge-test-prompts"),
            Arc::new(forge_core::EventBus::new()),
        ))
    }

    #[tokio::test]
    async fn test_create_and_get_room() {
        let state = create_test_state();
        let app = routes().with_state(state.clone());

        // Create room
        let request = Request::builder()
            .method("POST")
            .uri("/v1/conferences")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"room_id":"test-room"}"#))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Get room
        let request = Request::builder()
            .method("GET")
            .uri("/v1/conferences/test-room")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_list_rooms() {
        let state = create_test_state();
        state.conference_bridge.create_room("room-1", None).unwrap();
        state.conference_bridge.create_room("room-2", None).unwrap();

        let app = routes().with_state(state);

        let request = Request::builder()
            .method("GET")
            .uri("/v1/conferences")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
