//! Forge Media Processor
//!
//! Advanced media processing capabilities including:
//! - Audio recording (WAV, Opus)
//! - Multi-party audio mixing
//! - Codec transcoding
//! - Conference bridge management
//!
//! ## Codec Support Status
//!
//! | Codec | Status | Features |
//! |-------|--------|----------|
//! | **G.711 μ-law (PCMU)** | ✅ Complete | Production-ready, 64 kbit/s |
//! | **G.711 A-law (PCMA)** | ✅ Complete | Production-ready, 64 kbit/s |
//! | **Opus** | ✅ Complete | Requires `opus` feature, 6-510 kbit/s |
//! | **G.729** | 🚧 Skeleton | Not yet implemented, 8 kbit/s |
//!
//! ### Enabling Optional Codecs
//!
//! ```toml
//! [dependencies]
//! forge-media-processor = { version = "0.1", features = ["opus"] }
//! ```
//!
//! **Note:** Opus requires `libopus-dev` (or equivalent) and `cmake` to be installed.

pub mod codecs;

use thiserror::Error;

/// Media processor errors
#[derive(Debug, Error)]
pub enum MediaError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Audio encoding error: {0}")]
    Encoding(String),

    #[error("Audio decoding error: {0}")]
    Decoding(String),

    #[error("Invalid audio format: {0}")]
    InvalidFormat(String),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("Conference not found: {0}")]
    ConferenceNotFound(String),

    #[error("Buffer overflow")]
    BufferOverflow,

    #[error("Not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Resampler error: {0}")]
    Resampler(#[from] forge_resampler::ResamplerError),

    #[error("Recorder error: {0}")]
    Recorder(#[from] forge_recorder::RecorderError),

    #[error("Mixer error: {0}")]
    Mixer(#[from] forge_mixer::MixerError),
}

/// Result type for media operations
pub type Result<T> = std::result::Result<T, MediaError>;

// Re-export core types
pub use forge_core::{AudioCodec, AudioFormat};
