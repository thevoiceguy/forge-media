//! Audio codec implementations
//!
//! This module provides encoding and decoding for various audio codecs
//! used in telephony and media processing.

pub mod g711;
pub mod g729;
pub mod opus;

use crate::{AudioFormat, MediaError, Result};

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
    use crate::AudioCodec as CodecType;

    match format.codec {
        CodecType::PCMU => Ok(Box::new(g711::G711MuLaw::new(format.sample_rate))),
        CodecType::PCMA => Ok(Box::new(g711::G711ALaw::new(format.sample_rate))),
        CodecType::PCM => Err(MediaError::InvalidFormat(
            "PCM codec doesn't need encoding/decoding".to_string(),
        )),
        CodecType::Opus => {
            let config = opus::OpusConfig {
                sample_rate: format.sample_rate,
                channels: format.channels,
                ..Default::default()
            };
            Ok(Box::new(opus::OpusCodec::with_config(config)?))
        }
        CodecType::G729 => Ok(Box::new(g729::G729Codec::new())),
        _ => Err(MediaError::InvalidFormat(format!(
            "Unsupported codec: {:?}",
            format.codec
        ))),
    }
}
