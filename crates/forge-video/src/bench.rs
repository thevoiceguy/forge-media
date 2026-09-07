//! Self-benchmark: synthetic sources and cost measurements.
//!
//! FCP's capacity model (`docs/VIDEO_CONFERENCING.md` §9.3) prices a room
//! as `Σ decode + Σ encode + Σ compose`, each term a constant in
//! nanoseconds per pixel per second of video times the pixel rate. The
//! constants were measured once in phase 0 (§9.4) and every node
//! re-measures them for its own devices when it starts. This module is
//! what a node runs: [`measure_codec`] times a codec's encode and decode
//! of the hard synthetic source, [`measure_compose`] times the host
//! compositor, and both report ns/px so the caller can turn them into
//! whatever budget unit it accounts in.
//!
//! The sources are also what the codec bindings' tests use: [`synth`] is
//! a moving gradient with a bright block, recognisable after a round
//! trip; [`noisy`] adds deterministic per-pixel noise so an encoder has to
//! spend its whole bitrate, which is what a bitrate check and a worst-case
//! cost need. Real camera footage compresses better; these numbers are a
//! bracket, not a promise.

use crate::codec::{CodecError, CodecRegistry, EncoderSettings};
use crate::compose::{Compositor, HostCompositor, TileSource};
use crate::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use crate::layout::Layout;
use forge_core::VideoCodec;
use std::time::{Duration, Instant};

/// A moving gradient with a bright block: enough structure that a
/// decoded frame is recognisably the source. `pts` is `3000 × n`.
pub fn synth(n: usize, w: u32, h: u32) -> HostFrame {
    let mut f = HostFrame::black(w, h).with_pts((n as u32) * 3000);
    let (wu, hu) = (w as usize, h as usize);
    let bx = (n * 5) % (wu.saturating_sub(40).max(1));
    for y in 0..hu {
        for x in 0..wu {
            let grad = ((x * 120 / wu) + (y * 60 / hu) + n * 2) % 180 + 30;
            let block = if (bx..bx + 40).contains(&x) && (10..40).contains(&y) {
                50
            } else {
                0
            };
            f.y[y * f.y_stride + x] = (grad + block).min(235) as u8;
        }
    }
    for y in 0..hu / 2 {
        for x in 0..wu / 2 {
            f.u[y * f.uv_stride + x] = (100 + x * 50 / (wu / 2)) as u8;
            f.v[y * f.uv_stride + x] = (100 + y * 50 / (hu / 2)) as u8;
        }
    }
    f
}

/// [`synth`] plus deterministic per-pixel noise (±24 in luma, ±8 in
/// chroma, xorshift seeded by the frame number): incompressible enough
/// that an encoder must spend its whole budget. The plain gradient
/// compresses to a fraction of any sensible target.
pub fn noisy(n: usize, w: u32, h: u32) -> HostFrame {
    let mut f = synth(n, w, h);
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (n as u64 + 1).wrapping_mul(0x2545_F491_4F6C_DD1D);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for p in f.y.iter_mut() {
        let noise = (next() % 49) as i32 - 24;
        *p = (*p as i32 + noise).clamp(16, 235) as u8;
    }
    for plane in [&mut f.u, &mut f.v] {
        for p in plane.iter_mut() {
            let noise = (next() % 17) as i32 - 8;
            *p = (*p as i32 + noise).clamp(16, 240) as u8;
        }
    }
    f
}

/// How a measurement is run.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchSettings {
    /// Source and canvas size. 640×360 is enough to measure per-pixel
    /// constants and small enough for a node with little memory.
    pub resolution: Resolution,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Frames to encode; fewer when `budget` runs out first.
    pub frames: u32,
    /// Wall-clock cap for one codec's encode pass. A codec that cannot
    /// keep up stops early and reports `complete: false`; its ns/px is
    /// still valid, over the frames it did.
    pub budget: Duration,
}

impl Default for BenchSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::new(640, 360),
            fps: 30,
            bitrate_kbps: 800,
            frames: 90,
            budget: Duration::from_secs(3),
        }
    }
}

/// What one codec costs on one device, in nanoseconds of that device's
/// time per source pixel: multiply by `width × height × fps` for the
/// nanoseconds per second of video, i.e. the fraction of one execution
/// unit (a core, a media engine) a stream keeps busy.
#[derive(Debug, Clone, PartialEq)]
pub struct CodecCost {
    pub codec: VideoCodec,
    pub device: MediaDevice,
    pub resolution: Resolution,
    pub fps: u32,
    /// Frames actually encoded.
    pub frames: u32,
    /// Frames the decoder gave back.
    pub decoded: u32,
    pub encode_ns_per_px: f64,
    pub decode_ns_per_px: f64,
    /// Bitrate the encoder delivered for the requested target.
    pub kbps: f64,
    /// `frames == settings.frames`: the encode pass fit in the budget.
    pub complete: bool,
}

impl CodecCost {
    /// Encodes per second one execution unit can sustain at this size and
    /// frame rate (the "streams per core" of §9.4).
    pub fn encode_streams_per_unit(&self) -> f64 {
        streams(self.encode_ns_per_px, self.resolution, self.fps)
    }

    pub fn decode_streams_per_unit(&self) -> f64 {
        streams(self.decode_ns_per_px, self.resolution, self.fps)
    }
}

fn streams(ns_per_px: f64, resolution: Resolution, fps: u32) -> f64 {
    let ns_per_second = ns_per_px * resolution.pixels() as f64 * fps as f64;
    if ns_per_second <= 0.0 {
        f64::INFINITY
    } else {
        1e9 / ns_per_second
    }
}

/// Distinct source frames kept in memory; the sequence cycles through
/// them with fresh timestamps, which no real-time encoder exploits.
const RING: usize = 16;

/// Time `codec` on `device`: encode the noisy source, then decode what
/// came out. Fails when the registry has no encoder or decoder for the
/// pair, or the codec fails outright.
pub fn measure_codec(
    registry: &CodecRegistry,
    device: &MediaDevice,
    codec: VideoCodec,
    settings: &BenchSettings,
) -> Result<CodecCost, CodecError> {
    let (w, h) = (settings.resolution.width, settings.resolution.height);
    let enc_settings = EncoderSettings {
        codec,
        resolution: settings.resolution,
        fps: settings.fps.max(1),
        bitrate_kbps: settings.bitrate_kbps.max(1),
        keyframe_interval: settings.fps.max(1) * 2,
        profile: String::new(),
    };
    let mut enc = registry.encoder(&enc_settings, device)?;
    let mut dec = registry.decoder(codec, device)?;

    // Generating the source is not the codec's cost: build it first.
    let ring: Vec<HostFrame> = (0..RING).map(|i| noisy(i, w, h)).collect();

    let started = Instant::now();
    let mut encode_ns = 0u128;
    let mut coded = Vec::new();
    let mut frames = 0u32;
    let mut last = None;
    for i in 0..settings.frames.max(1) {
        let frame = VideoFrame::Host(ring[i as usize % RING].clone().with_pts(i * 3000));
        let t = Instant::now();
        coded.extend(enc.encode(&frame, false)?);
        encode_ns += t.elapsed().as_nanos();
        frames += 1;
        last = Some(frame);
        if started.elapsed() > settings.budget {
            break;
        }
    }
    // Low-delay encoders may hold the last frame until the next send.
    if let Some(frame) = &last {
        let t = Instant::now();
        coded.extend(enc.encode(frame, false)?);
        encode_ns += t.elapsed().as_nanos();
    }

    let bytes: usize = coded.iter().map(|c| c.data.len()).sum();
    let mut decode_ns = 0u128;
    let mut decoded = 0u32;
    for c in &coded {
        let t = Instant::now();
        if dec.decode(c)?.is_some() {
            decoded += 1;
        }
        decode_ns += t.elapsed().as_nanos();
    }

    let px = settings.resolution.pixels() as f64;
    let seconds = frames as f64 / settings.fps.max(1) as f64;
    Ok(CodecCost {
        codec,
        device: device.clone(),
        resolution: settings.resolution,
        fps: settings.fps,
        frames,
        decoded,
        encode_ns_per_px: encode_ns as f64 / (frames as f64 * px),
        decode_ns_per_px: decode_ns as f64 / (decoded.max(1) as f64 * px),
        kbps: bytes as f64 * 8.0 / seconds / 1000.0,
        complete: frames == settings.frames,
    })
}

/// Time every codec the registry can both encode and decode on `device`.
/// A codec that fails is left out rather than failing the run.
pub fn measure_all(
    registry: &CodecRegistry,
    device: &MediaDevice,
    settings: &BenchSettings,
) -> Vec<CodecCost> {
    registry
        .codecs_on(device)
        .into_iter()
        .filter_map(|codec| measure_codec(registry, device, codec, settings).ok())
        .collect()
}

/// Time the host compositor drawing a grid of `tiles` sources onto a
/// canvas of `resolution`, `frames` times, in nanoseconds per canvas
/// pixel per render (§9.3 `k_cmp`, before its `1 + 0.1 × tiles` term).
/// Sources are 640×360 so each tile is scaled, as in a room.
pub fn measure_compose(resolution: Resolution, tiles: usize, frames: u32) -> f64 {
    let tiles = tiles.clamp(1, Layout::Grid.capacity());
    // Two frames per tile, alternated, so every render repaints.
    let sources: Vec<[VideoFrame; 2]> = (0..tiles)
        .map(|i| {
            [
                VideoFrame::Host(noisy(2 * i, 640, 360)),
                VideoFrame::Host(noisy(2 * i + 1, 640, 360)),
            ]
        })
        .collect();
    let names: Vec<String> = (0..tiles).map(|i| format!("Participant {i}")).collect();
    let mut compositor = HostCompositor::new(resolution.width, resolution.height, Layout::Grid);
    let mut total_ns = 0u128;
    let frames = frames.max(1);
    for n in 0..frames {
        let tile_sources: Vec<TileSource<'_>> = sources
            .iter()
            .enumerate()
            .map(|(i, pair)| TileSource {
                id: &names[i],
                name: &names[i],
                frame: Some(&pair[n as usize % 2]),
                speaking: i == n as usize % tiles,
                muted: false,
            })
            .collect();
        let t = Instant::now();
        compositor
            .render(&tile_sources, n * 3000)
            .expect("host frames render on the host compositor");
        total_ns += t.elapsed().as_nanos();
    }
    total_ns as f64 / (frames as f64 * resolution.pixels() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::raw_registry;

    #[test]
    fn sources_are_deterministic_and_stamped() {
        let a = noisy(3, 64, 36);
        let b = noisy(3, 64, 36);
        assert_eq!(a.y, b.y);
        assert_eq!(a.pts, 9000);
        assert_ne!(noisy(4, 64, 36).y, a.y);
        assert_ne!(synth(3, 64, 36).y, a.y, "noise changes the picture");
    }

    #[test]
    fn the_raw_codec_measures_cheap_and_complete() {
        let settings = BenchSettings {
            resolution: Resolution::new(160, 90),
            frames: 12,
            ..BenchSettings::default()
        };
        let cost = measure_codec(
            &raw_registry(),
            &MediaDevice::Host,
            VideoCodec::VP8,
            &settings,
        )
        .unwrap();
        assert_eq!(cost.frames, 12);
        assert!(cost.complete);
        assert_eq!(
            cost.decoded, 13,
            "every coded frame, plus the drain, decodes"
        );
        assert!(cost.encode_ns_per_px > 0.0 && cost.encode_ns_per_px < 100.0);
        assert!(cost.decode_ns_per_px > 0.0 && cost.decode_ns_per_px < 100.0);
        assert!(cost.kbps > 0.0);
        assert!(cost.encode_streams_per_unit() > 1.0);
        assert_eq!(cost.resolution, Resolution::new(160, 90));
    }

    #[test]
    fn an_exhausted_budget_stops_after_one_frame() {
        let settings = BenchSettings {
            resolution: Resolution::new(160, 90),
            frames: 50,
            budget: Duration::ZERO,
            ..BenchSettings::default()
        };
        let cost = measure_codec(
            &raw_registry(),
            &MediaDevice::Host,
            VideoCodec::H264,
            &settings,
        )
        .unwrap();
        assert_eq!(cost.frames, 1);
        assert!(!cost.complete);
        assert!(cost.encode_ns_per_px > 0.0);
    }

    #[test]
    fn measure_all_covers_the_registry() {
        let settings = BenchSettings {
            resolution: Resolution::new(96, 54),
            frames: 3,
            ..BenchSettings::default()
        };
        let costs = measure_all(&raw_registry(), &MediaDevice::Host, &settings);
        assert_eq!(costs.len(), 5);
        let mut codecs: Vec<VideoCodec> = costs.iter().map(|c| c.codec).collect();
        codecs.sort_by_key(|c| format!("{c:?}"));
        codecs.dedup();
        assert_eq!(codecs.len(), 5);
    }

    #[test]
    fn composing_costs_something_per_pixel() {
        let k = measure_compose(Resolution::new(320, 180), 4, 3);
        assert!(k > 0.0 && k < 1_000.0, "{k}");
    }
}
