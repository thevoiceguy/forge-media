//! Inband DTMF detection using Goertzel algorithm
//!
//! Detects DTMF tones directly from audio samples by analyzing
//! the frequency content using the Goertzel algorithm.
//!
//! DTMF frequencies:
//! ```text
//!        1209 Hz  1336 Hz  1477 Hz  1633 Hz
//! 697 Hz    1        2        3        A
//! 770 Hz    4        5        6        B
//! 852 Hz    7        8        9        C
//! 941 Hz    *        0        #        D
//! ```

use crate::detector::{DtmfDetector, DtmfDigit, DtmfEvent, DtmfEventType, DtmfMethod};
use crate::{DtmfError, Result};
use std::f32::consts::PI;

/// DTMF low frequencies (Hz)
const LOW_FREQS: [u32; 4] = [697, 770, 852, 941];

/// DTMF high frequencies (Hz)
const HIGH_FREQS: [u32; 4] = [1209, 1336, 1477, 1633];

/// Goertzel filter for single frequency detection
struct GoertzelFilter {
    /// Target frequency
    frequency: f32,
    /// Sample rate
    sample_rate: u32,
    /// Coefficient
    coeff: f32,
    /// Previous sample 1
    s_prev1: f32,
    /// Previous sample 2
    s_prev2: f32,
    /// Number of samples processed
    n: usize,
}

impl GoertzelFilter {
    /// Create a new Goertzel filter
    fn new(frequency: u32, sample_rate: u32) -> Self {
        let normalized_freq = 2.0 * PI * (frequency as f32) / (sample_rate as f32);
        let coeff = 2.0 * normalized_freq.cos();

        Self {
            frequency: frequency as f32,
            sample_rate,
            coeff,
            s_prev1: 0.0,
            s_prev2: 0.0,
            n: 0,
        }
    }

    /// Process a single sample
    fn process_sample(&mut self, sample: f32) {
        let s = sample + self.coeff * self.s_prev1 - self.s_prev2;
        self.s_prev2 = self.s_prev1;
        self.s_prev1 = s;
        self.n += 1;
    }

    /// Get magnitude squared of the filtered signal
    fn magnitude_squared(&self) -> f32 {
        let real = self.s_prev1 - self.s_prev2 * self.coeff * 0.5;
        let imag = self.s_prev2 * (2.0 * PI * self.frequency / self.sample_rate as f32).sin();
        real * real + imag * imag
    }

    /// Reset filter state
    fn reset(&mut self) {
        self.s_prev1 = 0.0;
        self.s_prev2 = 0.0;
        self.n = 0;
    }
}

/// Goertzel-based DTMF detector
pub struct GoertzelDetector {
    /// Sample rate
    sample_rate: u32,
    /// Low frequency filters
    low_filters: Vec<GoertzelFilter>,
    /// High frequency filters
    high_filters: Vec<GoertzelFilter>,
    /// Minimum samples for detection (typically 100-200ms)
    min_samples: usize,
    /// Current sample count
    sample_count: usize,
    /// Energy threshold (relative)
    energy_threshold: f32,
    /// Twist ratio threshold (low/high power ratio, typically 4dB = 1.58)
    twist_threshold: f32,
    /// Current detected digit
    current_digit: Option<DtmfDigit>,
    /// Frames since digit detected
    frames_detected: u32,
    /// Minimum frames to confirm detection
    min_detection_frames: u32,
}

impl GoertzelDetector {
    /// Default energy threshold
    pub const DEFAULT_ENERGY_THRESHOLD: f32 = 1000000.0;

    /// Default twist threshold (4dB)
    pub const DEFAULT_TWIST_THRESHOLD: f32 = 1.58;

    /// Default minimum detection duration (100ms)
    pub const DEFAULT_MIN_DURATION_MS: u32 = 100;

    /// Create a new Goertzel detector
    ///
    /// # Arguments
    /// * `sample_rate` - Audio sample rate (e.g., 8000)
    /// * `frame_size` - Number of samples per detection frame (e.g., 160 for 20ms at 8kHz)
    pub fn new(sample_rate: u32, frame_size: usize) -> Self {
        let low_filters = LOW_FREQS
            .iter()
            .map(|&freq| GoertzelFilter::new(freq, sample_rate))
            .collect();

        let high_filters = HIGH_FREQS
            .iter()
            .map(|&freq| GoertzelFilter::new(freq, sample_rate))
            .collect();

        // Calculate minimum frames for 100ms detection
        let frame_duration_ms = (frame_size as u32 * 1000) / sample_rate;
        let min_detection_frames = Self::DEFAULT_MIN_DURATION_MS / frame_duration_ms;

        Self {
            sample_rate,
            low_filters,
            high_filters,
            min_samples: frame_size,
            sample_count: 0,
            energy_threshold: Self::DEFAULT_ENERGY_THRESHOLD,
            twist_threshold: Self::DEFAULT_TWIST_THRESHOLD,
            current_digit: None,
            frames_detected: 0,
            min_detection_frames: min_detection_frames.max(1),
        }
    }

    /// Set energy threshold
    pub fn set_energy_threshold(&mut self, threshold: f32) {
        self.energy_threshold = threshold;
    }

    /// Set twist threshold
    pub fn set_twist_threshold(&mut self, threshold: f32) {
        self.twist_threshold = threshold;
    }

    /// Process PCM samples (i16)
    pub fn process_samples(&mut self, samples: &[i16]) -> Result<Vec<DtmfEvent>> {
        let mut events = Vec::new();

        // Convert i16 to f32 and normalize
        for &sample in samples {
            let normalized = (sample as f32) / 32768.0;

            // Process through all filters
            for filter in &mut self.low_filters {
                filter.process_sample(normalized);
            }
            for filter in &mut self.high_filters {
                filter.process_sample(normalized);
            }

            self.sample_count += 1;

            // Check for detection every min_samples
            if self.sample_count >= self.min_samples {
                if let Some(event) = self.detect()? {
                    events.push(event);
                }

                // Reset filters for next frame
                for filter in &mut self.low_filters {
                    filter.reset();
                }
                for filter in &mut self.high_filters {
                    filter.reset();
                }
                self.sample_count = 0;
            }
        }

        Ok(events)
    }

    /// Attempt to detect DTMF digit from current filter states
    fn detect(&mut self) -> Result<Option<DtmfEvent>> {
        // Find strongest low and high frequencies
        let (low_idx, low_mag) = self
            .low_filters
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.magnitude_squared()))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();

        let (high_idx, high_mag) = self
            .high_filters
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.magnitude_squared()))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();

        // Check energy threshold
        if low_mag < self.energy_threshold || high_mag < self.energy_threshold {
            return self.handle_no_detection();
        }

        // Check twist ratio (low/high power ratio should be reasonable)
        let twist_ratio = (low_mag / high_mag).sqrt();
        if twist_ratio > self.twist_threshold || twist_ratio < 1.0 / self.twist_threshold {
            return self.handle_no_detection();
        }

        // Map to DTMF digit
        let digit = Self::map_frequencies(low_idx, high_idx)?;

        // Check if this is a new digit or continuation
        if let Some(current) = self.current_digit {
            if current == digit {
                // Same digit - increment frame count
                self.frames_detected += 1;

                // Emit Continue event every few frames
                if self.frames_detected % 5 == 0 {
                    Ok(Some(DtmfEvent::new(
                        digit,
                        DtmfEventType::Continue,
                        DtmfMethod::Inband,
                    )))
                } else {
                    Ok(None)
                }
            } else {
                // Different digit - end previous, start new
                let end_event = DtmfEvent::with_duration(
                    current,
                    DtmfEventType::End,
                    DtmfMethod::Inband,
                    self.frames_detected * 20, // Approximate ms
                );

                self.current_digit = Some(digit);
                self.frames_detected = 1;

                // Emit both end and start events
                Ok(Some(end_event))
            }
        } else {
            // New digit detected
            self.current_digit = Some(digit);
            self.frames_detected = 1;

            // Only emit start event if we've detected enough frames
            if self.frames_detected >= self.min_detection_frames {
                Ok(Some(DtmfEvent::new(
                    digit,
                    DtmfEventType::Start,
                    DtmfMethod::Inband,
                )))
            } else {
                Ok(None)
            }
        }
    }

    /// Handle no detection (tone ended)
    fn handle_no_detection(&mut self) -> Result<Option<DtmfEvent>> {
        if let Some(digit) = self.current_digit.take() {
            // Emit end event only if we had enough valid detections
            if self.frames_detected >= self.min_detection_frames {
                let event = DtmfEvent::with_duration(
                    digit,
                    DtmfEventType::End,
                    DtmfMethod::Inband,
                    self.frames_detected * 20, // Approximate ms
                );
                self.frames_detected = 0;
                Ok(Some(event))
            } else {
                // Too short, ignore
                self.frames_detected = 0;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Map low and high frequency indices to DTMF digit
    fn map_frequencies(low_idx: usize, high_idx: usize) -> Result<DtmfDigit> {
        match (low_idx, high_idx) {
            (0, 0) => Ok(DtmfDigit::One),
            (0, 1) => Ok(DtmfDigit::Two),
            (0, 2) => Ok(DtmfDigit::Three),
            (0, 3) => Ok(DtmfDigit::A),
            (1, 0) => Ok(DtmfDigit::Four),
            (1, 1) => Ok(DtmfDigit::Five),
            (1, 2) => Ok(DtmfDigit::Six),
            (1, 3) => Ok(DtmfDigit::B),
            (2, 0) => Ok(DtmfDigit::Seven),
            (2, 1) => Ok(DtmfDigit::Eight),
            (2, 2) => Ok(DtmfDigit::Nine),
            (2, 3) => Ok(DtmfDigit::C),
            (3, 0) => Ok(DtmfDigit::Star),
            (3, 1) => Ok(DtmfDigit::Zero),
            (3, 2) => Ok(DtmfDigit::Hash),
            (3, 3) => Ok(DtmfDigit::D),
            _ => Err(DtmfError::DetectionError(format!(
                "Invalid frequency combination: ({}, {})",
                low_idx, high_idx
            ))),
        }
    }
}

impl DtmfDetector for GoertzelDetector {
    fn process(&mut self, data: &[u8]) -> Result<Vec<DtmfEvent>> {
        // Convert bytes to i16 samples (assuming little-endian PCM)
        if data.len() % 2 != 0 {
            return Err(DtmfError::InvalidAudioFormat(
                "Audio data must have even number of bytes for i16 samples".to_string(),
            ));
        }

        let samples: Vec<i16> = data
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        self.process_samples(&samples)
    }

    fn reset(&mut self) {
        for filter in &mut self.low_filters {
            filter.reset();
        }
        for filter in &mut self.high_filters {
            filter.reset();
        }
        self.sample_count = 0;
        self.current_digit = None;
        self.frames_detected = 0;
    }

    fn method(&self) -> DtmfMethod {
        DtmfMethod::Inband
    }
}

/// Convenience type alias
pub type InbandDetector = GoertzelDetector;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_goertzel_filter() {
        let mut filter = GoertzelFilter::new(697, 8000);

        // Generate 697 Hz tone
        for i in 0..160 {
            let t = i as f32 / 8000.0;
            let sample = (2.0 * PI * 697.0 * t).sin();
            filter.process_sample(sample);
        }

        let mag = filter.magnitude_squared();
        assert!(mag > 1.0); // Should detect significant energy
    }

    #[test]
    fn test_frequency_mapping() {
        assert_eq!(GoertzelDetector::map_frequencies(0, 0).unwrap(), DtmfDigit::One);
        assert_eq!(GoertzelDetector::map_frequencies(1, 1).unwrap(), DtmfDigit::Five);
        assert_eq!(GoertzelDetector::map_frequencies(3, 1).unwrap(), DtmfDigit::Zero);
        assert_eq!(GoertzelDetector::map_frequencies(3, 0).unwrap(), DtmfDigit::Star);
        assert_eq!(GoertzelDetector::map_frequencies(3, 2).unwrap(), DtmfDigit::Hash);
    }

    #[test]
    fn test_goertzel_detector_creation() {
        let detector = GoertzelDetector::new(8000, 160);
        assert_eq!(detector.sample_rate, 8000);
        assert_eq!(detector.low_filters.len(), 4);
        assert_eq!(detector.high_filters.len(), 4);
    }

    #[test]
    fn test_detector_reset() {
        let mut detector = GoertzelDetector::new(8000, 160);
        detector.sample_count = 100;
        detector.current_digit = Some(DtmfDigit::Five);

        detector.reset();

        assert_eq!(detector.sample_count, 0);
        assert_eq!(detector.current_digit, None);
    }
}
