//! On a machine with a GPU: every codec round-trips on the device with
//! the picture the host codecs would give, a keyframe comes on demand, a
//! bitrate retarget is honoured, frames survive the bus both ways, the
//! scaler scales, and the benchmark records what the device sustains.
//! Anywhere else these skip.

use forge_core::VideoCodec;
use forge_video::bench::{noisy, synth};
use forge_video::codec::{CodecRegistry, EncoderSettings};
use forge_video::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use forge_video::metrics::psnr_luma;
use forge_video::scale::Scaler;
use forge_video_hw::bench::{saturate, Stage};
use forge_video_hw::frame::{download, upload};
use forge_video_hw::{DeviceScaler, HwDevice};
use std::sync::Arc;
use std::time::Duration;

fn device() -> Option<MediaDevice> {
    let d = MediaDevice::parse(
        std::env::var("FORGE_HW_DEVICE")
            .as_deref()
            .unwrap_or("cuda:0"),
    )?;
    if forge_video_hw::probe(&d).is_none() {
        eprintln!("no {d}: skipping");
        return None;
    }
    Some(d)
}

fn registry(d: &MediaDevice) -> CodecRegistry {
    let mut r = forge_video::raw::raw_registry();
    forge_video_hw::register(&mut r, d).expect("registers");
    r
}

fn settings(codec: VideoCodec, w: u32, h: u32) -> EncoderSettings {
    EncoderSettings {
        codec,
        resolution: Resolution::new(w, h),
        fps: 30,
        bitrate_kbps: 1200,
        keyframe_interval: 60,
        profile: String::new(),
        content: Default::default(),
    }
}

#[test]
fn frames_survive_the_bus_both_ways() {
    let Some(d) = device() else { return };
    let hw = HwDevice::open(&d).unwrap();
    let src = synth(3, 320, 180);
    let pool = hw.frames(src.resolution()).unwrap();
    let up = upload(&hw, &pool, &src).unwrap();
    assert_eq!(up.device(), d);
    let back = download(&up, &d).unwrap();
    assert_eq!(back.resolution(), src.resolution());
    assert_eq!(back.pts, src.pts);
    // NV12 keeps every sample of I420: the copy is exact.
    assert_eq!(back.y, src.y);
    assert_eq!(back.u, src.u);
    assert_eq!(back.v, src.v);
}

#[test]
fn every_hardware_codec_round_trips_on_the_device() {
    let Some(d) = device() else { return };
    let caps = forge_video_hw::probe(&d).unwrap();
    println!(
        "{d}: decoders {:?}, encoders {:?}",
        caps.decoders, caps.encoders
    );
    let r = registry(&d);
    for codec in r.codecs_on(&d) {
        let s = settings(codec, 640, 360);
        let mut enc = r.encoder(&s, &d).unwrap();
        let mut dec = r.decoder(codec, &d).unwrap();
        let mut decoded = 0;
        let mut keyframes = 0;
        let mut psnr_sum = 0.0;
        let mut psnr_n = 0;
        let mut bytes = 0usize;
        for i in 0..60 {
            let src = noisy(i, 640, 360).with_pts(i as u32 * 3000);
            let force = i == 30;
            let packets = enc.encode(&VideoFrame::Host(src.clone()), force).unwrap();
            for p in &packets {
                bytes += p.data.len();
                keyframes += usize::from(p.keyframe);
                if let Some(out) = dec.decode(p).unwrap() {
                    assert_eq!(
                        out.device(),
                        d,
                        "{codec}: decoded frames stay on the device"
                    );
                    let host = download(&out, &d).unwrap();
                    assert_eq!(host.resolution(), Resolution::new(640, 360));
                    decoded += 1;
                    if let Some(p) = psnr_luma(&host, &src) {
                        psnr_sum += p;
                        psnr_n += 1;
                    }
                }
            }
        }
        let psnr = psnr_sum / psnr_n.max(1) as f64;
        let kbps = bytes as f64 * 8.0 / 2.0 / 1000.0;
        println!("{codec}: decoded {decoded}, keyframes {keyframes}, mean psnr {psnr:.1}, {kbps:.0} kb/s for 1200");
        assert!(decoded >= 55, "{codec} decoded {decoded}");
        assert!(keyframes >= 2, "{codec} keyframes {keyframes}");
        assert!(psnr > 22.0, "{codec} psnr {psnr}");
        assert!(kbps < 1200.0 * 2.5, "{codec} kbps {kbps}");
        // A retarget reopens the encoder: the next frame is a keyframe.
        enc.set_bitrate(400).unwrap();
        let after = enc
            .encode(&VideoFrame::Host(synth(99, 640, 360)), false)
            .unwrap();
        assert!(
            after.iter().any(|p| p.keyframe),
            "{codec}: keyframe after retarget"
        );
    }
}

#[test]
fn a_device_frame_feeds_the_encoder_without_a_copy_and_scales() {
    let Some(d) = device() else { return };
    let r = registry(&d);
    let hw = Arc::new(HwDevice::open(&d).unwrap());
    let codec = *r.codecs_on(&d).first().expect("a codec");
    let s = settings(codec, 640, 360);
    let mut enc = r.encoder(&s, &d).unwrap();
    let pool = hw.frames(Resolution::new(640, 360)).unwrap();
    let src = noisy(1, 640, 360);
    let up = upload(&hw, &pool, &src).unwrap();
    let packets = enc.encode(&up, true).unwrap();
    assert!(packets.iter().any(|p| p.keyframe));
    // Scaled on the device, downloaded: the right size and a picture.
    let mut scaler = DeviceScaler::new(Arc::clone(&hw));
    let small = scaler.scale(&up, Resolution::new(320, 180)).unwrap();
    assert_eq!(small.device(), d);
    let host = download(&small, &d).unwrap();
    assert_eq!(host.resolution(), Resolution::new(320, 180));
    let reference = forge_video::scale::resize(&src, 320, 180);
    let psnr = psnr_luma(&host, &reference).unwrap();
    println!("scale_cuda vs host bilinear: psnr {psnr:.1}");
    assert!(psnr > 25.0, "psnr {psnr}");
    // The same size again is the same frame.
    let same = scaler.scale(&up, Resolution::new(640, 360)).unwrap();
    let _: HostFrame = download(&same, &d).unwrap();
}

/// The GPU column of §9.4, recorded rather than asserted (beyond
/// sanity): single-stream constants and the saturation point.
#[test]
fn the_device_is_measured_single_and_saturated() {
    let Some(d) = device() else { return };
    let r = Arc::new(registry(&d));
    let res = Resolution::new(1280, 720);
    let bench = forge_video::bench::BenchSettings {
        resolution: res,
        fps: 30,
        bitrate_kbps: 1200,
        frames: 120,
        budget: Duration::from_secs(5),
    };
    for codec in r.codecs_on(&d) {
        let cost = forge_video::bench::measure_codec(&r, &d, codec, &bench).unwrap();
        println!(
            "{codec} 720p30 single: encode {:.3} ns/px ({:.0} streams/unit), decode {:.3} ns/px ({:.0} streams/unit), {:.0} kb/s",
            cost.encode_ns_per_px,
            cost.encode_streams_per_unit(),
            cost.decode_ns_per_px,
            cost.decode_streams_per_unit(),
            cost.kbps
        );
        assert!(cost.encode_ns_per_px > 0.0 && cost.decode_ns_per_px > 0.0);
        for stage in [Stage::Encode, Stage::Decode] {
            let s = saturate(&r, &d, codec, stage, res, 30, Duration::from_secs(3), 32).unwrap();
            println!(
                "{codec} 720p30 {stage:?} saturates at {} streams: {:.0} fps total = {:.1} real-time streams{}",
                s.streams,
                s.total_fps,
                s.realtime_streams,
                if s.saturated { "" } else { " (cap reached)" }
            );
            assert!(s.realtime_streams >= 1.0, "{codec} {stage:?}: {s:?}");
        }
    }
}
