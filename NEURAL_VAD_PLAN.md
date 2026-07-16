# Neural VAD for forge-vad — Implementation Plan

Status: **IMPLEMENTED (Chunks 0–2, 2026-07-16)** — forge-media side complete;
Phase 2 (siphon-ai plumbing, §5.4) remains. Spike results below.

## Chunk 0 spike results (2026-07-16, dev box, tract-onnx 0.23.4)

- **Model**: Silero VAD **v6.2.1**, via the upstream `silero_vad_op18_ifless.onnx`
  export (SHA-256 `7671cd04…6bbd28`, MIT). The plain v5/v4/16k-op15 exports all
  **fail** under tract: their `If` nodes have branches whose output ranks differ,
  which tract's type inference rejects. The ifless export has exactly one
  top-level `If` switching on `sr` between two self-contained per-rate subgraphs
  (43 plain nodes each: Conv/Gemm/Split/Sigmoid/Tanh/Pad/Slice — no inner Ifs).
  We **specialize** that graph offline (committed `specialize.py`): extract each
  branch into a standalone per-rate model — same weights, no control flow, no
  `sr` input. Parity vs. the original under onnxruntime is **bit-exact** (max
  |Δprob| = 0.0 over the full fixture, both rates).
  - `silero_vad_v6_16k.onnx` 1,556,692 B, SHA-256 `871f0828…0337`, input
    `[1,576]` (512-sample window + 64 context), state `[2,1,128]`
  - `silero_vad_v6_8k.onnx` 1,261,615 B, SHA-256 `03fb3a9d…c242`, input
    `[1,288]` (256-sample window + 32 context), state `[2,1,128]`
- **Op support (kill a)**: PASS — tract builds a fully *optimized* plan for both
  specialized models.
- **Quality (kill b)**: PASS — JFK speech fixture: mean prob 0.69 / max 1.00 /
  68% windows > 0.5; silence, 440 Hz tone, white noise all ≤ 0.03 max, at both
  rates.
- **Latency (kill c)**: PASS — glibc: p50 95 µs / p99 180 µs per window @16 k
  (68/84 µs @8 k). musl static: p50 180 µs / p99 530 µs. Budget is 1500 µs —
  ~3–8× headroom.
- **musl static (kill d)**: PASS — `--target x86_64-unknown-linux-musl` builds
  (static-pie) and runs with identical probabilities. tract-linalg's build
  script needs a C toolchain for its assembly kernels; plain host `gcc` works
  (`CC_x86_64_unknown_linux_musl=gcc`), as does zig cc. **Not** a C++ runtime
  dep — kernels only.
- **Caveat**: tract 0.23.4 declares `rust-version = 1.91`; workspace MSRV is
  1.75. Builds **with** `neural` enabled need rustc ≥ 1.91. Default builds are
  unaffected (tract is an optional dep, feature off by default).
Consumer driving this: **siphon-ai** (ROADMAP P2, *upstream-gated* item "Neural
VAD upgrade in forge-vad"). Written 2026-07-16.

---

## 1. Why

siphon-ai's barge-in pipeline is driven by `ForgeEvent::SpeechStarted` from
forge-vad's energy + zero-crossing detector. On real calls the acoustic
false-positive class — coughs, keyboard clatter, music-on-hold bleed, the
bot's own echo — fires enough false `SpeechStarted` events that siphon-ai had
to grow two mitigations: a playout-gated debounce (`debounce_ms`, siphon-ai
#173) and reversible barge-in (`mode = "pause"` + server arbitration,
siphon-ai v0.32.0). Both work, but they arm *after* a false positive; a
Silero-class neural model (~1–2 MB ONNX, ~1 ms per inference on a modern
core) cuts the false-positive class before pause-mode arbitration even arms.

This is **complementary** to the semantic layer (the WS server's
confirm/reject), not a replacement. The goal is a materially better
`SpeechStarted` signal with the same event contract.

## 2. Current state (anchors)

- `crates/forge-vad/src/lib.rs` (466 lines, zero deps beyond `thiserror`):
  `VadConfig` (sensitivity, `min_speech_duration_ms`, `min_silence_duration_ms`,
  `sample_rate` — *documentation-only today*, `frame_size_ms`,
  `energy_threshold` 0.0 = adaptive, `zcr_threshold`) and `VadDetector` with
  `process(&[i16]) -> Result<(VadState, f32)>`, `state()`, `reset()`.
  `VadState = Speech | Silence | Unknown`, hysteresis via frame counters.
- `crates/forge-engine/src/session.rs:37` — engine-level `VadConfig
  { enabled: bool, detector: forge_vad::VadConfig }`, default enabled.
  Sessions hold `Arc<Mutex<forge_vad::VadDetector>>` (constructed at
  session.rs:812, :970, :1172, :2698; accessor `vad_detector()` at :1285).
- `crates/forge-engine/src/forwarding.rs:452–503` — per-decoded-packet loop:
  lock detector → `process(&pcm_samples)` → on state flip publish
  `ForgeEvent::SpeechStarted` / `SpeechStopped { duration_ms }` on the
  EventBus. The true PCM rate is already computed a few lines above via
  `MediaSession::codec_audio_sample_rate(sender_codec.codec,
  sender_codec.clock_rate)` (forwarding.rs:434) — the media-bridge frame uses
  it; the VAD block currently does not.
- Consumers: siphon-ai (`media-glue` tap: barge-in, silence/dead-air timers)
  and `forge-ai-stream` adapters. **siphon-ai constructs no VadConfig today —
  it runs engine defaults.** That means `detector.sample_rate` is the default
  16000 even on 8 kHz G.711 calls; harmless for energy+ZCR (which ignores it),
  load-bearing for a neural model. Fix in this work (§5.4).
- Workspace: MSRV 1.75, no ML/ONNX deps anywhere in the tree today.
  `benches/` exists (criterion-style: `srtp_bench.rs`, `transcoding_bench.rs`).

## 3. Scope

**In:** a neural VAD backend inside `forge-vad`, selectable per session via
engine config, same `VadState` + event contract, off by default.

**Out (explicitly):**
- "Duck" barge-in reaction / per-leg playout-gain API — separate ROADMAP item.
- AMD (`forge-amd`) — separate item.
- Echo cancellation (libwebrtc APM) — separate fallback track.
- Cloud/remote VAD of any kind — this must be local inference only.
- Changing the `SpeechStarted`/`SpeechStopped` event shapes — the contract
  siphon-ai and forge-ai-stream consume is frozen.

## 4. Locked decisions (recommendations — confirm or override at kickoff)

| # | Decision | Recommendation | Why |
|---|---|---|---|
| 1 | Inference runtime | **`tract-onnx`** (pure Rust, Sonos) | The hard constraint is downstream: siphon-ai ships **static musl** multi-arch binaries (its release pipeline already fights libopus for this). `ort`/onnxruntime is a C++ runtime that is painful-to-impossible under static musl and would poison every consumer's build. tract is pure Rust, MIT/Apache, and fast enough for a model this small. **Validated by the Chunk 0 spike before anything else.** |
| 2 | Model | **Silero VAD** ONNX (prefer the newest version tract can run; step back a version if the spike hits unsupported ops — Silero v5's graph is known to be more exotic than v4's) | The ROADMAP names Silero-class quality; it supports 8 kHz and 16 kHz natively — exactly forge's bridge rates. Verify the model file's license (silero-vad repo is MIT) and record version + SHA-256 in the crate. |
| 3 | Model distribution | **`include_bytes!` embedded** in forge-vad behind the feature flag, with an optional `model_path` override in `NeuralVadConfig` | ~1–2 MB in the binary only for builds that enable the feature; no runtime download, no deployment asset to forget. Path override keeps ops flexibility (model refresh without rebuild). |
| 4 | Feature gating | Cargo feature **`neural`** on forge-vad, **off by default**; forge-engine re-exposes it as feature `neural-vad` | Consumers that don't want tract + a 2 MB model pay zero cost. siphon-ai will enable it via its forge-media pin when it plumbs config (Phase 2). |
| 5 | API shape | New `NeuralVadDetector` with the **same surface** as `VadDetector` (`process(&[i16]) -> Result<(VadState, f32)>`, `state()`, `reset()`), plus a dispatch enum `AnyVadDetector { EnergyZcr(VadDetector), Neural(NeuralVadDetector) }` built from `VadEngineConfig { EnergyZcr(VadConfig), Neural(NeuralVadConfig) }` via `VadEngineConfig::build() -> Result<AnyVadDetector>` | Enum over `dyn Trait`: keeps `Debug`, no vtable, exhaustive match, and the engine's `Arc<Mutex<…>>` slot changes type once. `build()` is where fail-loud validation lives (unsupported sample rate, unreadable model). |
| 6 | Engine config | `session.rs` `VadConfig` becomes `{ enabled: bool, engine: forge_vad::VadEngineConfig }` (breaking rename of the `detector` field) | siphon-ai never constructs this struct (defaults only), so the break is contained to forge-media's own call sites. Default stays `EnergyZcr(VadConfig::default())` — **behavior of existing deployments is unchanged**. |
| 7 | Windowing | Neural detector owns an internal sample buffer: accepts arbitrary frame sizes (forge feeds 20 ms), slices into the model's native window (Silero: 512 samples @ 16 kHz, 256 @ 8 kHz — confirm exact shapes + any context/state tensors at spike time), carries recurrent state across windows, `reset()` clears buffer + state | Decouples RTP framing (20 ms) from model framing (~32 ms). Decision latency becomes one model window (~32 ms) + hysteresis — negligible vs. the existing 100 ms `min_speech_duration_ms`. |
| 8 | Decision smoothing | Map window speech-probability → `VadState` through the **existing hysteresis semantics** (`min_speech_duration_ms` / `min_silence_duration_ms`, counted in windows) plus dual thresholds: `speech_prob_threshold` (default 0.5) to enter, `silence_prob_threshold` (default 0.35) to exit | Keeps the one-event-per-transition contract and the tuning knobs operators already understand; dual thresholds prevent flapping around 0.5. Confidence returned = raw model probability. |
| 9 | Sample rate | `NeuralVadConfig.sample_rate` must be 8000 or 16000 — anything else is a `build()` error. The engine passes the **actual** decoded rate (`codec_audio_sample_rate`, forwarding.rs:434) at detector construction; if the rate can change mid-call (re-INVITE codec switch), the forwarding loop resets/rebuilds the detector on rate change | The model is rate-specific; feeding 8 kHz audio to a 16 kHz-configured model silently degrades. No resampling in v1 — forge bridge rates are exactly 8 k/16 k. |
| 10 | Hot-path budget | Inference runs inline in the forwarding loop (inside the existing tokio-Mutex hold), **budget ≤ 1.5 ms p99 per window** on the dev box, verified by a criterion bench. Preallocate input/state tensors in the detector — zero steady-state allocation. If the bench misses budget, escalate before shipping (options: dedicated VAD task fed by a channel; `spawn_blocking`) | The loop already does per-packet decode + DTMF inline; a sub-ms model fits. ~31 windows/sec/call ≈ low single-digit % of a core per call — acceptable for an **opt-in** backend, but measure, don't assume. |
| 11 | Observability | At detector build: one `info!` (model version, sample rate, backend). Metrics: `forge_vad_neural_inference_seconds` histogram + `forge_vad_windows_total{backend}` counter (the `metrics` facade is already in the engine). No per-frame logging | Matches forge/siphon observability norms; lets siphon-ai's dashboards compare false-positive rates across backends via existing barge-in metrics. |
| 12 | Testing | Three layers: (a) pure state-machine tests with an injected scorer (make the probability source a small internal trait so hysteresis/windowing is testable without the model); (b) feature-gated integration test running the real model over **one short committed speech fixture** (≤100 KB WAV, public-domain/CC0, 16 kHz mono, and a synthetic noise/tone negative case) asserting speech > threshold and tones/noise < threshold; (c) criterion bench for decision 10 | Generated tones can't prove ML quality; one tiny vetted fixture is the pragmatic middle ground. forge-media has no fixture ban (unlike siphon-ai). |

## 5. Design detail

### 5.1 forge-vad crate changes

```
crates/forge-vad/
├── Cargo.toml        # + [features] neural = ["dep:tract-onnx"]; model bytes gated
└── src/
    ├── lib.rs        # existing VadDetector untouched; re-export new types
    ├── engine.rs     # VadEngineConfig + AnyVadDetector (enum dispatch)
    └── neural.rs     # cfg(feature = "neural"): NeuralVadConfig, NeuralVadDetector
```

- `NeuralVadConfig`: `sample_rate` (8000|16000), `speech_prob_threshold`
  (0.5), `silence_prob_threshold` (0.35), `min_speech_duration_ms` (100),
  `min_silence_duration_ms` (500), `model_path: Option<PathBuf>` (None =
  embedded bytes).
- `NeuralVadDetector` internals: loaded tract model plan (typed, optimized at
  build), preallocated input tensor + recurrent state tensors, sample ring
  buffer (frame in → drain in model-window chunks), window counter–based
  hysteresis, `last_probability`.
- Keep the probability→state logic behind an internal `Scorer` trait
  (`fn score_window(&mut self, window: &[i16]) -> Result<f32>`) so the state
  machine is unit-testable with a scripted scorer and the tract impl is the
  only untested-without-model part.
- When built **without** the `neural` feature, `VadEngineConfig::Neural(..)`
  must still parse/exist but `build()` returns a clear
  `VadError::InvalidConfig("forge-vad built without the `neural` feature")`
  — consumers get fail-loud, not cfg-dependent API shape.

### 5.2 forge-engine changes

- `session.rs`: `VadConfig { enabled, engine: VadEngineConfig }`; the four
  construction sites build `AnyVadDetector` via `engine.build()` — a build
  error fails session setup loudly (config error, not per-frame error).
- `forwarding.rs` VAD block: type changes to `AnyVadDetector`; add the
  rate-change guard (compare the frame's computed sample rate against the
  detector's configured rate; on mismatch, rebuild/reset — log once at
  `warn!`). Event publication logic unchanged.
- Feature `neural-vad` on forge-engine forwards to `forge-vad/neural`.
- `forge-api` / `config/forge.toml`: expose backend selection wherever
  engine VadConfig is currently surfaced (survey during Chunk 2 — if the
  daemon config doesn't surface VAD today, don't add new surface beyond the
  library API; siphon-ai is the driving consumer and configures via code).

### 5.3 Docs
- New `docs/NEURAL_VAD.md`: backend selection, model provenance
  (version, SHA-256, license), tuning knobs, CPU expectations, feature-flag
  matrix. Update `CHANGELOG.md` + the capabilities list in `CLAUDE.MD`.

### 5.4 Phase 2 — siphon-ai integration (separate repo, after forge ships)

For the forge session this is **contract only** (do not implement here):
- siphon-ai adds `[media].vad = "energy" | "neural"` (global; per-route
  override `[route.media].vad` following the existing srtp/codecs override
  pattern), maps to `VadEngineConfig` at session setup, passing the
  negotiated bridge rate as `sample_rate` — fixing the latent
  default-16000-on-8k mismatch.
- siphon-ai's forge-media pin gains `features = ["neural-vad"]`; its
  **static-musl multi-arch release build must stay green** — this is the
  acceptance test for decision 1.
- WS protocol, `speech_started`/`speech_stopped` events, CDR: **unchanged.**
- Success metric (gate on real calls): false-barge-in rate under
  `mode = "pause"` + `debounce_ms` with neural vs energy, via the existing
  barge-in metrics/quality telemetry.

## 6. Chunked implementation plan

**Chunk 0 — feasibility spike (throwaway branch, no PR merge needed).**
1. Vendor the Silero ONNX model (newest version first); write a scratch bin
   that loads it with `tract-onnx`, streams a WAV through 8 k and 16 k paths,
   prints per-window probabilities.
2. Kill criteria checked here, in order: (a) tract loads + runs the model
   (op support); (b) probabilities sane on a speech sample vs. noise/tones;
   (c) p99 inference ≤ 1.5 ms/window on this box; (d)
   `cargo zigbuild --target x86_64-unknown-linux-musl` of a bin depending on
   forge-vad/neural compiles and runs the model.
3. If (a) fails on the newest model → try the previous Silero version. If all
   Silero versions fail → **stop and report**; the fallback ladder is: `ort`
   behind a *non-default* feature (accepting siphon-ai can't use it under
   musl until proven), or shelving neural in favor of the server-side AEC
   track. Do not silently substitute a different model family.
4. Record spike results (model version, ops, timings, musl result) at the top
   of this file before Chunk 1.

**Chunk 1 — forge-vad neural backend (PR 1).**
`engine.rs` + `neural.rs` + feature wiring per §5.1; scripted-scorer state
machine tests; feature-gated real-model integration test + committed fixture;
criterion bench `benches/vad_bench.rs` covering both backends; docs stub.

**Chunk 2 — forge-engine wiring (PR 2).**
§5.2 changes; engine-level test that a neural-configured session publishes
`SpeechStarted`/`SpeechStopped` (scripted scorer or fixture); rate-change
guard test; `docs/NEURAL_VAD.md` + CHANGELOG; bump workspace version per
forge release conventions so siphon-ai has a pinnable rev.

**Chunk 3 — siphon-ai plumbing (separate repo/session, per §5.4).**

## 7. Risks

| Risk | Mitigation |
|---|---|
| tract lacks ops for current Silero graphs | Spike-first (Chunk 0); older model version; `ort` as explicitly-non-musl optional feature; escalate rather than substitute silently |
| Inference blows the hot-path budget under load | Criterion bench is a merge gate; fallback design (dedicated VAD task) sketched but not built until needed |
| Model/licensing drift | Pin model version + SHA-256 in-repo; license file copied next to the model bytes |
| Mid-call sample-rate change feeds the model wrong-rate audio | Rate guard in forwarding loop (decision 9) with test |
| Binary size creep for non-users | Feature off by default; embedded bytes only compiled in with `neural` |
| False-negative regression (neural misses real speech energy VAD caught) | Keep energy backend as default + per-session selectable; siphon-ai gates rollout per-route and compares via barge-in/quality metrics |

## 8. Definition of done (forge-media side)

- [x] Spike results recorded; runtime/model decision confirmed against §4.1–2
      (tract-onnx 0.23 + Silero v6.2.1 via per-rate "ifless" specialization).
- [x] `forge-vad` builds with and without `neural`; existing energy tests
      untouched and green (22 unit tests).
- [x] `AnyVadDetector` neural path: state-machine tests (scripted scorer, run
      without the feature too), real-model fixture tests (6, feature-gated).
- [x] Bench: neural ~60–81 µs mean / window (≤ 1.5 ms budget, ~20× headroom);
      numbers recorded in `docs/NEURAL_VAD.md`.
- [x] Engine: neural-configured session emits `SpeechStarted`/`SpeechStopped`
      (session-level tests); default behavior unchanged (energy backend via
      the same enum; parity test); rate-change guard tested (rebuild at
      stream rate + disable-on-unsupported-rate).
- [x] musl static build of a `neural`-enabled consumer verified against the
      final crate (static-pie binary, fixture detects speech).
- [x] Docs: `docs/NEURAL_VAD.md`, CHANGELOG entry, CLAUDE.MD capability line;
      `forge-vad` 0.1.0→0.2.0, `forge-engine` 0.4.0→0.5.0 (workspace version
      left to the release process since [Unreleased] isn't being cut here);
      siphon-ai pins the git rev once this merges.
