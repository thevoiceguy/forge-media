//! Session-scoped bidirectional media WebSocket endpoint.

use crate::error::ApiError;
use crate::middleware::auth::RequireOperator;
use crate::routes::sessions::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use forge_core::{CallId, DtmfDetectionMethod, DtmfEventKind, ForgeEvent};
use forge_engine::{MediaBridgeHandle, MediaTarget, OutboundMediaFrame, ParticipantLabel};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Create session media WebSocket routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/ws/sessions/{id}/media", get(websocket_handler))
}

/// Maximum size, in bytes, of a single client → server WebSocket message.
///
/// Defaults in `axum`/`tungstenite` are 64 MiB / 16 MiB, which an
/// authenticated operator client could weaponise into a per-message
/// allocation amplifier (base64 decode + PCM expand). 512 KiB is enough
/// envelope for any sane PCM frame (e.g. ~1 s of 48 kHz mono ≈ 128 KiB
/// after base64).
const MAX_WS_MESSAGE_BYTES: usize = 512 * 1024;

/// Maximum length of the base64-encoded `data` field in an inbound `Audio`
/// message. Decoding produces ~3/4 this many bytes, then `chunks_exact(2)`
/// allocates another ~1/2 — so a 256 KiB cap bounds the worst-case
/// allocation per frame to ~448 KiB.
const MAX_AUDIO_DATA_BASE64_LEN: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Ping,
    Audio {
        target: MediaTarget,
        sample_rate: u32,
        data: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Connected {
        call_id: String,
        state: String,
        sample_format: &'static str,
        channels: u8,
        participant_a: forge_engine::ParticipantMediaState,
        participant_b: forge_engine::ParticipantMediaState,
    },
    Pong,
    Audio {
        leg: ParticipantLabel,
        codec: forge_core::AudioCodec,
        payload_type: u8,
        sample_rate: u32,
        sequence_number: u16,
        timestamp: u32,
        data: String,
    },
    Dtmf {
        digit: char,
        method: DtmfDetectionMethod,
        event_type: DtmfEventKind,
    },
    SessionState {
        state: &'static str,
    },
    SessionTerminated {
        reason: String,
    },
    Error {
        message: String,
    },
}

/// GET /ws/sessions/{id}/media
pub async fn websocket_handler(
    _auth: RequireOperator,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let call_id = CallId::new(id);
    let session = state
        .session_manager
        .get_session(&call_id)
        .ok_or_else(|| ApiError::SessionNotFound(call_id.0.clone()))?;

    session
        .set_media_bridge_manager(Arc::clone(&state.media_bridge_manager))
        .await;

    let bridge = state
        .media_bridge_manager
        .attach_call(call_id.clone())
        .map_err(|err| match err {
            forge_core::ForgeError::ResourceLimit(message) => ApiError::Conflict(message),
            forge_core::ForgeError::SessionNotFound(message) => ApiError::SessionNotFound(message),
            other => ApiError::Internal(other.to_string()),
        })?;

    let participant_a = session.participant_media_state(ParticipantLabel::A).await;
    let participant_b = session.participant_media_state(ParticipantLabel::B).await;
    let state_name = format!("{:?}", session.state().await).to_ascii_lowercase();

    Ok(ws
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                state,
                call_id,
                bridge,
                participant_a,
                participant_b,
                state_name,
            )
        }))
}

async fn handle_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    call_id: CallId,
    mut bridge: MediaBridgeHandle,
    participant_a: forge_engine::ParticipantMediaState,
    participant_b: forge_engine::ParticipantMediaState,
    session_state: String,
) {
    let client_id = uuid::Uuid::new_v4();
    let outbound_tx = bridge.outbound_sender();
    let mut event_rx = state.core_event_bus.subscribe();

    info!(
        "Session media WebSocket client {} connected for call {}",
        client_id, call_id.0
    );

    if !send_json(
        &mut socket,
        &ServerMessage::Connected {
            call_id: call_id.0.clone(),
            state: session_state,
            sample_format: "pcm_s16le",
            channels: 1,
            participant_a,
            participant_b,
        },
    )
    .await
    {
        return;
    }

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Ping) => {
                                if !send_json(&mut socket, &ServerMessage::Pong).await {
                                    break;
                                }
                            }
                            Ok(ClientMessage::Audio { target, sample_rate, data }) => {
                                match decode_pcm16_base64(&data) {
                                    Ok(samples) => {
                                        if let Err(e) = outbound_tx.send(OutboundMediaFrame {
                                            target,
                                            sample_rate,
                                            samples,
                                        }).await {
                                            warn!(
                                                "Failed to queue outbound media for call {}: {}",
                                                call_id.0,
                                                e
                                            );
                                            if !send_json(
                                                &mut socket,
                                                &ServerMessage::Error {
                                                    message: format!("Failed to queue outbound media: {}", e),
                                                },
                                            )
                                            .await
                                            {
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        if !send_json(
                                            &mut socket,
                                            &ServerMessage::Error {
                                                message: e.to_string(),
                                            },
                                        )
                                        .await
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if !send_json(
                                    &mut socket,
                                    &ServerMessage::Error {
                                        message: format!("Invalid message: {}", e),
                                    },
                                )
                                .await
                                {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        error!(
                            "Session media WebSocket error for client {} on call {}: {}",
                            client_id, call_id.0, e
                        );
                        break;
                    }
                    None => break,
                }
            }
            frame = bridge.recv_frame() => {
                let Some(frame) = frame else {
                    break;
                };

                if !send_json(&mut socket, &ServerMessage::Audio {
                    leg: frame.leg,
                    codec: frame.codec,
                    payload_type: frame.payload_type,
                    sample_rate: frame.sample_rate,
                    sequence_number: frame.sequence_number,
                    timestamp: frame.timestamp,
                    data: encode_pcm16_base64(&frame.samples),
                }).await {
                    break;
                }
            }
            event = event_rx.recv() => {
                match event {
                    Ok(event) => {
                        if !forward_call_event(&mut socket, &call_id, event).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(
                            "Session media WebSocket lagged for call {}, skipped {} events",
                            call_id.0,
                            skipped
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    bridge.detach();
    info!(
        "Session media WebSocket client {} disconnected for call {}",
        client_id, call_id.0
    );
}

async fn forward_call_event(socket: &mut WebSocket, call_id: &CallId, event: ForgeEvent) -> bool {
    match event {
        ForgeEvent::DtmfDigitDetected {
            call_id: event_call_id,
            digit,
            method,
            event_type,
            ..
        } if event_call_id == *call_id => {
            send_json(
                socket,
                &ServerMessage::Dtmf {
                    digit,
                    method,
                    event_type,
                },
            )
            .await
        }
        ForgeEvent::SessionCreated {
            call_id: event_call_id,
            ..
        }
        | ForgeEvent::SessionActive {
            call_id: event_call_id,
            ..
        }
        | ForgeEvent::SessionResumed {
            call_id: event_call_id,
            ..
        } if event_call_id == *call_id => {
            send_json(socket, &ServerMessage::SessionState { state: "active" }).await
        }
        ForgeEvent::SessionOnHold {
            call_id: event_call_id,
            ..
        } if event_call_id == *call_id => {
            send_json(socket, &ServerMessage::SessionState { state: "on_hold" }).await
        }
        ForgeEvent::SessionTerminated {
            call_id: event_call_id,
            reason,
            ..
        } if event_call_id == *call_id => {
            let _ = send_json(socket, &ServerMessage::SessionTerminated { reason }).await;
            false
        }
        _ => true,
    }
}

async fn send_json(socket: &mut WebSocket, message: &ServerMessage) -> bool {
    match serde_json::to_string(message) {
        Ok(json) => socket.send(Message::Text(json.into())).await.is_ok(),
        Err(e) => {
            error!("Failed to serialize session media WebSocket message: {}", e);
            false
        }
    }
}

fn encode_pcm16_base64(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    STANDARD.encode(bytes)
}

fn decode_pcm16_base64(data: &str) -> Result<Vec<i16>, ApiError> {
    // Reject before allocating the decode buffer — the WebSocket layer
    // also caps total message size, but the explicit per-payload bound
    // keeps the post-decode + post-chunks_exact allocation budget bounded
    // even if the envelope ever changes shape.
    if data.len() > MAX_AUDIO_DATA_BASE64_LEN {
        return Err(ApiError::InvalidRequest(format!(
            "Audio payload too large ({} > {} bytes)",
            data.len(),
            MAX_AUDIO_DATA_BASE64_LEN
        )));
    }

    let decoded = STANDARD
        .decode(data)
        .map_err(|e| ApiError::InvalidRequest(format!("Invalid audio payload: {}", e)))?;

    if decoded.len() % 2 != 0 {
        return Err(ApiError::InvalidRequest(
            "Audio payload must contain an even number of bytes".to_string(),
        ));
    }

    Ok(decoded
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use forge_conference::ConferenceBridge;
    use forge_engine::{SessionManager, SessionManagerConfig};

    fn create_test_state() -> Arc<AppState> {
        let session_manager = SessionManager::new(SessionManagerConfig::default(), None);
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
            #[cfg(feature = "ha")]
            None,
        ))
    }

    #[test]
    fn test_audio_payload_roundtrip() {
        let samples = vec![-32768, -1234, 0, 4321, 32767];
        let encoded = encode_pcm16_base64(&samples);
        let decoded = decode_pcm16_base64(&encoded).unwrap();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn test_audio_message_parsing() {
        let msg = r#"{"type":"audio","target":"both","sample_rate":16000,"data":"AAE="}"#;
        let parsed: ClientMessage = serde_json::from_str(msg).unwrap();
        match parsed {
            ClientMessage::Audio {
                target,
                sample_rate,
                ..
            } => {
                assert_eq!(target, MediaTarget::Both);
                assert_eq!(sample_rate, 16000);
            }
            _ => panic!("expected audio message"),
        }
    }

    #[test]
    fn test_router_creation() {
        let state = create_test_state();
        let _router: Router = routes().with_state(state);
    }

    // DoS regression: an oversized base64 audio payload must be rejected
    // before any decode allocation happens. The WebSocket layer also caps
    // total message size, but the per-payload bound is defense-in-depth.
    #[test]
    fn test_decode_pcm16_base64_rejects_oversize_payload() {
        let oversize = "A".repeat(MAX_AUDIO_DATA_BASE64_LEN + 1);
        let err = decode_pcm16_base64(&oversize)
            .expect_err("payload above MAX_AUDIO_DATA_BASE64_LEN must be rejected");
        match err {
            ApiError::InvalidRequest(msg) => assert!(msg.contains("too large")),
            other => panic!("expected InvalidRequest, got {:?}", other),
        }
    }

    #[test]
    fn test_decode_pcm16_base64_accepts_payload_at_limit() {
        // base64 input of length MAX is the boundary case — must succeed.
        // Use a multiple of 4 so it decodes cleanly to PCM16.
        let len = (MAX_AUDIO_DATA_BASE64_LEN / 4) * 4;
        let payload = "A".repeat(len);
        // "A" decodes to 0x00 in standard b64; padding handling will fail
        // for non-padded inputs, so we just assert size handling — pass if
        // it returns InvalidRequest (decode error) or Ok, but NOT the
        // "too large" branch.
        match decode_pcm16_base64(&payload) {
            Ok(_) => {}
            Err(ApiError::InvalidRequest(msg)) => {
                assert!(
                    !msg.contains("too large"),
                    "payload at the limit must not be rejected for size: {}",
                    msg
                );
            }
            Err(other) => panic!("unexpected error: {:?}", other),
        }
    }
}
