//! Core traits for media processing
//!
//! This module defines the fundamental traits used throughout Forge for
//! codec implementation, media processing, and audio manipulation.

use crate::error::Result;
use std::fmt::Debug;

/// Audio sample format
///
/// Forge uses signed 16-bit PCM as the internal format for all audio processing.
pub type AudioSample = i16;

/// Audio frame - a buffer of PCM samples
pub type AudioFrame = Vec<AudioSample>;

/// Decoder trait for converting encoded audio to PCM samples
///
/// All codec decoders implement this trait to provide a uniform interface
/// for decoding compressed audio into PCM samples.
///
/// # Example
///
/// ```rust,ignore
/// struct OpusDecoder { /* ... */ }
///
/// impl Decoder for OpusDecoder {
///     fn decode(&mut self, data: &[u8]) -> Result<AudioFrame> {
///         // Decode opus to PCM
///     }
///
///     fn sample_rate(&self) -> u32 {
///         48000
///     }
/// }
/// ```
pub trait Decoder: Send + Sync + Debug {
    /// Decode compressed audio data into PCM samples
    ///
    /// # Arguments
    ///
    /// * `data` - Compressed audio data
    ///
    /// # Returns
    ///
    /// A vector of PCM samples (16-bit signed integers)
    fn decode(&mut self, data: &[u8]) -> Result<AudioFrame>;

    /// Get the output sample rate in Hz
    fn sample_rate(&self) -> u32;

    /// Get the number of audio channels (1 = mono, 2 = stereo)
    fn channels(&self) -> u8 {
        1 // Default to mono
    }

    /// Get the expected frame size in samples (if fixed)
    ///
    /// Returns `None` for variable frame size codecs
    fn frame_size(&self) -> Option<usize> {
        None
    }

    /// Reset decoder state
    ///
    /// Useful when seeking or recovering from packet loss
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Encoder trait for converting PCM samples to encoded audio
///
/// All codec encoders implement this trait to provide a uniform interface
/// for encoding PCM samples into compressed audio.
///
/// # Example
///
/// ```rust,ignore
/// struct OpusEncoder { /* ... */ }
///
/// impl Encoder for OpusEncoder {
///     fn encode(&mut self, samples: &[AudioSample]) -> Result<Vec<u8>> {
///         // Encode PCM to opus
///     }
///
///     fn sample_rate(&self) -> u32 {
///         48000
///     }
///
///     fn frame_size(&self) -> usize {
///         960 // 20ms at 48kHz
///     }
/// }
/// ```
pub trait Encoder: Send + Sync + Debug {
    /// Encode PCM samples into compressed audio data
    ///
    /// # Arguments
    ///
    /// * `samples` - PCM audio samples (16-bit signed integers)
    ///
    /// # Returns
    ///
    /// Compressed audio data
    ///
    /// # Note
    ///
    /// The input should typically be `frame_size()` samples, but some
    /// encoders may accept variable input sizes.
    fn encode(&mut self, samples: &[AudioSample]) -> Result<Vec<u8>>;

    /// Get the input sample rate in Hz
    fn sample_rate(&self) -> u32;

    /// Get the number of audio channels (1 = mono, 2 = stereo)
    fn channels(&self) -> u8 {
        1 // Default to mono
    }

    /// Get the expected frame size in samples
    ///
    /// This is the number of samples that should be passed to `encode()`
    /// for optimal performance and quality.
    fn frame_size(&self) -> usize;

    /// Get the target bitrate in bits per second
    fn bitrate(&self) -> u32 {
        0 // 0 = default/unknown
    }

    /// Set the target bitrate in bits per second
    ///
    /// Not all encoders support dynamic bitrate changes.
    fn set_bitrate(&mut self, _bitrate: u32) -> Result<()> {
        Ok(())
    }

    /// Reset encoder state
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Combined Codec trait for types that can both encode and decode
///
/// This is a convenience trait for codecs that implement both encoding
/// and decoding functionality.
///
/// # Example
///
/// ```rust,ignore
/// struct G711Codec { /* ... */ }
///
/// impl Codec for G711Codec {
///     type Encoder = G711Encoder;
///     type Decoder = G711Decoder;
///
///     fn encoder(&self) -> Self::Encoder {
///         G711Encoder::new()
///     }
///
///     fn decoder(&self) -> Self::Decoder {
///         G711Decoder::new()
///     }
/// }
/// ```
pub trait Codec: Send + Sync + Debug {
    /// The encoder type for this codec
    type Encoder: Encoder;

    /// The decoder type for this codec
    type Decoder: Decoder;

    /// Create a new encoder instance
    fn encoder(&self) -> Result<Self::Encoder>;

    /// Create a new decoder instance
    fn decoder(&self) -> Result<Self::Decoder>;

    /// Get the codec name
    fn name(&self) -> &str;

    /// Get the typical sample rate for this codec
    fn sample_rate(&self) -> u32;
}

/// Audio processor trait for signal processing
///
/// Implement this trait for audio effects, filters, AGC, noise reduction, etc.
///
/// # Example
///
/// ```rust,ignore
/// struct NoiseGate {
///     threshold: f32,
/// }
///
/// impl AudioProcessor for NoiseGate {
///     fn process(&mut self, samples: &mut [AudioSample]) -> Result<()> {
///         for sample in samples.iter_mut() {
///             if sample.abs() < (self.threshold * i16::MAX as f32) as i16 {
///                 *sample = 0;
///             }
///         }
///         Ok(())
///     }
/// }
/// ```
pub trait AudioProcessor: Send + Sync + Debug {
    /// Process audio samples in-place
    ///
    /// # Arguments
    ///
    /// * `samples` - Audio samples to process (modified in-place)
    fn process(&mut self, samples: &mut [AudioSample]) -> Result<()>;

    /// Get the sample rate this processor operates at
    fn sample_rate(&self) -> u32 {
        48000 // Default to 48kHz
    }

    /// Reset processor state
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Audio mixer trait for combining multiple audio sources
///
/// # Example
///
/// ```rust,ignore
/// struct SimpleMixer;
///
/// impl AudioMixer for SimpleMixer {
///     fn mix(&self, sources: &[&[AudioSample]]) -> Result<AudioFrame> {
///         let len = sources.first().map(|s| s.len()).unwrap_or(0);
///         let mut output = vec![0i32; len];
///
///         for source in sources {
///             for (i, &sample) in source.iter().enumerate() {
///                 output[i] += sample as i32;
///             }
///         }
///
///         // Clip to i16 range
///         Ok(output.into_iter()
///             .map(|s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
///             .collect())
///     }
/// }
/// ```
pub trait AudioMixer: Send + Sync + Debug {
    /// Mix multiple audio sources into a single output
    ///
    /// # Arguments
    ///
    /// * `sources` - Slice of audio source buffers to mix
    ///
    /// # Returns
    ///
    /// Mixed audio frame
    ///
    /// # Note
    ///
    /// All source buffers should have the same length. If they don't,
    /// the implementation should either pad with silence or truncate.
    fn mix(&self, sources: &[&[AudioSample]]) -> Result<AudioFrame>;

    /// Get the sample rate this mixer operates at
    fn sample_rate(&self) -> u32 {
        48000 // Default to 48kHz
    }
}

/// Resampler trait for converting between sample rates
///
/// # Example
///
/// ```rust,ignore
/// struct LinearResampler {
///     from_rate: u32,
///     to_rate: u32,
/// }
///
/// impl Resampler for LinearResampler {
///     fn resample(&self, input: &[AudioSample]) -> Result<AudioFrame> {
///         // Resample using linear interpolation
///     }
///
///     fn from_rate(&self) -> u32 {
///         self.from_rate
///     }
///
///     fn to_rate(&self) -> u32 {
///         self.to_rate
///     }
/// }
/// ```
pub trait Resampler: Send + Sync + Debug {
    /// Resample audio from one sample rate to another
    ///
    /// # Arguments
    ///
    /// * `input` - Input audio samples at `from_rate()`
    ///
    /// # Returns
    ///
    /// Resampled audio at `to_rate()`
    fn resample(&self, input: &[AudioSample]) -> Result<AudioFrame>;

    /// Get the input sample rate in Hz
    fn from_rate(&self) -> u32;

    /// Get the output sample rate in Hz
    fn to_rate(&self) -> u32;

    /// Reset resampler state
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock decoder for testing
    #[derive(Debug)]
    struct MockDecoder {
        sample_rate: u32,
    }

    impl Decoder for MockDecoder {
        fn decode(&mut self, data: &[u8]) -> Result<AudioFrame> {
            // Return same number of samples as input bytes
            Ok(vec![0i16; data.len()])
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }

    // Mock encoder for testing
    #[derive(Debug)]
    struct MockEncoder {
        sample_rate: u32,
        frame_size: usize,
    }

    impl Encoder for MockEncoder {
        fn encode(&mut self, samples: &[AudioSample]) -> Result<Vec<u8>> {
            // Return same number of bytes as input samples
            Ok(vec![0u8; samples.len()])
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn frame_size(&self) -> usize {
            self.frame_size
        }
    }

    #[test]
    fn test_decoder_trait() {
        let mut decoder = MockDecoder { sample_rate: 8000 };
        let data = vec![1, 2, 3, 4];
        let samples = decoder.decode(&data).unwrap();
        assert_eq!(samples.len(), 4);
        assert_eq!(decoder.sample_rate(), 8000);
    }

    #[test]
    fn test_encoder_trait() {
        let mut encoder = MockEncoder {
            sample_rate: 8000,
            frame_size: 160,
        };
        let samples = vec![0i16; 160];
        let data = encoder.encode(&samples).unwrap();
        assert_eq!(data.len(), 160);
        assert_eq!(encoder.sample_rate(), 8000);
        assert_eq!(encoder.frame_size(), 160);
    }
}
