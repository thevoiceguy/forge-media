//! Audio mixing modes for injection

use forge_core::AudioSample;

/// Audio mixing mode for injection
///
/// Defines how injected audio should be combined with the existing call audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixMode {
    /// Mix injected audio with call audio (additive mixing)
    ///
    /// Both audio streams are audible. The injected audio is added to the
    /// call audio with optional volume adjustment.
    Mix,

    /// Replace call audio with injected audio
    ///
    /// The call audio is completely replaced by the injected audio.
    /// Useful for announcements or hold music.
    Replace,

    /// Duck call audio when playing injected audio
    ///
    /// The call audio volume is reduced (ducked) when injected audio is playing,
    /// then restored when injection stops. The duck_factor determines how much
    /// to reduce the volume (0.0 = silence, 1.0 = no change).
    Duck {
        /// Volume reduction factor (0.0 to 1.0)
        duck_factor: f32,
    },
}

impl Default for MixMode {
    fn default() -> Self {
        MixMode::Mix
    }
}

impl MixMode {
    /// Apply the mix mode to combine call audio and injected audio
    ///
    /// # Arguments
    ///
    /// * `call_audio` - Original call audio samples (modified in-place)
    /// * `injected_audio` - Audio to inject
    /// * `injection_volume` - Volume multiplier for injected audio (0.0 to 1.0)
    ///
    /// # Panics
    ///
    /// Panics if the two audio slices have different lengths.
    pub fn apply(
        &self,
        call_audio: &mut [AudioSample],
        injected_audio: &[AudioSample],
        injection_volume: f32,
    ) {
        assert_eq!(
            call_audio.len(),
            injected_audio.len(),
            "Audio buffer length mismatch"
        );

        match self {
            MixMode::Mix => {
                // Additive mixing with volume control
                for (call_sample, &inject_sample) in call_audio.iter_mut().zip(injected_audio) {
                    let mixed =
                        *call_sample as i32 + (inject_sample as f32 * injection_volume) as i32;
                    *call_sample = mixed.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                }
            }
            MixMode::Replace => {
                // Complete replacement with volume control
                for (call_sample, &inject_sample) in call_audio.iter_mut().zip(injected_audio) {
                    *call_sample = (inject_sample as f32 * injection_volume) as i16;
                }
            }
            MixMode::Duck { duck_factor } => {
                // Duck call audio and mix with injected audio
                for (call_sample, &inject_sample) in call_audio.iter_mut().zip(injected_audio) {
                    let ducked_call = (*call_sample as f32 * duck_factor) as i32;
                    let injection = (inject_sample as f32 * injection_volume) as i32;
                    let mixed = ducked_call + injection;
                    *call_sample = mixed.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                }
            }
        }
    }
}

/// Fade envelope for smooth audio transitions
///
/// Provides linear fade-in and fade-out effects to avoid clicks and pops.
#[derive(Debug, Clone)]
pub struct FadeEnvelope {
    /// Fade-in duration in samples
    pub fade_in_samples: usize,

    /// Fade-out duration in samples
    pub fade_out_samples: usize,

    /// Current sample position
    position: usize,

    /// Total duration in samples (if known)
    total_samples: Option<usize>,
}

impl FadeEnvelope {
    /// Create a new fade envelope
    ///
    /// # Arguments
    ///
    /// * `fade_in_ms` - Fade-in duration in milliseconds
    /// * `fade_out_ms` - Fade-out duration in milliseconds
    /// * `sample_rate` - Audio sample rate in Hz
    /// * `total_samples` - Total audio duration in samples (for fade-out)
    pub fn new(
        fade_in_ms: u32,
        fade_out_ms: u32,
        sample_rate: u32,
        total_samples: Option<usize>,
    ) -> Self {
        let fade_in_samples = (sample_rate as f32 * fade_in_ms as f32 / 1000.0) as usize;
        let fade_out_samples = (sample_rate as f32 * fade_out_ms as f32 / 1000.0) as usize;

        Self {
            fade_in_samples,
            fade_out_samples,
            position: 0,
            total_samples,
        }
    }

    /// Apply fade envelope to audio samples
    ///
    /// # Arguments
    ///
    /// * `samples` - Audio samples to apply fade to (modified in-place)
    pub fn apply(&mut self, samples: &mut [AudioSample]) {
        for sample in samples.iter_mut() {
            let gain = self.compute_gain();
            *sample = (*sample as f32 * gain) as i16;
            self.position += 1;
        }
    }

    /// Compute the current gain value (0.0 to 1.0)
    fn compute_gain(&self) -> f32 {
        // Fade-in
        if self.position < self.fade_in_samples {
            return self.position as f32 / self.fade_in_samples as f32;
        }

        // Fade-out
        if let Some(total) = self.total_samples {
            if self.position >= total.saturating_sub(self.fade_out_samples) {
                let fade_out_position = self.position - (total - self.fade_out_samples);
                return 1.0 - (fade_out_position as f32 / self.fade_out_samples as f32);
            }
        }

        // Full gain
        1.0
    }

    /// Reset the envelope position
    pub fn reset(&mut self) {
        self.position = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mix_mode_mix() {
        let mut call_audio = vec![100i16; 10];
        let injected_audio = vec![50i16; 10];

        MixMode::Mix.apply(&mut call_audio, &injected_audio, 1.0);

        // Should add the two signals
        assert_eq!(call_audio[0], 150);
    }

    #[test]
    fn test_mix_mode_replace() {
        let mut call_audio = vec![100i16; 10];
        let injected_audio = vec![200i16; 10];

        MixMode::Replace.apply(&mut call_audio, &injected_audio, 1.0);

        // Should replace with injected audio
        assert_eq!(call_audio[0], 200);
    }

    #[test]
    fn test_mix_mode_duck() {
        let mut call_audio = vec![1000i16; 10];
        let injected_audio = vec![100i16; 10];

        MixMode::Duck { duck_factor: 0.5 }.apply(&mut call_audio, &injected_audio, 1.0);

        // Should duck call audio and add injection
        // 1000 * 0.5 + 100 = 600
        assert_eq!(call_audio[0], 600);
    }

    #[test]
    fn test_fade_envelope() {
        // 10ms fade-in and fade-out at 8kHz = 80 samples each
        let mut envelope = FadeEnvelope::new(10, 10, 8000, Some(1000));

        // At start, gain should be 0
        assert_eq!(envelope.compute_gain(), 0.0);

        // After 40 samples (halfway through fade-in), gain should be ~0.5
        envelope.position = 40;
        assert!((envelope.compute_gain() - 0.5).abs() < 0.01);

        // At 500 samples (middle of audio), gain should be 1.0
        envelope.position = 500;
        assert_eq!(envelope.compute_gain(), 1.0);

        // At 960 samples (start of fade-out: 1000 - 80 + 40), gain should be ~0.5
        envelope.position = 960;
        assert!((envelope.compute_gain() - 0.5).abs() < 0.01);
    }
}
