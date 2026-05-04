//! OpenAI Realtime API integration
//!
//! Client for OpenAI's Realtime API for voice conversations.

use crate::connector::{
    AIConnector, AIConnectorConfig, AIConnectorType, AISession, AISessionState,
};
use crate::events::{AIEvent, SessionConfig};
use crate::{AIStreamError, AIStreamStats, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, Message},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error};

/// OpenAI model
#[derive(Debug, Clone)]
pub enum OpenAIModel {
    /// GPT-4o Realtime Preview
    Gpt4oRealtimePreview,
    /// GPT-4o Realtime Preview (2024-10-01)
    Gpt4oRealtimePreview20241001,
    /// Custom model
    Custom(String),
}

impl OpenAIModel {
    /// Get model string
    pub fn as_str(&self) -> &str {
        match self {
            OpenAIModel::Gpt4oRealtimePreview => "gpt-4o-realtime-preview",
            OpenAIModel::Gpt4oRealtimePreview20241001 => "gpt-4o-realtime-preview-2024-10-01",
            OpenAIModel::Custom(s) => s,
        }
    }
}

/// OpenAI configuration
pub type OpenAIConfig = AIConnectorConfig;

/// OpenAI Realtime API connector
pub struct OpenAIConnector {
    config: OpenAIConfig,
    session: Option<AISession>,
    stats: AIStreamStats,
    ws: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl OpenAIConnector {
    /// Create a new OpenAI connector
    pub async fn new(config: OpenAIConfig) -> Result<Self> {
        Ok(Self {
            config,
            session: None,
            stats: AIStreamStats::new(),
            ws: None,
        })
    }

    /// Get WebSocket URL
    fn get_ws_url(&self) -> String {
        self.config
            .endpoint
            .clone()
            .unwrap_or_else(|| "wss://api.openai.com/v1/realtime".to_string())
    }

    /// Parse incoming WebSocket message into AIEvent
    fn parse_event(&mut self, msg: Value) -> Result<Option<AIEvent>> {
        let event_type = msg["type"]
            .as_str()
            .ok_or_else(|| AIStreamError::Protocol("Missing event type in message".to_string()))?;

        match event_type {
            "session.created" => {
                let session_id = msg["session"]["id"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                Ok(Some(AIEvent::Connected { session_id }))
            }
            "response.audio.delta" => {
                let delta_b64 = msg["delta"].as_str().ok_or_else(|| {
                    AIStreamError::Protocol("Missing delta in audio message".to_string())
                })?;
                let audio_bytes = BASE64.decode(delta_b64).map_err(|e| {
                    AIStreamError::AudioFormat(format!("Failed to decode base64 audio: {}", e))
                })?;

                // Convert bytes to i16 samples
                let samples: Vec<i16> = audio_bytes
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();

                self.stats.samples_received += samples.len() as u64;

                Ok(Some(AIEvent::AudioResponse {
                    audio_data: samples,
                    sample_rate: 24000, // OpenAI uses 24kHz
                    item_id: msg["item_id"].as_str().map(|s| s.to_string()),
                }))
            }
            "response.audio_transcript.delta"
            | "conversation.item.input_audio_transcription.completed" => {
                let transcript = msg["delta"]
                    .as_str()
                    .or_else(|| msg["transcript"].as_str())
                    .unwrap_or("")
                    .to_string();

                let item_id = msg["item_id"].as_str().unwrap_or("unknown");
                let is_final = event_type.contains("completed");

                Ok(Some(AIEvent::Transcript {
                    segment: crate::events::TranscriptSegment {
                        id: item_id.to_string(),
                        role: if event_type.contains("input") {
                            crate::events::TranscriptRole::User
                        } else {
                            crate::events::TranscriptRole::Assistant
                        },
                        text: transcript,
                        start_ms: None,
                        end_ms: None,
                        confidence: None,
                        is_final,
                    },
                }))
            }
            "response.function_call_arguments.done" => {
                let call_id = msg["call_id"]
                    .as_str()
                    .ok_or_else(|| {
                        AIStreamError::Protocol("Missing call_id in function call".to_string())
                    })?
                    .to_string();

                let name = msg["name"]
                    .as_str()
                    .ok_or_else(|| {
                        AIStreamError::Protocol("Missing name in function call".to_string())
                    })?
                    .to_string();

                let arguments_str = msg["arguments"].as_str().unwrap_or("{}");
                let arguments: serde_json::Value =
                    serde_json::from_str(arguments_str).map_err(|e| {
                        AIStreamError::Protocol(format!("Invalid function arguments: {}", e))
                    })?;

                let item_id = msg["item_id"].as_str().map(|s| s.to_string());

                self.stats.function_calls += 1;

                Ok(Some(AIEvent::FunctionCall {
                    call: crate::events::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        item_id,
                    },
                }))
            }
            "input_audio_buffer.speech_started" => Ok(Some(AIEvent::VadStateChange {
                is_speech: true,
                confidence: 1.0,
            })),
            "input_audio_buffer.speech_stopped" => Ok(Some(AIEvent::VadStateChange {
                is_speech: false,
                confidence: 0.0,
            })),
            "response.cancelled" => {
                let response_id = msg["response_id"].as_str().unwrap_or("unknown").to_string();
                Ok(Some(AIEvent::BargeIn { response_id }))
            }
            "error" => {
                let message = msg["error"]["message"]
                    .as_str()
                    .unwrap_or("Unknown error")
                    .to_string();
                let code = msg["error"]["code"].as_str().map(|s| s.to_string());
                Ok(Some(AIEvent::Error { message, code }))
            }
            _ => {
                debug!("Unhandled event type: {}", event_type);
                Ok(None)
            }
        }
    }

    /// Send a DTMF event to the AI as a text message
    ///
    /// This creates a conversation item that notifies the AI that the user pressed a digit.
    pub async fn send_dtmf_event(&mut self, digit: char, detection_method: &str) -> Result<()> {
        let ws = self
            .ws
            .as_mut()
            .ok_or_else(|| AIStreamError::Connection("Not connected".to_string()))?;

        // Create a message that the AI can understand
        let content = format!("[DTMF: User pressed '{}' via {}]", digit, detection_method);

        let msg = json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": content
                }]
            }
        });

        ws.send(Message::Text(msg.to_string()))
            .await
            .map_err(|e: tungstenite::Error| {
                AIStreamError::Connection(format!("Failed to send DTMF event: {}", e))
            })?;

        // Trigger a response from the AI
        let response_msg = json!({
            "type": "response.create"
        });

        ws.send(Message::Text(response_msg.to_string()))
            .await
            .map_err(|e: tungstenite::Error| {
                AIStreamError::Connection(format!("Failed to trigger AI response: {}", e))
            })?;

        self.stats.events_sent += 2;
        Ok(())
    }

    /// Send labeled audio from a specific participant (for conference Individual mode)
    ///
    /// This is used in conference Individual audio mode where each participant's audio
    /// is sent separately with a label for speaker identification.
    ///
    /// Note: This sends audio through the normal input_audio_buffer.append mechanism.
    /// The participant label is included in the audio metadata for transcription purposes.
    pub async fn send_labeled_audio(
        &mut self,
        participant_id: &str,
        audio_data: &[i16],
        sample_rate: u32,
    ) -> Result<()> {
        // For now, just send the audio with the participant ID as context
        // OpenAI Realtime API doesn't have native multi-speaker support,
        // so we send audio sequentially and rely on the model to distinguish speakers
        // based on voice characteristics and context

        // In the future, we could prepend a very short text label before each audio chunk,
        // but that may interrupt the audio flow. For now, we just send the audio directly.

        debug!(
            "Sending labeled audio from participant {}: {} samples @ {}Hz",
            participant_id,
            audio_data.len(),
            sample_rate
        );

        self.send_audio(audio_data, sample_rate).await
    }
}

#[async_trait]
impl AIConnector for OpenAIConnector {
    async fn connect(&mut self) -> Result<String> {
        let url = format!("{}?model={}", self.get_ws_url(), self.config.model);

        // Build WebSocket request with authorization
        let request = tungstenite::http::Request::builder()
            .uri(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.api_key.expose_secret()),
            )
            .header("OpenAI-Beta", "realtime=v1")
            .body(())
            .map_err(|e| AIStreamError::Connection(format!("Failed to build request: {}", e)))?;

        // Connect to WebSocket
        debug!("Connecting to OpenAI Realtime API: {}", url);
        let (ws_stream, _) =
            tokio::time::timeout(self.config.connect_timeout, connect_async(request))
                .await
                .map_err(|_| AIStreamError::Timeout(self.config.connect_timeout))?
                .map_err(|e| {
                    AIStreamError::Connection(format!("WebSocket connection failed: {}", e))
                })?;

        debug!("WebSocket connected successfully");
        self.ws = Some(ws_stream);

        // Create session
        let session_id = format!("session_{}", uuid::Uuid::new_v4());
        let mut session = AISession::new(
            session_id.clone(),
            SessionConfig {
                model: self.config.model.clone(),
                voice: self.config.voice.clone(),
                temperature: self.config.temperature,
                max_tokens: self.config.max_tokens,
                instructions: self.config.instructions.clone(),
                turn_detection: None,
                tools: self.config.tools.clone(),
            },
        );
        session.set_state(AISessionState::Active);
        self.session = Some(session);

        // Caller is responsible for pushing the desired SessionConfig via update_config()
        // once they have constructed it (see forge-engine::AISession::new). Sending one
        // here would queue a redundant session.update right before the real one.
        Ok(session_id)
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut ws) = self.ws.take() {
            debug!("Closing WebSocket connection");
            ws.close(None).await.map_err(|e| {
                AIStreamError::Connection(format!("Failed to close WebSocket: {}", e))
            })?;
        }

        if let Some(session) = &mut self.session {
            session.set_state(AISessionState::Ended);
        }

        Ok(())
    }

    async fn send_audio(&mut self, audio_data: &[i16], _sample_rate: u32) -> Result<()> {
        let ws = self
            .ws
            .as_mut()
            .ok_or_else(|| AIStreamError::Connection("Not connected".to_string()))?;

        // Convert i16 samples to bytes
        let mut audio_bytes = Vec::with_capacity(audio_data.len() * 2);
        for sample in audio_data {
            audio_bytes.extend_from_slice(&sample.to_le_bytes());
        }

        // Encode as base64
        let audio_b64 = BASE64.encode(&audio_bytes);

        // Send audio append event
        let msg = json!({
            "type": "input_audio_buffer.append",
            "audio": audio_b64
        });

        ws.send(Message::Text(msg.to_string()))
            .await
            .map_err(|e: tungstenite::Error| {
                AIStreamError::Connection(format!("Failed to send audio: {}", e))
            })?;

        self.stats.samples_sent += audio_data.len() as u64;
        self.stats.events_sent += 1;

        Ok(())
    }

    async fn next_event(&mut self) -> Result<Option<AIEvent>> {
        let ws = self
            .ws
            .as_mut()
            .ok_or_else(|| AIStreamError::Connection("Not connected".to_string()))?;

        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg: Value = serde_json::from_str(&text)
                    .map_err(|e| AIStreamError::Protocol(format!("Failed to parse JSON: {}", e)))?;
                self.parse_event(msg)
            }
            Some(Ok(Message::Close(_))) => {
                debug!("WebSocket closed by server");
                if let Some(session) = &mut self.session {
                    session.set_state(AISessionState::Ended);
                }
                Ok(None)
            }
            Some(Err(e)) => {
                error!("WebSocket error: {}", e);
                Err(AIStreamError::Connection(format!("WebSocket error: {}", e)))
            }
            None => {
                debug!("WebSocket stream ended");
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn send_function_response(&mut self, call_id: String, output: String) -> Result<()> {
        let ws = self
            .ws
            .as_mut()
            .ok_or_else(|| AIStreamError::Connection("Not connected".to_string()))?;

        let msg = json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": output
            }
        });

        ws.send(Message::Text(msg.to_string()))
            .await
            .map_err(|e: tungstenite::Error| {
                AIStreamError::Connection(format!("Failed to send function response: {}", e))
            })?;

        self.stats.events_sent += 1;
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<()> {
        let ws = self
            .ws
            .as_mut()
            .ok_or_else(|| AIStreamError::Connection("Not connected".to_string()))?;

        let msg = json!({
            "type": "response.cancel"
        });

        ws.send(Message::Text(msg.to_string()))
            .await
            .map_err(|e: tungstenite::Error| {
                AIStreamError::Connection(format!("Failed to send interrupt: {}", e))
            })?;

        self.stats.events_sent += 1;
        Ok(())
    }

    async fn update_config(&mut self, config: SessionConfig) -> Result<()> {
        if let Some(session) = &mut self.session {
            session.config = config.clone();
        }

        if let Some(ws) = self.ws.as_mut() {
            // Only emit fields the caller actually set. OpenAI treats explicit `null` as
            // "clear this setting", so serializing every Option as null on a partial
            // update would wipe instructions/voice/etc. that were set on a previous turn.
            // tools is always sent because it's a Vec; an empty list explicitly clears tools.
            let mut session = serde_json::Map::new();
            session.insert("input_audio_format".into(), json!("pcm16"));
            session.insert("output_audio_format".into(), json!("pcm16"));
            session.insert("tools".into(), json!(config.tools));
            if let Some(instructions) = &config.instructions {
                session.insert("instructions".into(), json!(instructions));
            }
            if let Some(voice) = &config.voice {
                session.insert("voice".into(), json!(voice));
            }
            if let Some(temperature) = config.temperature {
                session.insert("temperature".into(), json!(temperature));
            }
            if let Some(max_tokens) = config.max_tokens {
                session.insert("max_response_output_tokens".into(), json!(max_tokens));
            }
            if let Some(turn_detection) = &config.turn_detection {
                session.insert("turn_detection".into(), json!(turn_detection));
            }

            let msg = json!({
                "type": "session.update",
                "session": session,
            });

            ws.send(Message::Text(msg.to_string()))
                .await
                .map_err(|e: tungstenite::Error| {
                    AIStreamError::Connection(format!("Failed to update session config: {}", e))
                })?;

            self.stats.events_sent += 1;
        }

        Ok(())
    }

    async fn send_labeled_audio(
        &mut self,
        participant_id: &str,
        audio_data: &[i16],
        sample_rate: u32,
    ) -> Result<()> {
        OpenAIConnector::send_labeled_audio(self, participant_id, audio_data, sample_rate).await
    }

    async fn send_dtmf_event(&mut self, digit: char, detection_method: &str) -> Result<()> {
        OpenAIConnector::send_dtmf_event(self, digit, detection_method).await
    }

    fn session(&self) -> Option<&AISession> {
        self.session.as_ref()
    }

    fn session_mut(&mut self) -> Option<&mut AISession> {
        self.session.as_mut()
    }

    fn connector_type(&self) -> AIConnectorType {
        AIConnectorType::OpenAI
    }

    fn stats(&self) -> &AIStreamStats {
        &self.stats
    }

    fn reset_stats(&mut self) {
        self.stats = AIStreamStats::new();
    }

    fn is_connected(&self) -> bool {
        self.session
            .as_ref()
            .map(|s| s.state == AISessionState::Active)
            .unwrap_or(false)
    }
}
