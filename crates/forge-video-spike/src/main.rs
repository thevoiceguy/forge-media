//! Phase 0 spike for `docs/VIDEO_CONFERENCING.md` (FCP): do the codec
//! crates build on this machine, and what do encode, decode and
//! composition cost per core. Throwaway; not a workspace citizen.
//!
//! Every measurement is single-threaded so the result is "per core".
//! A 4-way parallel run at the end checks that cores add up.
//!
//! Usage: `cargo run --release -p forge-video-spike [frames]`

use anyhow::{anyhow, Context, Result};
use std::time::{Duration, Instant};

const W: usize = 1280;
const H: usize = 720;
const FPS: u32 = 30;
const BITRATE_KBPS: u32 = 1200;
const KEYFRAME_INTERVAL: u32 = 300;

/// I420 frame with contiguous planes, stride = width.
#[derive(Clone)]
struct Frame {
    w: usize,
    h: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Frame {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            y: vec![0; w * h],
            u: vec![128; w * h / 4],
            v: vec![128; w * h / 4],
        }
    }

    /// Contiguous I420 buffer (Y, then U, then V).
    fn packed(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 3 / 2);
        out.extend_from_slice(&self.y);
        out.extend_from_slice(&self.u);
        out.extend_from_slice(&self.v);
        out
    }
}

/// A synthetic "camera": a slow gradient, a moving bright block, and
/// low-amplitude texture so the encoders have real work to do.
fn synth_frame(n: usize, w: usize, h: usize) -> Frame {
    // SPIKE_HARD=1: four times the noise, three moving blocks, a
    // scrolling texture — a pessimistic stand-in for a busy camera.
    let hard = std::env::var("SPIKE_HARD")
        .map(|v| v == "1")
        .unwrap_or(false);
    let mut f = Frame::new(w, h);
    let bx = (n * 7) % (w - 160);
    let by = (n * 3) % (h - 120);
    let mut seed: u32 = 0x9E37_79B9 ^ (n as u32);
    for yy in 0..h {
        for xx in 0..w {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = if hard {
                (seed & 0x3F) as i32 - 32
            } else {
                (seed & 0x0F) as i32 - 8
            };
            let grad = ((xx * 160 / w) + (yy * 60 / h) + n * 2) as i32 % 200 + 20;
            let mut block = if xx >= bx && xx < bx + 160 && yy >= by && yy < by + 120 {
                60
            } else {
                0
            };
            if hard {
                let bx2 = (w - 200) - (n * 11) % (w - 200);
                let by2 = (n * 5) % (h - 100);
                if xx >= bx2 && xx < bx2 + 200 && yy >= by2 && yy < by2 + 100 {
                    block -= 50;
                }
                if xx >= (n * 13) % (w - 90) && xx < (n * 13) % (w - 90) + 90 && yy < 90 {
                    block += 80;
                }
                // Scrolling texture (checker of 4 px with drift).
                let t = (((xx + n * 4) / 4) + (yy / 4)) % 2 == 0;
                block += if t { 18 } else { -18 };
            }
            f.y[yy * w + xx] = (grad + block + noise).clamp(16, 235) as u8;
        }
    }
    let cw = w / 2;
    for yy in 0..h / 2 {
        for xx in 0..cw {
            f.u[yy * cw + xx] = (96 + (xx * 64 / cw) + n % 32) as u8;
            f.v[yy * cw + xx] = (96 + (yy * 64 / (h / 2))) as u8;
        }
    }
    f
}

/// Process CPU time (user + system) in seconds, from /proc/self/stat.
fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the parenthesised command name; utime is field 14, stime 15.
    let rest = stat.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks: f64 = f.get(11).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
        + f.get(12).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    ticks / 100.0
}

struct Measure {
    name: String,
    frames: usize,
    elapsed: Duration,
    bytes: usize,
    keyframes: usize,
}

impl Measure {
    fn fps(&self) -> f64 {
        self.frames as f64 / self.elapsed.as_secs_f64()
    }
    /// How many 720p30 streams one core sustains at this speed.
    fn streams_per_core(&self) -> f64 {
        self.fps() / FPS as f64
    }
    /// Nanoseconds per pixel, the `k` of the capacity model.
    fn ns_per_pixel(&self) -> f64 {
        self.elapsed.as_nanos() as f64 / (self.frames as f64 * (W * H) as f64)
    }
    fn kbps(&self) -> f64 {
        (self.bytes as f64 * 8.0) / (self.frames as f64 / FPS as f64) / 1000.0
    }
}

fn report(m: &Measure) {
    if m.bytes > 0 {
        println!(
            "{:<24} {:>7.1} fps/core  {:>5.2} x 720p30/core  {:>6.2} ns/px  {:>6.0} kb/s  {} key",
            m.name,
            m.fps(),
            m.streams_per_core(),
            m.ns_per_pixel(),
            m.kbps(),
            m.keyframes
        );
    } else {
        println!(
            "{:<24} {:>7.1} fps/core  {:>5.2} x 720p30/core  {:>6.2} ns/px",
            m.name,
            m.fps(),
            m.streams_per_core(),
            m.ns_per_pixel()
        );
    }
}

// ─── H.264 (OpenH264) ────────────────────────────────────────────────────

mod h264 {
    use super::*;
    use openh264::encoder::{
        BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Level, Profile,
        RateControlMode, UsageType,
    };
    use openh264::formats::YUVBuffer;
    use openh264::OpenH264API;

    pub fn encode(frames: &[Frame]) -> Result<(Measure, Vec<Vec<u8>>)> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(BITRATE_KBPS * 1000))
            .max_frame_rate(FrameRate::from_hz(FPS as f32))
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .profile(Profile::Baseline)
            .level(Level::Level_3_1)
            .complexity(Complexity::Low)
            .intra_frame_period(IntraFramePeriod::from_num_frames(KEYFRAME_INTERVAL))
            .scene_change_detect(false)
            // Skipping is what a conference would run with, but for the
            // hard source it hides the cost: measure every frame there.
            .skip_frames(
                std::env::var("SPIKE_HARD")
                    .map(|v| v != "1")
                    .unwrap_or(true),
            )
            .num_threads(1);
        let mut enc = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| anyhow!("openh264 encoder: {e}"))?;
        let mut out = Vec::with_capacity(frames.len());
        let mut bytes = 0;
        let mut keyframes = 0;
        let mut skipped = 0;
        let start = Instant::now();
        for f in frames {
            let src = YUVBuffer::from_vec(f.packed(), f.w, f.h);
            let bs = enc
                .encode(&src)
                .map_err(|e| anyhow!("openh264 encode: {e}"))?;
            let data = bs.to_vec();
            if data.is_empty() {
                skipped += 1;
            }
            // IDR NAL (type 5) anywhere in the access unit = keyframe.
            if data
                .windows(5)
                .any(|w| w[..4] == [0, 0, 0, 1] && (w[4] & 0x1F) == 5)
            {
                keyframes += 1;
            }
            bytes += data.len();
            out.push(data);
        }
        let elapsed = start.elapsed();
        if skipped > 0 {
            eprintln!(
                "openh264 skipped {skipped} of {} frames for rate control",
                frames.len()
            );
        }
        Ok((
            Measure {
                name: "H.264 encode (openh264)".into(),
                frames: frames.len(),
                elapsed,
                bytes,
                keyframes,
            },
            out,
        ))
    }

    pub fn decode(packets: &[Vec<u8>]) -> Result<Measure> {
        use openh264::formats::YUVSource;
        let mut dec =
            openh264::decoder::Decoder::new().map_err(|e| anyhow!("openh264 decoder: {e}"))?;
        let mut decoded = 0;
        let start = Instant::now();
        for p in packets {
            if let Some(yuv) = dec.decode(p).map_err(|e| anyhow!("openh264 decode: {e}"))? {
                let _ = yuv.dimensions();
                decoded += 1;
            }
        }
        let elapsed = start.elapsed();
        if decoded < packets.len() / 2 {
            return Err(anyhow!(
                "openh264 decoded only {decoded} of {}",
                packets.len()
            ));
        }
        Ok(Measure {
            name: "H.264 decode (openh264)".into(),
            frames: decoded,
            elapsed,
            bytes: 0,
            keyframes: 0,
        })
    }
}

// ─── VP8 / VP9 (libvpx) ──────────────────────────────────────────────────

mod vpx {
    use super::*;
    use std::ffi::CStr;
    use std::os::raw::{c_int, c_long, c_uint, c_ulong};
    use std::ptr;
    use vpx_sys as ffi;

    #[derive(Clone, Copy)]
    pub enum Codec {
        Vp8,
        Vp9,
    }

    impl Codec {
        fn name(self) -> &'static str {
            match self {
                Codec::Vp8 => "VP8",
                Codec::Vp9 => "VP9",
            }
        }
    }

    fn check(err: ffi::vpx_codec_err_t, what: &str) -> Result<()> {
        if err == ffi::vpx_codec_err_t::VPX_CODEC_OK {
            Ok(())
        } else {
            let msg = unsafe { CStr::from_ptr(ffi::vpx_codec_err_to_string(err)) };
            Err(anyhow!("{what}: {}", msg.to_string_lossy()))
        }
    }

    pub fn encode(
        codec: Codec,
        frames: &[Frame],
        cpu_used: c_int,
    ) -> Result<(Measure, Vec<Vec<u8>>)> {
        unsafe {
            let iface = match codec {
                Codec::Vp8 => ffi::vpx_codec_vp8_cx(),
                Codec::Vp9 => ffi::vpx_codec_vp9_cx(),
            };
            // The config holds enums without a zero variant, so let libvpx
            // fill it rather than zero-initialising.
            let mut cfg = std::mem::MaybeUninit::<ffi::vpx_codec_enc_cfg>::uninit();
            check(
                ffi::vpx_codec_enc_config_default(iface, cfg.as_mut_ptr(), 0),
                "enc_config_default",
            )?;
            let mut cfg = cfg.assume_init();
            cfg.g_w = W as c_uint;
            cfg.g_h = H as c_uint;
            cfg.g_timebase = ffi::vpx_rational {
                num: 1,
                den: FPS as c_int,
            };
            cfg.g_threads = 1;
            cfg.g_lag_in_frames = 0;
            cfg.g_error_resilient = 0;
            cfg.rc_end_usage = ffi::vpx_rc_mode::VPX_CBR;
            cfg.rc_target_bitrate = BITRATE_KBPS;
            cfg.rc_min_quantizer = 4;
            cfg.rc_max_quantizer = 56;
            cfg.kf_max_dist = KEYFRAME_INTERVAL;
            let mut ctx = std::mem::MaybeUninit::<ffi::vpx_codec_ctx>::zeroed();
            check(
                ffi::vpx_codec_enc_init_ver(
                    ctx.as_mut_ptr(),
                    iface,
                    &cfg,
                    0,
                    ffi::VPX_ENCODER_ABI_VERSION as c_int,
                ),
                "enc_init",
            )?;
            let mut ctx = ctx.assume_init();
            check(
                ffi::vpx_codec_control_(
                    &mut ctx,
                    ffi::vp8e_enc_control_id::VP8E_SET_CPUUSED as c_int,
                    cpu_used,
                ),
                "set cpu-used",
            )?;
            if matches!(codec, Codec::Vp9) {
                // Single-thread realtime VP9 the way WebRTC runs it.
                let _ = ffi::vpx_codec_control_(
                    &mut ctx,
                    ffi::vp8e_enc_control_id::VP9E_SET_ROW_MT as c_int,
                    0 as c_int,
                );
            }

            let mut out = Vec::with_capacity(frames.len());
            let mut bytes = 0;
            let mut keyframes = 0;
            let start = Instant::now();
            for (i, f) in frames.iter().enumerate() {
                let mut packed = f.packed();
                let mut img = std::mem::MaybeUninit::<ffi::vpx_image>::zeroed();
                let p = ffi::vpx_img_wrap(
                    img.as_mut_ptr(),
                    ffi::vpx_img_fmt::VPX_IMG_FMT_I420,
                    f.w as c_uint,
                    f.h as c_uint,
                    1,
                    packed.as_mut_ptr(),
                );
                if p.is_null() {
                    return Err(anyhow!("vpx_img_wrap failed"));
                }
                let img = img.assume_init();
                check(
                    ffi::vpx_codec_encode(
                        &mut ctx,
                        &img,
                        i as ffi::vpx_codec_pts_t,
                        1,
                        0,
                        ffi::VPX_DL_REALTIME as c_ulong,
                    ),
                    "encode",
                )?;
                let mut iter: ffi::vpx_codec_iter_t = ptr::null_mut();
                let mut frame_bytes = Vec::new();
                loop {
                    let pkt = ffi::vpx_codec_get_cx_data(&mut ctx, &mut iter);
                    if pkt.is_null() {
                        break;
                    }
                    if (*pkt).kind == ffi::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                        let fr = (*pkt).data.frame;
                        let slice = std::slice::from_raw_parts(fr.buf as *const u8, fr.sz);
                        if fr.flags & ffi::VPX_FRAME_IS_KEY != 0 {
                            keyframes += 1;
                        }
                        frame_bytes.extend_from_slice(slice);
                    }
                }
                bytes += frame_bytes.len();
                out.push(frame_bytes);
            }
            let elapsed = start.elapsed();
            ffi::vpx_codec_destroy(&mut ctx);
            Ok((
                Measure {
                    name: format!("{} encode (libvpx, cpu-used {cpu_used})", codec.name()),
                    frames: frames.len(),
                    elapsed,
                    bytes,
                    keyframes,
                },
                out,
            ))
        }
    }

    pub fn decode(codec: Codec, packets: &[Vec<u8>]) -> Result<Measure> {
        unsafe {
            let iface = match codec {
                Codec::Vp8 => ffi::vpx_codec_vp8_dx(),
                Codec::Vp9 => ffi::vpx_codec_vp9_dx(),
            };
            let mut ctx = std::mem::MaybeUninit::<ffi::vpx_codec_ctx>::zeroed();
            let cfg = ffi::vpx_codec_dec_cfg {
                threads: 1,
                w: 0,
                h: 0,
            };
            check(
                ffi::vpx_codec_dec_init_ver(
                    ctx.as_mut_ptr(),
                    iface,
                    &cfg,
                    0,
                    ffi::VPX_DECODER_ABI_VERSION as c_int,
                ),
                "dec_init",
            )?;
            let mut ctx = ctx.assume_init();
            let mut decoded = 0;
            let start = Instant::now();
            for p in packets {
                if p.is_empty() {
                    continue;
                }
                check(
                    ffi::vpx_codec_decode(
                        &mut ctx,
                        p.as_ptr(),
                        p.len() as c_uint,
                        ptr::null_mut(),
                        0 as c_long,
                    ),
                    "decode",
                )?;
                let mut iter: ffi::vpx_codec_iter_t = ptr::null_mut();
                loop {
                    let img = ffi::vpx_codec_get_frame(&mut ctx, &mut iter);
                    if img.is_null() {
                        break;
                    }
                    decoded += 1;
                }
            }
            let elapsed = start.elapsed();
            ffi::vpx_codec_destroy(&mut ctx);
            Ok(Measure {
                name: format!("{} decode (libvpx)", codec.name()),
                frames: decoded,
                elapsed,
                bytes: 0,
                keyframes: 0,
            })
        }
    }
}

// ─── AV1 (rav1e / dav1d) ─────────────────────────────────────────────────

mod av1 {
    use super::{Frame, Measure, BITRATE_KBPS, FPS, H, KEYFRAME_INTERVAL, W};
    use anyhow::anyhow;
    use anyhow::Result;
    use rav1e::prelude::{
        ChromaSampling, Config, EncoderConfig, EncoderStatus, FrameType, Rational, SpeedSettings,
    };
    use rav1e::Context as Rav1eContext;
    use std::time::Instant;

    pub fn encode(frames: &[Frame], speed: u8) -> Result<(Measure, Vec<Vec<u8>>)> {
        let mut enc = EncoderConfig::with_speed_preset(speed);
        enc.width = W;
        enc.height = H;
        enc.bit_depth = 8;
        enc.chroma_sampling = ChromaSampling::Cs420;
        enc.time_base = Rational::new(1, FPS as u64);
        enc.low_latency = true;
        enc.bitrate = BITRATE_KBPS as i32;
        enc.min_key_frame_interval = 0;
        enc.max_key_frame_interval = KEYFRAME_INTERVAL as u64;
        enc.speed_settings = SpeedSettings::from_preset(speed);
        let cfg = Config::new().with_encoder_config(enc).with_threads(1);
        let mut ctx: Rav1eContext<u8> = cfg.new_context().map_err(|e| anyhow!("rav1e: {e}"))?;
        let mut out = Vec::with_capacity(frames.len());
        let mut bytes = 0;
        let mut keyframes = 0;
        let start = Instant::now();
        for f in frames {
            let mut frame = ctx.new_frame();
            frame.planes[0].copy_from_raw_u8(&f.y, f.w, 1);
            frame.planes[1].copy_from_raw_u8(&f.u, f.w / 2, 1);
            frame.planes[2].copy_from_raw_u8(&f.v, f.w / 2, 1);
            ctx.send_frame(frame)
                .map_err(|e| anyhow!("rav1e send_frame: {e}"))?;
            loop {
                match ctx.receive_packet() {
                    Ok(p) => {
                        if p.frame_type == FrameType::KEY {
                            keyframes += 1;
                        }
                        bytes += p.data.len();
                        out.push(p.data);
                    }
                    Err(EncoderStatus::NeedMoreData) | Err(EncoderStatus::Encoded) => break,
                    Err(e) => return Err(anyhow!("rav1e receive_packet: {e}")),
                }
            }
        }
        ctx.flush();
        while let Ok(p) = ctx.receive_packet() {
            bytes += p.data.len();
            out.push(p.data);
        }
        let elapsed = start.elapsed();
        Ok((
            Measure {
                name: format!("AV1 encode (rav1e, speed {speed})"),
                frames: frames.len(),
                elapsed,
                bytes,
                keyframes,
            },
            out,
        ))
    }

    pub fn decode(packets: &[Vec<u8>]) -> Result<Measure> {
        let mut settings = dav1d::Settings::new();
        settings.set_n_threads(1);
        settings.set_max_frame_delay(1);
        let mut dec =
            dav1d::Decoder::with_settings(&settings).map_err(|e| anyhow!("dav1d: {e}"))?;
        let mut decoded = 0;
        let start = Instant::now();
        for p in packets {
            let mut pending = dec.send_data(p.clone(), None, None, None);
            loop {
                match dec.get_picture() {
                    Ok(pic) => {
                        let _ = pic.stride(dav1d::PlanarImageComponent::Y);
                        decoded += 1;
                    }
                    Err(dav1d::Error::Again) => {}
                    Err(e) => return Err(anyhow!("dav1d get_picture: {e}")),
                }
                match pending {
                    Err(dav1d::Error::Again) => pending = dec.send_pending_data(),
                    Err(e) => return Err(anyhow!("dav1d send_data: {e}")),
                    Ok(()) => break,
                }
            }
        }
        // Drain.
        dec.flush();
        while dec.get_picture().is_ok() {
            decoded += 1;
        }
        let elapsed = start.elapsed();
        Ok(Measure {
            name: "AV1 decode (dav1d)".into(),
            frames: decoded,
            elapsed,
            bytes: 0,
            keyframes: 0,
        })
    }
}

// ─── AV1 (SVT-AV1, system libsvtav1enc via bindgen in build.rs) ──────────

mod svt {
    #![allow(
        non_upper_case_globals,
        non_camel_case_types,
        non_snake_case,
        dead_code
    )]
    include!(concat!(env!("OUT_DIR"), "/svt.rs"));
}

mod svt_av1 {
    use super::svt::*;
    use super::{Frame, Measure, BITRATE_KBPS, FPS, H, KEYFRAME_INTERVAL, W};
    use anyhow::{anyhow, Result};
    use std::ptr;
    use std::time::Instant;

    fn check(err: EbErrorType, what: &str) -> Result<()> {
        if err == EB_ErrorNone {
            Ok(())
        } else {
            Err(anyhow!("svt-av1 {what}: error {err:#x}"))
        }
    }

    /// Collected output.
    struct Collected {
        packets: Vec<Vec<u8>>,
        bytes: usize,
        keyframes: usize,
    }

    /// `svt_av1_enc_get_packet` blocks in low-delay mode, so packets are
    /// read on their own thread until the EOS packet arrives.
    fn reader(handle: usize) -> std::thread::JoinHandle<Result<Collected>> {
        std::thread::spawn(move || unsafe {
            let handle = handle as *mut EbComponentType;
            let mut c = Collected {
                packets: Vec::new(),
                bytes: 0,
                keyframes: 0,
            };
            loop {
                let mut pkt: *mut EbBufferHeaderType = ptr::null_mut();
                let err = svt_av1_enc_get_packet(handle, &mut pkt, 0);
                if err == EB_NoErrorEmptyQueue {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                check(err, "get_packet")?;
                if pkt.is_null() {
                    continue;
                }
                let data = std::slice::from_raw_parts((*pkt).p_buffer, (*pkt).n_filled_len as usize)
                    .to_vec();
                if (*pkt).pic_type == EB_AV1_KEY_PICTURE {
                    c.keyframes += 1;
                }
                let eos = (*pkt).flags & EB_BUFFERFLAG_EOS != 0;
                c.bytes += data.len();
                if !data.is_empty() {
                    c.packets.push(data);
                }
                svt_av1_enc_release_out_buffer(&mut pkt);
                if eos {
                    return Ok(c);
                }
            }
        })
    }

    pub fn encode(frames: &[Frame], preset: i8, threads: u32) -> Result<(Measure, Vec<Vec<u8>>)> {
        unsafe {
            let mut handle: *mut EbComponentType = ptr::null_mut();
            let mut cfg = std::mem::MaybeUninit::<EbSvtAv1EncConfiguration>::zeroed();
            check(
                svt_av1_enc_init_handle(&mut handle, ptr::null_mut(), cfg.as_mut_ptr()),
                "init_handle",
            )?;
            let mut cfg = cfg.assume_init();
            cfg.source_width = W as u32;
            cfg.source_height = H as u32;
            cfg.frame_rate_numerator = FPS;
            cfg.frame_rate_denominator = 1;
            cfg.encoder_bit_depth = 8;
            cfg.encoder_color_format = EB_YUV420;
            cfg.enc_mode = preset;
            cfg.pred_structure = 1; // SVT_AV1_PRED_LOW_DELAY_B
            cfg.rate_control_mode = 2; // SVT_AV1_RC_MODE_CBR
            cfg.target_bit_rate = BITRATE_KBPS * 1000;
            cfg.intra_period_length = KEYFRAME_INTERVAL as i32 - 1;
            cfg.look_ahead_distance = 0;
            cfg.enable_tpl_la = 0;
            cfg.level_of_parallelism = threads;
            check(svt_av1_enc_set_parameter(handle, &mut cfg), "set_parameter")?;
            check(svt_av1_enc_init(handle), "init")?;

            let start = Instant::now();
            let reader = reader(handle as usize);
            for (i, f) in frames.iter().enumerate() {
                let mut y = f.y.clone();
                let mut u = f.u.clone();
                let mut v = f.v.clone();
                let mut io = EbSvtIOFormat {
                    luma: y.as_mut_ptr(),
                    cb: u.as_mut_ptr(),
                    cr: v.as_mut_ptr(),
                    y_stride: f.w as u32,
                    cb_stride: (f.w / 2) as u32,
                    cr_stride: (f.w / 2) as u32,
                    width: f.w as u32,
                    height: f.h as u32,
                    org_x: 0,
                    org_y: 0,
                    color_fmt: EB_YUV420,
                    bit_depth: EB_EIGHT_BIT,
                };
                let mut hdr: EbBufferHeaderType = std::mem::zeroed();
                hdr.size = std::mem::size_of::<EbBufferHeaderType>() as u32;
                hdr.p_buffer = &mut io as *mut EbSvtIOFormat as *mut u8;
                hdr.n_filled_len = (f.w * f.h * 3 / 2) as u32;
                hdr.n_alloc_len = hdr.n_filled_len;
                hdr.pts = i as i64;
                check(svt_av1_enc_send_picture(handle, &mut hdr), "send_picture")?;
            }
            let mut eos: EbBufferHeaderType = std::mem::zeroed();
            eos.size = std::mem::size_of::<EbBufferHeaderType>() as u32;
            eos.flags = EB_BUFFERFLAG_EOS;
            check(svt_av1_enc_send_picture(handle, &mut eos), "send eos")?;
            let collected = reader
                .join()
                .map_err(|_| anyhow!("svt-av1 reader thread panicked"))??;
            let elapsed = start.elapsed();
            svt_av1_enc_deinit(handle);
            svt_av1_enc_deinit_handle(handle);
            Ok((
                Measure {
                    name: format!("AV1 encode (SVT-AV1 preset {preset}, lp {threads})"),
                    frames: frames.len(),
                    elapsed,
                    bytes: collected.bytes,
                    keyframes: collected.keyframes,
                },
                collected.packets,
            ))
        }
    }
}

// ─── Composition (plain Rust, no SIMD) ───────────────────────────────────

mod compose {
    use super::*;

    /// Bilinear scale of one plane into a rectangle of the destination plane.
    fn scale_plane(
        src: &[u8],
        sw: usize,
        sh: usize,
        dst: &mut [u8],
        dstride: usize,
        dx: usize,
        dy: usize,
        dw: usize,
        dh: usize,
    ) {
        let fx = ((sw - 1) as u32) * 256 / (dw.max(2) - 1) as u32;
        let fy = ((sh - 1) as u32) * 256 / (dh.max(2) - 1) as u32;
        for j in 0..dh {
            let sy = j as u32 * fy;
            let y0 = (sy >> 8) as usize;
            let wy = sy & 0xFF;
            let y1 = (y0 + 1).min(sh - 1);
            let row0 = &src[y0 * sw..y0 * sw + sw];
            let row1 = &src[y1 * sw..y1 * sw + sw];
            let out = &mut dst[(dy + j) * dstride + dx..(dy + j) * dstride + dx + dw];
            for (i, o) in out.iter_mut().enumerate() {
                let sx = i as u32 * fx;
                let x0 = (sx >> 8) as usize;
                let wx = sx & 0xFF;
                let x1 = (x0 + 1).min(sw - 1);
                let top = row0[x0] as u32 * (256 - wx) + row0[x1] as u32 * wx;
                let bot = row1[x0] as u32 * (256 - wx) + row1[x1] as u32 * wx;
                *o = ((top * (256 - wy) + bot * wy) >> 16) as u8;
            }
        }
    }

    /// Composite `sources` into a `cols`×`rows` grid on a W×H canvas.
    pub fn grid(sources: &[Frame], cols: usize, rows: usize, canvas: &mut Frame) {
        let tw = (W / cols) & !1;
        let th = (H / rows) & !1;
        for (n, src) in sources.iter().enumerate().take(cols * rows) {
            let dx = (n % cols) * tw;
            let dy = (n / cols) * th;
            scale_plane(&src.y, src.w, src.h, &mut canvas.y, W, dx, dy, tw, th);
            scale_plane(
                &src.u,
                src.w / 2,
                src.h / 2,
                &mut canvas.u,
                W / 2,
                dx / 2,
                dy / 2,
                tw / 2,
                th / 2,
            );
            scale_plane(
                &src.v,
                src.w / 2,
                src.h / 2,
                &mut canvas.v,
                W / 2,
                dx / 2,
                dy / 2,
                tw / 2,
                th / 2,
            );
        }
    }

    pub fn measure(sources: &[Frame], cols: usize, rows: usize, iterations: usize) -> Measure {
        let mut canvas = Frame::new(W, H);
        let start = Instant::now();
        for _ in 0..iterations {
            grid(sources, cols, rows, &mut canvas);
        }
        Measure {
            name: format!("compose {cols}x{rows} grid → 720p"),
            frames: iterations,
            elapsed: start.elapsed(),
            bytes: 0,
            keyframes: 0,
        }
    }
}

fn main() -> Result<()> {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);
    println!(
        "forge video spike — {}x{} @ {} fps, {} frames, {} kb/s target, single-threaded\n",
        W, H, FPS, n, BITRATE_KBPS
    );
    let t = Instant::now();
    let frames: Vec<Frame> = (0..n).map(|i| synth_frame(i, W, H)).collect();
    println!("synthesised {n} frames in {:.2?}\n", t.elapsed());

    let mut results: Vec<Measure> = Vec::new();

    if std::env::var("SPIKE_ONLY_SVT").is_ok() {
        for (preset, lp) in [(12i8, 1u32), (10, 1), (8, 1)] {
            let cpu0 = cpu_seconds();
            match svt_av1::encode(&frames, preset, lp) {
                Ok((m, pk)) => {
                    report(&m);
                    let cpu = cpu_seconds() - cpu0;
                    let cpu_ns_px = cpu * 1e9 / (frames.len() as f64 * (W * H) as f64);
                    println!(
                        "  cpu {:.2} s for {:.2} s wall → {:.2} cpu-ns/px, {:.2} x 720p30 per core",
                        cpu,
                        m.elapsed.as_secs_f64(),
                        cpu_ns_px,
                        1.0 / (cpu_ns_px * 1e-9 * (W * H) as f64 * FPS as f64)
                    );
                    let m = av1::decode(&pk).context("dav1d decode of SVT-AV1")?;
                    report(&m);
                }
                Err(e) => println!("SVT-AV1 preset {preset}: {e:#}"),
            }
        }
        return Ok(());
    }

    let (m, h264) = h264::encode(&frames).context("H.264 encode")?;
    report(&m);
    results.push(m);
    let m = h264::decode(&h264).context("H.264 decode")?;
    report(&m);
    results.push(m);

    for (codec, cpu_used) in [
        (vpx::Codec::Vp8, 8),
        (vpx::Codec::Vp8, 4),
        (vpx::Codec::Vp9, 8),
    ] {
        let (m, pk) = vpx::encode(codec, &frames, cpu_used).context("libvpx encode")?;
        report(&m);
        results.push(m);
        if cpu_used == 8 {
            let m = vpx::decode(codec, &pk).context("libvpx decode")?;
            report(&m);
            results.push(m);
        }
    }

    if std::env::var("SPIKE_SKIP_AV1").is_err() {
        for speed in [10u8] {
            let (m, pk) =
                av1::encode(&frames[..frames.len().min(45)], speed).context("rav1e encode")?;
            report(&m);
            results.push(m);
            let m = av1::decode(&pk).context("dav1d decode")?;
            report(&m);
            results.push(m);
        }
    }

    for (preset, lp) in [(10i8, 1u32), (12, 1), (12, 4)] {
        match svt_av1::encode(&frames, preset, lp) {
            Ok((m, pk)) => {
                report(&m);
                results.push(m);
                if lp == 1 && preset == 12 {
                    let m = av1::decode(&pk).context("dav1d decode of SVT-AV1")?;
                    report(&m);
                }
            }
            Err(e) => println!("SVT-AV1 preset {preset}: {e:#}"),
        }
    }

    println!();
    let sources: Vec<Frame> = frames.iter().take(16).cloned().collect();
    for (c, r) in [(1, 1), (2, 2), (3, 3), (4, 4)] {
        let m = compose::measure(&sources, c, r, 60);
        report(&m);
        results.push(m);
    }

    // Do cores add up? Four independent H.264 encoders at once.
    println!();
    let t = Instant::now();
    let per: Vec<f64> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| s.spawn(|| h264::encode(&frames).map(|(m, _)| m.fps())))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap().unwrap()).collect()
    });
    let wall = t.elapsed();
    println!(
        "4 parallel H.264 encoders: {:.1} fps each ({:.1} aggregate) in {:.2?}; single was {:.1} fps",
        per.iter().sum::<f64>() / 4.0,
        per.iter().sum::<f64>(),
        wall,
        results[0].fps()
    );

    println!("\ncapacity constants (ns per pixel per frame, one core):");
    for m in &results {
        println!("  k[{:<38}] = {:>6.2}", m.name, m.ns_per_pixel());
    }
    Ok(())
}
