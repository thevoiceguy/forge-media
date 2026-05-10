//! Voice Activity Detection (VAD) for `forge-media`.
//!
//! Detects when speech is present in 16-bit linear-PCM audio frames
//! using energy (RMS) and zero-crossing-rate analysis with hysteresis
//! and an adaptive noise-floor estimator. The algorithm has zero
//! external runtime dependencies — it runs synchronously, allocates
//! a fixed-size sliding window, and is safe to call from a per-frame
//! audio loop.
//!
//! ## Where it fits
//!
//! `forge-engine`'s RTP forwarding loop runs the detector on each
//! decoded inbound PCM frame and publishes
//! [`forge_core::ForgeEvent::SpeechStarted`] /
//! [`forge_core::ForgeEvent::SpeechStopped`] on state transitions
//! (one event per transition, debounced by hysteresis). External
//! consumers (`siphon-ai`, `forge-ai-stream`'s OpenAI / Anthropic
//! / Deepgram / ElevenLabs adapters) subscribe to the event bus.
//!
//! Until 2026-05 the algorithm lived inside `forge-ai-stream`. It
//! moved here so non-AI consumers (notably `siphon-ai`, which is
//! provider-neutral and explicitly forbidden from depending on
//! `forge-ai-stream`) can use it without dragging in OpenAI /
//! Anthropic / Deepgram / ElevenLabs WebSocket clients.
//!
//! ## Tuning
//!
//! Defaults target speech-band PCM at 8 kHz / 16 kHz. The
//! `sensitivity` knob (0.0 → 1.0) trades false negatives against
//! false positives: 0.5 is balanced; lower values bias toward
//! "definitely speech only", higher values bias toward "lean toward
//! speech." If `energy_threshold = 0.0` the detector estimates the
//! noise floor adaptively from the 20th percentile of the recent
//! energy history.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VadError {
    /// The detector saw a configuration value that doesn't make
    /// sense (e.g., `frame_size_ms` of 0). Caller bug.
    #[error("invalid VAD configuration: {0}")]
    InvalidConfig(String),
}

pub type Result<T> = std::result::Result<T, VadError>;

/// VAD state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    /// Speech detected (held until `min_silence_duration_ms` elapses).
    Speech,
    /// Silence detected (held until `min_speech_duration_ms` elapses).
    Silence,
    /// Initialising — no decision yet. Detectors return this until
    /// enough frames have flowed through to run hysteresis.
    Unknown,
}

/// VAD configuration.
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Sensitivity (0.0–1.0, higher = more sensitive).
    pub sensitivity: f32,

    /// Minimum speech duration in milliseconds before the detector
    /// commits to the `Speech` state. Hysteresis filter — a single
    /// noisy frame won't flip the state.
    pub min_speech_duration_ms: u32,

    /// Minimum silence duration in milliseconds before the detector
    /// commits to `Silence`. Higher values smooth over normal
    /// inter-syllable pauses; lower values produce more
    /// `SpeechStopped` events per utterance.
    pub min_silence_duration_ms: u32,

    /// Sample rate of audio fed to [`VadDetector::process`]. Used
    /// only for documentation today — the algorithm itself runs on
    /// raw `&[i16]` slices.
    pub sample_rate: u32,

    /// Frame size in milliseconds (typically 10–30 ms). Used to
    /// translate the duration thresholds into frame counts.
    pub frame_size_ms: u32,

    /// Energy threshold. Set to `0.0` to enable adaptive
    /// thresholding from the noise floor estimate.
    pub energy_threshold: f32,

    /// Zero-crossing-rate threshold above which the frame is
    /// treated as noise (typical ZCR for noise is high; for voiced
    /// speech it's lower).
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
            energy_threshold: 0.0, // Auto-adjust.
            zcr_threshold: 0.3,
        }
    }
}

/// VAD detector using energy and zero-crossing rate with hysteresis.
pub struct VadDetector {
    config: VadConfig,
    current_state: VadState,
    speech_frames: u32,
    silence_frames: u32,

    // Adaptive thresholds.
    energy_threshold: f32,
    energy_history: Vec<f32>,
    max_history_size: usize,

    // Noise estimation.
    noise_level: f32,
    snr_threshold: f32,
}

impl VadDetector {
    /// Create a new VAD detector.
    pub fn new(config: VadConfig) -> Self {
        let energy_threshold = if config.energy_threshold > 0.0 {
            config.energy_threshold
        } else {
            // Default threshold based on sensitivity.
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
            snr_threshold: 3.0, // 3:1 SNR for speech detection.
        }
    }

    /// Process one audio frame and return the resulting `(state,
    /// confidence)` pair. `confidence` is `0.0` when the frame is
    /// classified as silence; for speech frames it scales 0.0–1.0
    /// with how far above the energy threshold the frame is.
    pub fn process(&mut self, audio: &[i16]) -> Result<(VadState, f32)> {
        if audio.is_empty() {
            return Ok((self.current_state, 0.0));
        }

        // RMS energy.
        let energy = self.calculate_energy(audio);

        // Zero-crossing rate.
        let zcr = self.calculate_zcr(audio);

        // Update history for adaptive threshold.
        self.update_energy_history(energy);

        // Estimate noise level from lower percentile of energy history.
        self.update_noise_level();

        // Calculate SNR.
        let snr = if self.noise_level > 0.0 {
            energy / self.noise_level
        } else {
            energy / 1.0
        };

        // Frame contains speech if ALL three signals agree.
        let is_speech = energy > self.energy_threshold
            && zcr < self.config.zcr_threshold
            && snr > self.snr_threshold;

        // Confidence (0.0–1.0).
        let confidence = if is_speech {
            ((energy / self.energy_threshold).min(3.0) / 3.0)
                .max(0.0)
                .min(1.0)
        } else {
            0.0
        };

        self.update_state(is_speech);

        Ok((self.current_state, confidence))
    }

    /// Calculate RMS energy of audio frame.
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

    /// Calculate zero-crossing rate (normalized 0.0–1.0).
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

    /// Update sliding-window energy history for adaptive thresholding.
    fn update_energy_history(&mut self, energy: f32) {
        self.energy_history.push(energy);
        if self.energy_history.len() > self.max_history_size {
            self.energy_history.remove(0);
        }
    }

    /// Update noise level estimate (using 20th percentile of recent
    /// frames' energies).
    fn update_noise_level(&mut self) {
        if self.energy_history.len() < 10 {
            return;
        }

        let mut sorted = self.energy_history.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // 20th percentile.
        let percentile_idx = (sorted.len() as f32 * 0.2) as usize;
        self.noise_level = sorted[percentile_idx];

        // Adaptive threshold adjustment based on noise.
        if self.config.energy_threshold == 0.0 {
            self.energy_threshold =
                self.noise_level * self.snr_threshold * (2.0 - self.config.sensitivity);
        }
    }

    /// Update VAD state with hysteresis.
    fn update_state(&mut self, is_speech: bool) {
        let min_speech_frames =
            (self.config.min_speech_duration_ms / self.config.frame_size_ms.max(1)).max(1);
        let min_silence_frames =
            (self.config.min_silence_duration_ms / self.config.frame_size_ms.max(1)).max(1);

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

    /// Current VAD state.
    pub fn state(&self) -> VadState {
        self.current_state
    }

    /// Current energy threshold (post adaptive adjustment).
    pub fn energy_threshold(&self) -> f32 {
        self.energy_threshold
    }

    /// Current noise level estimate.
    pub fn noise_level(&self) -> f32 {
        self.noise_level
    }

    /// Reset state. Caller drops any pending speech bookkeeping
    /// after a session change (mid-call codec switch, etc.).
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
    fn vad_creation_starts_unknown() {
        let detector = VadDetector::new(VadConfig::default());
        assert_eq!(detector.state(), VadState::Unknown);
    }

    #[test]
    fn energy_calculation_grows_with_amplitude() {
        let detector = VadDetector::new(VadConfig::default());

        let silent = vec![0i16; 320];
        assert_eq!(detector.calculate_energy(&silent), 0.0);

        let low: Vec<i16> = (0..320).map(|i| (i % 100) as i16).collect();
        let energy_low = detector.calculate_energy(&low);
        assert!(energy_low > 0.0 && energy_low < 100.0);

        let high: Vec<i16> = (0..320).map(|i| (i % 1000) as i16 * 10).collect();
        let energy_high = detector.calculate_energy(&high);
        assert!(energy_high > energy_low);
    }

    #[test]
    fn zero_crossing_rate_distinguishes_signal_from_noise() {
        let detector = VadDetector::new(VadConfig::default());

        // Mostly positive — low ZCR.
        let low_zcr: Vec<i16> = (0..320).map(|i| 1000 + (i % 100) as i16).collect();
        assert!(detector.calculate_zcr(&low_zcr) < 0.1);

        // Alternating sign — ZCR ≈ 1.
        let high_zcr: Vec<i16> = (0..320)
            .map(|i| if i % 2 == 0 { 1000 } else { -1000 })
            .collect();
        assert!(detector.calculate_zcr(&high_zcr) > 0.9);
    }

    #[test]
    fn speech_detection_after_min_speech_duration() {
        let mut detector = VadDetector::new(VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        });

        // Sine-ish frame: high energy, low ZCR — speech-like.
        let speech: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();

        for _ in 0..10 {
            detector.process(&speech).unwrap();
        }
        assert_eq!(detector.state(), VadState::Speech);
    }

    #[test]
    fn silence_after_speech_with_hysteresis() {
        let mut detector = VadDetector::new(VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        });

        let speech: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();
        for _ in 0..10 {
            detector.process(&speech).unwrap();
        }
        assert_eq!(detector.state(), VadState::Speech);

        let silence = vec![0i16; 320];
        for _ in 0..10 {
            detector.process(&silence).unwrap();
        }
        assert_eq!(detector.state(), VadState::Silence);
    }

    #[test]
    fn single_speech_frame_does_not_trigger_speech() {
        let mut detector = VadDetector::new(VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 60,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        });

        let speech: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();
        detector.process(&speech).unwrap();
        assert_eq!(detector.state(), VadState::Unknown);

        for _ in 0..10 {
            detector.process(&speech).unwrap();
        }
        assert_eq!(detector.state(), VadState::Speech);
    }

    #[test]
    fn adaptive_threshold_kicks_in_after_history_warms_up() {
        let mut detector = VadDetector::new(VadConfig {
            sensitivity: 0.7,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 0.0, // Auto.
            zcr_threshold: 0.3,
        });

        for i in 0..50 {
            let energy_level = (i * 10) as i16;
            let frame: Vec<i16> = vec![energy_level; 320];
            detector.process(&frame).unwrap();
        }

        assert!(detector.noise_level() > 0.0);
        assert!(detector.energy_threshold() > 0.0);
    }

    #[test]
    fn reset_returns_to_unknown() {
        let mut detector = VadDetector::new(VadConfig::default());

        let speech: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();
        for _ in 0..10 {
            detector.process(&speech).unwrap();
        }

        detector.reset();
        assert_eq!(detector.state(), VadState::Unknown);
        assert_eq!(detector.noise_level(), 0.0);
    }

    #[test]
    fn empty_frame_is_a_no_op() {
        let mut detector = VadDetector::new(VadConfig::default());
        let (state, conf) = detector.process(&[]).unwrap();
        assert_eq!(state, VadState::Unknown);
        assert_eq!(conf, 0.0);
    }
}
