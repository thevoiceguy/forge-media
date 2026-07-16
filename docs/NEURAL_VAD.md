# Neural VAD (Silero) backend

`forge-vad` ships two Voice Activity Detection backends behind one surface:

| Backend | Selected by | Cost | Strengths |
|---|---|---|---|
| `energy_zcr` (default) | `VadEngineConfig::EnergyZcr(VadConfig)` | ~200 ns per 20 ms frame, zero deps | Cheap; adaptive noise floor |
| `neural` | `VadEngineConfig::Neural(NeuralVadConfig)` | ~60–80 µs per 32 ms model window (see benchmarks) | Rejects the acoustic false-positive class: coughs, keyboard clatter, music-on-hold, echo |

Both emit the same `VadState` transitions, and the engine publishes the same
`ForgeEvent::SpeechStarted` / `SpeechStopped { duration_ms }` — consumers
(siphon-ai, forge-ai-stream) see an identical event contract.

## Feature-flag matrix

| Crate | Feature | Effect |
|---|---|---|
| `forge-vad` | `neural` | Compiles the tract-onnx runtime + embeds the two per-rate model files (~2.8 MB) |
| `forge-engine` | `neural-vad` | Forwards to `forge-vad/neural` |
| `forge-media` (root) | `neural-vad` | Forwards to both |

Everything is **off by default**: default builds carry no ML runtime, no
model bytes, and behave byte-identically to pre-neural releases.
`VadEngineConfig::Neural(..)` still parses in a non-neural build, but
`build()` fails loudly with `VadError::InvalidConfig` (a session-setup
error, not a per-frame one).

**Toolchain**: tract 0.23 declares `rust-version = 1.91`, so builds with the
feature enabled need rustc ≥ 1.91. The workspace MSRV (1.75) still holds for
default builds. Static-musl builds work (tract is pure Rust); tract-linalg's
build script needs a C toolchain to assemble its SIMD kernels — plain host
`gcc` (`CC_x86_64_unknown_linux_musl=gcc`) or `cargo zigbuild` both work.

## Model

Silero VAD **v6.2.1** (MIT), embedded as two per-sample-rate ONNX graphs
specialized from the upstream `silero_vad_op18_ifless.onnx` export — same
weights, control flow removed so the pure-Rust tract runtime can optimize
the whole graph. Provenance, SHA-256s, and the regeneration script live in
`crates/forge-vad/models/README.md`.

- 16 kHz: 512-sample (32 ms) windows, 64 samples of carried context
- 8 kHz: 256-sample (32 ms) windows, 32 samples of carried context

These are exactly forge's bridge PCM rates (G.711/G.729 → 8 kHz,
G.722/Opus-bridge → 16 kHz). There is **no resampling** in v1; any other
rate is rejected at build (and the engine's rate guard disables VAD for the
session if the decoded rate can't be matched — see below).

## Selection & tuning

```rust
use forge_engine::session::{MediaSessionConfig, VadConfig};
use forge_vad::{NeuralVadConfig, VadEngineConfig};

let config = MediaSessionConfig {
    vad_config: VadConfig {
        enabled: true,
        engine: VadEngineConfig::Neural(NeuralVadConfig {
            sample_rate: 8000, // pass the negotiated bridge rate
            ..NeuralVadConfig::default()
        }),
    },
    ..MediaSessionConfig::default()
};
```

`NeuralVadConfig` knobs:

| Knob | Default | Meaning |
|---|---|---|
| `sample_rate` | 16000 | 8000 or 16000; selects the model. Load-bearing (unlike the energy backend's documentation-only field) |
| `speech_prob_threshold` | 0.5 | Window probability ≥ this counts toward entering `Speech` |
| `silence_prob_threshold` | 0.35 | Window probability < this counts toward entering `Silence`; probabilities in between hold the current state (anti-flap dual threshold) |
| `min_speech_duration_ms` | 100 | Hysteresis to enter `Speech`, rounded up to whole 32 ms windows |
| `min_silence_duration_ms` | 500 | Hysteresis to enter `Silence` |
| `model_path` | `None` | Load a per-rate model file from disk instead of the embedded bytes (ops override; model refresh without rebuild) |

Decision latency is one model window (32 ms) plus the hysteresis you
configure — against the default 100 ms `min_speech_duration_ms` the model
adds nothing perceptible. The confidence value returned with each state is
the raw model probability of the last scored window.

## Engine behavior

- The forwarding loop feeds every decoded packet's PCM to the session
  detector along with the frame's true PCM rate
  (`codec_audio_sample_rate`). If a neural detector's configured rate
  differs from the stream's (config default vs. negotiated codec, or a
  mid-call re-INVITE codec switch), the detector is **rebuilt at the stream
  rate** — one `warn!`, detection restarts (~4 ms rebuild). If the stream
  rate has no model (e.g. 48 kHz), VAD is disabled for that session with
  one `warn!` instead of erroring per frame.
- A `VadEngineConfig` that can't build (unsupported rate, unreadable
  `model_path`, neural without the feature) **fails session setup** with
  `ForgeError::InvalidConfig`.
- At neural detector build the engine logs one `info!` with backend, model
  version, and sample rate.

### Metrics

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `forge_vad_windows_total` | counter | `backend` | Model windows scored (neural); frames processed (energy) |
| `forge_vad_neural_inference_seconds` | histogram | — | Wall time of each `process()` call that scored ≥1 window (usually exactly 1) |
| `forge_vad_errors_total` | counter | `backend` | Inference failures (non-fatal; frame skipped) |

## Benchmarks (dev box, 2026-07-16, `cargo bench --bench vad_bench --features neural-vad`)

| Bench | Result |
|---|---|
| `energy_zcr/process_20ms_frame` | ~201 ns |
| `neural/score_one_window_8000hz` | ~60 µs |
| `neural/score_one_window_16000hz` | ~81 µs |
| `neural/detector_build_16khz` | ~4 ms |
| Spike p99 (glibc / musl-static, 16 kHz) | 180 µs / 530 µs |

Budget from NEURAL_VAD_PLAN.md decision 10 is ≤ 1.5 ms p99 per window —
met with ~8–20× headroom. At ~31 windows/s per call the neural backend
costs roughly 0.2–0.3 % of one core per call.

## Testing

- `cargo test -p forge-vad` — state-machine tests (scripted scorer, no
  model needed).
- `cargo test -p forge-vad --features neural` — adds real-model
  integration tests over the committed public-domain speech fixture
  (`tests/fixtures/`, JFK 1961 inaugural excerpt) and synthetic tone/noise
  negatives.
- `cargo test -p forge-engine --features neural-vad` — engine-level event
  emission, rate-guard rebuild/disable, and build-failure tests.
