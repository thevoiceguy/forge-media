//! Barge-in Detection
//!
//! Detects when a user interrupts AI speech and automatically
//! sends interrupt signals to the AI service.

use crate::vad::{VadDetector, VadState};
use crate::Result;
use std::time::{Duration, Instant};

/// Barge-in detector state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BargeInState {
    /// Idle - no AI speech active
    Idle,
    /// AI is speaking
    AISpeaking,
    /// User interrupted (barge-in detected)
    UserInterrupted,
    /// Cooldown period after interrupt
    Cooldown,
}

/// Barge-in configuration
#[derive(Debug, Clone)]
pub struct BargeInConfig {
    /// Enable barge-in detection
    pub enabled: bool,

    /// Cooldown period after barge-in before detecting again
    pub cooldown_duration: Duration,

    /// Minimum confidence threshold for barge-in (0.0-1.0)
    pub confidence_threshold: f32,

    /// Minimum duration of user speech before triggering barge-in
    pub min_user_speech_duration: Duration,
}

impl Default for BargeInConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_duration: Duration::from_millis(500),
            confidence_threshold: 0.6,
            min_user_speech_duration: Duration::from_millis(300),
        }
    }
}

/// Barge-in detector
///
/// Monitors VAD state and AI playback state to detect when
/// the user interrupts the AI.
pub struct BargeInDetector {
    config: BargeInConfig,
    state: BargeInState,
    vad: VadDetector,

    // Timing tracking
    ai_speech_start: Option<Instant>,
    user_speech_start: Option<Instant>,
    last_interrupt: Option<Instant>,

    // Statistics
    interrupt_count: u64,
}

impl BargeInDetector {
    /// Create a new barge-in detector
    pub fn new(config: BargeInConfig, vad: VadDetector) -> Self {
        Self {
            config,
            state: BargeInState::Idle,
            vad,
            ai_speech_start: None,
            user_speech_start: None,
            last_interrupt: None,
            interrupt_count: 0,
        }
    }

    /// Notify that AI started speaking
    pub fn ai_started_speaking(&mut self) {
        if !self.config.enabled {
            return;
        }

        self.state = BargeInState::AISpeaking;
        self.ai_speech_start = Some(Instant::now());
        self.user_speech_start = None;
    }

    /// Notify that AI stopped speaking
    pub fn ai_stopped_speaking(&mut self) {
        self.state = BargeInState::Idle;
        self.ai_speech_start = None;
        self.user_speech_start = None;
    }

    /// Process audio frame and check for barge-in
    ///
    /// Returns (barge_in_detected, vad_state, confidence)
    pub fn process_audio(&mut self, audio: &[i16]) -> Result<(bool, VadState, f32)> {
        if !self.config.enabled {
            return Ok((false, VadState::Unknown, 0.0));
        }

        // Check if we're in cooldown
        if self.state == BargeInState::Cooldown {
            if let Some(last_interrupt) = self.last_interrupt {
                if last_interrupt.elapsed() >= self.config.cooldown_duration {
                    self.state = BargeInState::Idle;
                }
            }
        }

        // Process audio with VAD
        let (vad_state, confidence) = self.vad.process(audio)?;

        // Track user speech timing
        match vad_state {
            VadState::Speech => {
                if self.user_speech_start.is_none() {
                    self.user_speech_start = Some(Instant::now());
                }
            }
            _ => {
                self.user_speech_start = None;
            }
        }

        // Detect barge-in conditions
        let barge_in_detected = self.check_barge_in(vad_state, confidence);

        if barge_in_detected {
            self.state = BargeInState::UserInterrupted;
            self.last_interrupt = Some(Instant::now());
            self.interrupt_count += 1;
        }

        Ok((barge_in_detected, vad_state, confidence))
    }

    /// Check if barge-in conditions are met
    fn check_barge_in(&self, vad_state: VadState, confidence: f32) -> bool {
        // Must be enabled and AI must be speaking
        if !self.config.enabled || self.state != BargeInState::AISpeaking {
            return false;
        }

        // Must detect user speech with sufficient confidence
        if vad_state != VadState::Speech || confidence < self.config.confidence_threshold {
            return false;
        }

        // Check minimum user speech duration
        if let Some(user_start) = self.user_speech_start {
            if user_start.elapsed() < self.config.min_user_speech_duration {
                return false;
            }
        } else {
            return false;
        }

        // In cooldown period?
        if self.state == BargeInState::Cooldown {
            return false;
        }

        true
    }

    /// Get current barge-in state
    pub fn state(&self) -> BargeInState {
        self.state
    }

    /// Get interrupt count
    pub fn interrupt_count(&self) -> u64 {
        self.interrupt_count
    }

    /// Check if AI is currently speaking
    pub fn is_ai_speaking(&self) -> bool {
        self.state == BargeInState::AISpeaking
    }

    /// Get duration of current AI speech
    pub fn ai_speech_duration(&self) -> Option<Duration> {
        self.ai_speech_start.map(|start| start.elapsed())
    }

    /// Get VAD detector reference
    pub fn vad(&self) -> &VadDetector {
        &self.vad
    }

    /// Get mutable VAD detector reference
    pub fn vad_mut(&mut self) -> &mut VadDetector {
        &mut self.vad
    }

    /// Reset barge-in detector
    pub fn reset(&mut self) {
        self.state = BargeInState::Idle;
        self.ai_speech_start = None;
        self.user_speech_start = None;
        self.last_interrupt = None;
        self.vad.reset();
    }

    /// Reset statistics
    pub fn reset_stats(&mut self) {
        self.interrupt_count = 0;
        self.last_interrupt = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::VadConfig;

    #[test]
    fn test_bargein_creation() {
        let vad_config = VadConfig::default();
        let vad = VadDetector::new(vad_config);
        let config = BargeInConfig::default();
        let detector = BargeInDetector::new(config, vad);

        assert_eq!(detector.state(), BargeInState::Idle);
        assert_eq!(detector.interrupt_count(), 0);
        assert!(!detector.is_ai_speaking());
    }

    #[test]
    fn test_ai_speaking_state() {
        let vad_config = VadConfig::default();
        let vad = VadDetector::new(vad_config);
        let config = BargeInConfig::default();
        let mut detector = BargeInDetector::new(config, vad);

        detector.ai_started_speaking();
        assert_eq!(detector.state(), BargeInState::AISpeaking);
        assert!(detector.is_ai_speaking());

        detector.ai_stopped_speaking();
        assert_eq!(detector.state(), BargeInState::Idle);
        assert!(!detector.is_ai_speaking());
    }

    #[test]
    fn test_barge_in_detection() {
        let vad_config = VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        };
        let vad = VadDetector::new(vad_config);

        let config = BargeInConfig {
            enabled: true,
            cooldown_duration: Duration::from_millis(500),
            confidence_threshold: 0.5,
            min_user_speech_duration: Duration::from_millis(100),
        };

        let mut detector = BargeInDetector::new(config, vad);

        // AI starts speaking
        detector.ai_started_speaking();

        // Generate speech-like audio
        let speech_frame: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();

        // Process several frames to build up user speech duration
        let mut barge_in_detected = false;
        for _ in 0..20 {
            let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
            if detected {
                barge_in_detected = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Should eventually detect barge-in
        assert!(barge_in_detected);
        assert_eq!(detector.state(), BargeInState::UserInterrupted);
        assert_eq!(detector.interrupt_count(), 1);
    }

    #[test]
    fn test_no_barge_in_when_ai_not_speaking() {
        let vad_config = VadConfig {
            sensitivity: 0.5,
            min_speech_duration_ms: 50,
            min_silence_duration_ms: 100,
            sample_rate: 16000,
            frame_size_ms: 20,
            energy_threshold: 100.0,
            zcr_threshold: 0.3,
        };
        let vad = VadDetector::new(vad_config);
        let config = BargeInConfig::default();
        let mut detector = BargeInDetector::new(config, vad);

        // AI is NOT speaking
        let speech_frame: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();

        for _ in 0..10 {
            let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
            assert!(!detected); // Should not detect barge-in
        }

        assert_eq!(detector.interrupt_count(), 0);
    }

    #[test]
    fn test_disabled_barge_in() {
        let vad_config = VadConfig::default();
        let vad = VadDetector::new(vad_config);
        let config = BargeInConfig {
            enabled: false,
            ..Default::default()
        };
        let mut detector = BargeInDetector::new(config, vad);

        detector.ai_started_speaking();

        let speech_frame: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
            .collect();

        for _ in 0..10 {
            let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
            assert!(!detected); // Should not detect when disabled
        }
    }

    #[test]
    fn test_reset() {
        let vad_config = VadConfig::default();
        let vad = VadDetector::new(vad_config);
        let config = BargeInConfig::default();
        let mut detector = BargeInDetector::new(config, vad);

        detector.ai_started_speaking();
        detector.reset();

        assert_eq!(detector.state(), BargeInState::Idle);
        assert!(!detector.is_ai_speaking());
        assert_eq!(detector.ai_speech_duration(), None);
    }
}
