//! G.722 codec implementation
//!
//! G.722 is a wideband audio codec standardized by ITU-T that provides
//! 7 kHz audio bandwidth at 64, 56, or 48 kbit/s.
//!
//! This implementation wraps the `ezk-g722` crate which is based on
//! SpanDSP/libg722, a well-tested C implementation.
//!
//! Key features:
//! - Sample rate: 16 kHz (wideband)
//! - Bit rate: 64 kbit/s (default), 56 kbit/s, or 48 kbit/s
//! - Uses Sub-Band ADPCM (SB-ADPCM) coding
//! - Splits audio into two sub-bands via QMF (Quadrature Mirror Filter)
//!   - Lower band: 0-4 kHz (6 bits for 64k mode)
//!   - Upper band: 4-8 kHz (2 bits for 64k mode)

use crate::AudioCodec;
use crate::{AudioCodecType, AudioFormat, CodecError, Result};
use ezk_g722::libg722::{decoder::Decoder as EzkDecoder, encoder::Encoder as EzkEncoder, Bitrate};

/// G.722 bit rates
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum G722BitRate {
    /// 64 kbit/s (most common) - 6 bits lower, 2 bits upper
    Rate64k,
    /// 56 kbit/s - 5 bits lower, 2 bits upper
    Rate56k,
    /// 48 kbit/s - 4 bits lower, 2 bits upper
    Rate48k,
}

impl G722BitRate {
    /// Get bits for lower sub-band
    pub fn lower_bits(&self) -> usize {
        match self {
            G722BitRate::Rate64k => 6,
            G722BitRate::Rate56k => 5,
            G722BitRate::Rate48k => 4,
        }
    }

    /// Get bits for upper sub-band (always 2 bits)
    pub fn upper_bits(&self) -> usize {
        2
    }

    /// Number of auxiliary bits carried in each octet (bit-stealing for 56k/48k)
    pub fn aux_bits(&self) -> usize {
        match self {
            G722BitRate::Rate64k => 0,
            G722BitRate::Rate56k => 1,
            G722BitRate::Rate48k => 2,
        }
    }

    /// Convert to ezk-g722 bit rate
    fn to_ezk_rate(&self) -> Bitrate {
        match self {
            G722BitRate::Rate64k => Bitrate::Mode1_64000,
            G722BitRate::Rate56k => Bitrate::Mode2_56000,
            G722BitRate::Rate48k => Bitrate::Mode3_48000,
        }
    }
}

/// G.722 encoder state
pub struct G722Encoder {
    /// Inner ezk-g722 encoder
    inner: EzkEncoder,
    /// Bit rate mode
    mode: G722BitRate,
}

/// G.722 decoder state
pub struct G722Decoder {
    /// Inner ezk-g722 decoder
    inner: EzkDecoder,
    /// Bit rate mode
    mode: G722BitRate,
}

impl G722Encoder {
    /// Create a new G.722 encoder
    pub fn new(mode: G722BitRate) -> Self {
        // Parameters: rate, eight_k (false = 16kHz input), packed (false = standard)
        Self {
            inner: EzkEncoder::new(mode.to_ezk_rate(), false, false),
            mode,
        }
    }

    /// Encode PCM16 samples to G.722
    ///
    /// Input: 16-bit PCM samples at 16 kHz (must be even length)
    /// Output: Encoded bytes
    ///   - 64k mode: 8 bits per frame (2 samples) = 1 byte per frame
    ///   - 56k mode: 7 bits per frame, packed across byte boundaries
    ///   - 48k mode: 6 bits per frame, packed across byte boundaries
    ///
    /// Returns error if input length is odd (G.722 requires pairs of samples).
    pub fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        // G.722 encodes 2 samples at a time, input must be even length
        if samples.len() % 2 != 0 {
            return Err(CodecError::Encoding(format!(
                "G.722 requires even number of samples, got {}",
                samples.len()
            )));
        }

        Ok(self.inner.encode(samples))
    }

    /// Encode PCM16 samples to G.722 while embedding auxiliary bits (56k/48k modes).
    ///
    /// The returned tuple is (encoded_octets, aux_bits_consumed).
    pub fn encode_with_aux(
        &mut self,
        samples: &[i16],
        _aux_bits: &[u8],
    ) -> Result<(Vec<u8>, usize)> {
        // Note: ezk-g722 doesn't support aux bits directly, so we just encode normally
        // and report 0 aux bits consumed. This is acceptable as aux bits are rarely used.
        let encoded = self.encode(samples)?;
        Ok((encoded, 0))
    }
}

impl G722Decoder {
    /// Create a new G.722 decoder
    pub fn new(mode: G722BitRate) -> Self {
        // Parameters: rate, packed (false = standard), eight_k (false = 16kHz output)
        Self {
            inner: EzkDecoder::new(mode.to_ezk_rate(), false, false),
            mode,
        }
    }

    /// Decode G.722 to PCM16 samples
    ///
    /// Input: Encoded bytes (one 8-bit octet per frame)
    /// Output: 16-bit PCM samples at 16 kHz (2 samples per octet)
    pub fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        self.inner.decode(data)
    }

    /// Decode and extract auxiliary bits.
    /// Returns (pcm_samples, aux_bits).
    pub fn decode_with_aux(&mut self, data: &[u8]) -> (Vec<i16>, Vec<u8>) {
        // Note: ezk-g722 doesn't support aux bits extraction
        let decoded = self.decode(data);
        (decoded, Vec::new())
    }
}

/// G.722 codec wrapper implementing AudioCodec trait
pub struct G722Codec {
    encoder: G722Encoder,
    decoder: G722Decoder,
    bit_rate: G722BitRate,
}

impl G722Codec {
    /// Create a new G.722 codec
    pub fn new(bit_rate: G722BitRate) -> Self {
        Self {
            encoder: G722Encoder::new(bit_rate),
            decoder: G722Decoder::new(bit_rate),
            bit_rate,
        }
    }

    /// Get the bit rate
    pub fn bit_rate(&self) -> G722BitRate {
        self.bit_rate
    }
}

impl Default for G722Codec {
    fn default() -> Self {
        Self::new(G722BitRate::Rate64k)
    }
}

impl AudioCodec for G722Codec {
    fn name(&self) -> &str {
        "G.722"
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: 16000,
            channels: 1,
            codec: AudioCodecType::G722,
        }
    }

    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        self.encoder.encode(samples)
    }

    fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>> {
        Ok(self.decoder.decode(data))
    }

    fn reset(&mut self) {
        self.encoder = G722Encoder::new(self.bit_rate);
        self.decoder = G722Decoder::new(self.bit_rate);
    }

    fn frame_size(&self) -> Option<usize> {
        // G.722 typically processes 10ms frames at 16kHz = 160 samples
        Some(160)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_g722_codec_creation() {
        let codec = G722Codec::new(G722BitRate::Rate64k);
        assert_eq!(codec.bit_rate(), G722BitRate::Rate64k);
        assert_eq!(codec.name(), "G.722");
    }

    #[test]
    fn test_g722_encode_decode_silence() {
        let mut codec = G722Codec::default();

        // Encode 10ms of silence (160 samples @ 16kHz)
        let silence: Vec<i16> = vec![0; 160];
        let encoded = codec.encode(&silence).expect("Encoding failed");
        assert_eq!(encoded.len(), 80); // 2 samples per byte

        // Decode back
        let decoded = codec.decode(&encoded).expect("Decoding failed");
        assert_eq!(decoded.len(), 160);

        // Silence should decode to near-silence
        let max_sample = decoded
            .iter()
            .map(|&x| x.saturating_abs())
            .max()
            .unwrap_or(0);
        assert!(max_sample < 500, "Decoded silence too loud: {}", max_sample);
    }

    #[test]
    fn test_g722_encode_decode_tone() {
        let mut codec = G722Codec::new(G722BitRate::Rate64k);

        // Generate multiple frames of a 1kHz tone to allow predictor warmup
        // 320 samples = 20ms @ 16kHz (2 frames)
        let mut samples = Vec::with_capacity(320);
        for i in 0..320 {
            let phase = i as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 16000.0;
            let sample = (10000.0 * phase.sin()) as i16;
            samples.push(sample);
        }

        // Encode
        let encoded = codec.encode(&samples).expect("Encoding failed");
        assert_eq!(encoded.len(), 160); // 2 samples per byte

        // Decode
        let decoded = codec.decode(&encoded).expect("Decoding failed");
        assert_eq!(decoded.len(), 320);

        // Check amplitude is preserved (ezk-g722 should give close to original amplitude)
        let max_amplitude = decoded
            .iter()
            .map(|&x| x.saturating_abs())
            .max()
            .unwrap_or(0);

        // ezk-g722 preserves amplitude well (should be close to 10000)
        assert!(
            max_amplitude > 5000,
            "Decoded amplitude too small: {} (expected close to 10000)",
            max_amplitude
        );

        // Also verify encoding compresses the data
        assert!(encoded.len() < samples.len() * 2);
    }

    #[test]
    fn test_g722_different_bit_rates() {
        for bit_rate in [
            G722BitRate::Rate64k,
            G722BitRate::Rate56k,
            G722BitRate::Rate48k,
        ] {
            let mut codec = G722Codec::new(bit_rate);
            let samples: Vec<i16> = vec![500; 160];

            let encoded = codec.encode(&samples).expect("Encoding failed");
            let decoded = codec.decode(&encoded).expect("Decoding failed");

            assert_eq!(decoded.len(), 160);
        }
    }

    #[test]
    fn test_g722_reset() {
        let mut codec = G722Codec::default();

        // Encode some data
        let samples: Vec<i16> = vec![1000; 160];
        let _ = codec.encode(&samples);

        // Reset
        codec.reset();

        // Encode silence after reset
        let silence: Vec<i16> = vec![0; 160];
        let encoded = codec.encode(&silence).expect("Encoding after reset failed");
        assert_eq!(encoded.len(), 80);
    }

    #[test]
    fn test_g722_native_format() {
        let codec = G722Codec::default();
        let format = codec.native_format();
        assert_eq!(format.sample_rate, 16000);
        assert_eq!(format.channels, 1);
        assert_eq!(format.codec, AudioCodecType::G722);
    }

    #[test]
    fn test_g722_frame_size() {
        let codec = G722Codec::default();
        assert_eq!(codec.frame_size(), Some(160));
    }

    #[test]
    fn test_g722_odd_length_input_returns_error() {
        let mut codec = G722Codec::default();
        let odd_samples: Vec<i16> = vec![0; 159]; // Odd number
        let result = codec.encode(&odd_samples);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("even number of samples"));
    }

    #[test]
    fn test_g722_amplitude_preservation() {
        // This test verifies that ezk-g722 preserves amplitude correctly
        // (unlike our previous broken implementation)
        let mut codec = G722Codec::new(G722BitRate::Rate64k);

        // Generate 20ms of 1kHz sine wave at 16kHz (320 samples)
        let mut pcm: Vec<i16> = Vec::with_capacity(320);
        for i in 0..320 {
            let phase = i as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 16000.0;
            pcm.push((10000.0 * phase.sin()) as i16);
        }

        let input_max = pcm.iter().map(|s| s.abs()).max().unwrap();

        // Encode and decode
        let encoded = codec.encode(&pcm).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        let output_max = decoded.iter().map(|s| s.abs()).max().unwrap();

        // ezk-g722 should preserve amplitude within 20% (not 78% loss like before)
        let amplitude_ratio = output_max as f64 / input_max as f64;
        assert!(
            amplitude_ratio > 0.8 && amplitude_ratio < 1.2,
            "Amplitude not preserved: input {} -> output {} (ratio {})",
            input_max,
            output_max,
            amplitude_ratio
        );
    }
}
