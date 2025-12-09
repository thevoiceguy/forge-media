//! Audio transcoding pipeline
//!
//! This module provides end-to-end audio transcoding between different
//! codecs and sample rates. It combines codec encode/decode operations
//! with sample rate conversion (resampling) to enable seamless audio
//! format conversion.
//!
//! # Example
//!
//! ```rust,no_run
//! use forge_media_processor::{AudioFormat, AudioCodec, transcoder::Transcoder};
//!
//! let src_format = AudioFormat {
//!     sample_rate: 8000,
//!     channels: 1,
//!     codec: AudioCodec::PCMU,
//! };
//!
//! let dst_format = AudioFormat {
//!     sample_rate: 48000,
//!     channels: 1,
//!     codec: AudioCodec::Opus,
//! };
//!
//! let mut transcoder = Transcoder::new(src_format, dst_format).unwrap();
//!
//! // Transcode audio data
//! let g711_data = vec![0u8; 80]; // 10ms of G.711 μ-law
//! let opus_data = transcoder.transcode(&g711_data).unwrap();
//! ```

use crate::{AudioFormat, TranscoderError, Result};
use forge_codecs::{self as codecs, AudioCodec};
use forge_resampler::Resampler;

/// Audio transcoder for format conversion
///
/// The transcoder handles the complete pipeline:
/// 1. Decode source format → PCM
/// 2. Resample PCM (if sample rates differ)
/// 3. Encode PCM → destination format
pub struct Transcoder {
    src_format: AudioFormat,
    dst_format: AudioFormat,

    // Source codec (decoder)
    src_codec: Option<Box<dyn AudioCodec>>,

    // Destination codec (encoder)
    dst_codec: Option<Box<dyn AudioCodec>>,

    // Resampler (if sample rates differ)
    resampler: Option<Resampler>,

    // Intermediate PCM buffer
    pcm_buffer: Vec<i16>,
}

impl Transcoder {
    /// Create a new transcoder
    ///
    /// # Arguments
    /// * `src_format` - Source audio format
    /// * `dst_format` - Destination audio format
    pub fn new(src_format: AudioFormat, dst_format: AudioFormat) -> Result<Self> {
        // Validate formats
        if src_format.channels != dst_format.channels {
            return Err(TranscoderError::InvalidFormat(
                "Channel count mismatch - channel conversion not yet supported".to_string(),
            ));
        }

        // Create source codec (decoder) if not PCM
        let src_codec = if src_format.codec != crate::AudioCodec::PCM {
            Some(codecs::create_codec(&src_format)?)
        } else {
            None
        };

        // Create destination codec (encoder) if not PCM
        let dst_codec = if dst_format.codec != crate::AudioCodec::PCM {
            Some(codecs::create_codec(&dst_format)?)
        } else {
            None
        };

        // Create resampler if sample rates differ
        let resampler = if src_format.sample_rate != dst_format.sample_rate {
            Some(Resampler::new(
                src_format.sample_rate,
                dst_format.sample_rate,
                src_format.channels,
            )?)
        } else {
            None
        };

        Ok(Self {
            src_format,
            dst_format,
            src_codec,
            dst_codec,
            resampler,
            pcm_buffer: Vec::new(),
        })
    }

    /// Transcode audio data from source format to destination format
    ///
    /// # Arguments
    /// * `input` - Encoded audio data in source format
    ///
    /// # Returns
    /// Encoded audio data in destination format
    pub fn transcode(&mut self, input: &[u8]) -> Result<Vec<u8>> {
        // Step 1: Decode to PCM (or use input directly if source is PCM)
        let pcm = if let Some(ref mut codec) = self.src_codec {
            codec.decode(input)?
        } else {
            // Source is PCM, convert bytes to i16 samples
            self.bytes_to_pcm(input)?
        };

        // Step 2: Resample if needed
        let resampled = if let Some(ref mut resampler) = self.resampler {
            resampler.resample(&pcm)?
        } else {
            pcm
        };

        // Step 3: Encode to destination format (or convert to bytes if dest is PCM)
        let output = if let Some(ref mut codec) = self.dst_codec {
            codec.encode(&resampled)?
        } else {
            // Destination is PCM, convert i16 samples to bytes
            self.pcm_to_bytes(&resampled)
        };

        Ok(output)
    }

    /// Transcode a batch of audio frames
    ///
    /// This is more efficient than calling transcode() multiple times
    /// for fixed-frame-size codecs.
    pub fn transcode_batch(&mut self, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        inputs.iter().map(|input| self.transcode(input)).collect()
    }

    /// Reset transcoder state
    ///
    /// This resets all codec and resampler state, useful when
    /// starting a new stream or after packet loss.
    pub fn reset(&mut self) {
        if let Some(ref mut codec) = self.src_codec {
            codec.reset();
        }
        if let Some(ref mut codec) = self.dst_codec {
            codec.reset();
        }
        if let Some(ref mut resampler) = self.resampler {
            resampler.reset();
        }
        self.pcm_buffer.clear();
    }

    /// Get source audio format
    pub fn src_format(&self) -> &AudioFormat {
        &self.src_format
    }

    /// Get destination audio format
    pub fn dst_format(&self) -> &AudioFormat {
        &self.dst_format
    }

    /// Check if transcoding is actually needed
    ///
    /// Returns false if source and destination formats are identical
    pub fn needs_transcoding(&self) -> bool {
        self.src_format.codec != self.dst_format.codec
            || self.src_format.sample_rate != self.dst_format.sample_rate
            || self.src_format.channels != self.dst_format.channels
    }

    /// Get frame size in samples for the source codec
    pub fn src_frame_size(&self) -> Option<usize> {
        self.src_codec.as_ref().and_then(|c| c.frame_size())
    }

    /// Get frame size in samples for the destination codec
    pub fn dst_frame_size(&self) -> Option<usize> {
        self.dst_codec.as_ref().and_then(|c| c.frame_size())
    }

    /// Convert raw PCM bytes (little-endian i16) to samples
    fn bytes_to_pcm(&self, bytes: &[u8]) -> Result<Vec<i16>> {
        if bytes.len() % 2 != 0 {
            return Err(TranscoderError::InvalidFormat(
                "PCM data must have even number of bytes".to_string(),
            ));
        }

        Ok(bytes
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect())
    }

    /// Convert PCM samples to raw bytes (little-endian i16)
    fn pcm_to_bytes(&self, pcm: &[i16]) -> Vec<u8> {
        pcm.iter()
            .flat_map(|&sample| sample.to_le_bytes())
            .collect()
    }
}

/// Builder for creating transcoders with custom configurations
pub struct TranscoderBuilder {
    src_format: AudioFormat,
    dst_format: AudioFormat,
}

impl TranscoderBuilder {
    /// Create a new transcoder builder
    pub fn new(src_format: AudioFormat, dst_format: AudioFormat) -> Self {
        Self {
            src_format,
            dst_format,
        }
    }

    /// Build the transcoder
    pub fn build(self) -> Result<Transcoder> {
        Transcoder::new(self.src_format, self.dst_format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcoder_create() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMA,
        };

        let transcoder = Transcoder::new(src, dst).unwrap();
        assert!(transcoder.needs_transcoding());
        assert_eq!(transcoder.src_format().codec, crate::AudioCodec::PCMU);
        assert_eq!(transcoder.dst_format().codec, crate::AudioCodec::PCMA);
    }

    #[test]
    fn test_transcoder_no_transcoding_needed() {
        let format = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };

        let transcoder = Transcoder::new(format, format).unwrap();
        assert!(!transcoder.needs_transcoding());
    }

    #[test]
    fn test_transcoder_mulaw_to_alaw() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMA,
        };

        let mut transcoder = Transcoder::new(src, dst).unwrap();

        // Encode test signal with G.711 μ-law
        use crate::codecs::g711::G711MuLaw;
        let mut mulaw = G711MuLaw::new(8000);
        let pcm = vec![1000i16; 80]; // 10ms at 8kHz
        let mulaw_data = mulaw.encode(&pcm).unwrap();

        // Transcode to A-law
        let alaw_data = transcoder.transcode(&mulaw_data).unwrap();

        assert_eq!(alaw_data.len(), mulaw_data.len());
        assert_eq!(alaw_data.len(), 80); // Same length for G.711 codecs
    }

    #[test]
    fn test_transcoder_with_resampling() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 16000,
            channels: 1,
            codec: crate::AudioCodec::PCMA,
        };

        let transcoder = Transcoder::new(src, dst).unwrap();
        assert!(transcoder.needs_transcoding());
        assert!(transcoder.resampler.is_some());
    }

    #[test]
    fn test_transcoder_channel_mismatch() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 8000,
            channels: 2,
            codec: crate::AudioCodec::PCMA,
        };

        let result = Transcoder::new(src, dst);
        assert!(result.is_err());
    }

    #[test]
    fn test_transcoder_reset() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 16000,
            channels: 1,
            codec: crate::AudioCodec::PCMA,
        };

        let mut transcoder = Transcoder::new(src, dst).unwrap();

        // Process some data
        use crate::codecs::g711::G711MuLaw;
        let mut mulaw = G711MuLaw::new(8000);
        let pcm = vec![1000i16; 80];
        let mulaw_data = mulaw.encode(&pcm).unwrap();
        let _ = transcoder.transcode(&mulaw_data).unwrap();

        // Reset should clear state
        transcoder.reset();
        assert_eq!(transcoder.pcm_buffer.len(), 0);
    }

    #[test]
    fn test_transcoder_batch() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMA,
        };

        let mut transcoder = Transcoder::new(src, dst).unwrap();

        // Create test frames
        use crate::codecs::g711::G711MuLaw;
        let mut mulaw = G711MuLaw::new(8000);
        let pcm = vec![1000i16; 80];
        let frame = mulaw.encode(&pcm).unwrap();

        let inputs = vec![frame.as_slice(); 3];
        let outputs = transcoder.transcode_batch(&inputs).unwrap();

        assert_eq!(outputs.len(), 3);
        for output in outputs {
            assert_eq!(output.len(), 80);
        }
    }

    #[test]
    fn test_bytes_to_pcm() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCM,
        };
        let dst = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };

        let transcoder = Transcoder::new(src, dst).unwrap();

        // Test byte conversion
        let bytes = vec![0x00, 0x01, 0xFF, 0xFE]; // Two i16 samples
        let pcm = transcoder.bytes_to_pcm(&bytes).unwrap();

        assert_eq!(pcm.len(), 2);
        assert_eq!(pcm[0], 256); // 0x0100 in little-endian
        assert_eq!(pcm[1], -257); // 0xFEFF in little-endian
    }

    #[test]
    fn test_pcm_to_bytes() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCM,
        };

        let transcoder = Transcoder::new(src, dst).unwrap();

        // Test sample conversion
        let pcm = vec![256i16, -257];
        let bytes = transcoder.pcm_to_bytes(&pcm);

        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes, vec![0x00, 0x01, 0xFF, 0xFE]);
    }

    #[test]
    fn test_transcoder_builder() {
        let src = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            codec: crate::AudioCodec::PCMU,
        };
        let dst = AudioFormat {
            sample_rate: 16000,
            channels: 1,
            codec: crate::AudioCodec::PCMA,
        };

        let transcoder = TranscoderBuilder::new(src, dst).build().unwrap();

        assert_eq!(transcoder.src_format().sample_rate, 8000);
        assert_eq!(transcoder.dst_format().sample_rate, 16000);
    }
}
