//! AI integration endpoints for media sessions

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forge_core::CallId;
use forge_engine::{AISessionConfig, AISessionState};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::response::{created, no_content, success, ApiSuccess};
use crate::routes::sessions::AppState;

/// Request to attach AI to a session
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct AttachAIRequest {
    /// AI service API key
    #[validate(length(min = 1, max = 512))]
    pub api_key: String,
    /// AI model to use (defaults to "gpt-4o-realtime-preview")
    #[validate(length(min = 1, max = 256))]
    pub model: Option<String>,
    /// Voice/persona for AI (defaults to "alloy")
    #[validate(length(min = 1, max = 64))]
    pub voice: Option<String>,
    /// System instructions for AI behavior
    #[validate(length(max = 4096))]
    pub instructions: Option<String>,
    /// Temperature for response generation (0.0-1.0)
    #[validate(range(min = 0.0, max = 1.0))]
    pub temperature: Option<f32>,
    /// Enable Voice Activity Detection (defaults to true)
    pub enable_vad: Option<bool>,
    /// Enable barge-in detection (defaults to true)
    pub enable_barge_in: Option<bool>,
}

/// AI session status response
#[derive(Debug, Serialize, Deserialize)]
pub struct AISessionResponse {
    pub call_id: String,
    pub state: String,
    pub model: String,
    pub voice: Option<String>,
    pub enable_vad: bool,
    pub enable_barge_in: bool,
}

/// Function call response request
#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct FunctionResponseRequest {
    /// Function call ID (from FunctionCall event)
    #[validate(length(min = 1, max = 256))]
    pub call_id: String,
    /// Function output/result as JSON string
    #[validate(length(max = 65536))]
    pub output: String,
}

/// Attach AI to a media session
///
/// POST /v1/sessions/:id/ai
#[tracing::instrument(skip(state, request), fields(call_id = %call_id))]
async fn attach_ai(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
    Json(request): Json<AttachAIRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to attach AI to session");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    let call_id = CallId(call_id.clone());

    // Verify session exists
    let session = state
        .session_manager
        .get_session(&call_id)
        .ok_or_else(|| ApiError::SessionNotFound(call_id.0.clone()))?;

    // Set AI manager on the session so forwarding loop can access it
    session
        .set_ai_manager(Arc::clone(&state.ai_session_manager))
        .await;

    // Get EventBus from session for DTMF integration
    let event_bus = session.event_bus().cloned();

    // Create AI session config
    let config = AISessionConfig {
        connector_type: forge_ai_stream::AIConnectorType::OpenAI,
        api_key: request.api_key,
        endpoint: None,
        model: request.model.unwrap_or_else(|| "gpt-4o-realtime-preview".to_string()),
        voice: request.voice.or_else(|| Some("alloy".to_string())),
        temperature: request.temperature,
        instructions: request.instructions,
        enable_vad: request.enable_vad.unwrap_or(true),
        enable_barge_in: request.enable_barge_in.unwrap_or(true),
        sample_rate: 16000, // TODO: Get from session codec config
    };

    // Attach AI to session with EventBus for DTMF integration
    state
        .ai_session_manager
        .attach_ai(call_id.clone(), config.clone(), event_bus)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to attach AI: {}", e)))?;

    let ai_state = state
        .ai_session_manager
        .get_state(&call_id)
        .await
        .unwrap_or(AISessionState::Active);

    let response = AISessionResponse {
        call_id: call_id.0,
        state: format!("{:?}", ai_state),
        model: config.model,
        voice: config.voice,
        enable_vad: config.enable_vad,
        enable_barge_in: config.enable_barge_in,
    };

    tracing::info!("AI attached to session: {}", response.call_id);

    Ok(created(response))
}

/// Get AI session status
///
/// GET /v1/sessions/:id/ai
#[tracing::instrument(skip(state), fields(call_id = %call_id))]
async fn get_ai_status(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<ApiSuccess<AISessionResponse>> {
    tracing::debug!("API request to get AI session status");

    let call_id = CallId(call_id.clone());

    // Verify AI session exists
    if !state.ai_session_manager.has_ai(&call_id) {
        return Err(ApiError::NotFound(format!(
            "No AI session found for call {}",
            call_id.0
        )));
    }

    let ai_state = state
        .ai_session_manager
        .get_state(&call_id)
        .await
        .unwrap_or(AISessionState::Terminated);

    // TODO: Store and retrieve config details
    let response = AISessionResponse {
        call_id: call_id.0,
        state: format!("{:?}", ai_state),
        model: "gpt-4o-realtime-preview".to_string(), // TODO: Get from stored config
        voice: Some("alloy".to_string()),              // TODO: Get from stored config
        enable_vad: true,                              // TODO: Get from stored config
        enable_barge_in: true,                         // TODO: Get from stored config
    };

    Ok(success(response))
}

/// Detach AI from a media session
///
/// DELETE /v1/sessions/:id/ai
#[tracing::instrument(skip(state), fields(call_id = %call_id))]
async fn detach_ai(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to detach AI from session");

    let call_id = CallId(call_id.clone());

    // Detach AI from session
    state
        .ai_session_manager
        .detach_ai(&call_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to detach AI: {}", e)))?;

    tracing::info!("AI detached from session: {}", call_id.0);

    Ok(no_content())
}

/// Send function call response to AI
///
/// POST /v1/sessions/:id/ai/function-response
#[tracing::instrument(skip(state, request), fields(call_id = %call_id))]
async fn send_function_response(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
    Json(request): Json<FunctionResponseRequest>,
) -> ApiResult<axum::response::Response> {
    tracing::info!("API request to send function response to AI");

    // Validate request
    request
        .validate()
        .map_err(|e| ApiError::InvalidRequest(format!("Validation failed: {}", e)))?;

    let call_id = CallId(call_id.clone());

    // Send function response to AI
    state
        .ai_session_manager
        .send_function_response(&call_id, &request.call_id, request.output)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to send function response: {}", e)))?;

    tracing::info!("Function response sent to AI for session: {}", call_id.0);

    Ok(no_content())
}

/// Create AI routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/sessions/:id/ai", post(attach_ai))
        .route("/v1/sessions/:id/ai", get(get_ai_status))
        .route("/v1/sessions/:id/ai", delete(detach_ai))
        .route(
            "/v1/sessions/:id/ai/function-response",
            post(send_function_response),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use forge_engine::SessionManager;
    use tower::util::ServiceExt as _;

    fn test_state() -> Arc<AppState> {
        let base = 60000 + ((std::process::id() % 5000) * 2) as u16;
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
            Arc::new(forge_core::EventBus::new()),
        ))
    }

    #[tokio::test]
    async fn test_attach_ai_session_not_found() {
        let app = routes().with_state(test_state());

        let request_body = serde_json::json!({
            "api_key": "sk-test-key",
            "model": "gpt-4o-realtime-preview"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/sessions/nonexistent-session/ai")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_ai_status_not_found() {
        let app = routes().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions/nonexistent-session/ai")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
