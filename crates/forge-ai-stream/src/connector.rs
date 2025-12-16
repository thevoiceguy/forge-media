//! AI connector framework
//!
//! Defines the trait and types for pluggable AI service integration.

use crate::events::{AIEvent, SessionConfig, ToolDefinition};
use crate::{AIStreamStats, Result};
use async_trait::async_trait;
use forge_core::SecureString;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// AI connector type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AIConnectorType {
    /// OpenAI Realtime API
    OpenAI,
    /// Anthropic Claude Voice API
    Anthropic,
    /// ElevenLabs Conversational AI
    ElevenLabs,
    /// Deepgram Voice Agent API
    Deepgram,
    /// Google Dialogflow
    Dialogflow,
    /// Amazon Lex
    Lex,
    /// Azure Cognitive Services
    Azure,
    /// Custom connector
    Custom,
}

/// AI connector configuration
#[derive(Debug, Clone)]
pub struct AIConnectorConfig {
    /// Connector type
    pub connector_type: AIConnectorType,

    /// API key or credentials
    pub api_key: SecureString,

    /// API endpoint (optional, uses default if not provided)
    pub endpoint: Option<String>,

    /// Model identifier
    pub model: String,

    /// Voice identifier (if applicable)
    pub voice: Option<String>,

    /// Temperature for response generation (0.0-1.0)
    pub temperature: Option<f32>,

    /// Maximum response tokens
    pub max_tokens: Option<u32>,

    /// System instructions/prompt
    pub instructions: Option<String>,

    /// Available tools/functions
    pub tools: Vec<ToolDefinition>,

    /// Enable VAD (Voice Activity Detection)
    pub enable_vad: bool,

    /// Enable barge-in detection
    pub enable_barge_in: bool,

    /// Connection timeout
    pub connect_timeout: Duration,

    /// Request timeout
    pub request_timeout: Duration,
}

impl Default for AIConnectorConfig {
    fn default() -> Self {
        Self {
            connector_type: AIConnectorType::OpenAI,
            api_key: SecureString::new(String::new()),
            endpoint: None,
            model: "gpt-4o-realtime-preview".to_string(),
            voice: Some("alloy".to_string()),
            temperature: Some(0.8),
            max_tokens: Some(4096),
            instructions: None,
            tools: Vec::new(),
            enable_vad: true,
            enable_barge_in: true,
            connect_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(60),
        }
    }
}

/// AI session state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AISessionState {
    /// Session not started
    Idle,
    /// Connecting to AI service
    Connecting,
    /// Session active and ready
    Active,
    /// Session paused
    Paused,
    /// Session ended
    Ended,
    /// Error state
    Error,
}

/// AI session information
#[derive(Debug, Clone)]
pub struct AISession {
    /// Session identifier
    pub session_id: String,

    /// Session state
    pub state: AISessionState,

    /// Session configuration
    pub config: SessionConfig,

    /// Session statistics
    pub stats: AIStreamStats,

    /// Session start time
    pub start_time: std::time::Instant,
}

impl AISession {
    /// Create a new session
    pub fn new(session_id: impl Into<String>, config: SessionConfig) -> Self {
        Self {
            session_id: session_id.into(),
            state: AISessionState::Idle,
            config,
            stats: AIStreamStats::new(),
            start_time: std::time::Instant::now(),
        }
    }

    /// Get session duration
    pub fn duration(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Update session state
    pub fn set_state(&mut self, state: AISessionState) {
        self.state = state;
    }
}

/// AI connector trait
///
/// Implementations provide integration with specific AI services.
#[async_trait]
pub trait AIConnector: Send + Sync {
    /// Connect to the AI service
    async fn connect(&mut self) -> Result<String>;

    /// Disconnect from the AI service
    async fn disconnect(&mut self) -> Result<()>;

    /// Send audio data to the AI (PCM16, configurable sample rate)
    ///
    /// # Arguments
    /// * `audio_data` - Audio samples (i16 PCM)
    /// * `sample_rate` - Sample rate in Hz (e.g., 24000, 16000, 8000)
    async fn send_audio(&mut self, audio_data: &[i16], sample_rate: u32) -> Result<()>;

    /// Receive the next event from the AI
    ///
    /// Returns None if no event is available (non-blocking) or the connection is closed.
    async fn next_event(&mut self) -> Result<Option<AIEvent>>;

    /// Send a function call response
    async fn send_function_response(&mut self, call_id: impl Into<String> + Send, output: impl Into<String> + Send) -> Result<()>;

    /// Interrupt the current AI response (barge-in)
    async fn interrupt(&mut self) -> Result<()>;

    /// Update session configuration
    async fn update_config(&mut self, config: SessionConfig) -> Result<()>;

    /// Get current session information
    fn session(&self) -> Option<&AISession>;

    /// Get mutable session information
    fn session_mut(&mut self) -> Option<&mut AISession>;

    /// Get connector type
    fn connector_type(&self) -> AIConnectorType;

    /// Get statistics
    fn stats(&self) -> &AIStreamStats;

    /// Reset statistics
    fn reset_stats(&mut self);

    /// Check if connected
    fn is_connected(&self) -> bool;

    /// Get session state
    fn state(&self) -> AISessionState {
        self.session()
            .map(|s| s.state.clone())
            .unwrap_or(AISessionState::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = AIConnectorConfig::default();
        assert_eq!(config.connector_type, AIConnectorType::OpenAI);
        assert_eq!(config.model, "gpt-4o-realtime-preview");
        assert!(config.enable_vad);
    }

    #[test]
    fn test_session_creation() {
        let config = SessionConfig {
            model: "gpt-4o".to_string(),
            voice: Some("alloy".to_string()),
            temperature: Some(0.8),
            max_tokens: Some(4096),
            instructions: None,
            turn_detection: None,
            tools: Vec::new(),
        };

        let session = AISession::new("test-session", config.clone());
        assert_eq!(session.session_id, "test-session");
        assert_eq!(session.state, AISessionState::Idle);
        assert_eq!(session.config.model, "gpt-4o");
    }

    #[test]
    fn test_session_state_transitions() {
        let config = SessionConfig {
            model: "test".to_string(),
            voice: None,
            temperature: None,
            max_tokens: None,
            instructions: None,
            turn_detection: None,
            tools: Vec::new(),
        };

        let mut session = AISession::new("test", config);
        assert_eq!(session.state, AISessionState::Idle);

        session.set_state(AISessionState::Connecting);
        assert_eq!(session.state, AISessionState::Connecting);

        session.set_state(AISessionState::Active);
        assert_eq!(session.state, AISessionState::Active);
    }
}
