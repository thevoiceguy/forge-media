//! AV1 decode through libdav1d.

use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use forge_video::codec::{CodecError, CodecRegistry, DecoderFactory, VideoDecoder};
use forge_video::frame::{HostFrame, MediaDevice, VideoFrame};

fn d_err(what: &str, e: impl std::fmt::Display) -> CodecError {
    CodecError::Codec(format!("dav1d {what}: {e}"))
}

pub struct Dav1dDecoder {
    decoder: dav1d::Decoder,
    threads: u32,
}

impl Dav1dDecoder {
    /// `threads = 0` lets dav1d pick.
    pub fn new(threads: u32) -> Result<Self, CodecError> {
        let mut settings = dav1d::Settings::new();
        settings.set_n_threads(threads);
        settings.set_max_frame_delay(1);
        Ok(Self {
            decoder: dav1d::Decoder::with_settings(&settings).map_err(|e| d_err("init", e))?,
            threads,
        })
    }

    fn picture_to_frame(pic: &dav1d::Picture, pts: u32) -> Option<HostFrame> {
        use dav1d::PlanarImageComponent as C;
        if pic.bit_depth() != 8 || pic.pixel_layout() != dav1d::PixelLayout::I420 {
            return None;
        }
        let (w, h) = (pic.width(), pic.height());
        let mut f = HostFrame::black(w, h).with_pts(pts);
        let (w, h) = (f.width as usize, f.height as usize);
        let y = pic.plane(C::Y);
        let ys = pic.stride(C::Y) as usize;
        for row in 0..h {
            f.y[row * f.y_stride..row * f.y_stride + w].copy_from_slice(&y[row * ys..row * ys + w]);
        }
        for (comp, dst) in [(C::U, &mut f.u), (C::V, &mut f.v)] {
            let p = pic.plane(comp);
            let s = pic.stride(comp) as usize;
            for row in 0..h / 2 {
                dst[row * (w / 2)..row * (w / 2) + w / 2]
                    .copy_from_slice(&p[row * s..row * s + w / 2]);
            }
        }
        Some(f)
    }
}

impl VideoDecoder for Dav1dDecoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::AV1
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn decode(&mut self, frame: &CodedFrame) -> Result<Option<VideoFrame>, CodecError> {
        let mut pending =
            self.decoder
                .send_data(frame.data.clone(), None, Some(frame.timestamp as i64), None);
        let mut last = None;
        loop {
            match self.decoder.get_picture() {
                Ok(pic) => {
                    if let Some(f) = Self::picture_to_frame(&pic, frame.timestamp) {
                        last = Some(VideoFrame::Host(f));
                    }
                }
                Err(dav1d::Error::Again) => {}
                Err(e) => return Err(d_err("get_picture", e)),
            }
            match pending {
                Err(dav1d::Error::Again) => pending = self.decoder.send_pending_data(),
                Err(e) => return Err(d_err("send_data", e)),
                Ok(()) => break,
            }
        }
        // Whatever became ready after the last send.
        while let Ok(pic) = self.decoder.get_picture() {
            if let Some(f) = Self::picture_to_frame(&pic, frame.timestamp) {
                last = Some(VideoFrame::Host(f));
            }
        }
        Ok(last)
    }
    fn reset(&mut self) {
        self.decoder.flush();
        if let Ok(fresh) = Dav1dDecoder::new(self.threads) {
            self.decoder = fresh.decoder;
        }
    }
}

struct Factory;

impl DecoderFactory for Factory {
    fn codec(&self) -> VideoCodec {
        VideoCodec::AV1
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self) -> Result<Box<dyn VideoDecoder>, CodecError> {
        Ok(Box::new(Dav1dDecoder::new(1)?))
    }
}

pub fn register(registry: &mut CodecRegistry) {
    registry.register_decoder(Box::new(Factory));
}

#[cfg(all(test, feature = "svt-av1"))]
mod tests {
    use super::*;
    use crate::svt_av1::SvtAv1Encoder;
    use crate::testsrc;

    #[test]
    fn av1_round_trips_between_svt_and_dav1d() {
        let s = testsrc::settings(VideoCodec::AV1, 320, 180);
        let mut enc = SvtAv1Encoder::new(s).unwrap();
        let mut dec = Dav1dDecoder::new(1).unwrap();
        let (decoded, keyframes, psnr) = testsrc::round_trip(&mut enc, &mut dec, 30, 320, 180);
        assert!(decoded >= 25, "decoded {decoded}");
        assert!(keyframes >= 2, "keyframes {keyframes}");
        assert!(psnr > 25.0, "psnr {psnr}");
    }
}
