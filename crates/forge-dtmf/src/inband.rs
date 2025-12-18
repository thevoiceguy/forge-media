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

/// Goertzel-based DTMF detector with state machine for reliable event emission
///
/// # State Transitions
/// ```text
/// None → Detecting → Active → None
///        (< min)    (>= min)
///
/// Events:
/// - Start: Emitted when frames_detected >= min_detection_frames
/// - Continue: Emitted every 5 frames after Start
/// - End: Emitted when detection stops (only if Start was emitted)
/// ```
///
/// # Timing Accuracy
/// Duration is calculated as: `frames_detected × frame_duration_ms`
/// where `frame_duration_ms = (frame_size / sample_rate) × 1000`
///
/// # Example
/// For 8kHz audio with 160 samples per frame:
/// - Frame duration = 160/8000 × 1000 = 20ms
/// - Min detection = 100ms / 20ms = 5 frames
/// - A 200ms tone press = 10 frames × 20ms = 200ms duration
pub struct GoertzelDetector {
    /// Sample rate
    #[allow(dead_code)]
    sample_rate: u32,
    /// Duration of one frame in milliseconds (derived from frame_size/sample_rate)
    frame_duration_ms: u32,
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
    /// Whether the start event has been emitted for the current digit
    start_emitted: bool,
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
        // Derive frame duration (guard against invalid inputs to avoid division by zero)
        let frame_duration_ms = (frame_size as u32)
            .saturating_mul(1000)
            .checked_div(sample_rate)
            .unwrap_or(0)
            .max(1);

        let low_filters = LOW_FREQS
            .iter()
            .map(|&freq| GoertzelFilter::new(freq, sample_rate))
            .collect();

        let high_filters = HIGH_FREQS
            .iter()
            .map(|&freq| GoertzelFilter::new(freq, sample_rate))
            .collect();

        // Calculate minimum frames for ~100ms detection (ceiling)
        let min_detection_frames =
            (Self::DEFAULT_MIN_DURATION_MS + frame_duration_ms - 1) / frame_duration_ms;

        Self {
            sample_rate,
            frame_duration_ms,
            low_filters,
            high_filters,
            min_samples: frame_size,
            sample_count: 0,
            energy_threshold: Self::DEFAULT_ENERGY_THRESHOLD,
            twist_threshold: Self::DEFAULT_TWIST_THRESHOLD,
            current_digit: None,
            frames_detected: 0,
            min_detection_frames: min_detection_frames.max(1),
            start_emitted: false,
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

        // Map to DTMF digit and handle state transition
        let digit = Self::map_frequencies(low_idx, high_idx)?;
        self.handle_detected_digit(digit)
    }

    /// Handle state transitions for a detected digit (factored for testability)
    fn handle_detected_digit(&mut self, digit: DtmfDigit) -> Result<Option<DtmfEvent>> {
        if let Some(current) = self.current_digit {
            if current == digit {
                self.frames_detected = self.frames_detected.saturating_add(1);

                if !self.start_emitted && self.frames_detected >= self.min_detection_frames {
                    self.start_emitted = true;
                    return Ok(Some(DtmfEvent::new(
                        digit,
                        DtmfEventType::Start,
                        DtmfMethod::Inband,
                    )));
                }

                if self.start_emitted && self.frames_detected % 5 == 0 {
                    return Ok(Some(DtmfEvent::new(
                        digit,
                        DtmfEventType::Continue,
                        DtmfMethod::Inband,
                    )));
                }

                Ok(None)
            } else {
                // Different digit - end previous (if started), prime state for new digit
                let end_event = if self.start_emitted {
                    Some(DtmfEvent::with_duration(
                        current,
                        DtmfEventType::End,
                        DtmfMethod::Inband,
                        self.frames_detected
                            .saturating_mul(self.frame_duration_ms),
                    ))
                } else {
                    None
                };

                self.current_digit = Some(digit);
                self.frames_detected = 1;
                self.start_emitted = self.min_detection_frames <= 1;

                if self.start_emitted {
                    return Ok(Some(DtmfEvent::new(
                        digit,
                        DtmfEventType::Start,
                        DtmfMethod::Inband,
                    )));
                }

                Ok(end_event)
            }
        } else {
            // New digit detected
            self.current_digit = Some(digit);
            self.frames_detected = 1;
            self.start_emitted = self.min_detection_frames <= 1;

            if self.start_emitted {
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
            if self.start_emitted && self.frames_detected >= self.min_detection_frames {
                let event = DtmfEvent::with_duration(
                    digit,
                    DtmfEventType::End,
                    DtmfMethod::Inband,
                    self.frames_detected
                        .saturating_mul(self.frame_duration_ms),
                );
                self.frames_detected = 0;
                self.start_emitted = false;
                Ok(Some(event))
            } else {
                // Too short or never started, ignore
                self.frames_detected = 0;
                self.start_emitted = false;
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
        self.start_emitted = false;
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
        assert_eq!(
            GoertzelDetector::map_frequencies(0, 0).unwrap(),
            DtmfDigit::One
        );
        assert_eq!(
            GoertzelDetector::map_frequencies(1, 1).unwrap(),
            DtmfDigit::Five
        );
        assert_eq!(
            GoertzelDetector::map_frequencies(3, 1).unwrap(),
            DtmfDigit::Zero
        );
        assert_eq!(
            GoertzelDetector::map_frequencies(3, 0).unwrap(),
            DtmfDigit::Star
        );
        assert_eq!(
            GoertzelDetector::map_frequencies(3, 2).unwrap(),
            DtmfDigit::Hash
        );
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

    #[test]
    fn test_start_emitted_after_min_frames() {
        let mut detector = GoertzelDetector::new(8000, 160);

        // Speed up detection for the test
        detector.min_detection_frames = 3;

        // First two detections should not emit anything
        assert!(detector
            .handle_detected_digit(DtmfDigit::Five)
            .unwrap()
            .is_none());
        assert!(detector
            .handle_detected_digit(DtmfDigit::Five)
            .unwrap()
            .is_none());

        // Third detection reaches the threshold and should emit Start
        let event = detector
            .handle_detected_digit(DtmfDigit::Five)
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, DtmfEventType::Start);
        assert_eq!(event.digit, DtmfDigit::Five);
    }

    #[test]
    fn test_duration_respects_frame_size() {
        // 160 samples at 16 kHz = 10ms frames
        let mut detector = GoertzelDetector::new(16000, 160);
        detector.current_digit = Some(DtmfDigit::One);
        detector.frames_detected = 4;
        detector.start_emitted = true;
        detector.min_detection_frames = 1;

        let event = detector.handle_no_detection().unwrap().unwrap();
        assert_eq!(event.duration_ms, Some(40)); // 4 frames * 10ms
    }

    #[test]
    fn test_digit_change_before_start() {
        let mut detector = GoertzelDetector::new(8000, 160);
        detector.min_detection_frames = 5;

        // Detect digit '1' for 2 frames (below threshold)
        assert!(detector
            .handle_detected_digit(DtmfDigit::One)
            .unwrap()
            .is_none());
        assert!(detector
            .handle_detected_digit(DtmfDigit::One)
            .unwrap()
            .is_none());

        // Switch to digit '2' (should not emit End for '1' since Start never emitted)
        let result = detector.handle_detected_digit(DtmfDigit::Two).unwrap();
        assert!(result.is_none()); // No End event for '1'

        // Continue with '2' - first 3 detections emit nothing
        for _ in 0..3 {
            assert!(detector
                .handle_detected_digit(DtmfDigit::Two)
                .unwrap()
                .is_none());
        }

        // 4th detection reaches threshold (frames_detected becomes 5) and emits Start
        let event = detector
            .handle_detected_digit(DtmfDigit::Two)
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, DtmfEventType::Start);
        assert_eq!(event.digit, DtmfDigit::Two);
    }

    #[test]
    fn test_continue_events_every_5_frames() {
        let mut detector = GoertzelDetector::new(8000, 160);
        detector.min_detection_frames = 1;

        // First detection emits Start (frames_detected = 1)
        let event = detector
            .handle_detected_digit(DtmfDigit::Five)
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, DtmfEventType::Start);
        assert_eq!(event.digit, DtmfDigit::Five);

        // Next 3 detections emit nothing (frames_detected = 2, 3, 4)
        for _ in 0..3 {
            assert!(detector
                .handle_detected_digit(DtmfDigit::Five)
                .unwrap()
                .is_none());
        }

        // 4th additional detection (frames_detected = 5) emits Continue
        let event = detector
            .handle_detected_digit(DtmfDigit::Five)
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, DtmfEventType::Continue);
        assert_eq!(event.digit, DtmfDigit::Five);

        // Next 4 detections emit nothing (frames_detected = 6, 7, 8, 9)
        for _ in 0..4 {
            assert!(detector
                .handle_detected_digit(DtmfDigit::Five)
                .unwrap()
                .is_none());
        }

        // 9th additional detection (frames_detected = 10) emits Continue again
        let event = detector
            .handle_detected_digit(DtmfDigit::Five)
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, DtmfEventType::Continue);
        assert_eq!(event.digit, DtmfDigit::Five);
    }
}
