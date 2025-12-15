//! Tone generation for DTMF and comfort noise

use crate::error::{InjectionError, Result};
use crate::source::AudioSource;
use forge_core::AudioFrame;
use std::f32::consts::PI;

/// Tone generator for creating audio waveforms
///
/// Supports DTMF tones, single-frequency tones, and comfort noise.
///
/// # Example
///
/// ```rust,ignore
/// use forge_injection::ToneGenerator;
///
/// // Generate DTMF '5' tone at 8kHz
/// let mut tone = ToneGenerator::dtmf('5', 8000);
/// let samples = tone.read_frame(160)?;
/// ```
pub struct ToneGenerator {
    /// Sample rate in Hz
    sample_rate: u32,

    /// Tone type
    tone_type: ToneType,

    /// Current sample position (for phase continuity)
    position: u64,

    /// Duration in samples (None = infinite)
    duration: Option<u64>,

    /// Amplitude (0.0 to 1.0)
    amplitude: f32,
}

/// Type of tone to generate
#[derive(Debug, Clone)]
enum ToneType {
    /// Single frequency tone
    SingleTone { frequency: f32 },

    /// Dual-tone multi-frequency (DTMF)
    DualTone {
        freq1: f32,
        freq2: f32,
    },

    /// Comfort noise (white noise)
    ComfortNoise,

    /// Silence
    Silence,
}

impl ToneGenerator {
    /// Create a DTMF tone generator
    ///
    /// # Arguments
    ///
    /// * `digit` - DTMF digit ('0'-'9', '*', '#', 'A'-'D')
    /// * `sample_rate` - Sample rate in Hz
    ///
    /// # Returns
    ///
    /// A tone generator configured for the specified DTMF digit.
    ///
    /// # Errors
    ///
    /// Returns an error if the digit is invalid.
    pub fn dtmf(digit: char, sample_rate: u32) -> Result<Self> {
        let (low_freq, high_freq) = dtmf_frequencies(digit)?;

        Ok(Self {
            sample_rate,
            tone_type: ToneType::DualTone {
                freq1: low_freq,
                freq2: high_freq,
            },
            position: 0,
            duration: None,
            amplitude: 0.5, // -6dB to avoid clipping when both tones are mixed
        })
    }

    /// Create a single-frequency tone generator
    ///
    /// # Arguments
    ///
    /// * `frequency` - Frequency in Hz
    /// * `sample_rate` - Sample rate in Hz
    pub fn single_tone(frequency: f32, sample_rate: u32) -> Self {
        Self {
            sample_rate,
            tone_type: ToneType::SingleTone { frequency },
            position: 0,
            duration: None,
            amplitude: 0.8, // -2dB
        }
    }

    /// Create a comfort noise generator
    ///
    /// Generates pink noise at -40dBm0.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz
    pub fn comfort_noise(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            tone_type: ToneType::ComfortNoise,
            position: 0,
            duration: None,
            amplitude: 0.01, // -40dBm0
        }
    }

    /// Create a silence generator
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz
    pub fn silence(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            tone_type: ToneType::Silence,
            position: 0,
            duration: None,
            amplitude: 0.0,
        }
    }

    /// Set the duration of the tone
    ///
    /// # Arguments
    ///
    /// * `duration_ms` - Duration in milliseconds
    pub fn with_duration(mut self, duration_ms: u32) -> Self {
        self.duration =
            Some((self.sample_rate as u64 * duration_ms as u64) / 1000);
        self
    }

    /// Set the amplitude of the tone
    ///
    /// # Arguments
    ///
    /// * `amplitude` - Amplitude from 0.0 to 1.0
    pub fn with_amplitude(mut self, amplitude: f32) -> Self {
        self.amplitude = amplitude.clamp(0.0, 1.0);
        self
    }

    /// Generate samples for the tone
    fn generate_samples(&mut self, num_samples: usize) -> AudioFrame {
        let mut samples = Vec::with_capacity(num_samples);

        match &self.tone_type {
            ToneType::SingleTone { frequency } => {
                for i in 0..num_samples {
                    let t = (self.position + i as u64) as f32 / self.sample_rate as f32;
                    let sample =
                        (2.0 * PI * frequency * t).sin() * self.amplitude * i16::MAX as f32;
                    samples.push(sample as i16);
                }
            }
            ToneType::DualTone { freq1, freq2 } => {
                for i in 0..num_samples {
                    let t = (self.position + i as u64) as f32 / self.sample_rate as f32;
                    let tone1 = (2.0 * PI * freq1 * t).sin();
                    let tone2 = (2.0 * PI * freq2 * t).sin();
                    let sample = (tone1 + tone2) * self.amplitude * i16::MAX as f32;
                    samples.push(sample as i16);
                }
            }
            ToneType::ComfortNoise => {
                use std::collections::hash_map::RandomState;
                use std::hash::{BuildHasher, Hasher};

                let hasher = RandomState::new();
                for i in 0..num_samples {
                    // Simple pseudo-random noise
                    let mut h = hasher.build_hasher();
                    h.write_u64(self.position + i as u64);
                    let random = (h.finish() as f32 / u64::MAX as f32) * 2.0 - 1.0;
                    let sample = random * self.amplitude * i16::MAX as f32;
                    samples.push(sample as i16);
                }
            }
            ToneType::Silence => {
                samples.resize(num_samples, 0);
            }
        }

        self.position += num_samples as u64;
        samples
    }
}

impl AudioSource for ToneGenerator {
    fn read_frame(&mut self, num_samples: usize) -> Result<AudioFrame> {
        // Check if we've reached the duration limit
        if let Some(dur) = self.duration {
            if self.position >= dur {
                return Err(InjectionError::EndOfFile);
            }

            // Limit num_samples to not exceed duration
            let remaining = (dur - self.position) as usize;
            let to_generate = num_samples.min(remaining);
            Ok(self.generate_samples(to_generate))
        } else {
            Ok(self.generate_samples(num_samples))
        }
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u8 {
        1 // Mono
    }

    fn is_finished(&self) -> bool {
        if let Some(dur) = self.duration {
            self.position >= dur
        } else {
            false // Infinite tone
        }
    }

    fn reset(&mut self) -> Result<()> {
        self.position = 0;
        Ok(())
    }

    fn duration(&self) -> Option<f64> {
        self.duration
            .map(|samples| samples as f64 / self.sample_rate as f64)
    }

    fn position(&self) -> f64 {
        self.position as f64 / self.sample_rate as f64
    }

    fn description(&self) -> String {
        match &self.tone_type {
            ToneType::SingleTone { frequency } => {
                format!("SingleTone: {:.0}Hz", frequency)
            }
            ToneType::DualTone { freq1, freq2 } => {
                format!("DTMF: {:.0}Hz + {:.0}Hz", freq1, freq2)
            }
            ToneType::ComfortNoise => "ComfortNoise".to_string(),
            ToneType::Silence => "Silence".to_string(),
        }
    }
}

/// Get DTMF frequencies for a digit
///
/// # Arguments
///
/// * `digit` - DTMF digit ('0'-'9', '*', '#', 'A'-'D')
///
/// # Returns
///
/// A tuple of (low frequency, high frequency) in Hz.
fn dtmf_frequencies(digit: char) -> Result<(f32, f32)> {
    // DTMF frequency table (ITU-T Recommendation Q.23)
    //        1209Hz  1336Hz  1477Hz  1633Hz
    //  697Hz    1       2       3       A
    //  770Hz    4       5       6       B
    //  852Hz    7       8       9       C
    //  941Hz    *       0       #       D

    let (low, high) = match digit {
        '1' => (697.0, 1209.0),
        '2' => (697.0, 1336.0),
        '3' => (697.0, 1477.0),
        'A' | 'a' => (697.0, 1633.0),
        '4' => (770.0, 1209.0),
        '5' => (770.0, 1336.0),
        '6' => (770.0, 1477.0),
        'B' | 'b' => (770.0, 1633.0),
        '7' => (852.0, 1209.0),
        '8' => (852.0, 1336.0),
        '9' => (852.0, 1477.0),
        'C' | 'c' => (852.0, 1633.0),
        '*' => (941.0, 1209.0),
        '0' => (941.0, 1336.0),
        '#' => (941.0, 1477.0),
        'D' | 'd' => (941.0, 1633.0),
        _ => {
            return Err(InjectionError::InvalidTone(format!(
                "Invalid DTMF digit: '{}'",
                digit
            )))
        }
    };

    Ok((low, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dtmf_frequencies() {
        let (low, high) = dtmf_frequencies('5').unwrap();
        assert_eq!(low, 770.0);
        assert_eq!(high, 1336.0);
    }

    #[test]
    fn test_dtmf_invalid_digit() {
        assert!(dtmf_frequencies('X').is_err());
    }

    #[test]
    fn test_dtmf_generator() {
        let mut tone = ToneGenerator::dtmf('5', 8000).unwrap();
        let frame = tone.read_frame(160).unwrap();
        assert_eq!(frame.len(), 160);

        // Check that samples are non-zero
        assert!(frame.iter().any(|&s| s != 0));
    }

    #[test]
    fn test_single_tone() {
        let mut tone = ToneGenerator::single_tone(440.0, 8000);
        let frame = tone.read_frame(160).unwrap();
        assert_eq!(frame.len(), 160);
    }

    #[test]
    fn test_comfort_noise() {
        let mut noise = ToneGenerator::comfort_noise(8000);
        let frame = noise.read_frame(160).unwrap();
        assert_eq!(frame.len(), 160);

        // Check that samples vary
        let unique_samples: std::collections::HashSet<_> = frame.iter().collect();
        assert!(unique_samples.len() > 1);
    }

    #[test]
    fn test_tone_duration() {
        let mut tone = ToneGenerator::dtmf('5', 8000).unwrap().with_duration(100);

        // 100ms at 8kHz = 800 samples
        let frame1 = tone.read_frame(400).unwrap();
        assert_eq!(frame1.len(), 400);

        let frame2 = tone.read_frame(400).unwrap();
        assert_eq!(frame2.len(), 400);

        // Should be at end now
        assert!(tone.read_frame(400).is_err());
    }

    #[test]
    fn test_silence() {
        let mut silence = ToneGenerator::silence(8000);
        let frame = silence.read_frame(160).unwrap();
        assert_eq!(frame.len(), 160);
        assert!(frame.iter().all(|&s| s == 0));
    }
}
