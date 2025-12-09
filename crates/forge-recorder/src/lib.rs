//! Audio recording library for Forge Media
//!
//! Records RTP audio packets to WAV or Opus files

use thiserror::Error;

mod recorder;

pub use forge_core::{AudioCodec, AudioFormat};
pub use recorder::AudioRecorder;

/// Recorder error types
#[derive(Error, Debug)]
pub enum RecorderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Audio encoding error: {0}")]
    Encoding(String),

    #[error("Invalid audio format: {0}")]
    InvalidFormat(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for recorder operations
pub type Result<T> = std::result::Result<T, RecorderError>;
