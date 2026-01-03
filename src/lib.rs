//! Forge Media Engine
//!
//! A high-performance RTP and WebRTC media engine for real-time communications.
//!
//! # Usage as a Library
//!
//! ```rust,no_run
//! use forge_media::{ForgeEngine, ForgeConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = ForgeConfig::default();
//!     let engine = ForgeEngine::new(config).await?;
//!
//!     // Use the engine...
//!
//!     Ok(())
//! }
//! ```
//!
//! # Features
//!
//! - `transcoding` - Codec transcoding support (default)
//! - `conference` - Audio conferencing (default)
//! - `recording` - Call recording (default)
//! - `dtmf` - DTMF detection and generation (default)
//! - `transcription` - Real-time transcription
//! - `injection` - Audio injection and TTS
//! - `webrtc` - WebRTC support
//! - `sbc` - SBC features
//! - `siprec` - SIPREC recording
//! - `ai-stream` - AI streaming integration
//! - `ha` - High availability
//! - `full` - Enable all features

// Re-export core types
pub use forge_core::{
    ApiConfig,
    AudioCodec,
    AudioFrame,

    AudioMixer,
    AudioProcessor,
    AudioSample,
    // Types
    CallId,
    Codec,
    CodecConfig,
    Decoder,
    // Traits
    Encoder,
    EngineConfig,
    EventBus,
    // Config
    ForgeConfig,
    // Errors
    ForgeError,
    // Events
    ForgeEvent,
    InterfaceConfig,

    IpVersionConfig,
    LegIdentifier,
    MediaDirection,
    MediaType,
    ParticipantId,
    PortRange,
    RecordingId,
    Resampler,
    Result,

    RoomId,
    SessionState,
    Transport,
};

// Re-export RTP types
pub use forge_rtp::{
    JitterBuffer, RtcpPacketType, RtpExtension, RtpHeader, RtpPacket, SrtpProfile,
};

// Main engine (to be implemented)
/// Main Forge Media Engine
///
/// This is the primary interface for using Forge as a library.
///
/// # Example
///
/// ```rust,no_run
/// use forge_media::{ForgeEngine, ForgeConfig};
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let config = ForgeConfig::default();
/// let engine = ForgeEngine::new(config).await?;
///
/// // Create a session
/// // let session = engine.create_session(...).await?;
/// # Ok(())
/// # }
/// ```
pub struct ForgeEngine {
    config: ForgeConfig,
    event_bus: std::sync::Arc<EventBus>,
}

impl ForgeEngine {
    /// Create a new Forge engine with the given configuration
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use forge_media::{ForgeEngine, ForgeConfig};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = ForgeConfig::default();
    /// let engine = ForgeEngine::new(config).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(config: ForgeConfig) -> Result<Self> {
        config.validate()?;

        // Initialize event bus for broadcasting media events (sessions, SIPREC, etc.)
        let event_bus = std::sync::Arc::new(EventBus::new());

        // TODO: Initialize all subsystems, wiring event_bus where needed
        Ok(Self { config, event_bus })
    }

    /// Get the engine configuration
    pub fn config(&self) -> &ForgeConfig {
        &self.config
    }

    /// Subscribe to engine events (sessions, SIPREC, DTMF, etc.)
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<ForgeEvent> {
        self.event_bus.subscribe()
    }

    // TODO: Add session management methods
    // pub async fn create_session(&self, request: CreateSessionRequest) -> Result<Session> { }
    // pub async fn delete_session(&self, call_id: &CallId) -> Result<()> { }
    // pub async fn get_session(&self, call_id: &CallId) -> Result<Session> { }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_engine_creation() {
        let config = ForgeConfig::default();
        let engine = ForgeEngine::new(config).await;
        assert!(engine.is_ok());
    }
}
