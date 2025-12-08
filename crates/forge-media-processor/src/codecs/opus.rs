//! Opus codec implementation
//!
//! Opus is a modern, versatile audio codec standardized by IETF RFC 6716.
//! It combines SILK (for speech) and CELT (for music) to provide excellent
//! quality across a wide range of bit rates and applications.
//!
//! Key features:
//! - Sample rates: 8, 12, 16, 24, or 48 kHz
//! - Bit rates: 6 kbit/s to 510 kbit/s
//! - Frame sizes: 2.5, 5, 10, 20, 40, or 60 ms
//! - Low latency: 5-66.5 ms
//! - Adaptive: Seamlessly switches between speech and music modes
//! - Royalty-free and open standard
//!
//! This implementation uses the audiopus crate which provides
//! bindings to the reference libopus implementation.

#[cfg(feature = "opus")]
use audiopus::coder::{Decoder, Encoder};
#[cfg(feature = "opus")]
use audiopus::{Channels, SampleRate as OpusSampleRate, Application, Bitrate};

use crate::codecs::AudioCodec;
use crate::{AudioFormat, MediaError, Result};

/// Opus application mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusApplication {
    /// Optimize for VoIP (speech)
    Voip,
    /// Optimize for general audio
    Audio,
    /// Optimize for low-delay applications
    RestrictedLowdelay,
}

#[cfg(feature = "opus")]
impl From<OpusApplication> for Application {
    fn from(app: OpusApplication) -> Self {
        match app {
            OpusApplication::Voip => Application::Voip,
            OpusApplication::Audio => Application::Audio,
            OpusApplication::RestrictedLowdelay => Application::LowDelay,
        }
    }
}

/// Opus encoder/decoder configuration
#[derive(Debug, Clone)]
pub struct OpusConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (1 or 2)
    pub channels: u16,
    /// Application mode
    pub application: OpusApplication,
    /// Target bit rate in bits per second
    pub bitrate: u32,
    /// Frame duration in milliseconds (2.5, 5, 10, 20, 40, or 60)
    pub frame_duration_ms: u32,
}

impl Default for OpusConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            application: OpusApplication::Voip,
            bitrate: 64000,
            frame_duration_ms: 20,
        }
    }
}

impl OpusConfig {
    /// Create VoIP-optimized configuration
    pub fn voip() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            application: OpusApplication::Voip,
            bitrate: 24000, // Good quality for speech
            frame_duration_ms: 20,
        }
    }

    /// Create music-optimized configuration
    pub fn music() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            application: OpusApplication::Audio,
            bitrate: 128000, // Good quality for music
            frame_duration_ms: 20,
        }
    }

    /// Get frame size in samples
    pub fn frame_size(&self) -> usize {
        (self.sample_rate as usize * self.frame_duration_ms as usize) / 1000
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        // Validate sample rate
        if ![8000, 12000, 16000, 24000, 48000].contains(&self.sample_rate) {
            return Err(MediaError::InvalidFormat(
                format!("Opus sample rate must be 8, 12, 16, 24, or 48 kHz, got {}", self.sample_rate)
            ));
        }

        // Validate channels
        if self.channels != 1 && self.channels != 2 {
            return Err(MediaError::InvalidFormat(
                format!("Opus supports 1 or 2 channels, got {}", self.channels)
            ));
        }

        // Validate frame duration
        if ![2, 5, 10, 20, 40, 60].contains(&self.frame_duration_ms) {
            return Err(MediaError::InvalidFormat(
                format!("Opus frame duration must be 2.5, 5, 10, 20, 40, or 60 ms, got {}", self.frame_duration_ms)
            ));
        }

        // Validate bitrate
        if self.bitrate < 6000 || self.bitrate > 510000 {
            return Err(MediaError::InvalidFormat(
                format!("Opus bitrate must be between 6 and 510 kbit/s, got {}", self.bitrate)
            ));
        }

        Ok(())
    }
}

/// Opus codec implementation
#[cfg(feature = "opus")]
pub struct OpusCodec {
    config: OpusConfig,
    encoder: Encoder,
    decoder: Decoder,
}

#[cfg(feature = "opus")]
impl OpusCodec {
    /// Create a new Opus codec with default configuration
    pub fn new() -> Result<Self> {
        Self::with_config(OpusConfig::default())
    }

    /// Create a new Opus codec with custom configuration
    pub fn with_config(config: OpusConfig) -> Result<Self> {
        config.validate()?;

        let sample_rate = match config.sample_rate {
            8000 => OpusSampleRate::Hz8000,
            12000 => OpusSampleRate::Hz12000,
            16000 => OpusSampleRate::Hz16000,
            24000 => OpusSampleRate::Hz24000,
            48000 => OpusSampleRate::Hz48000,
            _ => return Err(MediaError::InvalidFormat("Invalid sample rate".to_string())),
        };

        let channels = if config.channels == 1 {
            Channels::Mono
        } else {
            Channels::Stereo
        };

        let application = config.application.into();

        let mut encoder = Encoder::new(sample_rate, channels, application)
            .map_err(|e| MediaError::Encoding(format!("Failed to create Opus encoder: {:?}", e)))?;

        encoder.set_bitrate(Bitrate::Bits(config.bitrate as i32))
            .map_err(|e| MediaError::Encoding(format!("Failed to set bitrate: {:?}", e)))?;

        let decoder = Decoder::new(sample_rate, channels)
            .map_err(|e| MediaError::Decoding(format!("Failed to create Opus decoder: {:?}", e)))?;

        Ok(Self {
            config,
            encoder,
            decoder,
        })
    }

    /// Encode a frame of PCM samples
    fn encode_frame(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        // Opus can encode variable-length frames, allocate max size
        let mut output = vec![0u8; 4000]; // Max Opus frame size

        let encoded_len = self.encoder.encode(pcm, &mut output)
            .map_err(|e| MediaError::Encoding(format!("Opus encoding failed: {:?}", e)))?;

        output.truncate(encoded_len);
        Ok(output)
    }

    /// Decode an Opus frame to PCM samples
    fn decode_frame(&mut self, encoded: &[u8]) -> Result<Vec<i16>> {
        let frame_size = self.config.frame_size();
        let mut output = vec![0i16; frame_size];

        let decoded_len = self.decoder.decode(Some(encoded), &mut output, false)
            .map_err(|e| MediaError::Decoding(format!("Opus decoding failed: {:?}", e)))?;

        output.truncate(decoded_len);
        Ok(output)
    }

    /// Get the current configuration
    pub fn config(&self) -> &OpusConfig {
        &self.config
    }
}

#[cfg(feature = "opus")]
impl Default for OpusCodec {
    fn default() -> Self {
        Self::new().expect("Failed to create default Opus codec")
    }
}

#[cfg(feature = "opus")]
impl AudioCodec for OpusCodec {
    fn name(&self) -> &str {
        "Opus"
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            codec: crate::AudioCodec::Opus,
        }
    }

    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let frame_size = self.config.frame_size();

        if pcm.len() % frame_size != 0 {
            return Err(MediaError::InvalidFormat(format!(
                "Opus input must be multiple of {} samples",
                frame_size
            )));
        }

        let mut encoded = Vec::new();
        for frame in pcm.chunks(frame_size) {
            encoded.extend_from_slice(&self.encode_frame(frame)?);
        }
        Ok(encoded)
    }

    fn decode(&mut self, encoded: &[u8]) -> Result<Vec<i16>> {
        // Opus frames are variable length, so we decode one complete frame
        // For simplicity, treat the entire input as one frame
        self.decode_frame(encoded)
    }

    fn frame_size(&self) -> Option<usize> {
        Some(self.config.frame_size())
    }

    fn reset(&mut self) {
        // Reset encoder and decoder state
        let _ = self.encoder.reset_state();
        let _ = self.decoder.reset_state();
    }
}

// Stub implementation when opus feature is disabled
#[cfg(not(feature = "opus"))]
pub struct OpusCodec;

#[cfg(not(feature = "opus"))]
impl OpusCodec {
    pub fn new() -> Result<Self> {
        Err(MediaError::InvalidFormat(
            "Opus support not enabled. Compile with --features opus".to_string()
        ))
    }

    pub fn with_config(_config: OpusConfig) -> Result<Self> {
        Err(MediaError::InvalidFormat(
            "Opus support not enabled. Compile with --features opus".to_string()
        ))
    }
}

#[cfg(not(feature = "opus"))]
impl AudioCodec for OpusCodec {
    fn name(&self) -> &str {
        "Opus (disabled)"
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: 48000,
            channels: 1,
            codec: crate::AudioCodec::Opus,
        }
    }

    fn encode(&mut self, _pcm: &[i16]) -> Result<Vec<u8>> {
        Err(MediaError::InvalidFormat(
            "Opus support not enabled. Compile with --features opus".to_string()
        ))
    }

    fn decode(&mut self, _encoded: &[u8]) -> Result<Vec<i16>> {
        Err(MediaError::InvalidFormat(
            "Opus support not enabled. Compile with --features opus".to_string()
        ))
    }

    fn frame_size(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opus_config_default() {
        let config = OpusConfig::default();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 1);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_opus_config_voip() {
        let config = OpusConfig::voip();
        assert_eq!(config.application, OpusApplication::Voip);
        assert_eq!(config.bitrate, 24000);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_opus_config_music() {
        let config = OpusConfig::music();
        assert_eq!(config.application, OpusApplication::Audio);
        assert_eq!(config.channels, 2);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_opus_frame_size() {
        let config = OpusConfig {
            sample_rate: 48000,
            frame_duration_ms: 20,
            ..Default::default()
        };
        assert_eq!(config.frame_size(), 960); // 48000 * 20 / 1000
    }

    #[test]
    fn test_opus_config_invalid_sample_rate() {
        let config = OpusConfig {
            sample_rate: 44100, // Not supported
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_opus_config_invalid_channels() {
        let config = OpusConfig {
            channels: 5, // Only 1 or 2 supported
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    #[cfg(feature = "opus")]
    fn test_opus_create() {
        let codec = OpusCodec::new();
        assert!(codec.is_ok());
        let codec = codec.unwrap();
        assert_eq!(codec.name(), "Opus");
    }

    #[test]
    #[cfg(feature = "opus")]
    fn test_opus_encode_decode() {
        let mut codec = OpusCodec::new().unwrap();
        let frame_size = codec.config().frame_size();

        // Create a simple test signal
        let pcm: Vec<i16> = (0..frame_size).map(|i| (i as i16) * 100).collect();

        // Encode
        let encoded = codec.encode(&pcm).unwrap();
        assert!(!encoded.is_empty());
        assert!(encoded.len() < pcm.len() * 2); // Should be compressed

        // Decode
        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), pcm.len());
    }
}
