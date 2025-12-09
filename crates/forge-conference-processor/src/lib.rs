//! Conference processing library for Forge Media
//!
//! Manages multi-party audio conferences with mixing and recording

use thiserror::Error;

mod conference;

pub use conference::{ConferenceBridge, ConferenceRoom, RoomId};
pub use forge_core::AudioFormat;

/// Conference error types
#[derive(Error, Debug)]
pub enum ConferenceError {
    #[error("Conference not found: {0}")]
    ConferenceNotFound(String),

    #[error("Room not found: {0}")]
    RoomNotFound(String),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Mixer error: {0}")]
    Mixer(#[from] forge_mixer::MixerError),

    #[error("Recorder error: {0}")]
    Recorder(#[from] forge_recorder::RecorderError),
}

/// Result type for conference operations
pub type Result<T> = std::result::Result<T, ConferenceError>;
