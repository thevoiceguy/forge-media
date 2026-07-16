//! Integration tests that run the real embedded Silero model.
//! Compiled only with the `neural` feature:
//! `cargo test -p forge-vad --features neural`
//!
//! Fixture provenance: `tests/fixtures/speech_16k.wav` is a 3 s
//! excerpt of John F. Kennedy's 1961 inaugural address (public
//! domain, U.S. government work; the same clip whisper.cpp ships as
//! `samples/jfk.wav`), 16 kHz mono s16le; `speech_8k.wav` is the same
//! excerpt resampled to 8 kHz with ffmpeg.
#![cfg(feature = "neural")]

use forge_vad::{NeuralVadConfig, VadEngineConfig, VadState};

/// Minimal PCM16 mono WAV reader — fixtures are known-good, so this
/// only walks RIFF chunks to the `data` payload.
fn wav_samples(bytes: &[u8]) -> Vec<i16> {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &bytes[pos..pos + 4] == b"data" {
            return bytes[pos + 8..pos + 8 + size]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
        }
        pos += 8 + size + (size & 1);
    }
    panic!("no data chunk in fixture");
}

fn detector(sample_rate: u32) -> forge_vad::AnyVadDetector {
    VadEngineConfig::Neural(NeuralVadConfig {
        sample_rate,
        ..NeuralVadConfig::default()
    })
    .build()
    .expect("embedded model must load")
}

/// Feed audio in RTP-sized 20 ms frames, returning (states seen, max
/// probability seen).
fn run(
    detector: &mut forge_vad::AnyVadDetector,
    samples: &[i16],
    rate: u32,
) -> (Vec<VadState>, f32) {
    let frame = (rate / 50) as usize; // 20 ms
    let mut states = Vec::new();
    let mut max_prob = 0.0f32;
    for chunk in samples.chunks(frame) {
        let (state, prob) = detector.process(chunk).expect("inference");
        states.push(state);
        max_prob = max_prob.max(prob);
    }
    (states, max_prob)
}

#[test]
fn speech_fixture_triggers_speech_at_16k() {
    let samples = wav_samples(include_bytes!("fixtures/speech_16k.wav"));
    let mut d = detector(16000);
    let (states, max_prob) = run(&mut d, &samples, 16000);
    assert!(
        states.contains(&VadState::Speech),
        "3 s of real speech must reach VadState::Speech"
    );
    assert!(max_prob > 0.9, "peak speech probability was {max_prob}");
}

#[test]
fn speech_fixture_triggers_speech_at_8k() {
    let samples = wav_samples(include_bytes!("fixtures/speech_8k.wav"));
    let mut d = detector(8000);
    let (states, max_prob) = run(&mut d, &samples, 8000);
    assert!(states.contains(&VadState::Speech));
    assert!(max_prob > 0.9, "peak speech probability was {max_prob}");
}

#[test]
fn tone_and_noise_never_trigger_speech() {
    for rate in [8000u32, 16000] {
        let four_secs = (rate * 4) as usize;

        // 440 Hz tone at substantial amplitude.
        let tone: Vec<i16> = (0..four_secs)
            .map(|i| {
                ((i as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 8000.0)
                    as i16
            })
            .collect();

        // Deterministic pseudo-white noise (xorshift).
        let mut seed = 0x2545F4914F6CDD1Du64;
        let noise: Vec<i16> = (0..four_secs)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed as i32 >> 16) as f32 * 0.1) as i16
            })
            .collect();

        for (name, samples) in [("tone", tone), ("noise", noise)] {
            let mut d = detector(rate);
            let (states, max_prob) = run(&mut d, &samples, rate);
            assert!(
                !states.contains(&VadState::Speech),
                "{name} @ {rate} Hz must not reach Speech (energy VAD's classic false positive)"
            );
            assert!(
                max_prob < 0.35,
                "{name} @ {rate} Hz peak probability {max_prob} above silence threshold"
            );
            assert_eq!(
                d.state(),
                VadState::Silence,
                "{name} @ {rate} settles to Silence"
            );
        }
    }
}

#[test]
fn model_path_override_loads_from_disk() {
    // Any valid per-rate model file works; reuse the embedded 16 k
    // bytes written out to a temp file.
    let dir = std::env::temp_dir().join("forge-vad-model-override-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("model.onnx");
    std::fs::write(&path, include_bytes!("../models/silero_vad_v6_16k.onnx")).unwrap();

    let mut d = VadEngineConfig::Neural(NeuralVadConfig {
        model_path: Some(path),
        ..NeuralVadConfig::default()
    })
    .build()
    .expect("model_path override must load");

    let samples = wav_samples(include_bytes!("fixtures/speech_16k.wav"));
    let (states, _) = run(&mut d, &samples, 16000);
    assert!(states.contains(&VadState::Speech));
}

#[test]
fn missing_model_path_fails_at_build_not_per_frame() {
    let err = VadEngineConfig::Neural(NeuralVadConfig {
        model_path: Some("/nonexistent/model.onnx".into()),
        ..NeuralVadConfig::default()
    })
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("cannot read"), "{err}");
}

#[test]
fn reset_recovers_full_detection() {
    let samples = wav_samples(include_bytes!("fixtures/speech_16k.wav"));
    let mut d = detector(16000);
    let (states, _) = run(&mut d, &samples, 16000);
    assert!(states.contains(&VadState::Speech));
    d.reset();
    assert_eq!(d.state(), VadState::Unknown);
    let (states, max_prob) = run(&mut d, &samples, 16000);
    assert!(
        states.contains(&VadState::Speech),
        "detects again after reset"
    );
    assert!(max_prob > 0.9);
}
