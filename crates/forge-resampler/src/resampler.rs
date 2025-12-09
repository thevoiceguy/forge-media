//! Audio resampling
//!
//! This module provides sample rate conversion for audio streams.
//! Resampling is necessary when transcoding between codecs with different
//! sample rates (e.g., G.711 at 8kHz to Opus at 48kHz).
//!
//! Implementation uses linear interpolation for upsampling and
//! averaging for downsampling, which provides reasonable quality
//! for telephony applications.

use crate::{ResamplerError, Result};

/// Audio resampler for sample rate conversion
pub struct Resampler {
    /// Source sample rate
    src_rate: u32,
    /// Destination sample rate
    dst_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    channels: u16,
    /// Fractional position for interpolation
    position: f64,
    /// Previous sample for interpolation (per channel)
    prev_samples: Vec<i16>,
}

impl Resampler {
    /// Create a new resampler
    ///
    /// # Arguments
    /// * `src_rate` - Input sample rate in Hz (e.g., 8000, 16000, 48000)
    /// * `dst_rate` - Output sample rate in Hz
    /// * `channels` - Number of channels (1 or 2)
    pub fn new(src_rate: u32, dst_rate: u32, channels: u16) -> Result<Self> {
        if src_rate == 0 || dst_rate == 0 {
            return Err(ResamplerError::InvalidFormat(
                "Sample rates must be greater than 0".to_string(),
            ));
        }

        if channels != 1 && channels != 2 {
            return Err(ResamplerError::InvalidFormat(
                "Only mono (1) and stereo (2) channels are supported".to_string(),
            ));
        }

        Ok(Self {
            src_rate,
            dst_rate,
            channels,
            position: 0.0,
            prev_samples: vec![0; channels as usize],
        })
    }

    /// Resample audio data
    ///
    /// # Arguments
    /// * `input` - Input PCM samples (16-bit signed, interleaved if stereo)
    ///
    /// # Returns
    /// Resampled PCM samples at the destination sample rate
    pub fn resample(&mut self, input: &[i16]) -> Result<Vec<i16>> {
        // Validate input length
        if input.len() % self.channels as usize != 0 {
            return Err(ResamplerError::InvalidFormat(
                "Input length must be multiple of channel count".to_string(),
            ));
        }

        // No resampling needed
        if self.src_rate == self.dst_rate {
            return Ok(input.to_vec());
        }

        // Calculate ratio and output length
        let ratio = self.src_rate as f64 / self.dst_rate as f64;
        let input_frames = input.len() / self.channels as usize;
        let output_frames = ((input_frames as f64) / ratio).ceil() as usize;
        let output_len = output_frames * self.channels as usize;

        let mut output = Vec::with_capacity(output_len);

        // Process each output frame
        for _ in 0..output_frames {
            // Get the current integer and fractional position
            let idx = self.position as usize;
            let frac = self.position - idx as f64;

            // For each channel, interpolate between current and next sample
            for ch in 0..self.channels as usize {
                let sample = if idx >= input_frames {
                    // Past the end, use the last sample
                    input[input.len() - self.channels as usize + ch]
                } else {
                    let current_idx = idx * self.channels as usize + ch;
                    let current = input[current_idx];

                    // Get next sample for interpolation
                    let next = if idx + 1 < input_frames {
                        input[current_idx + self.channels as usize]
                    } else {
                        current
                    };

                    // Linear interpolation
                    let interpolated = current as f64 + (next as f64 - current as f64) * frac;
                    interpolated.round() as i16
                };

                output.push(sample);
            }

            // Advance position
            self.position += ratio;
        }

        // Adjust position for next call (carry over fractional part)
        self.position -= input_frames as f64;
        if self.position < 0.0 {
            self.position = 0.0;
        }

        // Store last samples for next iteration
        if input.len() >= self.channels as usize {
            let start = input.len() - self.channels as usize;
            self.prev_samples.copy_from_slice(&input[start..]);
        }

        Ok(output)
    }

    /// Reset resampler state
    pub fn reset(&mut self) {
        self.position = 0.0;
        self.prev_samples.fill(0);
    }

    /// Get source sample rate
    pub fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// Get destination sample rate
    pub fn dst_rate(&self) -> u32 {
        self.dst_rate
    }

    /// Get number of channels
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Check if resampling is needed (rates are different)
    pub fn needs_resampling(&self) -> bool {
        self.src_rate != self.dst_rate
    }
}

/// Calculate the expected output length for a given input length
pub fn calculate_output_length(input_len: usize, src_rate: u32, dst_rate: u32, channels: u16) -> usize {
    let input_frames = input_len / channels as usize;
    let output_frames = ((input_frames as f64 * dst_rate as f64) / src_rate as f64).ceil() as usize;
    output_frames * channels as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resampler_create() {
        let resampler = Resampler::new(8000, 16000, 1).unwrap();
        assert_eq!(resampler.src_rate(), 8000);
        assert_eq!(resampler.dst_rate(), 16000);
        assert_eq!(resampler.channels(), 1);
        assert!(resampler.needs_resampling());
    }

    #[test]
    fn test_resampler_no_resampling() {
        let mut resampler = Resampler::new(48000, 48000, 1).unwrap();
        assert!(!resampler.needs_resampling());

        let input = vec![100i16, 200, 300, 400];
        let output = resampler.resample(&input).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn test_resampler_upsample_2x() {
        let mut resampler = Resampler::new(8000, 16000, 1).unwrap();

        // Input: 4 samples at 8kHz
        let input = vec![0i16, 100, 200, 100];

        // Output should be ~8 samples at 16kHz (2x upsampling)
        let output = resampler.resample(&input).unwrap();

        // Check output length is approximately 2x input
        assert!(output.len() >= 7 && output.len() <= 9);

        // First sample should be close to first input
        assert!((output[0] - input[0]).abs() < 10);
    }

    #[test]
    fn test_resampler_downsample_2x() {
        let mut resampler = Resampler::new(16000, 8000, 1).unwrap();

        // Input: 8 samples at 16kHz
        let input = vec![0i16, 50, 100, 150, 200, 150, 100, 50];

        // Output should be ~4 samples at 8kHz (2x downsampling)
        let output = resampler.resample(&input).unwrap();

        // Check output length is approximately half input
        assert!(output.len() >= 3 && output.len() <= 5);
    }

    #[test]
    fn test_resampler_8k_to_48k() {
        let mut resampler = Resampler::new(8000, 48000, 1).unwrap();

        // Input: 80 samples (10ms at 8kHz)
        let input = vec![1000i16; 80];

        // Output should be 480 samples (10ms at 48kHz)
        let output = resampler.resample(&input).unwrap();

        // Check output length (should be 6x input, approximately)
        assert!(output.len() >= 470 && output.len() <= 490);

        // All samples should be close to 1000
        for &sample in &output {
            assert!((sample - 1000).abs() < 50);
        }
    }

    #[test]
    fn test_resampler_48k_to_8k() {
        let mut resampler = Resampler::new(48000, 8000, 1).unwrap();

        // Input: 480 samples (10ms at 48kHz)
        let input = vec![1000i16; 480];

        // Output should be 80 samples (10ms at 8kHz)
        let output = resampler.resample(&input).unwrap();

        // Check output length (should be input/6, approximately)
        assert!(output.len() >= 75 && output.len() <= 85);

        // All samples should be close to 1000
        for &sample in &output {
            assert!((sample - 1000).abs() < 50);
        }
    }

    #[test]
    fn test_resampler_stereo() {
        let mut resampler = Resampler::new(8000, 16000, 2).unwrap();

        // Input: 4 frames (8 samples, 2 channels interleaved)
        let input = vec![100i16, 200, 150, 250, 200, 300, 250, 350];

        let output = resampler.resample(&input).unwrap();

        // Output should have even length (stereo)
        assert_eq!(output.len() % 2, 0);

        // Should be approximately 2x length
        assert!(output.len() >= 14 && output.len() <= 18);
    }

    #[test]
    fn test_resampler_reset() {
        let mut resampler = Resampler::new(8000, 16000, 1).unwrap();

        let input = vec![100i16, 200, 300];
        let _ = resampler.resample(&input).unwrap();

        // Reset should clear state
        resampler.reset();
        assert_eq!(resampler.position, 0.0);
        assert_eq!(resampler.prev_samples, vec![0]);
    }

    #[test]
    fn test_calculate_output_length() {
        // 80 samples at 8kHz -> 480 samples at 48kHz (6x)
        let output_len = calculate_output_length(80, 8000, 48000, 1);
        assert!(output_len >= 470 && output_len <= 490);

        // 480 samples at 48kHz -> 80 samples at 8kHz (1/6x)
        let output_len = calculate_output_length(480, 48000, 8000, 1);
        assert!(output_len >= 75 && output_len <= 85);
    }

    #[test]
    fn test_resampler_invalid_sample_rate() {
        assert!(Resampler::new(0, 48000, 1).is_err());
        assert!(Resampler::new(8000, 0, 1).is_err());
    }

    #[test]
    fn test_resampler_invalid_channels() {
        assert!(Resampler::new(8000, 48000, 0).is_err());
        assert!(Resampler::new(8000, 48000, 3).is_err());
    }

    #[test]
    fn test_resampler_invalid_input() {
        let mut resampler = Resampler::new(8000, 48000, 2).unwrap();

        // Odd number of samples for stereo should fail
        let input = vec![100i16, 200, 300];
        assert!(resampler.resample(&input).is_err());
    }
}
