//! Conference and recording endpoints

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use validator::Validate;

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
}

/// Request to start recording
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct StartRecordingRequest {
    #[validate(length(min = 1, max = 1024))]
    pub output_path: String,
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

/// Create routes for conference and recording management
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/conferences", get(list_rooms).post(create_room))
        .route("/v1/conferences/:room_id", get(get_room).delete(delete_room))
        .route("/v1/conferences/:room_id/participants", post(add_participant))
        .route("/v1/conferences/:room_id/participants/:participant_id", delete(remove_participant))
        .route("/v1/conferences/:room_id/recording", post(start_recording).delete(stop_recording))
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
    request.validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Create room
    let room = state.conference_bridge
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

    let room = state.conference_bridge
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

    state.conference_bridge
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
    request.validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Add participant
    state.conference_bridge
        .add_participant_to_room(&room_id, &request.participant_id)
        .map_err(|e| ApiError::Internal(format!("Failed to add participant: {}", e)))?;

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

    state.conference_bridge
        .remove_participant_from_room(&room_id, &participant_id)
        .map_err(|e| ApiError::Internal(format!("Failed to remove participant: {}", e)))?;

    Ok(no_content())
}

/// Start recording a conference room
///
/// POST /v1/conferences/:room_id/recording
#[tracing::instrument(skip(state, request), fields(room_id = %room_id, output_path = ?request.output_path))]
async fn start_recording(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    Json(request): Json<StartRecordingRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to start recording conference room");

    // Validate request
    request.validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    // Get room
    let room = state.conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Start recording
    room.start_recording(&request.output_path)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to start recording: {}", e)))?;

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
    let room = state.conference_bridge
        .get_room(&room_id)
        .map_err(|e| ApiError::RoomNotFound(format!("Room not found: {}", e)))?;

    // Stop recording
    room.stop_recording()
        .map_err(|e| ApiError::Internal(format!("Failed to stop recording: {}", e)))?;

    Ok(no_content())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use forge_media_processor::AudioFormat;
    use tower::ServiceExt;

    fn create_test_state() -> Arc<AppState> {
        let bridge = forge_media_processor::conference::ConferenceBridge::new(
            AudioFormat::pcm_mono(), 480
        ).unwrap();
        let session_manager = forge_engine::SessionManager::new(
            Default::default(), None
        );
        let metrics_handle = super::prometheus::MetricsHandle::init();
        Arc::new(AppState::new(
            Arc::new(session_manager),
            Arc::new(metrics_handle),
            Arc::new(bridge),
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
