//! Audio format conversion
//!
//! Provides conversion between different audio formats for AI streaming.
//! Supports PCM16, G.711 μ-law, G.711 A-law, and resampling.

use crate::Result;

/// Audio format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// PCM 16-bit signed, mono
    Pcm16Mono(u32), // sample rate

    /// PCM 16-bit signed, stereo
    Pcm16Stereo(u32),

    /// G.711 mu-law (8kHz)
    G711Mulaw,

    /// G.711 a-law (8kHz)
    G711Alaw,
}

impl AudioFormat {
    /// Get sample rate
    pub fn sample_rate(&self) -> u32 {
        match self {
            AudioFormat::Pcm16Mono(rate) => *rate,
            AudioFormat::Pcm16Stereo(rate) => *rate,
            AudioFormat::G711Mulaw => 8000,
            AudioFormat::G711Alaw => 8000,
        }
    }

    /// Check if stereo
    pub fn is_stereo(&self) -> bool {
        matches!(self, AudioFormat::Pcm16Stereo(_))
    }

    /// Get channel count
    pub fn channels(&self) -> u8 {
        if self.is_stereo() {
            2
        } else {
            1
        }
    }
}

/// Audio sample (i16 PCM)
pub type AudioSample = i16;

/// Audio format converter
pub struct AudioConverter {
    source_format: AudioFormat,
    target_format: AudioFormat,
}

impl AudioConverter {
    /// Create a new audio converter
    pub fn new(source_format: AudioFormat, target_format: AudioFormat) -> Self {
        Self {
            source_format,
            target_format,
        }
    }

    /// Convert audio samples
    pub fn convert(&self, input: &[AudioSample]) -> Result<Vec<AudioSample>> {
        match (&self.source_format, &self.target_format) {
            // Same format, no conversion needed
            (a, b) if a == b => Ok(input.to_vec()),

            // PCM to PCM with different sample rates
            (AudioFormat::Pcm16Mono(src_rate), AudioFormat::Pcm16Mono(dst_rate))
                if src_rate != dst_rate =>
            {
                self.resample(input, *src_rate, *dst_rate)
            }

            // Stereo to mono
            (AudioFormat::Pcm16Stereo(_), AudioFormat::Pcm16Mono(_)) => {
                Ok(self.stereo_to_mono(input))
            }

            // Mono to stereo
            (AudioFormat::Pcm16Mono(_), AudioFormat::Pcm16Stereo(_)) => {
                Ok(self.mono_to_stereo(input))
            }

            // G.711 μ-law to PCM
            (AudioFormat::G711Mulaw, AudioFormat::Pcm16Mono(_)) => Ok(self.mulaw_to_pcm16(input)),

            // PCM to G.711 μ-law
            (AudioFormat::Pcm16Mono(_), AudioFormat::G711Mulaw) => Ok(self.pcm16_to_mulaw(input)),

            // G.711 A-law to PCM
            (AudioFormat::G711Alaw, AudioFormat::Pcm16Mono(_)) => Ok(self.alaw_to_pcm16(input)),

            // PCM to G.711 A-law
            (AudioFormat::Pcm16Mono(_), AudioFormat::G711Alaw) => Ok(self.pcm16_to_alaw(input)),

            // Complex conversions (need multiple steps)
            _ => {
                // Convert via intermediate PCM16 mono
                let intermediate = if self.source_format.is_stereo() {
                    self.stereo_to_mono(input)
                } else {
                    input.to_vec()
                };

                // Then convert to target format
                let converter = AudioConverter::new(
                    AudioFormat::Pcm16Mono(self.source_format.sample_rate()),
                    self.target_format,
                );
                converter.convert(&intermediate)
            }
        }
    }

    /// Resample audio using linear interpolation
    fn resample(
        &self,
        input: &[AudioSample],
        src_rate: u32,
        dst_rate: u32,
    ) -> Result<Vec<AudioSample>> {
        if src_rate == dst_rate {
            return Ok(input.to_vec());
        }

        let ratio = src_rate as f64 / dst_rate as f64;
        let output_len = (input.len() as f64 / ratio).ceil() as usize;
        let mut output = Vec::with_capacity(output_len);

        for i in 0..output_len {
            let src_pos = i as f64 * ratio;
            let src_idx = src_pos as usize;
            let frac = src_pos - src_idx as f64;

            if src_idx + 1 < input.len() {
                // Linear interpolation
                let sample =
                    input[src_idx] as f64 * (1.0 - frac) + input[src_idx + 1] as f64 * frac;
                output.push(sample as i16);
            } else if src_idx < input.len() {
                output.push(input[src_idx]);
            }
        }

        Ok(output)
    }

    /// Convert stereo to mono by averaging channels
    fn stereo_to_mono(&self, input: &[AudioSample]) -> Vec<AudioSample> {
        input
            .chunks_exact(2)
            .map(|chunk| ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16)
            .collect()
    }

    /// Convert mono to stereo by duplicating channel
    fn mono_to_stereo(&self, input: &[AudioSample]) -> Vec<AudioSample> {
        input
            .iter()
            .flat_map(|&sample| vec![sample, sample])
            .collect()
    }

    /// Convert G.711 μ-law to PCM16
    fn mulaw_to_pcm16(&self, input: &[AudioSample]) -> Vec<AudioSample> {
        input
            .iter()
            .map(|&sample| mulaw_decode(sample as u8))
            .collect()
    }

    /// Convert PCM16 to G.711 μ-law
    fn pcm16_to_mulaw(&self, input: &[AudioSample]) -> Vec<AudioSample> {
        input
            .iter()
            .map(|&sample| mulaw_encode(sample) as i16)
            .collect()
    }

    /// Convert G.711 A-law to PCM16
    fn alaw_to_pcm16(&self, input: &[AudioSample]) -> Vec<AudioSample> {
        input
            .iter()
            .map(|&sample| alaw_decode(sample as u8))
            .collect()
    }

    /// Convert PCM16 to G.711 A-law
    fn pcm16_to_alaw(&self, input: &[AudioSample]) -> Vec<AudioSample> {
        input
            .iter()
            .map(|&sample| alaw_encode(sample) as i16)
            .collect()
    }
}

/// G.711 μ-law encoding
fn mulaw_encode(sample: i16) -> u8 {
    const BIAS: i16 = 0x84;
    const CLIP: i16 = 32635;

    let sign = if sample < 0 { 0x80 } else { 0x00 };
    let mag = sample.abs();

    // Find segment based on unbiased magnitude
    let seg = if mag >= 2048 {
        7
    } else if mag >= 1024 {
        6
    } else if mag >= 512 {
        5
    } else if mag >= 256 {
        4
    } else if mag >= 128 {
        3
    } else if mag >= 64 {
        2
    } else if mag >= 32 {
        1
    } else {
        0
    };

    // Add bias and clip
    let mut biased_mag = mag.saturating_add(BIAS);
    if biased_mag > CLIP {
        biased_mag = CLIP;
    }

    // Extract mantissa from biased magnitude
    let uval = sign | (seg << 4) | ((biased_mag >> (seg + 3)) & 0x0F);
    (!uval) as u8
}

/// G.711 μ-law decoding
fn mulaw_decode(mulaw: u8) -> i16 {
    const BIAS: i16 = 0x84;

    let mulaw = !mulaw;
    let sign = if (mulaw & 0x80) != 0 { -1 } else { 1 };
    let segment = ((mulaw & 0x70) >> 4) as i16;
    let mantissa = (mulaw & 0x0F) as i16;

    // Reconstruct the biased magnitude at quantization interval midpoint
    // For segment s, mantissa is at bits [s+6:s+3], reconstruct with midpoint of lost bits
    let biased_mag = (mantissa << (segment + 3)) + (1 << (segment + 2));
    let magnitude = biased_mag - BIAS;

    sign * magnitude
}

/// G.711 A-law encoding
fn alaw_encode(sample: i16) -> u8 {
    const SEG_SHIFT: i16 = 4;
    const QUANT_MASK: i16 = 0x0F;
    const CLIP: i16 = 0x1FFF; // Clip at 8191, segment 7 max

    let sign = if sample < 0 { 0x80 } else { 0x00 };
    let mut mag = sample.abs();

    // Clip to maximum value
    if mag > CLIP {
        mag = CLIP;
    }

    // Find segment based on magnitude
    let seg = if mag >= 2048 {
        7
    } else if mag >= 1024 {
        6
    } else if mag >= 512 {
        5
    } else if mag >= 256 {
        4
    } else if mag >= 128 {
        3
    } else if mag >= 64 {
        2
    } else if mag >= 32 {
        1
    } else {
        0
    };

    let aval = sign | (seg << SEG_SHIFT) | ((mag >> (seg + 3)) & QUANT_MASK);
    (aval ^ 0x55) as u8
}

/// G.711 A-law decoding
fn alaw_decode(alaw: u8) -> i16 {
    let alaw = alaw ^ 0x55;
    let sign = if (alaw & 0x80) != 0 { -1 } else { 1 };
    let segment = ((alaw & 0x70) >> 4) as i16;
    let mantissa = (alaw & 0x0F) as i16;

    // Reconstruct magnitude at quantization interval midpoint
    let magnitude = (mantissa << (segment + 3)) + (1 << (segment + 2));

    sign * magnitude
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format() {
        let format = AudioFormat::Pcm16Mono(24000);
        assert_eq!(format.sample_rate(), 24000);
        assert_eq!(format.channels(), 1);
        assert!(!format.is_stereo());

        let format = AudioFormat::Pcm16Stereo(16000);
        assert_eq!(format.sample_rate(), 16000);
        assert_eq!(format.channels(), 2);
        assert!(format.is_stereo());
    }

    #[test]
    fn test_stereo_to_mono() {
        let converter =
            AudioConverter::new(AudioFormat::Pcm16Stereo(8000), AudioFormat::Pcm16Mono(8000));

        let stereo = vec![100, 200, 300, 400]; // 2 stereo samples
        let mono = converter.convert(&stereo).unwrap();

        assert_eq!(mono.len(), 2);
        assert_eq!(mono[0], 150); // (100 + 200) / 2
        assert_eq!(mono[1], 350); // (300 + 400) / 2
    }

    #[test]
    fn test_mono_to_stereo() {
        let converter =
            AudioConverter::new(AudioFormat::Pcm16Mono(8000), AudioFormat::Pcm16Stereo(8000));

        let mono = vec![100, 200];
        let stereo = converter.convert(&mono).unwrap();

        assert_eq!(stereo.len(), 4);
        assert_eq!(stereo, vec![100, 100, 200, 200]);
    }

    #[test]
    fn test_mulaw_roundtrip() {
        let original = vec![1000i16, -2000, 3000, -4000, 0];

        let converter_encode =
            AudioConverter::new(AudioFormat::Pcm16Mono(8000), AudioFormat::G711Mulaw);
        let encoded = converter_encode.convert(&original).unwrap();

        let converter_decode =
            AudioConverter::new(AudioFormat::G711Mulaw, AudioFormat::Pcm16Mono(8000));
        let decoded = converter_decode.convert(&encoded).unwrap();

        // G.711 is lossy with quantization error up to ±512 for high segments
        for (orig, dec) in original.iter().zip(decoded.iter()) {
            let diff = (orig - dec).abs();
            assert!(diff < 600, "Difference too large: {} vs {}", orig, dec);
        }
    }

    #[test]
    fn test_alaw_roundtrip() {
        let original = vec![1000i16, -2000, 3000, -4000, 0];

        let converter_encode =
            AudioConverter::new(AudioFormat::Pcm16Mono(8000), AudioFormat::G711Alaw);
        let encoded = converter_encode.convert(&original).unwrap();

        let converter_decode =
            AudioConverter::new(AudioFormat::G711Alaw, AudioFormat::Pcm16Mono(8000));
        let decoded = converter_decode.convert(&encoded).unwrap();

        // G.711 is lossy with quantization error up to ±512 for high segments
        for (orig, dec) in original.iter().zip(decoded.iter()) {
            let diff = (orig - dec).abs();
            assert!(diff < 600, "Difference too large: {} vs {}", orig, dec);
        }
    }

    #[test]
    fn test_resampling() {
        let converter =
            AudioConverter::new(AudioFormat::Pcm16Mono(8000), AudioFormat::Pcm16Mono(16000));

        let input = vec![0, 1000, 2000, 3000, 4000];
        let output = converter.convert(&input).unwrap();

        // Upsampling 8kHz to 16kHz should double the length (approximately)
        assert!(output.len() >= 9 && output.len() <= 11);
    }

    #[test]
    fn test_no_conversion_needed() {
        let converter =
            AudioConverter::new(AudioFormat::Pcm16Mono(24000), AudioFormat::Pcm16Mono(24000));

        let input = vec![100, 200, 300];
        let output = converter.convert(&input).unwrap();

        assert_eq!(input, output);
    }
}
