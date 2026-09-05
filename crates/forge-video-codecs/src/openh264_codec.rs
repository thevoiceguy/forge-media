//! H.264 through OpenH264 (built from source). Constrained baseline /
//! main, no B-frames: what SIP phones want.

use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use forge_video::codec::{
    CodecError, CodecRegistry, DecoderFactory, EncoderFactory, EncoderSettings, VideoDecoder,
    VideoEncoder,
};
use forge_video::frame::{HostFrame, MediaDevice, VideoFrame};
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Level, Profile,
    RateControlMode, UsageType,
};
use openh264::formats::{YUVSlices, YUVSource};
use openh264::OpenH264API;

fn oh_err(what: &str, e: impl std::fmt::Display) -> CodecError {
    CodecError::Codec(format!("openh264 {what}: {e}"))
}

/// Profile from the negotiated `profile-level-id` (baseline unless the
/// idc says main or high; OpenH264 encodes up to high).
fn profile_for(fmtp: &str) -> Profile {
    let idc = fmtp
        .split(';')
        .find_map(|kv| kv.trim().strip_prefix("profile-level-id="))
        .and_then(|v| u32::from_str_radix(v.trim(), 16).ok())
        .map(|p| (p >> 16) as u8);
    match idc {
        Some(77) => Profile::Main,
        Some(100) => Profile::High,
        _ => Profile::Baseline,
    }
}

fn build_config(settings: &EncoderSettings) -> EncoderConfig {
    EncoderConfig::new()
        .bitrate(BitRate::from_bps(settings.bitrate_kbps * 1000))
        .max_frame_rate(FrameRate::from_hz(settings.fps as f32))
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .profile(profile_for(&settings.profile))
        .level(Level::Level_3_1)
        .complexity(Complexity::Low)
        .intra_frame_period(IntraFramePeriod::from_num_frames(
            settings.keyframe_interval.max(1),
        ))
        .scene_change_detect(false)
        .skip_frames(true)
        .num_threads(1)
}

pub struct H264Encoder {
    settings: EncoderSettings,
    encoder: Encoder,
    /// Recreate before the next frame (bitrate changed).
    reinit: bool,
}

impl H264Encoder {
    pub fn new(settings: EncoderSettings) -> Result<Self, CodecError> {
        settings.validate()?;
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), build_config(&settings))
            .map_err(|e| oh_err("encoder", e))?;
        Ok(Self {
            settings,
            encoder,
            reinit: false,
        })
    }
}

/// Whether an access unit holds an IDR slice.
fn has_idr(annexb: &[u8]) -> bool {
    forge_rtp::video::payload::annexb_nal_units(annexb)
        .iter()
        .any(|n| n.first().map(|b| b & 0x1F == 5).unwrap_or(false))
}

impl VideoEncoder for H264Encoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
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
        if self.reinit {
            self.encoder =
                Encoder::with_api_config(OpenH264API::from_source(), build_config(&self.settings))
                    .map_err(|e| oh_err("encoder", e))?;
            self.reinit = false;
        }
        let r = self.settings.resolution;
        let scaled;
        let src = if host.width == r.width && host.height == r.height {
            host
        } else {
            scaled = forge_video::scale::resize(host, r.width, r.height);
            &scaled
        };
        if keyframe {
            self.encoder.force_intra_frame();
        }
        let slices = YUVSlices::new(
            (&src.y, &src.u, &src.v),
            (src.width as usize, src.height as usize),
            (src.y_stride, src.uv_stride, src.uv_stride),
        );
        let bs = self
            .encoder
            .encode(&slices)
            .map_err(|e| oh_err("encode", e))?;
        let data = bs.to_vec();
        if data.is_empty() {
            // Rate control skipped this frame: the receiver repeats the last.
            return Ok(Vec::new());
        }
        let keyframe = has_idr(&data);
        Ok(vec![CodedFrame {
            timestamp: host.pts,
            keyframe,
            data: bytes::Bytes::from(data),
        }])
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == 0 {
            return Err(CodecError::InvalidConfig("bitrate is zero".into()));
        }
        if kbps != self.settings.bitrate_kbps {
            self.settings.bitrate_kbps = kbps;
            // The safe API sets bitrate at creation; recreating costs one
            // keyframe, which a retarget usually wants anyway.
            self.reinit = true;
        }
        Ok(())
    }
}

pub struct H264Decoder {
    decoder: Decoder,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            decoder: Decoder::new().map_err(|e| oh_err("decoder", e))?,
        })
    }
}

impl VideoDecoder for H264Decoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn decode(&mut self, frame: &CodedFrame) -> Result<Option<VideoFrame>, CodecError> {
        let yuv = match self.decoder.decode(&frame.data) {
            Ok(Some(y)) => y,
            Ok(None) => return Ok(None),
            Err(e) => return Err(oh_err("decode", e)),
        };
        let (w, h) = yuv.dimensions();
        let (ys, us, vs) = yuv.strides();
        let mut f = HostFrame::black(w as u32, h as u32).with_pts(frame.timestamp);
        let (w, h) = (f.width as usize, f.height as usize);
        for row in 0..h {
            f.y[row * f.y_stride..row * f.y_stride + w]
                .copy_from_slice(&yuv.y()[row * ys..row * ys + w]);
        }
        for row in 0..h / 2 {
            f.u[row * f.uv_stride..row * f.uv_stride + w / 2]
                .copy_from_slice(&yuv.u()[row * us..row * us + w / 2]);
            f.v[row * f.uv_stride..row * f.uv_stride + w / 2]
                .copy_from_slice(&yuv.v()[row * vs..row * vs + w / 2]);
        }
        Ok(Some(VideoFrame::Host(f)))
    }
    fn reset(&mut self) {
        if let Ok(d) = Decoder::new() {
            self.decoder = d;
        }
    }
}

struct Factory;

impl DecoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self) -> Result<Box<dyn VideoDecoder>, CodecError> {
        Ok(Box::new(H264Decoder::new()?))
    }
}

impl EncoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self, settings: &EncoderSettings) -> Result<Box<dyn VideoEncoder>, CodecError> {
        Ok(Box::new(H264Encoder::new(settings.clone())?))
    }
}

pub fn register(registry: &mut CodecRegistry) {
    registry.register_decoder(Box::new(Factory));
    registry.register_encoder(Box::new(Factory));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsrc;

    #[test]
    fn h264_round_trips_with_a_forced_keyframe() {
        let mut s = testsrc::settings(VideoCodec::H264, 320, 180);
        s.profile = "profile-level-id=42e01f;packetization-mode=1".into();
        let mut enc = H264Encoder::new(s).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let (decoded, keyframes, psnr) = testsrc::round_trip(&mut enc, &mut dec, 30, 320, 180);
        assert!(decoded >= 25, "decoded {decoded}");
        assert!(keyframes >= 2, "keyframes {keyframes}");
        assert!(psnr > 28.0, "psnr {psnr}");
        enc.set_bitrate(300).unwrap();
        let src = testsrc::synth(99, 320, 180);
        let after = enc.encode(&VideoFrame::Host(src), false).unwrap();
        assert!(
            after.iter().any(|p| p.keyframe),
            "recreated encoder starts with an IDR"
        );
        assert!(matches!(
            profile_for("profile-level-id=64001f"),
            Profile::High
        ));
        assert!(matches!(profile_for(""), Profile::Baseline));
    }
}
