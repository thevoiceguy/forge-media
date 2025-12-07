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
    // Types
    CallId,
    RoomId,
    ParticipantId,
    LegIdentifier,
    MediaDirection,
    MediaType,
    AudioCodec,
    CodecConfig,
    SessionState,
    RecordingId,
    IpVersionConfig,
    Transport,

    // Config
    ForgeConfig,
    EngineConfig,
    ApiConfig,
    PortRange,
    InterfaceConfig,

    // Errors
    ForgeError,
    Result,
};

// Re-export RTP types
pub use forge_rtp::{
    RtpHeader,
    RtpPacket,
    RtpExtension,
    RtcpPacketType,
    SrtpProfile,
    JitterBuffer,
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
        // TODO: Initialize all subsystems
        Ok(Self { config })
    }

    /// Get the engine configuration
    pub fn config(&self) -> &ForgeConfig {
        &self.config
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
