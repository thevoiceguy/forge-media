//! VP8 and VP9 through libvpx.

use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use forge_video::codec::{
    CodecError, CodecRegistry, DecoderFactory, EncoderFactory, EncoderSettings, VideoDecoder,
    VideoEncoder,
};
use forge_video::frame::{HostFrame, MediaDevice, VideoFrame};
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::os::raw::{c_int, c_long, c_uint, c_ulong};
use std::ptr;
use vpx_sys as ffi;

fn check(err: ffi::vpx_codec_err_t, what: &str) -> Result<(), CodecError> {
    if err == ffi::vpx_codec_err_t::VPX_CODEC_OK {
        Ok(())
    } else {
        let msg = unsafe { CStr::from_ptr(ffi::vpx_codec_err_to_string(err)) };
        Err(CodecError::Codec(format!(
            "libvpx {what}: {}",
            msg.to_string_lossy()
        )))
    }
}

fn iface_enc(codec: VideoCodec) -> *const ffi::vpx_codec_iface {
    unsafe {
        match codec {
            VideoCodec::VP9 => ffi::vpx_codec_vp9_cx(),
            _ => ffi::vpx_codec_vp8_cx(),
        }
    }
}

fn iface_dec(codec: VideoCodec) -> *const ffi::vpx_codec_iface {
    unsafe {
        match codec {
            VideoCodec::VP9 => ffi::vpx_codec_vp9_dx(),
            _ => ffi::vpx_codec_vp8_dx(),
        }
    }
}

/// libvpx encoder. Real-time settings: no lag, CBR, cpu-used 8 (VP8: the
/// fastest that still holds its bitrate; measured in phase 0).
pub struct VpxEncoder {
    codec: VideoCodec,
    settings: EncoderSettings,
    ctx: ffi::vpx_codec_ctx,
    cfg: ffi::vpx_codec_enc_cfg,
    frames: i64,
}

unsafe impl Send for VpxEncoder {}

impl VpxEncoder {
    pub fn new(codec: VideoCodec, settings: EncoderSettings) -> Result<Self, CodecError> {
        settings.validate()?;
        unsafe {
            let iface = iface_enc(codec);
            let mut cfg = MaybeUninit::<ffi::vpx_codec_enc_cfg>::uninit();
            check(
                ffi::vpx_codec_enc_config_default(iface, cfg.as_mut_ptr(), 0),
                "config_default",
            )?;
            let mut cfg = cfg.assume_init();
            cfg.g_w = settings.resolution.width;
            cfg.g_h = settings.resolution.height;
            cfg.g_timebase = ffi::vpx_rational {
                num: 1,
                den: settings.fps as c_int,
            };
            cfg.g_threads = 1;
            cfg.g_lag_in_frames = 0;
            cfg.g_error_resilient = 0;
            cfg.rc_end_usage = ffi::vpx_rc_mode::VPX_CBR;
            cfg.rc_target_bitrate = settings.bitrate_kbps;
            cfg.rc_min_quantizer = 4;
            cfg.rc_max_quantizer = 56;
            // Real-time buffers (libvpx defaults are six seconds deep,
            // which lets the rate drift far from the target for whole
            // seconds): a one-second buffer and tight over/undershoot.
            cfg.rc_buf_sz = 1000;
            cfg.rc_buf_initial_sz = 500;
            cfg.rc_buf_optimal_sz = 600;
            cfg.rc_undershoot_pct = 50;
            cfg.rc_overshoot_pct = 50;
            cfg.rc_dropframe_thresh = 0;
            cfg.rc_resize_allowed = 0;
            cfg.kf_max_dist = settings.keyframe_interval.max(1);
            let mut ctx = MaybeUninit::<ffi::vpx_codec_ctx>::zeroed();
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
                    8 as c_int,
                ),
                "set cpu-used",
            )?;
            if codec == VideoCodec::VP9 {
                let _ = ffi::vpx_codec_control_(
                    &mut ctx,
                    ffi::vp8e_enc_control_id::VP9E_SET_ROW_MT as c_int,
                    0 as c_int,
                );
                // Cyclic-refresh adaptive quantization: what real-time
                // VP9 uses to hold CBR without frame drops.
                let _ = ffi::vpx_codec_control_(
                    &mut ctx,
                    ffi::vp8e_enc_control_id::VP9E_SET_AQ_MODE as c_int,
                    3 as c_int,
                );
            }
            Ok(Self {
                codec,
                settings,
                ctx,
                cfg,
                frames: 0,
            })
        }
    }
}

impl Drop for VpxEncoder {
    fn drop(&mut self) {
        unsafe {
            ffi::vpx_codec_destroy(&mut self.ctx);
        }
    }
}

impl VideoEncoder for VpxEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn settings(&self) -> &EncoderSettings {
        &self.settings
    }
    fn encode(
        &mut self,
        frame: &VideoFrame,
        keyframe: bool,
    ) -> Result<Vec<CodedFrame>, CodecError> {
        let host = frame.as_host().ok_or_else(|| CodecError::WrongDevice {
            expected: MediaDevice::Host,
            actual: frame.device(),
        })?;
        let r = self.settings.resolution;
        let scaled;
        let src = if host.width == r.width && host.height == r.height {
            host
        } else {
            scaled = forge_video::scale::resize(host, r.width, r.height);
            &scaled
        };
        let mut packed = src.to_i420();
        unsafe {
            let mut img = MaybeUninit::<ffi::vpx_image>::zeroed();
            let p = ffi::vpx_img_wrap(
                img.as_mut_ptr(),
                ffi::vpx_img_fmt::VPX_IMG_FMT_I420,
                src.width as c_uint,
                src.height as c_uint,
                1,
                packed.as_mut_ptr(),
            );
            if p.is_null() {
                return Err(CodecError::Codec("vpx_img_wrap failed".into()));
            }
            let img = img.assume_init();
            let flags: c_long = if keyframe {
                ffi::VPX_EFLAG_FORCE_KF as c_long
            } else {
                0
            };
            check(
                ffi::vpx_codec_encode(
                    &mut self.ctx,
                    &img,
                    self.frames,
                    1,
                    flags,
                    ffi::VPX_DL_REALTIME as c_ulong,
                ),
                "encode",
            )?;
            self.frames += 1;
            let mut out = Vec::new();
            let mut iter: ffi::vpx_codec_iter_t = ptr::null_mut();
            loop {
                let pkt = ffi::vpx_codec_get_cx_data(&mut self.ctx, &mut iter);
                if pkt.is_null() {
                    break;
                }
                if (*pkt).kind == ffi::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                    let fr = (*pkt).data.frame;
                    let data = std::slice::from_raw_parts(fr.buf as *const u8, fr.sz);
                    out.push(CodedFrame {
                        timestamp: host.pts,
                        keyframe: fr.flags & ffi::VPX_FRAME_IS_KEY != 0,
                        data: bytes::Bytes::copy_from_slice(data),
                    });
                }
            }
            Ok(out)
        }
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == 0 {
            return Err(CodecError::InvalidConfig("bitrate is zero".into()));
        }
        self.cfg.rc_target_bitrate = kbps;
        unsafe {
            check(
                ffi::vpx_codec_enc_config_set(&mut self.ctx, &self.cfg),
                "config_set",
            )?;
        }
        self.settings.bitrate_kbps = kbps;
        Ok(())
    }
}

/// libvpx decoder.
pub struct VpxDecoder {
    codec: VideoCodec,
    ctx: ffi::vpx_codec_ctx,
}

unsafe impl Send for VpxDecoder {}

impl VpxDecoder {
    pub fn new(codec: VideoCodec) -> Result<Self, CodecError> {
        unsafe {
            let cfg = ffi::vpx_codec_dec_cfg {
                threads: 1,
                w: 0,
                h: 0,
            };
            let mut ctx = MaybeUninit::<ffi::vpx_codec_ctx>::zeroed();
            check(
                ffi::vpx_codec_dec_init_ver(
                    ctx.as_mut_ptr(),
                    iface_dec(codec),
                    &cfg,
                    0,
                    ffi::VPX_DECODER_ABI_VERSION as c_int,
                ),
                "dec_init",
            )?;
            Ok(Self {
                codec,
                ctx: ctx.assume_init(),
            })
        }
    }
}

impl Drop for VpxDecoder {
    fn drop(&mut self) {
        unsafe {
            ffi::vpx_codec_destroy(&mut self.ctx);
        }
    }
}

/// Copy a libvpx image into a host frame.
unsafe fn image_to_frame(img: *const ffi::vpx_image, pts: u32) -> Option<HostFrame> {
    let img = &*img;
    let (w, h) = (img.d_w, img.d_h);
    if w == 0 || h == 0 {
        return None;
    }
    let mut f = HostFrame::black(w, h).with_pts(pts);
    let (w, h) = (f.width as usize, f.height as usize);
    for row in 0..h {
        let src = std::slice::from_raw_parts(img.planes[0].add(row * img.stride[0] as usize), w);
        f.y[row * f.y_stride..row * f.y_stride + w].copy_from_slice(src);
    }
    for (plane, dst, stride) in [
        (1usize, &mut f.u, img.stride[1]),
        (2, &mut f.v, img.stride[2]),
    ] {
        for row in 0..h / 2 {
            let src =
                std::slice::from_raw_parts(img.planes[plane].add(row * stride as usize), w / 2);
            dst[row * (w / 2)..row * (w / 2) + w / 2].copy_from_slice(src);
        }
    }
    Some(f)
}

impl VideoDecoder for VpxDecoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn decode(&mut self, frame: &CodedFrame) -> Result<Option<VideoFrame>, CodecError> {
        if frame.data.is_empty() {
            return Ok(None);
        }
        unsafe {
            check(
                ffi::vpx_codec_decode(
                    &mut self.ctx,
                    frame.data.as_ptr(),
                    frame.data.len() as c_uint,
                    ptr::null_mut(),
                    0,
                ),
                "decode",
            )?;
            let mut iter: ffi::vpx_codec_iter_t = ptr::null_mut();
            let mut last = None;
            loop {
                let img = ffi::vpx_codec_get_frame(&mut self.ctx, &mut iter);
                if img.is_null() {
                    break;
                }
                if let Some(f) = image_to_frame(img, frame.timestamp) {
                    last = Some(VideoFrame::Host(f));
                }
            }
            Ok(last)
        }
    }
    fn reset(&mut self) {
        if let Ok(fresh) = VpxDecoder::new(self.codec) {
            *self = fresh;
        }
    }
}

struct Factory(VideoCodec);

impl DecoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        self.0
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self) -> Result<Box<dyn VideoDecoder>, CodecError> {
        Ok(Box::new(VpxDecoder::new(self.0)?))
    }
}

impl EncoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        self.0
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self, settings: &EncoderSettings) -> Result<Box<dyn VideoEncoder>, CodecError> {
        Ok(Box::new(VpxEncoder::new(self.0, settings.clone())?))
    }
}

pub fn register(registry: &mut CodecRegistry) {
    for codec in [VideoCodec::VP8, VideoCodec::VP9] {
        registry.register_decoder(Box::new(Factory(codec)));
        registry.register_encoder(Box::new(Factory(codec)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsrc;

    #[test]
    fn vp8_and_vp9_round_trip_with_forced_keyframes_and_bitrate_change() {
        for codec in [VideoCodec::VP8, VideoCodec::VP9] {
            let s = testsrc::settings(codec, 320, 180);
            let mut enc = VpxEncoder::new(codec, s).unwrap();
            let mut dec = VpxDecoder::new(codec).unwrap();
            let (decoded, keyframes, psnr) = testsrc::round_trip(&mut enc, &mut dec, 30, 320, 180);
            assert_eq!(decoded, 30, "{codec}");
            assert!(
                keyframes >= 2,
                "{codec}: first frame + forced = {keyframes}"
            );
            assert!(psnr > 28.0, "{codec}: psnr {psnr}");
            enc.set_bitrate(200).unwrap();
            assert_eq!(enc.settings().bitrate_kbps, 200);
            assert!(enc.set_bitrate(0).is_err());
            dec.reset();
        }
    }
}
