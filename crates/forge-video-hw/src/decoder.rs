//! Decoders whose output stays on the device: FFmpeg's `h264`, `hevc`,
//! `vp8`, `vp9` and `av1` decoders with the device's hardware
//! acceleration (NVDEC on CUDA).

use crate::device::HwDevice;
use crate::ffi::{self, CodecContext, Frame, Packet};
use crate::frame::wrap;
use ffmpeg_sys_next as ff;
use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use forge_video::codec::{CodecError, DecoderFactory, VideoDecoder};
use forge_video::frame::{MediaDevice, VideoFrame};
use std::os::raw::c_int;
use std::sync::Arc;

fn decoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "hevc",
        VideoCodec::VP8 => "vp8",
        VideoCodec::VP9 => "vp9",
        VideoCodec::AV1 => "av1",
    }
}

fn find(codec: VideoCodec) -> Option<*const ff::AVCodec> {
    let name = ffi::cstr(decoder_name(codec));
    let p = unsafe { ff::avcodec_find_decoder_by_name(name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

/// Whether FFmpeg's decoder for `codec` accelerates on `device`'s kind.
pub fn available(device: &HwDevice, codec: VideoCodec) -> bool {
    let Some(dec) = find(codec) else {
        return false;
    };
    let mut i = 0;
    loop {
        let cfg = unsafe { ff::avcodec_get_hw_config(dec, i) };
        if cfg.is_null() {
            return false;
        }
        let cfg = unsafe { &*cfg };
        if cfg.device_type == device.kind()
            && cfg.methods & ff::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int != 0
        {
            return true;
        }
        i += 1;
    }
}

/// The pixel format the device wants, chosen from what the decoder offers.
unsafe extern "C" fn pick_hw_format(
    ctx: *mut ff::AVCodecContext,
    formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // The wanted format rides in `opaque` as its integer value.
    let want = (*ctx).opaque as usize as i32;
    let mut p = formats;
    while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *p as i32 == want {
            return *p;
        }
        p = p.add(1);
    }
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}

/// A hardware decoder for one codec.
pub struct HwDecoder {
    device: Arc<HwDevice>,
    codec: VideoCodec,
    ctx: CodecContext,
}

impl HwDecoder {
    pub fn new(device: Arc<HwDevice>, codec: VideoCodec) -> Result<HwDecoder, CodecError> {
        let dec = find(codec).ok_or(CodecError::Unavailable {
            codec,
            role: "decoder",
            device: device.device().clone(),
        })?;
        if !available(&device, codec) {
            return Err(CodecError::Unavailable {
                codec,
                role: "hardware decoder",
                device: device.device().clone(),
            });
        }
        let raw = unsafe { ff::avcodec_alloc_context3(dec) };
        if raw.is_null() {
            return Err(CodecError::Codec("avcodec_alloc_context3 failed".into()));
        }
        let ctx = CodecContext(raw);
        unsafe {
            (*raw).hw_device_ctx = device.context().into_raw();
            (*raw).get_format = Some(pick_hw_format);
            (*raw).opaque = device.pix_fmt() as i32 as usize as *mut _;
            (*raw).pkt_timebase = ff::AVRational {
                num: 1,
                den: 90_000,
            };
            (*raw).time_base = ff::AVRational {
                num: 1,
                den: 90_000,
            };
            // Low delay: a frame out per frame in, as a mixer needs.
            (*raw).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
        }
        ffi::check(&format!("open {} decoder", decoder_name(codec)), unsafe {
            ff::avcodec_open2(raw, dec, std::ptr::null_mut())
        })?;
        Ok(HwDecoder { device, codec, ctx })
    }
}

impl VideoDecoder for HwDecoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn decode(&mut self, frame: &CodedFrame) -> Result<Option<VideoFrame>, CodecError> {
        let pkt = Packet::from_data(&frame.data)?;
        unsafe {
            (*pkt.0).pts = frame.timestamp as i64;
            (*pkt.0).dts = frame.timestamp as i64;
            if frame.keyframe {
                (*pkt.0).flags |= ff::AV_PKT_FLAG_KEY as c_int;
            }
        }
        let rc = unsafe { ff::avcodec_send_packet(self.ctx.0, pkt.0) };
        if rc < 0 && rc != ff::AVERROR(ff::EAGAIN) {
            // A frame the decoder cannot use (loss before a keyframe) is
            // not the end of the stream.
            tracing::debug!(codec = %self.codec, "hardware decode refused a frame: {}", ffi::av_error("", rc));
            return Ok(None);
        }
        let mut last = None;
        loop {
            let out = Frame::new()?;
            let rc = unsafe { ff::avcodec_receive_frame(self.ctx.0, out.0) };
            if rc == ff::AVERROR(ff::EAGAIN) || rc == ff::AVERROR_EOF {
                break;
            }
            ffi::check("avcodec_receive_frame", rc)?;
            let pts = unsafe { (*out.0).pts };
            let pts = if pts < 0 { frame.timestamp } else { pts as u32 };
            last = Some(wrap(self.device.device(), out, pts));
        }
        Ok(last)
    }

    fn reset(&mut self) {
        unsafe { ff::avcodec_flush_buffers(self.ctx.0) };
    }
}

pub struct Factory {
    pub device: Arc<HwDevice>,
    pub codec: VideoCodec,
}

impl DecoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn create(&self) -> Result<Box<dyn VideoDecoder>, CodecError> {
        Ok(Box::new(HwDecoder::new(
            Arc::clone(&self.device),
            self.codec,
        )?))
    }
}
