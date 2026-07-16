//! Backend selection: [`VadEngineConfig`] describes which detector to
//! run, [`AnyVadDetector`] is the enum-dispatch detector the engine
//! holds per session.
//!
//! Enum over `dyn Trait` on purpose: keeps `Debug`, no vtable in the
//! per-packet hot path, exhaustive matches, and the engine's
//! `Arc<Mutex<…>>` slot changes type exactly once.

use crate::neural::{NeuralVadConfig, NeuralVadDetector};
use crate::{Result, VadConfig, VadDetector, VadState};

/// Which VAD backend a session runs, plus its tuning.
///
/// `build()` is where fail-loud validation lives: an unsupported
/// sample rate, an unreadable model file, or a `Neural` config in a
/// build without the `neural` feature all fail detector construction
/// (a config error at session setup) rather than erroring per frame.
#[derive(Debug, Clone)]
pub enum VadEngineConfig {
    /// The zero-dependency energy + zero-crossing-rate detector.
    /// This is the default; existing deployments are unchanged.
    EnergyZcr(VadConfig),
    /// The Silero neural detector. Requires the `neural` Cargo
    /// feature (`neural-vad` on forge-engine); selecting it in a
    /// build without that feature is a `build()` error.
    Neural(NeuralVadConfig),
}

impl Default for VadEngineConfig {
    fn default() -> Self {
        Self::EnergyZcr(VadConfig::default())
    }
}

impl VadEngineConfig {
    /// Construct the configured detector.
    pub fn build(&self) -> Result<AnyVadDetector> {
        match self {
            Self::EnergyZcr(config) => {
                Ok(AnyVadDetector::EnergyZcr(VadDetector::new(config.clone())))
            }
            #[cfg(feature = "neural")]
            Self::Neural(config) => Ok(AnyVadDetector::Neural(NeuralVadDetector::new(
                config.clone(),
            )?)),
            #[cfg(not(feature = "neural"))]
            Self::Neural(_) => Err(crate::VadError::InvalidConfig(
                "VadEngineConfig::Neural requires forge-vad built with the `neural` feature \
                 (forge-engine feature `neural-vad`)"
                    .into(),
            )),
        }
    }

    /// Return the config with its sample rate replaced. The engine
    /// uses this to rebuild a detector when the negotiated codec's
    /// actual PCM rate differs from the configured one.
    pub fn with_sample_rate(mut self, sample_rate: u32) -> Self {
        match &mut self {
            Self::EnergyZcr(c) => c.sample_rate = sample_rate,
            Self::Neural(c) => c.sample_rate = sample_rate,
        }
        self
    }
}

/// A constructed VAD detector of either backend, with the common
/// `process`/`state`/`reset` surface.
pub enum AnyVadDetector {
    EnergyZcr(VadDetector),
    Neural(NeuralVadDetector),
}

impl AnyVadDetector {
    /// Process one audio frame; see the backend's own `process` for
    /// the confidence semantics (energy margin vs. raw model
    /// probability).
    pub fn process(&mut self, audio: &[i16]) -> Result<(VadState, f32)> {
        match self {
            Self::EnergyZcr(d) => d.process(audio),
            Self::Neural(d) => d.process(audio),
        }
    }

    /// Current VAD state.
    pub fn state(&self) -> VadState {
        match self {
            Self::EnergyZcr(d) => d.state(),
            Self::Neural(d) => d.state(),
        }
    }

    /// Reset all detection state (hysteresis, buffers, model state).
    pub fn reset(&mut self) {
        match self {
            Self::EnergyZcr(d) => d.reset(),
            Self::Neural(d) => d.reset(),
        }
    }

    /// The sample rate this detector requires its input to be at, or
    /// `None` if it is rate-agnostic (the energy detector operates on
    /// raw samples and only documents its rate). The engine's
    /// forwarding loop compares this against the decoded stream's
    /// actual rate and rebuilds the detector on mismatch.
    pub fn required_sample_rate(&self) -> Option<u32> {
        match self {
            Self::EnergyZcr(_) => None,
            Self::Neural(d) => Some(d.sample_rate()),
        }
    }

    /// Stable backend label for logs and metrics.
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::EnergyZcr(_) => "energy_zcr",
            Self::Neural(_) => "neural",
        }
    }

    /// Total model windows scored (neural backend only; `None` for
    /// backends without a window notion). The engine turns deltas of
    /// this into its windows counter.
    pub fn windows_processed(&self) -> Option<u64> {
        match self {
            Self::EnergyZcr(_) => None,
            Self::Neural(d) => Some(d.windows_processed()),
        }
    }
}

impl std::fmt::Debug for AnyVadDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnergyZcr(_) => f
                .debug_struct("AnyVadDetector::EnergyZcr")
                .field("state", &self.state())
                .finish_non_exhaustive(),
            Self::Neural(d) => d.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_builds_the_energy_backend() {
        let detector = VadEngineConfig::default().build().unwrap();
        assert_eq!(detector.backend_name(), "energy_zcr");
        assert_eq!(detector.state(), VadState::Unknown);
        assert_eq!(detector.required_sample_rate(), None);
        assert_eq!(detector.windows_processed(), None);
    }

    #[test]
    fn with_sample_rate_overrides_either_backend() {
        let config = VadEngineConfig::default().with_sample_rate(8000);
        match config {
            VadEngineConfig::EnergyZcr(c) => assert_eq!(c.sample_rate, 8000),
            VadEngineConfig::Neural(_) => unreachable!(),
        }
        let config =
            VadEngineConfig::Neural(NeuralVadConfig::default()).with_sample_rate(8000);
        match config {
            VadEngineConfig::Neural(c) => assert_eq!(c.sample_rate, 8000),
            VadEngineConfig::EnergyZcr(_) => unreachable!(),
        }
    }

    #[cfg(not(feature = "neural"))]
    #[test]
    fn neural_config_fails_loudly_without_the_feature() {
        let err = VadEngineConfig::Neural(NeuralVadConfig::default())
            .build()
            .unwrap_err();
        assert!(err.to_string().contains("`neural` feature"), "{err}");
    }

    #[cfg(feature = "neural")]
    #[test]
    fn neural_config_validates_sample_rate_at_build() {
        let err = VadEngineConfig::Neural(NeuralVadConfig {
            sample_rate: 44100,
            ..NeuralVadConfig::default()
        })
        .build()
        .unwrap_err();
        assert!(err.to_string().contains("sample rates"), "{err}");
    }
}
