//! Audio recording library for Forge Media
//!
//! Records RTP audio packets to WAV or Opus files

use thiserror::Error;

mod playback;
mod recorder;

pub use forge_core::{AudioCodec, AudioFormat};
pub use playback::PlaybackSource;
pub use recorder::AudioRecorder;

#[cfg(feature = "opus")]
pub use recorder::OpusMetadata;

/// Recorder error types
#[derive(Error, Debug)]
pub enum RecorderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Audio format error: {0}")]
    WavError(#[from] hound::Error),

    #[error("Audio encoding error: {0}")]
    Encoding(String),

    #[error("Unsupported codec: {0}")]
    UnsupportedCodec(String),

    #[error("Recorder already started")]
    AlreadyRecording,

    #[error("Recorder not started")]
    NotRecording,

    #[error("Invalid audio format: {0}")]
    InvalidFormat(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[cfg(feature = "opus")]
impl From<ogg::OggReadError> for RecorderError {
    fn from(err: ogg::OggReadError) -> Self {
        RecorderError::InvalidFormat(format!("Ogg read error: {}", err))
    }
}

/// Result type for recorder operations
pub type Result<T> = std::result::Result<T, RecorderError>;
