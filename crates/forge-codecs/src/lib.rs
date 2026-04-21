//! Forge Audio Codecs
//!
//! Provides encoding and decoding for various audio codecs used in telephony
//! and media processing: Opus, G.711 (µ-law/A-law), G.722, G.729.

// Pre-existing lint tolerances:
// - `dead_code`: g729.rs carries placeholder structs for a future pure-Rust
//   implementation (currently FFI-backed by bcg729). Documented as reserved.
// - Style lints that would require public-API changes to fix.
#![allow(dead_code)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::needless_return)]
#![allow(clippy::wrong_self_convention)]
#![allow(clippy::should_implement_trait)]

pub mod g711;
#[cfg(feature = "g722")]
pub mod g722;
pub mod g729;
pub mod opus;

use thiserror::Error;

/// Codec errors
#[derive(Debug, Error)]
pub enum CodecError {
    #[error("Audio encoding error: {0}")]
    Encoding(String),

    #[error("Audio decoding error: {0}")]
    Decoding(String),

    #[error("Invalid audio format: {0}")]
    InvalidFormat(String),

    #[error("Codec not supported: {0}")]
    NotSupported(String),

    #[error("Codec initialization failed: {0}")]
    InitializationFailed(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for codec operations
pub type Result<T> = std::result::Result<T, CodecError>;

/// Audio codec types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodecType {
    /// Opus codec
    Opus,
    /// G.711 µ-law (PCMU)
    PCMU,
    /// G.711 A-law (PCMA)
    PCMA,
    /// G.722 wideband codec
    G722,
    /// G.729 codec
    G729,
    /// Raw PCM
    PCM,
}

impl AudioCodecType {
    /// Parse codec from string (case-insensitive)
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "opus" => Ok(AudioCodecType::Opus),
            "pcmu" => Ok(AudioCodecType::PCMU),
            "pcma" => Ok(AudioCodecType::PCMA),
            "g722" => Ok(AudioCodecType::G722),
            "g729" => Ok(AudioCodecType::G729),
            "pcm" | "wav" => Ok(AudioCodecType::PCM),
            _ => Err(CodecError::InvalidFormat(format!("Unknown codec: {}", s))),
        }
    }

    /// Get the recommended file extension for this codec
    pub fn file_extension(&self) -> &str {
        match self {
            AudioCodecType::Opus => "opus",
            AudioCodecType::PCM => "wav",
            AudioCodecType::PCMU | AudioCodecType::PCMA => "wav",
            AudioCodecType::G722 => "g722",
            AudioCodecType::G729 => "g729",
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
    pub codec: AudioCodecType,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            codec: AudioCodecType::Opus,
        }
    }
}

impl AudioFormat {
    /// Create a new audio format
    pub fn new(sample_rate: u32, channels: u16, codec: AudioCodecType) -> Self {
        Self {
            sample_rate,
            channels,
            codec,
        }
    }

    /// Create Opus format (48kHz mono)
    pub fn opus_mono() -> Self {
        Self::new(48000, 1, AudioCodecType::Opus)
    }

    /// Create PCM format for WAV files (48kHz mono)
    pub fn pcm_mono() -> Self {
        Self::new(48000, 1, AudioCodecType::PCM)
    }
}

/// Audio codec trait for encoding and decoding
pub trait AudioCodec: Send + Sync {
    /// Get codec name
    fn name(&self) -> &str;

    /// Get the native sample format this codec works with
    fn native_format(&self) -> AudioFormat;

    /// Encode PCM samples to codec format
    ///
    /// Input: 16-bit PCM samples
    /// Output: Encoded bytes
    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>>;

    /// Decode codec format to PCM samples
    ///
    /// Input: Encoded bytes
    /// Output: 16-bit PCM samples
    fn decode(&mut self, encoded: &[u8]) -> Result<Vec<i16>>;

    /// Get frame size in samples (for codecs with fixed frame size)
    fn frame_size(&self) -> Option<usize> {
        None
    }

    /// Reset encoder/decoder state
    fn reset(&mut self) {
        // Default: no-op
    }
}

/// Create a codec instance from AudioFormat
pub fn create_codec(format: &AudioFormat) -> Result<Box<dyn AudioCodec>> {
    match format.codec {
        AudioCodecType::PCMU => Ok(Box::new(g711::G711MuLaw::new(format.sample_rate))),
        AudioCodecType::PCMA => Ok(Box::new(g711::G711ALaw::new(format.sample_rate))),
        AudioCodecType::PCM => Err(CodecError::InvalidFormat(
            "PCM codec doesn't need encoding/decoding".to_string(),
        )),
        AudioCodecType::Opus => {
            let config = opus::OpusConfig {
                sample_rate: format.sample_rate,
                channels: format.channels,
                ..Default::default()
            };
            Ok(Box::new(opus::OpusCodec::with_config(config)?))
        }
        #[cfg(feature = "g722")]
        AudioCodecType::G722 => Ok(Box::new(g722::G722Codec::new(g722::G722BitRate::Rate64k))),
        #[cfg(not(feature = "g722"))]
        AudioCodecType::G722 => Err(CodecError::NotSupported(
            "G.722 support not enabled. Compile with --features g722".to_string(),
        )),
        AudioCodecType::G729 => Err(CodecError::NotSupported(
            "G.729 codec not yet implemented".to_string(),
        )),
    }
}
