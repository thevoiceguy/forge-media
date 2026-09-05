//! Shared test source and round-trip check for the native bindings.

use forge_video::codec::{EncoderSettings, VideoDecoder, VideoEncoder};
use forge_video::frame::{HostFrame, Resolution, VideoFrame};
use forge_video::metrics::psnr_luma;

/// A moving gradient with a bright block: enough structure that a
/// decoded frame is recognisably the source.
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
/// that an encoder must spend its whole budget, which is what a bitrate
/// check needs. The plain gradient compresses to a fraction of any
/// sensible target.
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

pub fn settings(codec: forge_core::VideoCodec, w: u32, h: u32) -> EncoderSettings {
    EncoderSettings {
        codec,
        resolution: Resolution::new(w, h),
        fps: 30,
        bitrate_kbps: 600,
        keyframe_interval: 60,
        profile: String::new(),
    }
}

/// Encode `n` frames, decode everything, and check that most frames come
/// back and the last one resembles its source. Returns (decoded count,
/// keyframes, PSNR of the last decoded frame).
pub fn round_trip(
    enc: &mut dyn VideoEncoder,
    dec: &mut dyn VideoDecoder,
    n: usize,
    w: u32,
    h: u32,
) -> (usize, usize, f64) {
    let mut decoded = 0;
    let mut keyframes = 0;
    let mut last_psnr = 0.0;
    let mut last_src: Option<HostFrame> = None;
    for i in 0..n {
        let src = synth(i, w, h);
        let force = i == n / 2;
        let packets = enc.encode(&VideoFrame::Host(src.clone()), force).unwrap();
        for p in &packets {
            if p.keyframe {
                keyframes += 1;
            }
            if let Some(out) = dec.decode(p).unwrap() {
                let host = out.as_host().unwrap();
                assert_eq!(host.resolution(), Resolution::new(w, h));
                decoded += 1;
                if let Some(s) = &last_src {
                    // Low-delay encoders emit the frame just sent; anything
                    // else is at most a frame behind. Compare against the
                    // nearer of the two.
                    let a = psnr_luma(host, &src).unwrap_or(0.0);
                    let b = psnr_luma(host, s).unwrap_or(0.0);
                    last_psnr = a.max(b);
                } else {
                    last_psnr = psnr_luma(host, &src).unwrap_or(0.0);
                }
            }
        }
        last_src = Some(src);
    }
    (decoded, keyframes, last_psnr)
}
