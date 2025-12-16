//! forge-ai-stream - AI Integration for Real-time Voice
//!
//! This crate provides integration with AI services for real-time voice conversations:
//! - OpenAI Realtime API
//! - Anthropic Claude Voice API
//! - ElevenLabs Conversational AI
//! - Deepgram Voice Agent API
//! - Google Dialogflow
//! - Amazon Lex
//! - Azure Cognitive Services
//! - Custom AI connectors
//!
//! # Features
//!
//! - **AI Connector Framework**: Pluggable AI service integration
//! - **Audio Format Conversion**: PCM16 ↔ G.711 ↔ other formats
//! - **VAD Integration**: Voice Activity Detection for optimal turn-taking
//! - **Barge-in Detection**: Interrupt AI when user speaks
//! - **Tool/Function Calling**: Enable AI to call application functions
//! - **Event Streaming**: WebSocket-based real-time events
//!
//! # Example
//!
//! ```rust,ignore
//! use forge_ai_stream::{OpenAIConnector, AIConnectorConfig, AudioFormat};
//!
//! let config = AIConnectorConfig {
//!     api_key: forge_core::SecureString::new("sk-..."),
//!     model: "gpt-4o-realtime-preview".to_string(),
//!     ..Default::default()
//! };
//!
//! let mut connector = OpenAIConnector::new(config).await?;
//!
//! // Send audio
//! connector.send_audio(&audio_samples).await?;
//!
//! // Receive AI responses
//! while let Some(event) = connector.next_event().await? {
//!     match event {
//!         AIEvent::AudioResponse(audio) => play_audio(audio),
//!         AIEvent::Transcript(text) => println!("AI: {}", text),
//!         AIEvent::FunctionCall(call) => handle_function(call),
//!         _ => {}
//!     }
//! }
//! ```

pub mod audio;
pub mod bargein;
pub mod connector;
pub mod events;
pub mod openai;
pub mod anthropic;
pub mod elevenlabs;
pub mod deepgram;
pub mod vad;

pub use audio::{AudioConverter, AudioFormat, AudioSample};
pub use bargein::{BargeInConfig, BargeInDetector, BargeInState};
pub use connector::{AIConnector, AIConnectorConfig, AIConnectorType, AISession};
pub use events::{AIEvent, AIEventType, FunctionCall, TranscriptSegment};
pub use openai::{OpenAIConnector, OpenAIConfig, OpenAIModel};
pub use anthropic::{AnthropicConnector, AnthropicConfig, AnthropicModel};
pub use elevenlabs::{ElevenLabsConnector, ElevenLabsConfig, ElevenLabsModel, ElevenLabsVoices};
pub use deepgram::{DeepgramConnector, DeepgramConfig, DeepgramModel, DeepgramVoices};
pub use vad::{VadConfig, VadDetector, VadState};

use thiserror::Error;

/// AI streaming error types
#[derive(Error, Debug)]
pub enum AIStreamError {
    /// WebSocket connection error
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    /// Connection error
    #[error("Connection error: {0}")]
    Connection(String),

    /// Protocol error
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// HTTP/API error
    #[error("API error: {0}")]
    Api(String),

    /// Authentication error
    #[error("Authentication error: {0}")]
    Auth(String),

    /// Audio format error
    #[error("Audio format error: {0}")]
    AudioFormat(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Session error
    #[error("Session error: {0}")]
    Session(String),

    /// Network error
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),

    /// Timeout error
    #[error("Timeout after {0:?}")]
    Timeout(std::time::Duration),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for AI streaming operations
pub type Result<T> = std::result::Result<T, AIStreamError>;

/// AI streaming statistics
#[derive(Debug, Clone, Default)]
pub struct AIStreamStats {
    /// Total audio samples sent
    pub samples_sent: u64,

    /// Total audio samples received
    pub samples_received: u64,

    /// Total events sent
    pub events_sent: u64,

    /// Total events received
    pub events_received: u64,

    /// Function calls made
    pub function_calls: u64,

    /// WebSocket messages sent
    pub ws_messages_sent: u64,

    /// WebSocket messages received
    pub ws_messages_received: u64,

    /// Connection uptime in seconds
    pub uptime_seconds: u64,

    /// Average latency in milliseconds
    pub avg_latency_ms: u32,
}

impl AIStreamStats {
    /// Create new stats
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment samples sent
    pub fn increment_samples_sent(&mut self, count: u64) {
        self.samples_sent += count;
    }

    /// Increment samples received
    pub fn increment_samples_received(&mut self, count: u64) {
        self.samples_received += count;
    }

    /// Increment events
    pub fn increment_events(&mut self, sent: bool) {
        if sent {
            self.events_sent += 1;
        } else {
            self.events_received += 1;
        }
    }

    /// Increment function calls
    pub fn increment_function_calls(&mut self) {
        self.function_calls += 1;
    }
}
