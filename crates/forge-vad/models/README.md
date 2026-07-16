# Silero VAD model files

Per-sample-rate specializations of the Silero VAD neural network, embedded
into `forge-vad` when the `neural` Cargo feature is enabled.

## Provenance

- Upstream: [snakers4/silero-vad](https://github.com/snakers4/silero-vad),
  release **v6.2.1**, file `src/silero_vad/data/silero_vad_op18_ifless.onnx`
  - SHA-256: `7671cd04b004e9076da0d4a7b1a5aec36adf161c39230c1cb94a4fd5db6bbd28`
  - License: MIT (see `LICENSE` in this directory, copied from the upstream
    repository)
- The upstream "ifless" export wraps two complete per-rate subgraphs (16 kHz
  and 8 kHz) in a single top-level ONNX `If` node switched on the `sr` input.
  `tract-onnx` (the pure-Rust inference runtime used by `forge-vad`) rejects
  that `If` during type inference because the branch output ranks differ, so
  `specialize.py` (this directory) extracts each branch into a standalone
  graph: **same nodes, same weights, no control flow, no `sr` input**.
  Numerical parity with the upstream model was verified bit-exact under
  onnxruntime over a 343-window speech fixture at both rates (2026-07-16).

## Files

| File | SHA-256 | Input | State |
|---|---|---|---|
| `silero_vad_v6_16k.onnx` | `871f08287dd2ef99eca43602efa93091cee422fc9157bff09dcda33404f00337` | f32 `[1, 576]` (64 context + 512 window samples) | f32 `[2, 1, 128]` |
| `silero_vad_v6_8k.onnx` | `03fb3a9d44c513895f013653b001024961c42bde62ac65d86644e8506801c242` | f32 `[1, 288]` (32 context + 256 window samples) | f32 `[2, 1, 128]` |

Both models output `output` f32 `[1, 1]` (speech probability, 0.0–1.0) and
`stateN` f32 `[2, 1, 128]` (recurrent state to feed back on the next window).
Samples are linear PCM normalized to `[-1.0, 1.0]` (i16 / 32768).

## Regenerating

```sh
curl -LO https://github.com/snakers4/silero-vad/raw/v6.2.1/src/silero_vad/data/silero_vad_op18_ifless.onnx
python3 specialize.py silero_vad_op18_ifless.onnx silero_vad_v6_16k.onnx silero_vad_v6_8k.onnx
```

(`specialize.py` needs the `onnx` Python package.)
