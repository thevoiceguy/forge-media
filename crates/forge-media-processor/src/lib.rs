//! Forge Media Processor
//!
//! Advanced media processing capabilities including:
//! - Audio recording (WAV, Opus)
//! - Multi-party audio mixing
//! - Codec transcoding
//! - Conference bridge management

pub mod conference;
pub mod mixer;
pub mod recorder;
pub mod storage;

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
}

/// Result type for media operations
pub type Result<T> = std::result::Result<T, MediaError>;

/// Audio codec types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Opus codec
    Opus,
    /// G.711 µ-law (PCMU)
    PCMU,
    /// G.711 A-law (PCMA)
    PCMA,
    /// Raw PCM
    PCM,
}

impl AudioCodec {
    /// Parse codec from string (case-insensitive)
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "opus" => Ok(AudioCodec::Opus),
            "pcmu" => Ok(AudioCodec::PCMU),
            "pcma" => Ok(AudioCodec::PCMA),
            "pcm" | "wav" => Ok(AudioCodec::PCM),
            _ => Err(MediaError::InvalidFormat(format!("Unknown codec: {}", s))),
        }
    }

    /// Get the recommended file extension for this codec
    pub fn file_extension(&self) -> &str {
        match self {
            AudioCodec::Opus => "opus",
            AudioCodec::PCM => "wav",
            AudioCodec::PCMU | AudioCodec::PCMA => "wav",
        }
    }
}

/// Audio sample format
#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    /// Sample rate in Hz (e.g., 48000, 16000, 8000)
    pub sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo)
    pub channels: u16,
    /// Codec used for encoding
    pub codec: AudioCodec,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            codec: AudioCodec::Opus,
        }
    }
}

impl AudioFormat {
    /// Create a new audio format
    pub fn new(sample_rate: u32, channels: u16, codec: AudioCodec) -> Self {
        Self {
            sample_rate,
            channels,
            codec,
        }
    }

    /// Create Opus format (48kHz mono)
    pub fn opus_mono() -> Self {
        Self::new(48000, 1, AudioCodec::Opus)
    }

    /// Create PCM format for WAV files (48kHz mono)
    pub fn pcm_mono() -> Self {
        Self::new(48000, 1, AudioCodec::PCM)
    }
}
