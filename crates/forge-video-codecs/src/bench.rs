//! Bitrate targeting and speed for every native codec this build has,
//! per FCP `docs/VIDEO_CONFERENCING.md` §14: the round trip must hold its
//! bitrate within a bracket, and the phase 0 benchmark runs as a test
//! that records numbers (run with `--nocapture` to see them) rather than
//! asserting them. Absolute speeds are for the machine at hand; every
//! node self-benchmarks at start.
//!
//! The source is the synthetic gradient with per-pixel noise: hard enough
//! that every encoder has to spend its budget. Real camera clips (which
//! the design asks for) are not in the repository; the numbers here are a
//! bracket, not a capacity figure.

use crate::testsrc;
use forge_video::bench::{measure_codec, BenchSettings};
use forge_video::codec::CodecRegistry;
use forge_video::frame::{MediaDevice, VideoFrame};
use forge_video::metrics::psnr_luma;
use std::time::Instant;

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 30;
const FRAMES: usize = 90;
const TARGET_KBPS: u32 = 800;

/// Only the native bindings: the raw codec would hold any bitrate.
fn native() -> CodecRegistry {
    let mut r = CodecRegistry::new();
    crate::register_all(&mut r);
    r
}

#[test]
fn native_codecs_hold_their_bitrate_and_report_their_speed() {
    let registry = native();
    let codecs = registry.codecs_on(&MediaDevice::Host);
    assert!(!codecs.is_empty(), "no native codec with encode and decode");
    let sources: Vec<VideoFrame> = (0..FRAMES)
        .map(|i| VideoFrame::Host(testsrc::noisy(i, W, H)))
        .collect();
    for codec in codecs {
        let mut settings = testsrc::settings(codec, W, H);
        settings.fps = FPS;
        settings.bitrate_kbps = TARGET_KBPS;
        settings.keyframe_interval = FPS * 2;
        let mut enc = registry.encoder(&settings, &MediaDevice::Host).unwrap();
        let mut dec = registry.decoder(codec, &MediaDevice::Host).unwrap();

        let mut coded = Vec::new();
        let t = Instant::now();
        for src in &sources {
            coded.extend(enc.encode(src, false).unwrap());
        }
        // Low-delay encoders may hold the last frame until the next send.
        coded.extend(enc.encode(&sources[FRAMES - 1], false).unwrap());
        let enc_secs = t.elapsed().as_secs_f64();

        let bytes: usize = coded.iter().map(|c| c.data.len()).sum();
        // FRAMES frames plus the drain call were submitted.
        let seconds = (FRAMES + 1) as f64 / FPS as f64;
        let kbps = bytes as f64 * 8.0 / seconds / 1000.0;
        let ratio = kbps / TARGET_KBPS as f64;
        let keyframes = coded.iter().filter(|c| c.keyframe).count();

        let t = Instant::now();
        let mut decoded = Vec::new();
        for c in &coded {
            if let Some(f) = dec.decode(c).unwrap() {
                decoded.push(f);
            }
        }
        let dec_secs = t.elapsed().as_secs_f64();
        // Each decoded frame against its own source (`synth` stamps pts
        // as 3000 × frame number), averaged.
        let psnr = decoded
            .iter()
            .map(|f| {
                let src = sources[(f.pts() / 3000) as usize].as_host().unwrap();
                psnr_luma(f.as_host().unwrap(), src).unwrap()
            })
            .sum::<f64>()
            / decoded.len() as f64;

        println!(
            "{codec}: {W}x{H}@{FPS} target {TARGET_KBPS} kb/s -> {kbps:.0} kb/s ({ratio:.2}x), \
             {} coded / {} decoded of {FRAMES} / {keyframes} key, encode {:.0} fps, \
             decode {:.0} fps, mean psnr {psnr:.1} dB",
            coded.len(),
            decoded.len(),
            coded.len() as f64 / enc_secs,
            decoded.len() as f64 / dec_secs,
        );

        // Rate control bracket (phase 0 measured VP9 at +25 %). OpenH264
        // holds its bitrate by skipping frames, which the pipeline treats
        // as a repeated frame, so at least half the frames must arrive
        // rather than all of them.
        assert!(
            ratio > 0.5 && ratio < 1.6,
            "{codec} delivered {kbps:.0} kb/s for a {TARGET_KBPS} kb/s target"
        );
        assert!(
            decoded.len() >= FRAMES / 2,
            "{codec} decoded {} of {FRAMES}",
            decoded.len()
        );
        assert!(keyframes >= 2, "{codec} keyframes {keyframes}");
        assert!(psnr > 22.0, "{codec} mean psnr {psnr}");

        // The self-benchmark a node runs at start (FCP §9.3) sees the same
        // codec and reports the constants the capacity model uses.
        let cost = measure_codec(
            &registry,
            &MediaDevice::Host,
            codec,
            &BenchSettings {
                frames: 30,
                ..BenchSettings::default()
            },
        )
        .unwrap();
        println!(
            "{codec}: self-benchmark encode {:.2} ns/px ({:.1} streams/core), decode {:.2} ns/px ({:.1} streams/core), {:.0} kb/s, {} frames{}",
            cost.encode_ns_per_px,
            cost.encode_streams_per_unit(),
            cost.decode_ns_per_px,
            cost.decode_streams_per_unit(),
            cost.kbps,
            cost.frames,
            if cost.complete { "" } else { " (budget cut it short)" },
        );
        assert!(cost.encode_ns_per_px > 0.0 && cost.decode_ns_per_px > 0.0);
        assert!(
            cost.decoded > 0,
            "{codec} decoded nothing in the self-benchmark"
        );
    }
}
