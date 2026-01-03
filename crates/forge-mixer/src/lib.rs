//! Audio mixing library for Forge Media
//!
//! Combines multiple audio streams into a single mixed output

use thiserror::Error;

mod mixer;

pub use forge_core::AudioFormat;
pub use mixer::{AudioMixer, MixerOptions, ParticipantId, ParticipantMetadata, ParticipantState};

/// Mixer error types
#[derive(Error, Debug)]
pub enum MixerError {
    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Recorder error: {0}")]
    Recorder(#[from] forge_recorder::RecorderError),
}

/// Result type for mixer operations
pub type Result<T> = std::result::Result<T, MixerError>;
