//! Shared test settings and round-trip check for the native bindings; the
//! synthetic sources themselves live in `forge_video::bench`.

use forge_video::codec::{EncoderSettings, VideoDecoder, VideoEncoder};
use forge_video::frame::{HostFrame, Resolution, VideoFrame};
use forge_video::metrics::psnr_luma;

pub use forge_video::bench::{noisy, synth};

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
