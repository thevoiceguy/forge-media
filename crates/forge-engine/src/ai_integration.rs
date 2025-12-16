//! AI integration for media sessions
//!
//! This module provides AI conversation capabilities for media sessions,
//! allowing real-time audio streaming to AI services like OpenAI Realtime API.

use dashmap::DashMap;
use forge_ai_stream::{
    AIConnector, AIConnectorConfig, AIConnectorType, AIEvent, OpenAIConnector,
};
use forge_core::{CallId, ForgeError, Result};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

/// Configuration for AI integration with a media session
#[derive(Debug, Clone)]
pub struct AISessionConfig {
    /// AI service type (OpenAI, etc.)
    pub connector_type: AIConnectorType,
    /// API key for the AI service
    pub api_key: String,
    /// Optional custom endpoint URL
    pub endpoint: Option<String>,
    /// AI model to use (e.g., "gpt-4o-realtime-preview")
    pub model: String,
    /// Voice/persona for AI responses
    pub voice: Option<String>,
    /// Temperature for response generation (0.0-1.0)
    pub temperature: Option<f32>,
    /// System instructions for AI behavior
    pub instructions: Option<String>,
    /// Enable Voice Activity Detection
    pub enable_vad: bool,
    /// Enable barge-in detection (user can interrupt AI)
    pub enable_barge_in: bool,
    /// Audio sample rate for the session (typically 8000 or 16000 Hz)
    pub sample_rate: u32,
}

impl Default for AISessionConfig {
    fn default() -> Self {
        Self {
            connector_type: AIConnectorType::OpenAI,
            api_key: String::new(),
            endpoint: None,
            model: "gpt-4o-realtime-preview".to_string(),
            voice: Some("alloy".to_string()),
            temperature: Some(0.8),
            instructions: Some("You are a helpful voice assistant.".to_string()),
            enable_vad: true,
            enable_barge_in: true,
            sample_rate: 16000, // Default to 16kHz for better quality
        }
    }
}

/// State of an AI session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AISessionState {
    /// AI session is connecting
    Connecting,
    /// AI session is active and processing audio
    Active,
    /// AI is currently speaking
    Speaking,
    /// AI session is disconnecting
    Disconnecting,
    /// AI session has terminated
    Terminated,
}

/// Audio response from AI
#[derive(Debug, Clone)]
pub struct AIAudioResponse {
    /// PCM16 audio samples
    pub samples: Vec<i16>,
    /// Sample rate (Hz)
    pub sample_rate: u32,
}

/// An active AI session attached to a media session
pub struct AISession {
    /// Associated call ID
    call_id: CallId,
    /// AI connector (OpenAI connector)
    connector: Arc<Mutex<OpenAIConnector>>,
    /// Current state
    state: Arc<Mutex<AISessionState>>,
    /// Audio sample rate
    sample_rate: u32,
    /// Channel for sending audio to AI
    audio_tx: mpsc::UnboundedSender<Vec<i16>>,
    /// Channel for receiving audio responses from AI
    audio_response_rx: Arc<Mutex<mpsc::UnboundedReceiver<AIAudioResponse>>>,
    /// Event processing task
    event_task: Option<JoinHandle<()>>,
    /// Audio routing task
    audio_task: Option<JoinHandle<()>>,
    /// DTMF event processing task
    dtmf_task: Option<JoinHandle<()>>,
}

impl AISession {
    /// Create a new AI session
    pub async fn new(call_id: CallId, config: AISessionConfig, event_bus: Option<Arc<forge_core::EventBus>>) -> Result<Self> {
        let call_id_clone = call_id.clone();
        // Convert AISessionConfig to AIConnectorConfig
        let connector_config = AIConnectorConfig {
            connector_type: config.connector_type,
            api_key: config.api_key,
            endpoint: config.endpoint,
            model: config.model,
            voice: config.voice,
            temperature: config.temperature,
            max_tokens: Some(4096),
            instructions: config.instructions,
            tools: vec![],
            enable_vad: config.enable_vad,
            enable_barge_in: config.enable_barge_in,
            connect_timeout: std::time::Duration::from_secs(30),
            request_timeout: std::time::Duration::from_secs(60),
        };

        // Create OpenAI connector
        let mut connector = OpenAIConnector::new(connector_config)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to create AI connector: {}", e)))?;

        // Connect to AI service
        let session_id = connector
            .connect()
            .await
            .map_err(|e| ForgeError::Internal(format!("AI connection failed: {}", e)))?;

        tracing::info!(
            "AI session connected for call {}: session_id={}",
            call_id.0,
            session_id
        );

        let connector = Arc::new(Mutex::new(connector));
        let state = Arc::new(Mutex::new(AISessionState::Active));

        // Create audio channel for sending audio to AI
        let (audio_tx, audio_rx) = mpsc::unbounded_channel();

        // Create audio response channel for receiving audio from AI
        let (audio_response_tx, audio_response_rx) = mpsc::unbounded_channel();

        let session = Self {
            call_id: call_id.clone(),
            connector: Arc::clone(&connector),
            state: Arc::clone(&state),
            sample_rate: config.sample_rate,
            audio_tx,
            audio_response_rx: Arc::new(Mutex::new(audio_response_rx)),
            event_task: None,
            audio_task: None,
            dtmf_task: None,
        };

        // Start event processing task
        let event_task = tokio::spawn(Self::process_events(
            call_id_clone.clone(),
            Arc::clone(&connector),
            Arc::clone(&state),
            audio_response_tx,
        ));

        // Start audio routing task
        let audio_task = tokio::spawn(Self::process_audio(
            Arc::clone(&connector),
            audio_rx,
            config.sample_rate,
        ));

        // Start DTMF event processing task if EventBus is available
        let dtmf_task = if let Some(bus) = event_bus {
            Some(tokio::spawn(Self::process_dtmf_events(
                call_id,
                Arc::clone(&connector),
                bus,
            )))
        } else {
            None
        };

        Ok(Self {
            event_task: Some(event_task),
            audio_task: Some(audio_task),
            dtmf_task,
            ..session
        })
    }

    /// Get the call ID
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }

    /// Get the current state
    pub async fn state(&self) -> AISessionState {
        *self.state.lock().await
    }

    /// Send audio samples to AI
    pub async fn send_audio(&self, samples: &[i16]) -> Result<()> {
        self.audio_tx
            .send(samples.to_vec())
            .map_err(|e| ForgeError::Internal(format!("Failed to send audio to AI: {}", e)))?;
        Ok(())
    }

    /// Send a function call response to the AI
    pub async fn send_function_response(&self, call_id: &str, output: String) -> Result<()> {
        let mut connector = self.connector.lock().await;
        connector
            .send_function_response(call_id, output)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to send function response: {}", e)))
    }

    /// Disconnect the AI session
    pub async fn disconnect(&mut self) -> Result<()> {
        tracing::info!("Disconnecting AI session for call {}", self.call_id.0);

        *self.state.lock().await = AISessionState::Disconnecting;

        // Cancel tasks
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        if let Some(task) = self.audio_task.take() {
            task.abort();
        }
        if let Some(task) = self.dtmf_task.take() {
            task.abort();
        }

        // Disconnect connector
        let mut connector = self.connector.lock().await;
        connector
            .disconnect()
            .await
            .map_err(|e| ForgeError::Internal(format!("AI disconnect failed: {}", e)))?;

        *self.state.lock().await = AISessionState::Terminated;

        Ok(())
    }

    /// Try to receive an audio response (non-blocking)
    pub async fn try_recv_audio_response(&self) -> Option<AIAudioResponse> {
        self.audio_response_rx.lock().await.try_recv().ok()
    }

    /// Process AI events (transcripts, audio responses, errors, etc.)
    async fn process_events(
        call_id: CallId,
        connector: Arc<Mutex<OpenAIConnector>>,
        state: Arc<Mutex<AISessionState>>,
        audio_response_tx: mpsc::UnboundedSender<AIAudioResponse>,
    ) {
        loop {
            let event = {
                let mut conn = connector.lock().await;
                match conn.next_event().await {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("AI event error for call {}: {}", call_id.0, e);
                        break;
                    }
                }
            };

            match event {
                AIEvent::Transcript { segment } => {
                    tracing::info!(
                        "AI transcript for call {}: {:?}: {}",
                        call_id.0,
                        segment.role,
                        segment.text
                    );
                }
                AIEvent::AudioResponse {
                    audio_data,
                    sample_rate,
                    ..
                } => {
                    tracing::debug!(
                        "AI audio response for call {}: {} samples @ {}Hz",
                        call_id.0,
                        audio_data.len(),
                        sample_rate
                    );

                    // Send audio response through channel
                    if let Err(e) = audio_response_tx.send(AIAudioResponse {
                        samples: audio_data,
                        sample_rate,
                    }) {
                        tracing::error!("Failed to send AI audio response for call {}: {}", call_id.0, e);
                    }
                }
                AIEvent::FunctionCall { call } => {
                    tracing::info!(
                        "AI function call for call {}: {} ({})",
                        call_id.0,
                        call.name,
                        call.call_id
                    );
                    // TODO: Emit event to application layer for handling
                }
                AIEvent::VadStateChange {
                    is_speech,
                    confidence,
                } => {
                    tracing::debug!(
                        "AI VAD state change for call {}: {} (confidence: {})",
                        call_id.0,
                        if is_speech { "SPEECH" } else { "SILENCE" },
                        confidence
                    );
                }
                AIEvent::BargeIn { response_id } => {
                    tracing::info!(
                        "AI barge-in detected for call {}: response_id={:?}",
                        call_id.0,
                        response_id
                    );
                    *state.lock().await = AISessionState::Active;
                }
                AIEvent::Error { message, .. } => {
                    tracing::error!("AI error for call {}: {}", call_id.0, message);
                }
                AIEvent::Connected { session_id } => {
                    tracing::info!(
                        "AI connected for call {}: session={}",
                        call_id.0,
                        session_id
                    );
                }
                AIEvent::Disconnected { reason } => {
                    tracing::info!("AI disconnected for call {}: {}", call_id.0, reason);
                    break;
                }
                AIEvent::Custom { .. } => {
                    tracing::debug!("AI custom event for call {}", call_id.0);
                }
                AIEvent::SessionStarted { .. } => {
                    tracing::debug!("AI session started event for call {}", call_id.0);
                }
                AIEvent::SessionEnded { .. } => {
                    tracing::debug!("AI session ended event for call {}", call_id.0);
                }
                AIEvent::FunctionResponse { .. } => {
                    tracing::debug!("AI function response event for call {}", call_id.0);
                }
            }
        }

        *state.lock().await = AISessionState::Terminated;
    }

    /// Process audio samples and send to AI
    async fn process_audio(
        connector: Arc<Mutex<OpenAIConnector>>,
        mut audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
        target_sample_rate: u32,
    ) {
        while let Some(samples) = audio_rx.recv().await {
            let mut conn = connector.lock().await;
            if let Err(e) = conn.send_audio(&samples, target_sample_rate).await {
                tracing::error!("Failed to send audio to AI: {}", e);
                break;
            }
        }
    }

    /// Process DTMF events from EventBus and send to AI
    async fn process_dtmf_events(
        call_id: CallId,
        connector: Arc<Mutex<OpenAIConnector>>,
        event_bus: Arc<forge_core::EventBus>,
    ) {
        use forge_core::{DtmfEventKind, ForgeEvent};

        // Subscribe to events
        let mut rx = event_bus.subscribe();

        tracing::debug!("Started DTMF event processor for call {}", call_id.0);

        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Filter for DTMF events for this call
                    if let ForgeEvent::DtmfDigitDetected {
                        call_id: event_call_id,
                        digit,
                        method,
                        event_type,
                        ..
                    } = event
                    {
                        // Only process events for our call_id and only on "End" events to avoid duplicates
                        if event_call_id == call_id && event_type == DtmfEventKind::End {
                            tracing::info!(
                                "DTMF digit detected for AI session {}: {} via {:?}",
                                call_id.0,
                                digit,
                                method
                            );

                            // Send DTMF event to AI
                            let mut conn = connector.lock().await;
                            let method_str = match method {
                                forge_core::DtmfDetectionMethod::Rfc2833 => "RFC 2833",
                                forge_core::DtmfDetectionMethod::Inband => "Inband",
                                forge_core::DtmfDetectionMethod::SipInfo => "SIP INFO",
                            };

                            if let Err(e) = conn.send_dtmf_event(digit, method_str).await {
                                tracing::error!(
                                    "Failed to send DTMF event to AI for call {}: {}",
                                    call_id.0,
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    "Successfully sent DTMF '{}' to AI for call {}",
                                    digit,
                                    call_id.0
                                );
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        "DTMF event receiver lagged for call {}, skipped {} events",
                        call_id.0,
                        skipped
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::debug!("Event bus closed for call {}", call_id.0);
                    break;
                }
            }
        }

        tracing::debug!("DTMF event processor stopped for call {}", call_id.0);
    }
}

/// Manager for AI sessions
pub struct AISessionManager {
    /// Active AI sessions by call ID
    sessions: Arc<DashMap<CallId, Arc<Mutex<AISession>>>>,
}

impl Default for AISessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AISessionManager {
    /// Create a new AI session manager
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
        }
    }

    /// Create and attach an AI session to a call
    pub async fn attach_ai(&self, call_id: CallId, config: AISessionConfig, event_bus: Option<Arc<forge_core::EventBus>>) -> Result<()> {
        if self.sessions.contains_key(&call_id) {
            return Err(ForgeError::Internal(format!(
                "AI session already exists for call {}",
                call_id.0
            )));
        }

        let session = AISession::new(call_id.clone(), config, event_bus).await?;
        self.sessions
            .insert(call_id.clone(), Arc::new(Mutex::new(session)));

        tracing::info!("AI attached to call {}", call_id.0);

        Ok(())
    }

    /// Detach AI from a call
    pub async fn detach_ai(&self, call_id: &CallId) -> Result<()> {
        let session_arc = self.sessions.remove(call_id)
            .map(|(_, v)| v)
            .ok_or_else(|| ForgeError::Internal(format!(
                "No AI session found for call {}",
                call_id.0
            )))?;

        // Extract task handles and connector in a scoped block to drop the lock
        let (event_task, audio_task, dtmf_task, connector, state) = {
            let mut session = session_arc.lock().await;

            // Update state to disconnecting
            {
                let mut s = session.state.lock().await;
                *s = AISessionState::Disconnecting;
            }

            // Take task handles
            let event_task = session.event_task.take();
            let audio_task = session.audio_task.take();
            let dtmf_task = session.dtmf_task.take();

            // Clone Arc references we need
            let connector = Arc::clone(&session.connector);
            let state = Arc::clone(&session.state);

            (event_task, audio_task, dtmf_task, connector, state)
        }; // session lock dropped here

        // Abort tasks (no lock needed)
        if let Some(task) = event_task {
            task.abort();
        }
        if let Some(task) = audio_task {
            task.abort();
        }
        if let Some(task) = dtmf_task {
            task.abort();
        }

        // Disconnect connector (brief lock, then await)
        {
            let mut conn = connector.lock().await;
            conn.disconnect()
                .await
                .map_err(|e| ForgeError::Internal(format!("AI disconnect failed: {}", e)))?;
        } // connector lock dropped here

        // Update final state
        {
            let mut s = state.lock().await;
            *s = AISessionState::Terminated;
        }

        tracing::info!("AI detached from call {}", call_id.0);
        Ok(())
    }

    /// Get an AI session for a call
    pub fn get_session(&self, call_id: &CallId) -> Option<Arc<Mutex<AISession>>> {
        self.sessions.get(call_id).map(|entry| entry.value().clone())
    }

    /// Check if a call has an AI session
    pub fn has_ai(&self, call_id: &CallId) -> bool {
        self.sessions.contains_key(call_id)
    }

    /// Send audio to AI for a specific call
    pub async fn send_audio(&self, call_id: &CallId, samples: &[i16]) -> Result<()> {
        if let Some(session_arc) = self.get_session(call_id) {
            let session = session_arc.lock().await;
            session.send_audio(samples).await
        } else {
            Err(ForgeError::Internal(format!(
                "No AI session found for call {}",
                call_id.0
            )))
        }
    }

    /// Send a function response to AI for a specific call
    pub async fn send_function_response(
        &self,
        call_id: &CallId,
        function_call_id: &str,
        output: String,
    ) -> Result<()> {
        if let Some(session_arc) = self.get_session(call_id) {
            let session = session_arc.lock().await;
            session.send_function_response(function_call_id, output).await
        } else {
            Err(ForgeError::Internal(format!(
                "No AI session found for call {}",
                call_id.0
            )))
        }
    }

    /// Get the state of an AI session
    pub async fn get_state(&self, call_id: &CallId) -> Option<AISessionState> {
        if let Some(session_arc) = self.get_session(call_id) {
            let session = session_arc.lock().await;
            Some(session.state().await)
        } else {
            None
        }
    }

    /// List all active AI session call IDs
    pub fn list_sessions(&self) -> Vec<CallId> {
        self.sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get the count of active AI sessions
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Try to receive an audio response for a specific call (non-blocking)
    pub async fn try_recv_audio_response(&self, call_id: &CallId) -> Option<AIAudioResponse> {
        if let Some(session_arc) = self.get_session(call_id) {
            let session = session_arc.lock().await;
            session.try_recv_audio_response().await
        } else {
            None
        }
    }

    /// Send a DTMF event to the AI for a specific call
    ///
    /// This allows manual DTMF forwarding when not using the automatic event bus integration.
    /// Useful for conference scenarios where DTMF events need to be forwarded from conference participants.
    pub async fn send_dtmf_event(
        &self,
        call_id: &CallId,
        digit: char,
        detection_method: forge_core::DtmfDetectionMethod,
    ) -> Result<()> {
        if let Some(session_arc) = self.get_session(call_id) {
            let session = session_arc.lock().await;

            // Get connector reference
            let connector = Arc::clone(&session.connector);

            // Convert detection method to string
            let method_str = match detection_method {
                forge_core::DtmfDetectionMethod::Rfc2833 => "RFC 2833",
                forge_core::DtmfDetectionMethod::Inband => "Inband",
                forge_core::DtmfDetectionMethod::SipInfo => "SIP INFO",
            };

            // Release session lock before awaiting
            drop(session);

            // Send DTMF to AI connector
            let mut conn = connector.lock().await;
            conn.send_dtmf_event(digit, method_str)
                .await
                .map_err(|e| ForgeError::Internal(format!("Failed to send DTMF to AI: {}", e)))?;

            tracing::debug!("Sent DTMF '{}' to AI for call {}", digit, call_id.0);
            Ok(())
        } else {
            Err(ForgeError::Internal(format!(
                "No AI session found for call {}",
                call_id.0
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_ai_session_manager_creation() {
        let manager = AISessionManager::new();
        assert_eq!(manager.session_count(), 0);
    }

    #[test]
    fn test_ai_session_config_default() {
        let config = AISessionConfig::default();
        assert_eq!(config.model, "gpt-4o-realtime-preview");
        assert_eq!(config.sample_rate, 16000);
        assert!(config.enable_vad);
        assert!(config.enable_barge_in);
    }

    #[tokio::test]
    async fn test_ai_session_manager_has_ai() {
        let manager = AISessionManager::new();
        let call_id = CallId::generate();

        assert!(!manager.has_ai(&call_id));

        // Note: Can't actually attach without valid API key
        // This test just verifies the has_ai logic
    }

    #[tokio::test]
    async fn test_ai_session_manager_list_sessions() {
        let manager = AISessionManager::new();

        // Initially empty
        assert_eq!(manager.list_sessions().len(), 0);
        assert_eq!(manager.session_count(), 0);
    }

    #[tokio::test]
    async fn test_ai_session_manager_get_nonexistent() {
        let manager = AISessionManager::new();
        let call_id = CallId::generate();

        // Getting non-existent session should return None
        assert!(manager.get_session(&call_id).is_none());
        assert!(!manager.has_ai(&call_id));
        assert!(manager.get_state(&call_id).await.is_none());
    }

    #[tokio::test]
    async fn test_ai_session_manager_send_audio_no_session() {
        let manager = AISessionManager::new();
        let call_id = CallId::generate();
        let samples = vec![0i16; 160];

        // Sending audio to non-existent session should return error
        let result = manager.send_audio(&call_id, &samples).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ai_session_manager_detach_nonexistent() {
        let manager = AISessionManager::new();
        let call_id = CallId::generate();

        // Detaching non-existent session should return error
        let result = manager.detach_ai(&call_id).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_ai_audio_response_creation() {
        let samples = vec![100i16, 200, 300, 400];
        let sample_rate = 16000;

        let response = AIAudioResponse {
            samples: samples.clone(),
            sample_rate,
        };

        assert_eq!(response.samples.len(), 4);
        assert_eq!(response.samples[0], 100);
        assert_eq!(response.sample_rate, 16000);
    }

    #[test]
    fn test_ai_session_state_values() {
        // Verify all state variants exist
        let states = vec![
            AISessionState::Connecting,
            AISessionState::Active,
            AISessionState::Speaking,
            AISessionState::Disconnecting,
            AISessionState::Terminated,
        ];

        assert_eq!(states.len(), 5);
    }

    #[tokio::test]
    async fn test_ai_session_config_variations() {
        // Test with custom configurations
        let config1 = AISessionConfig {
            connector_type: forge_ai_stream::AIConnectorType::OpenAI,
            api_key: "test-key".to_string(),
            endpoint: Some("wss://custom.endpoint".to_string()),
            model: "custom-model".to_string(),
            voice: Some("nova".to_string()),
            temperature: Some(0.5),
            instructions: Some("Test instructions".to_string()),
            enable_vad: false,
            enable_barge_in: false,
            sample_rate: 8000,
        };

        assert_eq!(config1.model, "custom-model");
        assert_eq!(config1.sample_rate, 8000);
        assert!(!config1.enable_vad);
        assert!(!config1.enable_barge_in);
        assert_eq!(config1.voice.unwrap(), "nova");
    }

    #[tokio::test]
    async fn test_eventbus_dtmf_event_structure() {
        // Test that we can create and match DTMF events
        use forge_core::{DtmfDetectionMethod, DtmfEventKind, ForgeEvent};

        let call_id = CallId::generate();
        let event = ForgeEvent::DtmfDigitDetected {
            call_id: call_id.clone(),
            digit: '1',
            duration_ms: Some(100),
            method: DtmfDetectionMethod::Rfc2833,
            event_type: DtmfEventKind::End,
            timestamp: Utc::now(),
        };

        // Verify we can match on the event
        match event {
            ForgeEvent::DtmfDigitDetected {
                call_id: event_call_id,
                digit,
                method,
                event_type,
                ..
            } => {
                assert_eq!(event_call_id, call_id);
                assert_eq!(digit, '1');
                assert_eq!(method, DtmfDetectionMethod::Rfc2833);
                assert_eq!(event_type, DtmfEventKind::End);
            }
            _ => panic!("Expected DtmfDigitDetected event"),
        }
    }

    #[tokio::test]
    async fn test_eventbus_dtmf_filtering() {
        // Test DTMF event filtering by event type
        use forge_core::{DtmfDetectionMethod, DtmfEventKind, ForgeEvent};

        let call_id = CallId::generate();

        // Create events of different types
        let start_event = ForgeEvent::DtmfDigitDetected {
            call_id: call_id.clone(),
            digit: '1',
            duration_ms: None,
            method: DtmfDetectionMethod::Rfc2833,
            event_type: DtmfEventKind::Start,
            timestamp: Utc::now(),
        };

        let end_event = ForgeEvent::DtmfDigitDetected {
            call_id: call_id.clone(),
            digit: '1',
            duration_ms: Some(100),
            method: DtmfDetectionMethod::Rfc2833,
            event_type: DtmfEventKind::End,
            timestamp: Utc::now(),
        };

        // Verify we can distinguish between Start and End
        match start_event {
            ForgeEvent::DtmfDigitDetected { event_type, .. } => {
                assert_eq!(event_type, DtmfEventKind::Start);
            }
            _ => panic!("Expected DtmfDigitDetected"),
        }

        match end_event {
            ForgeEvent::DtmfDigitDetected { event_type, .. } => {
                assert_eq!(event_type, DtmfEventKind::End);
            }
            _ => panic!("Expected DtmfDigitDetected"),
        }
    }

    #[tokio::test]
    async fn test_concurrent_session_tracking() {
        let manager = Arc::new(AISessionManager::new());

        // Create multiple call IDs
        let call_ids: Vec<CallId> = (0..5).map(|_| CallId::generate()).collect();

        // Verify all are initially non-existent
        for call_id in &call_ids {
            assert!(!manager.has_ai(call_id));
        }

        // Verify count
        assert_eq!(manager.session_count(), 0);
        assert_eq!(manager.list_sessions().len(), 0);
    }

    #[test]
    fn test_ai_session_config_clone() {
        let config1 = AISessionConfig::default();
        let config2 = config1.clone();

        assert_eq!(config1.model, config2.model);
        assert_eq!(config1.sample_rate, config2.sample_rate);
        assert_eq!(config1.enable_vad, config2.enable_vad);
    }

    #[tokio::test]
    async fn test_try_recv_audio_response_no_session() {
        let manager = AISessionManager::new();
        let call_id = CallId::generate();

        // Trying to receive audio from non-existent session should return None
        let result = manager.try_recv_audio_response(&call_id).await;
        assert!(result.is_none());
    }
}
