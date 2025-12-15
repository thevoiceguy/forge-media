//! Voice Activity Detection (VAD)
//!
//! Detects when speech is present in audio streams using energy-based
//! and zero-crossing rate analysis.

use crate::Result;

/// VAD state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    /// Speech detected
    Speech,
    /// Silence detected
    Silence,
    /// Unknown/initializing
    Unknown,
}

/// VAD configuration
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Sensitivity (0.0-1.0, higher = more sensitive)
    pub sensitivity: f32,

    /// Minimum speech duration in milliseconds
    pub min_speech_duration_ms: u32,

    /// Minimum silence duration in milliseconds
    pub min_silence_duration_ms: u32,

    /// Sample rate
    pub sample_rate: u32,

    /// Frame size in milliseconds (typically 10-30ms)
    pub frame_size_ms: u32,

    /// Energy threshold (auto-adjusted if 0.0)
    pub energy_threshold: f32,

    /// Zero-crossing rate threshold
    pub zcr_threshold: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            min_speech_duration_ms: 100,
            min_silence_duration_ms: 500,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 0.0, // Auto-adjust
            zcr_threshold: 0.3,
        }
    }
}

/// VAD detector using energy and zero-crossing rate
pub struct VadDetector {
    config: VadConfig,
    current_state: VadState,
    speech_frames: u32,
    silence_frames: u32,

    // Adaptive thresholds
    energy_threshold: f32,
    energy_history: Vec<f32>,
    max_history_size: usize,

    // Noise estimation
    noise_level: f32,
    snr_threshold: f32,
}

impl VadDetector {
    /// Create a new VAD detector
    pub fn new(config: VadConfig) -> Self {
        let energy_threshold = if config.energy_threshold > 0.0 {
            config.energy_threshold
        } else {
            // Default threshold based on sensitivity
            500.0 * (1.0 - config.sensitivity)
        };

        Self {
            config,
            current_state: VadState::Unknown,
            speech_frames: 0,
            silence_frames: 0,
            energy_threshold,
            energy_history: Vec::with_capacity(100),
            max_history_size: 100,
            noise_level: 0.0,
            snr_threshold: 3.0, // 3:1 SNR for speech detection
        }
    }

    /// Process audio frame and return VAD state and confidence
    pub fn process(&mut self, audio: &[i16]) -> Result<(VadState, f32)> {
        if audio.is_empty() {
            return Ok((self.current_state, 0.0));
        }

        // Calculate energy (RMS)
        let energy = self.calculate_energy(audio);

        // Calculate zero-crossing rate
        let zcr = self.calculate_zcr(audio);

        // Update energy history for adaptive threshold
        self.update_energy_history(energy);

        // Estimate noise level from lower percentile of energy history
        self.update_noise_level();

        // Calculate SNR
        let snr = if self.noise_level > 0.0 {
            energy / self.noise_level
        } else {
            energy / 1.0
        };

        // Determine if frame contains speech
        let is_speech = energy > self.energy_threshold
            && zcr < self.config.zcr_threshold
            && snr > self.snr_threshold;

        // Calculate confidence (0.0-1.0)
        let confidence = if is_speech {
            ((energy / self.energy_threshold).min(3.0) / 3.0)
                .max(0.0)
                .min(1.0)
        } else {
            0.0
        };

        // Update state with hysteresis
        self.update_state(is_speech);

        Ok((self.current_state, confidence))
    }

    /// Calculate RMS energy of audio frame
    fn calculate_energy(&self, audio: &[i16]) -> f32 {
        if audio.is_empty() {
            return 0.0;
        }

        let sum_squares: f64 = audio
            .iter()
            .map(|&sample| (sample as f64) * (sample as f64))
            .sum();

        (sum_squares / audio.len() as f64).sqrt() as f32
    }

    /// Calculate zero-crossing rate (normalized)
    fn calculate_zcr(&self, audio: &[i16]) -> f32 {
        if audio.len() < 2 {
            return 0.0;
        }

        let mut crossings = 0;
        for i in 1..audio.len() {
            if (audio[i] >= 0 && audio[i - 1] < 0) || (audio[i] < 0 && audio[i - 1] >= 0) {
                crossings += 1;
            }
        }

        crossings as f32 / (audio.len() - 1) as f32
    }

    /// Update energy history for adaptive thresholding
    fn update_energy_history(&mut self, energy: f32) {
        self.energy_history.push(energy);
        if self.energy_history.len() > self.max_history_size {
            self.energy_history.remove(0);
        }
    }

    /// Update noise level estimate (using 20th percentile)
    fn update_noise_level(&mut self) {
        if self.energy_history.len() < 10 {
            return;
        }

        let mut sorted = self.energy_history.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Use 20th percentile as noise estimate
        let percentile_idx = (sorted.len() as f32 * 0.2) as usize;
        self.noise_level = sorted[percentile_idx];

        // Adaptive threshold adjustment based on noise
        if self.config.energy_threshold == 0.0 {
            self.energy_threshold = self.noise_level * self.snr_threshold * (2.0 - self.config.sensitivity);
        }
    }

    /// Update VAD state with hysteresis
    fn update_state(&mut self, is_speech: bool) {
        // Calculate minimum frames needed based on configured durations
        let min_speech_frames = (self.config.min_speech_duration_ms / self.config.frame_size_ms).max(1);
        let min_silence_frames = (self.config.min_silence_duration_ms / self.config.frame_size_ms).max(1);

        if is_speech {
            self.speech_frames += 1;
            self.silence_frames = 0;

            if self.speech_frames >= min_speech_frames {
                self.current_state = VadState::Speech;
            }
        } else {
            self.silence_frames += 1;
            self.speech_frames = 0;

            if self.silence_frames >= min_silence_frames {
                self.current_state = VadState::Silence;
            }
        }
    }

    /// Get current VAD state
    pub fn state(&self) -> VadState {
        self.current_state
    }

    /// Get current energy threshold
    pub fn energy_threshold(&self) -> f32 {
        self.energy_threshold
    }

    /// Get current noise level
    pub fn noise_level(&self) -> f32 {
        self.noise_level
    }

    /// Reset VAD state
    pub fn reset(&mut self) {
        self.current_state = VadState::Unknown;
        self.speech_frames = 0;
        self.silence_frames = 0;
        self.energy_history.clear();
        self.noise_level = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vad_creation() {
        let config = VadConfig::default();
        let detector = VadDetector::new(config);
        assert_eq!(detector.state(), VadState::Unknown);
    }

    #[test]
    fn test_energy_calculation() {
        let config = VadConfig::default();
        let detector = VadDetector::new(config);

        // Silent audio (all zeros)
        let silent = vec![0i16; 320];
        let energy = detector.calculate_energy(&silent);
        assert_eq!(energy, 0.0);

        // Low energy audio
        let low_energy: Vec<i16> = (0..320).map(|i| (i % 100) as i16).collect();
        let energy_low = detector.calculate_energy(&low_energy);
        assert!(energy_low > 0.0 && energy_low < 100.0);

        // High energy audio
        let high_energy: Vec<i16> = (0..320).map(|i| (i % 1000) as i16 * 10).collect();
        let energy_high = detector.calculate_energy(&high_energy);
        assert!(energy_high > energy_low);
    }

    #[test]
    fn test_zero_crossing_rate() {
        let config = VadConfig::default();
        let detector = VadDetector::new(config);

        // Low ZCR (mostly positive or mostly negative)
        let low_zcr: Vec<i16> = (0..320).map(|i| 1000 + (i % 100) as i16).collect();
        let zcr_low = detector.calculate_zcr(&low_zcr);
        assert!(zcr_low < 0.1);

        // High ZCR (alternating sign - like noise)
        let high_zcr: Vec<i16> = (0..320).map(|i| if i % 2 == 0 { 1000 } else { -1000 }).collect();
        let zcr_high = detector.calculate_zcr(&high_zcr);
        assert!(zcr_high > 0.9); // Should be close to 1.0
    }

    #[test]
    fn test_speech_detection() {
        let config = VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        };

        let mut detector = VadDetector::new(config);

        // Generate speech-like audio (high energy, low ZCR)
        let speech_frame: Vec<i16> = (0..320).map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16).collect();

        // Process multiple frames to exceed min_speech_duration
        for _ in 0..10 {
            let (state, confidence) = detector.process(&speech_frame).unwrap();
            // After enough frames, should detect speech
            if detector.speech_frames >= 3 {
                assert_eq!(state, VadState::Speech);
                assert!(confidence > 0.0);
            }
        }

        assert_eq!(detector.state(), VadState::Speech);
    }

    #[test]
    fn test_silence_detection() {
        let config = VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        };

        let mut detector = VadDetector::new(config);

        // Start with speech
        let speech_frame: Vec<i16> = (0..320).map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16).collect();
        for _ in 0..10 {
            detector.process(&speech_frame).unwrap();
        }
        assert_eq!(detector.state(), VadState::Speech);

        // Process silence frames
        let silence_frame = vec![0i16; 320];
        for _ in 0..10 {
            let (state, confidence) = detector.process(&silence_frame).unwrap();
            // Confidence should be 0 for silence
            if detector.silence_frames >= 5 {
                assert_eq!(state, VadState::Silence);
                assert_eq!(confidence, 0.0);
            }
        }

        assert_eq!(detector.state(), VadState::Silence);
    }

    #[test]
    fn test_hysteresis() {
        let config = VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 60,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        };

        let mut detector = VadDetector::new(config);

        // Single speech frame should not trigger speech state
        let speech_frame: Vec<i16> = (0..320).map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16).collect();
        detector.process(&speech_frame).unwrap();
        assert_eq!(detector.state(), VadState::Unknown);

        // Multiple consecutive speech frames should trigger
        for _ in 0..10 {
            detector.process(&speech_frame).unwrap();
        }
        assert_eq!(detector.state(), VadState::Speech);
    }

    #[test]
    fn test_adaptive_threshold() {
        let config = VadConfig {
            sensitivity: 0.7,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 0.0, // Auto-adjust
            zcr_threshold: 0.3,
        };

        let mut detector = VadDetector::new(config);

        // Process various energy levels to build history
        for i in 0..50 {
            let energy_level = (i * 10) as i16;
            let frame: Vec<i16> = vec![energy_level; 320];
            detector.process(&frame).unwrap();
        }

        // Threshold should have been adjusted
        assert!(detector.noise_level() > 0.0);
        assert!(detector.energy_threshold() > 0.0);
    }

    #[test]
    fn test_reset() {
        let config = VadConfig::default();
        let mut detector = VadDetector::new(config);

        // Process some audio
        let speech_frame: Vec<i16> = (0..320).map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16).collect();
        for _ in 0..10 {
            detector.process(&speech_frame).unwrap();
        }

        // Reset
        detector.reset();

        assert_eq!(detector.state(), VadState::Unknown);
        assert_eq!(detector.noise_level(), 0.0);
    }

    #[test]
    fn test_empty_audio() {
        let config = VadConfig::default();
        let mut detector = VadDetector::new(config);

        let empty: Vec<i16> = vec![];
        let (state, confidence) = detector.process(&empty).unwrap();

        assert_eq!(confidence, 0.0);
        // State should remain unchanged
        assert_eq!(state, VadState::Unknown);
    }
}
