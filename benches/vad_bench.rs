//! VAD backend benchmarks.
//!
//! Verifies the neural backend's hot-path budget from
//! NEURAL_VAD_PLAN.md decision 10: inference runs inline in the
//! forwarding loop, so one model window must score in ≤1.5 ms p99 on
//! the dev box. Also benchmarks detector construction (model load +
//! tract optimization), which the engine may pay mid-call on a
//! sample-rate change.
//!
//! The neural benches need the model: `cargo bench --bench vad_bench
//! --features neural-vad`. Without the feature only the energy
//! backend is measured.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use forge_vad::{VadConfig, VadEngineConfig};

/// Speech-like frame: 200 Hz fundamental with harmonics, well above
/// any adaptive threshold.
fn speech_frame(samples: usize, sample_rate: f32) -> Vec<i16> {
    (0..samples)
        .map(|i| {
            let t = i as f32 / sample_rate;
            let s = (t * 200.0 * std::f32::consts::TAU).sin() * 6000.0
                + (t * 400.0 * std::f32::consts::TAU).sin() * 3000.0
                + (t * 800.0 * std::f32::consts::TAU).sin() * 1500.0;
            s as i16
        })
        .collect()
}

fn bench_energy(c: &mut Criterion) {
    let frame = speech_frame(320, 16000.0); // 20 ms @ 16 kHz
    let mut detector = VadEngineConfig::EnergyZcr(VadConfig::default())
        .build()
        .unwrap();
    c.bench_function("energy_zcr/process_20ms_frame", |b| {
        b.iter(|| detector.process(black_box(&frame)).unwrap())
    });
}

#[cfg(feature = "neural-vad")]
fn bench_neural(c: &mut Criterion) {
    use forge_vad::NeuralVadConfig;

    for rate in [8000u32, 16000] {
        let window = if rate == 8000 { 256 } else { 512 };
        let frame = speech_frame(window, rate as f32);
        let mut detector = VadEngineConfig::Neural(NeuralVadConfig {
            sample_rate: rate,
            ..NeuralVadConfig::default()
        })
        .build()
        .expect("embedded model must load");

        // One full model window per iteration = exactly one inference.
        c.bench_function(&format!("neural/score_one_window_{rate}hz"), |b| {
            b.iter(|| detector.process(black_box(&frame)).unwrap())
        });
    }

    c.bench_function("neural/detector_build_16khz", |b| {
        b.iter(|| {
            VadEngineConfig::Neural(NeuralVadConfig::default())
                .build()
                .unwrap()
        })
    });
}

#[cfg(not(feature = "neural-vad"))]
fn bench_neural(_c: &mut Criterion) {}

criterion_group!(benches, bench_energy, bench_neural);
criterion_main!(benches);
