//! Forge Media Processor
//!
//! Advanced media processing capabilities including:
//! - Audio recording (WAV, Opus)
//! - Multi-party audio mixing
//! - Codec transcoding
//! - Conference bridge management

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
