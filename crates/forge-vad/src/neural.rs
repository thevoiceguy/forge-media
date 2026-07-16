//! Neural (Silero) VAD backend.
//!
//! [`NeuralVadDetector`] presents the same surface as
//! [`VadDetector`](crate::VadDetector) — `process(&[i16]) ->
//! Result<(VadState, f32)>`, `state()`, `reset()` — but scores audio
//! with the Silero VAD neural network instead of energy + ZCR
//! heuristics. It accepts arbitrary frame sizes (the engine feeds
//! 20 ms RTP frames), slices them into the model's native 32 ms
//! window internally, and carries the model's recurrent state across
//! windows.
//!
//! The probability→state hysteresis mirrors the energy detector's
//! semantics (`min_speech_duration_ms` / `min_silence_duration_ms`,
//! counted in whole model windows) with dual entry/exit thresholds so
//! the state doesn't flap when the model hovers around 0.5.
//!
//! Everything except the actual tract inference compiles without the
//! `neural` Cargo feature; the model, the tract runtime, and
//! [`NeuralVadDetector::new`] are gated behind it. See
//! `models/README.md` for model provenance (Silero VAD v6.2.1, MIT).

use crate::{Result, VadError, VadState};
use std::path::PathBuf;

/// Silero model version embedded when the `neural` feature is on.
/// Kept here (not just in docs) so the engine can log it at detector
/// build time.
pub const MODEL_VERSION: &str = "silero-vad v6.2.1 (per-rate specialization)";

/// Configuration for the neural VAD backend.
#[derive(Debug, Clone)]
pub struct NeuralVadConfig {
    /// Sample rate of the PCM fed to `process`. Must be 8000 or
    /// 16000 — the Silero model is rate-specific and anything else is
    /// a [`VadEngineConfig::build`](crate::VadEngineConfig::build)
    /// error. Unlike the energy detector's documentation-only field,
    /// this one selects the model and is load-bearing.
    pub sample_rate: u32,

    /// Window speech-probability at or above which a window counts
    /// toward entering `Speech`.
    pub speech_prob_threshold: f32,

    /// Window speech-probability below which a window counts toward
    /// entering `Silence`. Probabilities in
    /// `[silence_prob_threshold, speech_prob_threshold)` are
    /// ambiguous: they hold the current state and reset both
    /// hysteresis counters.
    pub silence_prob_threshold: f32,

    /// Minimum speech duration before committing to `Speech`,
    /// counted in whole 32 ms model windows (rounded up).
    pub min_speech_duration_ms: u32,

    /// Minimum silence duration before committing to `Silence`,
    /// counted in whole 32 ms model windows (rounded up).
    pub min_silence_duration_ms: u32,

    /// Load the ONNX model from this path instead of the embedded
    /// bytes. `None` (the default) uses the model compiled into the
    /// crate. The file must be a per-rate specialization with the
    /// same interface as the embedded ones (see `models/README.md`).
    pub model_path: Option<PathBuf>,
}

impl Default for NeuralVadConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            speech_prob_threshold: 0.5,
            silence_prob_threshold: 0.35,
            min_speech_duration_ms: 100,
            min_silence_duration_ms: 500,
            model_path: None,
        }
    }
}

impl NeuralVadConfig {
    /// Samples per model window at the configured rate (32 ms).
    pub fn window_samples(&self) -> usize {
        match self.sample_rate {
            8000 => 256,
            _ => 512,
        }
    }

    /// Context samples the model expects prepended to each window.
    #[cfg_attr(not(feature = "neural"), allow(dead_code))]
    fn context_samples(&self) -> usize {
        match self.sample_rate {
            8000 => 32,
            _ => 64,
        }
    }

    #[cfg_attr(not(feature = "neural"), allow(dead_code))]
    pub(crate) fn validate(&self) -> Result<()> {
        if self.sample_rate != 8000 && self.sample_rate != 16000 {
            return Err(VadError::InvalidConfig(format!(
                "neural VAD supports sample rates 8000 and 16000, got {}",
                self.sample_rate
            )));
        }
        for (name, v) in [
            ("speech_prob_threshold", self.speech_prob_threshold),
            ("silence_prob_threshold", self.silence_prob_threshold),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(VadError::InvalidConfig(format!(
                    "{name} must be within 0.0..=1.0, got {v}"
                )));
            }
        }
        if self.silence_prob_threshold > self.speech_prob_threshold {
            return Err(VadError::InvalidConfig(format!(
                "silence_prob_threshold ({}) must not exceed speech_prob_threshold ({})",
                self.silence_prob_threshold, self.speech_prob_threshold
            )));
        }
        Ok(())
    }
}

/// Source of per-window speech probabilities. Split out from the
/// windowing/hysteresis state machine so the latter is unit-testable
/// with a scripted scorer; the tract-backed implementation is the
/// only part that needs the real model.
pub(crate) trait Scorer: Send {
    /// Score one model-native window (`window_samples` i16 samples)
    /// and return the speech probability (0.0–1.0).
    fn score_window(&mut self, window: &[i16]) -> Result<f32>;

    /// Drop recurrent state and audio context.
    fn reset(&mut self);
}

/// Neural VAD detector: windowing + hysteresis around a [`Scorer`].
pub struct NeuralVadDetector {
    config: NeuralVadConfig,
    scorer: Box<dyn Scorer>,
    window: usize,
    buffer: Vec<i16>,
    min_speech_windows: u32,
    min_silence_windows: u32,
    speech_windows: u32,
    silence_windows: u32,
    state: VadState,
    last_probability: f32,
    windows_processed: u64,
}

impl std::fmt::Debug for NeuralVadDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralVadDetector")
            .field("sample_rate", &self.config.sample_rate)
            .field("state", &self.state)
            .field("last_probability", &self.last_probability)
            .field("windows_processed", &self.windows_processed)
            .finish_non_exhaustive()
    }
}

impl NeuralVadDetector {
    /// Create a detector backed by the real Silero model (embedded
    /// bytes, or `config.model_path` if set). Fails loudly on an
    /// unsupported sample rate, an unreadable model file, or a model
    /// tract cannot load.
    #[cfg(feature = "neural")]
    pub fn new(config: NeuralVadConfig) -> Result<Self> {
        config.validate()?;
        let scorer = match &config.model_path {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| {
                    VadError::InvalidConfig(format!(
                        "cannot read neural VAD model {}: {e}",
                        path.display()
                    ))
                })?;
                tract_scorer::TractScorer::from_bytes(&bytes, &config)?
            }
            None => tract_scorer::TractScorer::embedded(&config)?,
        };
        Ok(Self::with_scorer(config, Box::new(scorer)))
    }

    /// Assemble a detector around an arbitrary scorer. Used by the
    /// state-machine tests (scripted probabilities, no model); `new`
    /// is the production path. The config must already be validated.
    #[cfg_attr(not(any(feature = "neural", test)), allow(dead_code))]
    pub(crate) fn with_scorer(config: NeuralVadConfig, scorer: Box<dyn Scorer>) -> Self {
        let window = config.window_samples();
        let window_ms = 1000 * window as u32 / config.sample_rate;
        Self {
            window,
            min_speech_windows: config.min_speech_duration_ms.div_ceil(window_ms).max(1),
            min_silence_windows: config.min_silence_duration_ms.div_ceil(window_ms).max(1),
            scorer,
            buffer: Vec::with_capacity(window * 2),
            speech_windows: 0,
            silence_windows: 0,
            state: VadState::Unknown,
            last_probability: 0.0,
            windows_processed: 0,
            config,
        }
    }

    /// Process one audio frame (any length) and return the resulting
    /// `(state, confidence)` pair. Confidence is the raw model
    /// probability of the most recently scored window (0.0 until the
    /// first full window has flowed through).
    pub fn process(&mut self, audio: &[i16]) -> Result<(VadState, f32)> {
        self.buffer.extend_from_slice(audio);
        let mut consumed = 0;
        while self.buffer.len() - consumed >= self.window {
            let prob = self
                .scorer
                .score_window(&self.buffer[consumed..consumed + self.window])?;
            consumed += self.window;
            self.windows_processed += 1;
            self.last_probability = prob;
            self.update_state(prob);
        }
        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        Ok((self.state, self.last_probability))
    }

    fn update_state(&mut self, prob: f32) {
        if prob >= self.config.speech_prob_threshold {
            self.speech_windows += 1;
            self.silence_windows = 0;
            if self.speech_windows >= self.min_speech_windows {
                self.state = VadState::Speech;
            }
        } else if prob < self.config.silence_prob_threshold {
            self.silence_windows += 1;
            self.speech_windows = 0;
            if self.silence_windows >= self.min_silence_windows {
                self.state = VadState::Silence;
            }
        } else {
            // Ambiguous zone: hold the current state, restart both
            // hysteresis streaks.
            self.speech_windows = 0;
            self.silence_windows = 0;
        }
    }

    /// Current VAD state.
    pub fn state(&self) -> VadState {
        self.state
    }

    /// Configured sample rate (the model is specific to it).
    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Speech probability of the most recently scored window.
    pub fn last_probability(&self) -> f32 {
        self.last_probability
    }

    /// Total model windows scored since construction (monotonic;
    /// `reset` does not clear it). The engine turns deltas of this
    /// into its `forge_vad_windows_total` counter.
    pub fn windows_processed(&self) -> u64 {
        self.windows_processed
    }

    /// Reset state: hysteresis, buffered samples, and the model's
    /// recurrent state. Same contract as
    /// [`VadDetector::reset`](crate::VadDetector::reset).
    pub fn reset(&mut self) {
        self.state = VadState::Unknown;
        self.speech_windows = 0;
        self.silence_windows = 0;
        self.buffer.clear();
        self.last_probability = 0.0;
        self.scorer.reset();
    }
}

#[cfg(feature = "neural")]
mod tract_scorer {
    use super::{NeuralVadConfig, Result, Scorer, VadError};
    use std::sync::Arc;
    use tract_onnx::prelude::*;

    static SILERO_V6_16K: &[u8] = include_bytes!("../models/silero_vad_v6_16k.onnx");
    static SILERO_V6_8K: &[u8] = include_bytes!("../models/silero_vad_v6_8k.onnx");

    /// Silero VAD inference via tract. Holds the optimized plan, the
    /// recurrent state tensor, and the inter-window audio context the
    /// model expects prepended to each window.
    pub(crate) struct TractScorer {
        plan: Arc<TypedRunnableModel>,
        state: Tensor,
        context: Vec<f32>,
        input: Vec<f32>,
        window: usize,
        context_len: usize,
    }

    impl TractScorer {
        pub(crate) fn embedded(config: &NeuralVadConfig) -> Result<Self> {
            let bytes = match config.sample_rate {
                8000 => SILERO_V6_8K,
                _ => SILERO_V6_16K,
            };
            Self::from_bytes(bytes, config)
        }

        pub(crate) fn from_bytes(bytes: &[u8], config: &NeuralVadConfig) -> Result<Self> {
            let window = config.window_samples();
            let context_len = config.context_samples();
            let input_len = window + context_len;
            let load = || -> TractResult<Arc<TypedRunnableModel>> {
                let mut model = tract_onnx::onnx().model_for_read(&mut &bytes[..])?;
                model.set_input_fact(0, f32::fact([1, input_len]).into())?;
                model.set_input_fact(1, f32::fact([2, 1, 128]).into())?;
                model.into_optimized()?.into_runnable()
            };
            let plan = load().map_err(|e| {
                VadError::Backend(format!("failed to load Silero VAD model: {e:?}"))
            })?;
            Ok(Self {
                plan,
                state: Tensor::zero::<f32>(&[2, 1, 128])
                    .expect("zero tensor allocation cannot fail"),
                context: vec![0.0; context_len],
                input: vec![0.0; input_len],
                window,
                context_len,
            })
        }
    }

    impl Scorer for TractScorer {
        fn score_window(&mut self, window: &[i16]) -> Result<f32> {
            debug_assert_eq!(window.len(), self.window);
            self.input[..self.context_len].copy_from_slice(&self.context);
            for (dst, &s) in self.input[self.context_len..].iter_mut().zip(window) {
                *dst = f32::from(s) / 32768.0;
            }
            // The model wants the last `context_len` samples of this
            // window prepended to the next one.
            for (dst, &s) in self
                .context
                .iter_mut()
                .zip(&window[self.window - self.context_len..])
            {
                *dst = f32::from(s) / 32768.0;
            }

            let mut run = || -> TractResult<f32> {
                let input = Tensor::from_shape(&[1, self.input.len()], &self.input)?;
                let mut outputs = self
                    .plan
                    .run(tvec!(input.into(), self.state.clone().into()))?;
                let prob = outputs[0].to_plain_array_view::<f32>()?[[0, 0]];
                self.state = outputs.remove(1).into_tensor();
                Ok(prob)
            };
            run().map_err(|e| VadError::Backend(format!("Silero VAD inference failed: {e:?}")))
        }

        fn reset(&mut self) {
            self.state =
                Tensor::zero::<f32>(&[2, 1, 128]).expect("zero tensor allocation cannot fail");
            self.context.iter_mut().for_each(|s| *s = 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scorer that replays a scripted probability sequence and counts
    /// calls, so hysteresis and windowing are testable without the
    /// model.
    struct Scripted {
        probs: Vec<f32>,
        cursor: usize,
        resets: usize,
    }

    impl Scripted {
        fn new(probs: Vec<f32>) -> Self {
            Self {
                probs,
                cursor: 0,
                resets: 0,
            }
        }
    }

    impl Scorer for Scripted {
        fn score_window(&mut self, window: &[i16]) -> Result<f32> {
            assert_eq!(window.len(), 512, "scorer must see model-native windows");
            let p = self.probs[self.cursor.min(self.probs.len() - 1)];
            self.cursor += 1;
            Ok(p)
        }

        fn reset(&mut self) {
            self.resets += 1;
        }
    }

    fn detector(probs: Vec<f32>) -> NeuralVadDetector {
        // 100 ms / 32 ms → 4 windows to enter Speech; 128 ms → 4 to exit.
        NeuralVadDetector::with_scorer(
            NeuralVadConfig {
                min_speech_duration_ms: 100,
                min_silence_duration_ms: 128,
                ..NeuralVadConfig::default()
            },
            Box::new(Scripted::new(probs)),
        )
    }

    fn window() -> Vec<i16> {
        vec![0i16; 512]
    }

    #[test]
    fn starts_unknown_and_commits_to_speech_after_min_duration() {
        let mut d = detector(vec![0.9]);
        for _ in 0..3 {
            let (state, _) = d.process(&window()).unwrap();
            assert_eq!(state, VadState::Unknown);
        }
        let (state, prob) = d.process(&window()).unwrap();
        assert_eq!(state, VadState::Speech);
        assert_eq!(prob, 0.9);
    }

    #[test]
    fn exits_to_silence_after_min_silence_duration() {
        let probs = [[0.9; 4], [0.1; 4]].concat();
        let mut d = detector(probs);
        for _ in 0..4 {
            d.process(&window()).unwrap();
        }
        assert_eq!(d.state(), VadState::Speech);
        for _ in 0..3 {
            assert_eq!(d.process(&window()).unwrap().0, VadState::Speech);
        }
        assert_eq!(d.process(&window()).unwrap().0, VadState::Silence);
    }

    #[test]
    fn ambiguous_probabilities_hold_state_and_restart_streaks() {
        // 3 speechy windows, then an ambiguous one (between the two
        // thresholds), then 3 more speechy: the ambiguous window must
        // have restarted the streak, so Speech is NOT reached at
        // window 7 — only at window 8.
        let probs = vec![0.9, 0.9, 0.9, 0.4, 0.9, 0.9, 0.9, 0.9];
        let mut d = detector(probs);
        for _ in 0..7 {
            assert_eq!(d.process(&window()).unwrap().0, VadState::Unknown);
        }
        assert_eq!(d.process(&window()).unwrap().0, VadState::Speech);
    }

    #[test]
    fn buffers_sub_window_frames_until_a_window_fills() {
        // 20 ms @ 16 kHz = 320 samples; the 512-sample window fills
        // mid-second-frame.
        let mut d = detector(vec![0.9]);
        let frame = vec![0i16; 320];
        let (_, prob) = d.process(&frame).unwrap();
        assert_eq!(prob, 0.0, "no window scored yet");
        assert_eq!(d.windows_processed(), 0);
        let (_, prob) = d.process(&frame).unwrap();
        assert_eq!(prob, 0.9);
        assert_eq!(d.windows_processed(), 1);
    }

    #[test]
    fn oversized_frame_scores_multiple_windows() {
        let mut d = detector(vec![0.9]);
        d.process(&vec![0i16; 512 * 4]).unwrap();
        assert_eq!(d.windows_processed(), 4);
        assert_eq!(d.state(), VadState::Speech);
    }

    #[test]
    fn empty_frame_is_a_no_op() {
        let mut d = detector(vec![0.9]);
        let (state, prob) = d.process(&[]).unwrap();
        assert_eq!(state, VadState::Unknown);
        assert_eq!(prob, 0.0);
        assert_eq!(d.windows_processed(), 0);
    }

    #[test]
    fn reset_clears_state_buffer_and_scorer() {
        let mut d = detector(vec![0.9]);
        d.process(&vec![0i16; 512 * 4 + 100]).unwrap();
        assert_eq!(d.state(), VadState::Speech);
        d.reset();
        assert_eq!(d.state(), VadState::Unknown);
        assert_eq!(d.last_probability(), 0.0);
        // Buffered remainder was dropped: a fresh 412 samples must
        // not complete a window.
        d.process(&vec![0i16; 412]).unwrap();
        assert_eq!(d.windows_processed(), 4, "no new window after reset");
    }

    #[test]
    fn validate_rejects_bad_configs() {
        for (mutate, expect) in [
            (
                Box::new(|c: &mut NeuralVadConfig| c.sample_rate = 44100)
                    as Box<dyn Fn(&mut NeuralVadConfig)>,
                "sample rates",
            ),
            (Box::new(|c| c.speech_prob_threshold = 1.5), "0.0..=1.0"),
            (
                Box::new(|c| {
                    c.silence_prob_threshold = 0.8;
                    c.speech_prob_threshold = 0.5;
                }),
                "must not exceed",
            ),
        ] {
            let mut config = NeuralVadConfig::default();
            mutate(&mut config);
            let err = config.validate().unwrap_err().to_string();
            assert!(err.contains(expect), "{err:?} should contain {expect:?}");
        }
    }

    #[test]
    fn min_durations_round_up_to_whole_windows() {
        // 1 ms → 1 window minimum, 33 ms → 2 windows (32 ms windows).
        let d = NeuralVadDetector::with_scorer(
            NeuralVadConfig {
                min_speech_duration_ms: 1,
                min_silence_duration_ms: 33,
                ..NeuralVadConfig::default()
            },
            Box::new(Scripted::new(vec![0.0])),
        );
        assert_eq!(d.min_speech_windows, 1);
        assert_eq!(d.min_silence_windows, 2);
    }

    #[test]
    fn eight_khz_uses_256_sample_windows() {
        struct Expect256;
        impl Scorer for Expect256 {
            fn score_window(&mut self, window: &[i16]) -> Result<f32> {
                assert_eq!(window.len(), 256);
                Ok(0.9)
            }
            fn reset(&mut self) {}
        }
        let mut d = NeuralVadDetector::with_scorer(
            NeuralVadConfig {
                sample_rate: 8000,
                ..NeuralVadConfig::default()
            },
            Box::new(Expect256),
        );
        // 20 ms @ 8 kHz = 160 samples; two frames fill one window.
        d.process(&vec![0i16; 160]).unwrap();
        assert_eq!(d.windows_processed(), 0);
        d.process(&vec![0i16; 160]).unwrap();
        assert_eq!(d.windows_processed(), 1);
    }
}
