# Forge Media Performance Benchmarks

This directory contains performance benchmarks for critical Forge Media components using [Criterion.rs](https://github.com/bheisler/criterion.rs).

## Running Benchmarks

### Run all benchmarks
```bash
cargo bench
```

### Run specific benchmark suite
```bash
cargo bench --bench transcoding_bench
```

### Test benchmarks (verify they work without full measurement)
```bash
cargo bench -- --test
```

### Run with Opus support
```bash
cargo bench --features opus
```

## Benchmark Suites

### Transcoding Benchmarks (`transcoding_bench`)

Measures performance of audio transcoding operations between different codecs.

**Benchmark Groups:**

- **transcode_g711**: G.711 codec transcoding (PCMU ↔ PCMA)
  - `pcmu_to_pcma`: PCMU → PCMA conversion (same sample rate, 8kHz)
  - `pcma_to_pcmu`: PCMA → PCMU conversion (same sample rate, 8kHz)

- **transcode_opus** (requires `--features opus`): Opus codec transcoding
  - `opus_to_pcmu`: Opus (48kHz) → PCMU (8kHz) with resampling
  - `pcmu_to_opus`: PCMU (8kHz) → Opus (48kHz) with resampling

- **resampling**: Sample rate conversion benchmarks
  - `8khz_to_48khz`: Upsampling from 8kHz to 48kHz (160 samples → 960 samples)
  - `48khz_to_8khz`: Downsampling from 48kHz to 8kHz (960 samples → 160 samples)

- **decode**: Decoder-only benchmarks
  - `pcmu`: PCMU decoding (compressed → PCM samples)

- **encode**: Encoder-only benchmarks
  - `pcma`: PCMA encoding (PCM samples → compressed)

- **transcoder_init**: Transcoder initialization overhead
  - `pcmu_to_pcma`: G.711 transcoder creation time
  - `opus_to_pcmu`: Opus transcoder creation time (with resampling)

- **sustained_transcode**: Multi-frame transcoding
  - `pcmu_to_pcma_100frames`: 100 consecutive frames (2 seconds of audio)

- **reusable_transcode**: Transcoder reuse efficiency
  - `pcmu_to_pcma_reuse`: Single frame with reused transcoder instance

## Performance Targets

Based on real-time audio requirements (20ms frames):

- **Transcoding Latency**: <5ms per frame (target)
- **Resampling**: <2ms per frame
- **Encode/Decode**: <1ms per frame
- **Transcoder Init**: <10ms (one-time cost)

## Interpreting Results

Criterion outputs detailed statistics including:

- **Time**: Mean execution time with confidence intervals
- **Throughput**: Operations per second
- **Change**: Performance delta from previous run (if available)

Example output:
```
transcode_g711/pcmu_to_pcma
                        time:   [1.2345 µs 1.2567 µs 1.2789 µs]
                        thrpt:  [782.11 Kelem/s 795.74 Kelem/s 809.54 Kelem/s]
```

This shows:
- Mean time: ~1.26 µs per frame
- Throughput: ~795K frames/second
- Well below 5ms target ✓

## HTML Reports

Criterion generates HTML reports in `target/criterion/`:

```bash
# View reports
open target/criterion/report/index.html
```

Reports include:
- Performance graphs
- Distribution plots
- Historical comparisons
- Regression analysis

## Continuous Integration

Benchmark regressions can be detected with:

```bash
# Save baseline
cargo bench --bench transcoding_bench -- --save-baseline main

# Compare against baseline
cargo bench --bench transcoding_bench -- --baseline main
```

## Adding New Benchmarks

To add a new benchmark:

1. Create benchmark function:
```rust
fn bench_my_feature(c: &mut Criterion) {
    let mut group = c.benchmark_group("my_feature");
    group.throughput(Throughput::Elements(1));

    group.bench_function("test_case", |b| {
        b.iter(|| {
            // Code to benchmark
            black_box(my_function(black_box(input)))
        });
    });

    group.finish();
}
```

2. Add to criterion_group:
```rust
criterion_group!(benches, ..., bench_my_feature);
```

3. Document performance targets and interpretation.

## Notes

- Benchmarks use optimized release builds (`--release`)
- Each benchmark runs multiple iterations for statistical significance
- Results may vary based on CPU, system load, and thermal throttling
- Use `black_box()` to prevent compiler optimizations from skewing results
- Frame size: 20ms is standard (160 samples @ 8kHz, 960 @ 48kHz)

## References

- [Criterion.rs User Guide](https://bheisler.github.io/criterion.rs/book/)
- [Benchmarking Best Practices](https://easyperf.net/blog/)
- Forge Media Architecture: `docs/architecture.md`
