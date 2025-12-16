//! WebSocket endpoint for real-time conference events
//!
//! Provides WebSocket connections for clients to receive real-time updates about
//! conference events such as participants joining/leaving, speaking status, and recording state.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::{routes::sessions::AppState, ConferenceEvent};

/// Create WebSocket routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/ws/events", get(websocket_handler))
}

/// WebSocket subscription request
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SubscriptionRequest {
    /// Subscribe to all conference events
    SubscribeGlobal,
    /// Subscribe to a specific room's events
    SubscribeRoom { room_id: String },
    /// Unsubscribe from current subscription
    Unsubscribe,
    /// Ping to keep connection alive
    Ping,
}

/// WebSocket subscription response
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SubscriptionResponse {
    /// Successfully subscribed to global events
    SubscribedGlobal,
    /// Successfully subscribed to room events
    SubscribedRoom { room_id: String },
    /// Successfully unsubscribed
    Unsubscribed,
    /// Pong response to ping
    Pong,
    /// Event notification
    Event { event: ConferenceEvent },
    /// Error occurred
    Error { message: String },
}

/// WebSocket handler for conference events
///
/// # Endpoint
/// GET /ws/events
///
/// # Protocol
/// Clients connect via WebSocket and send JSON messages to subscribe to events.
///
/// ## Subscribe to all events
/// ```json
/// {"type": "subscribe_global"}
/// ```
///
/// ## Subscribe to room events
/// ```json
/// {"type": "subscribe_room", "room_id": "room-123"}
/// ```
///
/// ## Unsubscribe
/// ```json
/// {"type": "unsubscribe"}
/// ```
///
/// ## Ping
/// ```json
/// {"type": "ping"}
/// ```
///
/// # Response Format
/// All responses are JSON with a "type" field:
///
/// ## Success
/// ```json
/// {"type": "subscribed_global"}
/// {"type": "subscribed_room", "room_id": "room-123"}
/// {"type": "pong"}
/// ```
///
/// ## Events
/// ```json
/// {
///   "type": "event",
///   "event": {
///     "type": "participant_joined",
///     "room_id": "room-123",
///     "participant_id": "alice",
///     "timestamp": 1234567890
///   }
/// }
/// ```
///
/// ## Errors
/// ```json
/// {"type": "error", "message": "Invalid subscription request"}
/// ```
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle WebSocket connection
async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let client_id = uuid::Uuid::new_v4();

    info!("WebSocket client {} connected", client_id);

    // Track current subscription
    let mut current_subscription: Option<tokio::sync::broadcast::Receiver<ConferenceEvent>> = None;

    loop {
        tokio::select! {
            // Handle incoming messages from client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!("Received message from client {}: {}", client_id, text);

                        match serde_json::from_str::<SubscriptionRequest>(&text) {
                            Ok(req) => {
                                let response = match req {
                                    SubscriptionRequest::SubscribeGlobal => {
                                        debug!("Client {} subscribing to global events", client_id);
                                        current_subscription = Some(state.event_bus.subscribe_global());
                                        SubscriptionResponse::SubscribedGlobal
                                    }
                                    SubscriptionRequest::SubscribeRoom { room_id } => {
                                        debug!("Client {} subscribing to room {}", client_id, room_id);
                                        current_subscription = Some(state.event_bus.subscribe_room(&room_id).await);
                                        SubscriptionResponse::SubscribedRoom { room_id }
                                    }
                                    SubscriptionRequest::Unsubscribe => {
                                        debug!("Client {} unsubscribing", client_id);
                                        current_subscription = None;
                                        SubscriptionResponse::Unsubscribed
                                    }
                                    SubscriptionRequest::Ping => {
                                        SubscriptionResponse::Pong
                                    }
                                };

                                if let Ok(json) = serde_json::to_string(&response) {
                                    if let Err(e) = socket.send(Message::Text(json)).await {
                                        error!("Failed to send response to client {}: {}", client_id, e);
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Invalid subscription request from client {}: {}", client_id, e);
                                let error_response = SubscriptionResponse::Error {
                                    message: format!("Invalid subscription request: {}", e),
                                };
                                if let Ok(json) = serde_json::to_string(&error_response) {
                                    let _ = socket.send(Message::Text(json)).await;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("Client {} closed connection", client_id);
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if let Err(e) = socket.send(Message::Pong(data)).await {
                            error!("Failed to send pong to client {}: {}", client_id, e);
                            break;
                        }
                    }
                    Some(Ok(_)) => {
                        // Ignore other message types
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error for client {}: {}", client_id, e);
                        break;
                    }
                    None => {
                        info!("Client {} disconnected", client_id);
                        break;
                    }
                }
            }
            // Handle events from subscription
            event = async {
                if let Some(ref mut rx) = current_subscription {
                    rx.recv().await.ok()
                } else {
                    std::future::pending().await
                }
            } => {
                if let Some(event) = event {
                    debug!("Forwarding event to client {}: {:?}", client_id, event);

                    let response = SubscriptionResponse::Event { event };
                    if let Ok(json) = serde_json::to_string(&response) {
                        if let Err(e) = socket.send(Message::Text(json)).await {
                            error!("Failed to send event to client {}: {}", client_id, e);
                            break;
                        }
                    }
                }
            }
        }
    }

    info!("WebSocket client {} disconnected", client_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use forge_conference_processor::ConferenceBridge;
    use forge_engine::SessionManager;
    use std::sync::Arc;

    fn create_test_state() -> Arc<AppState> {
        let session_manager_config = forge_engine::SessionManagerConfig::default();
        let session_manager = SessionManager::new(session_manager_config, None);
        let metrics_handle = Arc::new(crate::routes::prometheus::MetricsHandle::init());
        let conference_bridge = Arc::new(ConferenceBridge::default());

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

    fn create_test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/ws/events", axum::routing::get(websocket_handler))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_websocket_subscription_format() {
        // Test serialization/deserialization of subscription messages
        let subscribe_global = r#"{"type":"subscribe_global"}"#;
        let req: SubscriptionRequest = serde_json::from_str(subscribe_global).unwrap();
        assert!(matches!(req, SubscriptionRequest::SubscribeGlobal));

        let subscribe_room = r#"{"type":"subscribe_room","room_id":"test-room"}"#;
        let req: SubscriptionRequest = serde_json::from_str(subscribe_room).unwrap();
        match req {
            SubscriptionRequest::SubscribeRoom { room_id } => {
                assert_eq!(room_id, "test-room");
            }
            _ => panic!("Expected SubscribeRoom"),
        }

        let unsubscribe = r#"{"type":"unsubscribe"}"#;
        let req: SubscriptionRequest = serde_json::from_str(unsubscribe).unwrap();
        assert!(matches!(req, SubscriptionRequest::Unsubscribe));

        let ping = r#"{"type":"ping"}"#;
        let req: SubscriptionRequest = serde_json::from_str(ping).unwrap();
        assert!(matches!(req, SubscriptionRequest::Ping));
    }

    #[tokio::test]
    async fn test_websocket_response_format() {
        // Test serialization of response messages
        let response = SubscriptionResponse::SubscribedGlobal;
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("subscribed_global"));

        let response = SubscriptionResponse::SubscribedRoom {
            room_id: "test-room".to_string(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("subscribed_room"));
        assert!(json.contains("test-room"));

        let response = SubscriptionResponse::Pong;
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("pong"));

        let event = ConferenceEvent::ParticipantJoined {
            room_id: "test-room".to_string(),
            participant_id: "alice".to_string(),
            timestamp: 12345,
        };
        let response = SubscriptionResponse::Event { event };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("event"));
        assert!(json.contains("participant_joined"));
    }

    #[test]
    fn test_router_creation() {
        // Simple test to verify router can be created
        let state = create_test_state();
        let _router = create_test_router(state);
    }
}
