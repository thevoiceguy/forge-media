//! Forge Media Processor
//!
//! Advanced media processing capabilities including:
//! - Audio recording (WAV, Opus)
//! - Multi-party audio mixing
//! - Codec transcoding
//! - Conference bridge management
//!
//! ## Codec Support
//!
//! This crate re-exports codecs from `forge-codecs`. See the `forge-codecs` crate
//! for detailed codec documentation.
//!
//! | Codec | Status | Features |
//! |-------|--------|----------|
//! | **G.711 μ-law (PCMU)** | ✅ Complete | Production-ready, 64 kbit/s |
//! | **G.711 A-law (PCMA)** | ✅ Complete | Production-ready, 64 kbit/s |
//! | **G.722** | ✅ Complete | Requires `g722` feature, 64 kbit/s |
//! | **G.729** | ✅ Complete | Requires `g729` feature, 8 kbit/s |
//! | **Opus** | ✅ Complete | Requires `opus` feature, 6-510 kbit/s |
//!
//! ### Enabling Optional Codecs
//!
//! ```toml
//! [dependencies]
//! forge-media-processor = { version = "0.1", features = ["opus", "g729"] }
//! # Or enable all codecs:
//! forge-media-processor = { version = "0.1", features = ["all-codecs"] }
//! ```
//!
//! **Note:**
//! - Opus requires `libopus-dev` (or equivalent) and `cmake`
//! - G.729 requires `libbcg729-dev` (or equivalent)

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

    #[error("Codec error: {0}")]
    Codec(#[from] forge_codecs::CodecError),
}

/// Result type for media operations
pub type Result<T> = std::result::Result<T, MediaError>;

// Re-export core types
pub use forge_core::{AudioCodec, AudioFormat};

// Re-export codecs from forge-codecs
pub use forge_codecs::{
    // G.711 codecs (always available)
    g711::{G711ALaw, G711MuLaw},
    // Core codec trait and utilities
    AudioCodec as CodecTrait,
    CodecError,
};

// Conditionally re-export optional codecs
#[cfg(feature = "g722")]
pub use forge_codecs::g722::G722Codec;

#[cfg(feature = "g729")]
pub use forge_codecs::g729::G729Codec;

#[cfg(feature = "opus")]
pub use forge_codecs::opus::{OpusCodec, OpusConfig};
