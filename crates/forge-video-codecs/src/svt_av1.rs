//! AV1 encode through SVT-AV1 (system library, bindings generated in
//! `build.rs`). Low-delay CBR at preset 12: 2–4 720p30 streams per core
//! measured in phase 0.
//!
//! In low-delay mode `svt_av1_enc_get_packet` blocks, so packets are
//! collected on a reader thread and handed back through a channel; the
//! encoder returns whatever has arrived by the time the next frame is
//! sent (one frame of latency at most).

#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code
)]

mod sys {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/svt.rs"));
}

use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use forge_video::codec::{
    CodecError, CodecRegistry, EncoderFactory, EncoderSettings, VideoEncoder,
};
use forge_video::frame::{MediaDevice, VideoFrame};
use std::ptr;
use std::sync::mpsc;
use sys::*;

fn check(err: EbErrorType, what: &str) -> Result<(), CodecError> {
    if err == EB_ErrorNone {
        Ok(())
    } else {
        Err(CodecError::Codec(format!("svt-av1 {what}: error {err:#x}")))
    }
}

/// Send-safe wrapper for the encoder handle.
#[derive(Clone, Copy)]
struct Handle(*mut EbComponentType);
unsafe impl Send for Handle {}

impl Handle {
    fn ptr(&self) -> *mut EbComponentType {
        self.0
    }
}

struct Packet {
    data: Vec<u8>,
    keyframe: bool,
    pts: i64,
    eos: bool,
}

fn spawn_reader(handle: Handle) -> (std::thread::JoinHandle<()>, mpsc::Receiver<Packet>) {
    let (tx, rx) = mpsc::channel();
    let t = std::thread::spawn(move || unsafe {
        // Capture the wrapper whole (edition 2021 would otherwise capture
        // the raw pointer field, which is not Send).
        let handle = handle;
        loop {
            let mut pkt: *mut EbBufferHeaderType = ptr::null_mut();
            let err = svt_av1_enc_get_packet(handle.ptr(), &mut pkt, 0);
            if err == EB_NoErrorEmptyQueue {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if err != EB_ErrorNone || pkt.is_null() {
                break;
            }
            // The EOS packet (and a skipped frame) carries no buffer.
            let len = (*pkt).n_filled_len as usize;
            let data = if (*pkt).p_buffer.is_null() || len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts((*pkt).p_buffer, len).to_vec()
            };
            let p = Packet {
                data,
                keyframe: (*pkt).pic_type == EB_AV1_KEY_PICTURE,
                pts: (*pkt).pts,
                eos: (*pkt).flags & EB_BUFFERFLAG_EOS != 0,
            };
            let eos = p.eos;
            svt_av1_enc_release_out_buffer(&mut pkt);
            if tx.send(p).is_err() || eos {
                break;
            }
        }
    });
    (t, rx)
}

struct Inner {
    handle: Handle,
    reader: Option<std::thread::JoinHandle<()>>,
    rx: mpsc::Receiver<Packet>,
    frames: i64,
    /// pts of every frame sent, to map packets back to RTP timestamps.
    sent_pts: std::collections::VecDeque<(i64, u32)>,
}

impl Inner {
    fn open(settings: &EncoderSettings) -> Result<Self, CodecError> {
        unsafe {
            let mut handle: *mut EbComponentType = ptr::null_mut();
            let mut cfg = std::mem::MaybeUninit::<EbSvtAv1EncConfiguration>::zeroed();
            check(
                svt_av1_enc_init_handle(&mut handle, ptr::null_mut(), cfg.as_mut_ptr()),
                "init_handle",
            )?;
            let mut cfg = cfg.assume_init();
            cfg.source_width = settings.resolution.width;
            cfg.source_height = settings.resolution.height;
            cfg.frame_rate_numerator = settings.fps;
            cfg.frame_rate_denominator = 1;
            cfg.encoder_bit_depth = 8;
            cfg.encoder_color_format = EB_YUV420;
            cfg.enc_mode = 12;
            cfg.pred_structure = 1; // SVT_AV1_PRED_LOW_DELAY_B
            cfg.rate_control_mode = 2; // SVT_AV1_RC_MODE_CBR
            cfg.target_bit_rate = settings.bitrate_kbps * 1000;
            cfg.intra_period_length = settings.keyframe_interval.max(1) as i32 - 1;
            cfg.look_ahead_distance = 0;
            cfg.enable_tpl_la = 0;
            cfg.level_of_parallelism = 1;
            let r = svt_av1_enc_set_parameter(handle, &mut cfg);
            if r != EB_ErrorNone {
                svt_av1_enc_deinit_handle(handle);
                return Err(CodecError::Codec(format!(
                    "svt-av1 set_parameter: error {r:#x}"
                )));
            }
            let r = svt_av1_enc_init(handle);
            if r != EB_ErrorNone {
                svt_av1_enc_deinit_handle(handle);
                return Err(CodecError::Codec(format!("svt-av1 init: error {r:#x}")));
            }
            let handle = Handle(handle);
            let (reader, rx) = spawn_reader(handle);
            Ok(Self {
                handle,
                reader: Some(reader),
                rx,
                frames: 0,
                sent_pts: Default::default(),
            })
        }
    }

    fn close(&mut self) {
        unsafe {
            let mut eos: EbBufferHeaderType = std::mem::zeroed();
            eos.size = std::mem::size_of::<EbBufferHeaderType>() as u32;
            eos.flags = EB_BUFFERFLAG_EOS;
            let _ = svt_av1_enc_send_picture(self.handle.0, &mut eos);
            if let Some(t) = self.reader.take() {
                let _ = t.join();
            }
            svt_av1_enc_deinit(self.handle.0);
            svt_av1_enc_deinit_handle(self.handle.0);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct SvtAv1Encoder {
    settings: EncoderSettings,
    inner: Inner,
    reinit: bool,
}

unsafe impl Send for SvtAv1Encoder {}

impl SvtAv1Encoder {
    pub fn new(settings: EncoderSettings) -> Result<Self, CodecError> {
        settings.validate()?;
        let inner = Inner::open(&settings)?;
        Ok(Self {
            settings,
            inner,
            reinit: false,
        })
    }

    fn drain(&mut self, out: &mut Vec<CodedFrame>) {
        while let Ok(p) = self.inner.rx.try_recv() {
            if p.eos || p.data.is_empty() {
                continue;
            }
            let ts = self
                .inner
                .sent_pts
                .iter()
                .find(|(pts, _)| *pts == p.pts)
                .map(|(_, ts)| *ts)
                .unwrap_or(0);
            while self
                .inner
                .sent_pts
                .front()
                .map(|(pts, _)| *pts <= p.pts)
                .unwrap_or(false)
            {
                self.inner.sent_pts.pop_front();
            }
            out.push(CodedFrame {
                timestamp: ts,
                keyframe: p.keyframe,
                data: bytes::Bytes::from(p.data),
            });
        }
    }
}

impl VideoEncoder for SvtAv1Encoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::AV1
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
            self.inner = Inner::open(&self.settings)?;
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
        let mut y = src.y.clone();
        let mut u = src.u.clone();
        let mut v = src.v.clone();
        unsafe {
            let mut io = EbSvtIOFormat {
                luma: y.as_mut_ptr(),
                cb: u.as_mut_ptr(),
                cr: v.as_mut_ptr(),
                y_stride: src.y_stride as u32,
                cb_stride: src.uv_stride as u32,
                cr_stride: src.uv_stride as u32,
                width: src.width,
                height: src.height,
                org_x: 0,
                org_y: 0,
                color_fmt: EB_YUV420,
                bit_depth: EB_EIGHT_BIT,
            };
            let mut hdr: EbBufferHeaderType = std::mem::zeroed();
            hdr.size = std::mem::size_of::<EbBufferHeaderType>() as u32;
            hdr.p_buffer = &mut io as *mut EbSvtIOFormat as *mut u8;
            hdr.n_filled_len = (src.width * src.height * 3 / 2) as u32;
            hdr.n_alloc_len = hdr.n_filled_len;
            hdr.pts = self.inner.frames;
            if keyframe {
                hdr.pic_type = EB_AV1_KEY_PICTURE;
            }
            check(
                svt_av1_enc_send_picture(self.inner.handle.0, &mut hdr),
                "send_picture",
            )?;
        }
        self.inner.sent_pts.push_back((self.inner.frames, host.pts));
        self.inner.frames += 1;
        // Give the (low-delay) encoder a moment to emit this frame's packet.
        let mut out = Vec::new();
        for _ in 0..50 {
            self.drain(&mut out);
            if !out.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(out)
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == 0 {
            return Err(CodecError::InvalidConfig("bitrate is zero".into()));
        }
        if kbps != self.settings.bitrate_kbps {
            self.settings.bitrate_kbps = kbps;
            // No runtime retarget in the library API: reopen at the next
            // frame (starts with a keyframe).
            self.reinit = true;
        }
        Ok(())
    }
}

struct Factory;

impl EncoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        VideoCodec::AV1
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self, settings: &EncoderSettings) -> Result<Box<dyn VideoEncoder>, CodecError> {
        Ok(Box::new(SvtAv1Encoder::new(settings.clone())?))
    }
}

pub fn register(registry: &mut CodecRegistry) {
    registry.register_encoder(Box::new(Factory));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsrc;

    #[test]
    fn svt_av1_emits_packets_and_forces_keyframes() {
        let s = testsrc::settings(VideoCodec::AV1, 320, 180);
        let mut enc = SvtAv1Encoder::new(s).unwrap();
        let mut packets = Vec::new();
        for i in 0..20 {
            let src = testsrc::synth(i, 320, 180);
            packets.extend(enc.encode(&VideoFrame::Host(src), i == 10).unwrap());
        }
        assert!(packets.len() >= 18, "{}", packets.len());
        assert!(packets[0].keyframe);
        assert!(
            packets.iter().skip(5).any(|p| p.keyframe),
            "forced keyframe"
        );
        assert!(enc.set_bitrate(0).is_err());
        enc.set_bitrate(300).unwrap();
        let src = testsrc::synth(21, 320, 180);
        let after = enc.encode(&VideoFrame::Host(src), false).unwrap();
        assert!(after.iter().all(|p| !p.data.is_empty()));
    }
}
