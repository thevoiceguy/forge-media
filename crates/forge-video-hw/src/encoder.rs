//! Encoders that take frames from the device: `h264_nvenc`,
//! `hevc_nvenc` and `av1_nvenc`.
//!
//! What forge-video's encoders promise, kept here: one packet out per
//! frame in (zero latency, no B-frames), a keyframe on demand as an IDR
//! with its parameter sets in-band, [`ContentHint`] mapped to NVENC's
//! tuning, and a bitrate retarget — which NVENC's wrapper cannot do to a
//! live encoder — by reopening on the next frame, which is then a
//! keyframe (§15.7, decision 6).

use crate::device::{HwDevice, HwFrames};
use crate::ffi::{self, CodecContext, Frame, Packet};
use crate::frame::{hw_frame, upload};
use ffmpeg_sys_next as ff;
use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use forge_video::codec::{CodecError, ContentHint, EncoderFactory, EncoderSettings, VideoEncoder};
use forge_video::frame::{MediaDevice, VideoFrame};
use std::os::raw::c_int;
use std::sync::Arc;

fn encoder_name(codec: VideoCodec) -> Option<&'static str> {
    match codec {
        VideoCodec::H264 => Some("h264_nvenc"),
        VideoCodec::H265 => Some("hevc_nvenc"),
        VideoCodec::AV1 => Some("av1_nvenc"),
        VideoCodec::VP8 | VideoCodec::VP9 => None,
    }
}

fn find(codec: VideoCodec) -> Option<*const ff::AVCodec> {
    let name = ffi::cstr(encoder_name(codec)?);
    let p = unsafe { ff::avcodec_find_encoder_by_name(name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

/// Whether this FFmpeg has an NVENC encoder for `codec`. (Whether the
/// GPU does is found out when it opens.)
pub fn available(codec: VideoCodec) -> bool {
    find(codec).is_some()
}

/// The H.264 profile an fmtp asks for (`profile-level-id`), as NVENC
/// names it.
fn h264_profile(fmtp: &str) -> &'static str {
    let idc = fmtp
        .split(';')
        .find_map(|p| p.trim().strip_prefix("profile-level-id="))
        .and_then(|v| u8::from_str_radix(v.get(..2)?, 16).ok());
    match idc {
        Some(0x42) => "baseline",
        Some(0x4d) => "main",
        Some(0x64) => "high",
        _ => "baseline",
    }
}

fn set_opt(priv_data: *mut std::ffi::c_void, name: &str, value: &str) -> Result<(), CodecError> {
    let (n, v) = (ffi::cstr(name), ffi::cstr(value));
    ffi::check(&format!("nvenc option {name}={value}"), unsafe {
        ff::av_opt_set(priv_data, n.as_ptr(), v.as_ptr(), 0)
    })
}

/// An NVENC encoder for one flavor.
pub struct NvEncoder {
    device: Arc<HwDevice>,
    settings: EncoderSettings,
    pool: HwFrames,
    ctx: CodecContext,
    /// Reopen before the next frame (a bitrate change).
    reinit: bool,
}

impl NvEncoder {
    pub fn new(device: Arc<HwDevice>, settings: EncoderSettings) -> Result<NvEncoder, CodecError> {
        settings.validate()?;
        let pool = device.frames(settings.resolution)?;
        let ctx = Self::open(&device, &pool, &settings)?;
        Ok(NvEncoder {
            device,
            settings,
            pool,
            ctx,
            reinit: false,
        })
    }

    fn open(
        device: &HwDevice,
        pool: &HwFrames,
        s: &EncoderSettings,
    ) -> Result<CodecContext, CodecError> {
        let enc = find(s.codec).ok_or(CodecError::Unavailable {
            codec: s.codec,
            role: "hardware encoder",
            device: device.device().clone(),
        })?;
        let raw = unsafe { ff::avcodec_alloc_context3(enc) };
        if raw.is_null() {
            return Err(CodecError::Codec("avcodec_alloc_context3 failed".into()));
        }
        let ctx = CodecContext(raw);
        let bps = s.bitrate_kbps as i64 * 1000;
        unsafe {
            let c = &mut *raw;
            c.width = s.resolution.width as c_int;
            c.height = s.resolution.height as c_int;
            c.time_base = ff::AVRational {
                num: 1,
                den: 90_000,
            };
            c.framerate = ff::AVRational {
                num: s.fps as c_int,
                den: 1,
            };
            c.pix_fmt = device.pix_fmt();
            c.hw_frames_ctx = pool.context().into_raw();
            c.bit_rate = bps;
            c.rc_max_rate = bps;
            c.rc_buffer_size = (bps / s.fps.max(1) as i64 * 2) as c_int;
            c.gop_size = s.keyframe_interval as c_int;
            c.max_b_frames = 0;
            c.refs = 1;
            // Parameter sets in-band with every keyframe, as RTP wants.
            c.flags &= !(ff::AV_CODEC_FLAG_GLOBAL_HEADER as c_int);
        }
        let p = unsafe { (*raw).priv_data };
        // The low-latency tunings: p1 is fastest, p7 best; p4 is the
        // middle a mixer can afford many of. A screen gets p5 and
        // spatial AQ, which keeps text edges at the same bitrate.
        let (preset, aq) = match s.content {
            ContentHint::Camera => ("p4", "0"),
            ContentHint::Screen => ("p5", "1"),
        };
        set_opt(p, "preset", preset)?;
        set_opt(p, "tune", "ll")?;
        set_opt(p, "rc", "cbr")?;
        set_opt(p, "zerolatency", "1")?;
        set_opt(p, "delay", "0")?;
        set_opt(p, "forced-idr", "1")?;
        set_opt(p, "spatial-aq", aq)?;
        // Without a global header NVENC repeats SPS/PPS (or VPS) on
        // every IDR, which is what an RTP receiver joining late needs.
        match s.codec {
            VideoCodec::H264 => set_opt(p, "profile", h264_profile(&s.profile))?,
            VideoCodec::H265 => set_opt(p, "profile", "main")?,
            _ => {}
        }
        ffi::check(
            &format!("open {}", encoder_name(s.codec).unwrap_or("nvenc")),
            unsafe { ff::avcodec_open2(raw, enc, std::ptr::null_mut()) },
        )?;
        Ok(ctx)
    }

    fn drain(&mut self, out: &mut Vec<CodedFrame>) -> Result<(), CodecError> {
        loop {
            let pkt = Packet::new()?;
            let rc = unsafe { ff::avcodec_receive_packet(self.ctx.0, pkt.0) };
            if rc == ff::AVERROR(ff::EAGAIN) || rc == ff::AVERROR_EOF {
                return Ok(());
            }
            ffi::check("avcodec_receive_packet", rc)?;
            let (pts, flags) = unsafe { ((*pkt.0).pts, (*pkt.0).flags) };
            out.push(CodedFrame {
                timestamp: pts.max(0) as u32,
                keyframe: flags & ff::AV_PKT_FLAG_KEY as c_int != 0,
                data: bytes::Bytes::copy_from_slice(pkt.data()),
            });
        }
    }
}

impl VideoEncoder for NvEncoder {
    fn codec(&self) -> VideoCodec {
        self.settings.codec
    }

    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn settings(&self) -> &EncoderSettings {
        &self.settings
    }

    fn encode(
        &mut self,
        frame: &VideoFrame,
        keyframe: bool,
    ) -> Result<Vec<CodedFrame>, CodecError> {
        let mut keyframe = keyframe;
        if self.reinit {
            self.ctx = Self::open(&self.device, &self.pool, &self.settings)?;
            self.reinit = false;
            keyframe = true;
        }
        // A host frame is uploaded — the bench and the tests feed those;
        // a room keeps its frames on the device.
        let uploaded;
        let (raw, pts) = match frame {
            VideoFrame::Host(h) => {
                let scaled;
                let src = if h.resolution() == self.settings.resolution {
                    h
                } else {
                    scaled = forge_video::scale::resize(
                        h,
                        self.settings.resolution.width,
                        self.settings.resolution.height,
                    );
                    &scaled
                };
                uploaded = upload(&self.device, &self.pool, src)?;
                (hw_frame(&uploaded, self.device.device())?.raw(), h.pts)
            }
            VideoFrame::Device(d) => (hw_frame(frame, self.device.device())?.raw(), d.pts),
        };
        // The encoder's view of the frame: its own reference, so a
        // forced keyframe does not mark the shared one.
        let input = Frame::new()?;
        ffi::check("av_frame_ref", unsafe { ff::av_frame_ref(input.0, raw) })?;
        unsafe {
            (*input.0).pts = pts as i64;
            (*input.0).pict_type = if keyframe {
                ff::AVPictureType::AV_PICTURE_TYPE_I
            } else {
                ff::AVPictureType::AV_PICTURE_TYPE_NONE
            };
            if keyframe {
                (*input.0).flags |= ff::AV_FRAME_FLAG_KEY as c_int;
            }
        }
        ffi::check("avcodec_send_frame", unsafe {
            ff::avcodec_send_frame(self.ctx.0, input.0)
        })?;
        let mut out = Vec::with_capacity(1);
        self.drain(&mut out)?;
        Ok(out)
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == 0 {
            return Err(CodecError::InvalidConfig("bitrate 0".into()));
        }
        if kbps != self.settings.bitrate_kbps {
            self.settings.bitrate_kbps = kbps;
            self.reinit = true;
        }
        Ok(())
    }
}

pub struct Factory {
    pub device: Arc<HwDevice>,
    pub codec: VideoCodec,
}

impl EncoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn create(&self, settings: &EncoderSettings) -> Result<Box<dyn VideoEncoder>, CodecError> {
        if settings.codec != self.codec {
            return Err(CodecError::InvalidConfig(format!(
                "{} settings given to the {} encoder",
                settings.codec, self.codec
            )));
        }
        Ok(Box::new(NvEncoder::new(
            Arc::clone(&self.device),
            settings.clone(),
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_follows_the_fmtp() {
        assert_eq!(
            h264_profile("profile-level-id=42e01f;packetization-mode=1"),
            "baseline"
        );
        assert_eq!(
            h264_profile("packetization-mode=1;profile-level-id=4d001f"),
            "main"
        );
        assert_eq!(h264_profile("profile-level-id=640028"), "high");
        assert_eq!(h264_profile(""), "baseline");
    }
}
