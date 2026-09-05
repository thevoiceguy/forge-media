//! An uncompressed "codec" for tests: a coded frame is a small header and
//! the I420 planes. Every keyframe. Lets the pipeline, layouts and the
//! conference server be tested end to end with no native library. It
//! registers itself under [`VideoCodec::VP8`] by default, so anything
//! that negotiates VP8 works unchanged against it; pick another codec
//! with [`RawFactory::for_codec`].

use crate::codec::{
    CodecError, DecoderFactory, EncoderFactory, EncoderSettings, VideoDecoder, VideoEncoder,
};
use crate::frame::{HostFrame, MediaDevice, VideoFrame};
use crate::scale;
use bytes::{BufMut, Bytes, BytesMut};
use forge_core::VideoCodec;
use forge_rtp::CodedFrame;

const MAGIC: &[u8; 4] = b"FRAW";
const HEADER: usize = 4 + 2 + 2 + 1;

/// Serialize a host frame as a raw coded frame.
pub fn encode_raw(frame: &HostFrame, keyframe: bool) -> CodedFrame {
    let mut b =
        BytesMut::with_capacity(HEADER + frame.width as usize * frame.height as usize * 3 / 2);
    b.put_slice(MAGIC);
    b.put_u16(frame.width as u16);
    b.put_u16(frame.height as u16);
    b.put_u8(keyframe as u8);
    b.put_slice(&frame.to_i420());
    CodedFrame {
        timestamp: frame.pts,
        keyframe,
        data: b.freeze(),
    }
}

/// Parse a raw coded frame back into a host frame.
pub fn decode_raw(data: &Bytes) -> Result<HostFrame, CodecError> {
    if data.len() < HEADER || &data[..4] != MAGIC {
        return Err(CodecError::Codec("not a raw frame".into()));
    }
    let w = u16::from_be_bytes([data[4], data[5]]) as u32;
    let h = u16::from_be_bytes([data[6], data[7]]) as u32;
    HostFrame::from_i420(w, h, &data[HEADER..])
        .ok_or_else(|| CodecError::Codec("raw frame truncated".into()))
}

/// Whether a coded frame is a raw frame (a keyframe by construction).
pub fn is_raw(data: &[u8]) -> bool {
    data.len() >= HEADER && &data[..4] == MAGIC
}

pub struct RawDecoder {
    codec: VideoCodec,
}

impl VideoDecoder for RawDecoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn decode(&mut self, frame: &CodedFrame) -> Result<Option<VideoFrame>, CodecError> {
        let f = decode_raw(&frame.data)?.with_pts(frame.timestamp);
        Ok(Some(VideoFrame::Host(f)))
    }
    fn reset(&mut self) {}
}

pub struct RawEncoder {
    codec: VideoCodec,
    settings: EncoderSettings,
    frames: u64,
}

impl RawEncoder {
    pub fn new(settings: EncoderSettings) -> Self {
        Self {
            codec: settings.codec,
            settings,
            frames: 0,
        }
    }

    /// Frames encoded so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

impl VideoEncoder for RawEncoder {
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
        _keyframe: bool,
    ) -> Result<Vec<CodedFrame>, CodecError> {
        let host = frame.as_host().ok_or_else(|| CodecError::WrongDevice {
            expected: MediaDevice::Host,
            actual: frame.device(),
        })?;
        // Encoders deliver the flavor's resolution whatever they are fed.
        let r = self.settings.resolution;
        let scaled;
        let src = if host.width == r.width && host.height == r.height {
            host
        } else {
            scaled = scale::resize(host, r.width, r.height);
            &scaled
        };
        self.frames += 1;
        Ok(vec![encode_raw(src, true)])
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        self.settings.bitrate_kbps = kbps;
        Ok(())
    }
}

/// Factories for the raw codec, registered as `codec` on the host.
pub struct RawFactory {
    codec: VideoCodec,
}

impl RawFactory {
    pub fn new() -> Self {
        Self {
            codec: VideoCodec::VP8,
        }
    }

    pub fn for_codec(codec: VideoCodec) -> Self {
        Self { codec }
    }
}

impl Default for RawFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoderFactory for RawFactory {
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self) -> Result<Box<dyn VideoDecoder>, CodecError> {
        Ok(Box::new(RawDecoder { codec: self.codec }))
    }
}

impl EncoderFactory for RawFactory {
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }
    fn create(&self, settings: &EncoderSettings) -> Result<Box<dyn VideoEncoder>, CodecError> {
        if settings.codec != self.codec {
            return Err(CodecError::InvalidConfig(format!(
                "raw factory is {} but settings ask for {}",
                self.codec, settings.codec
            )));
        }
        Ok(Box::new(RawEncoder::new(settings.clone())))
    }
}

/// A registry with the raw codec on the host for every video codec:
/// what tests and codec-less builds use.
pub fn raw_registry() -> crate::codec::CodecRegistry {
    let mut r = crate::codec::CodecRegistry::new();
    for codec in VideoCodec::ALL {
        r.register_decoder(Box::new(RawFactory::for_codec(codec)));
        r.register_encoder(Box::new(RawFactory::for_codec(codec)));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Resolution;

    #[test]
    fn raw_frames_round_trip_and_scale_to_the_flavor() {
        let r = raw_registry();
        assert_eq!(r.codecs_on(&MediaDevice::Host).len(), 5);
        let settings = EncoderSettings {
            codec: VideoCodec::H264,
            resolution: Resolution::new(64, 36),
            fps: 15,
            bitrate_kbps: 100,
            keyframe_interval: 30,
            profile: String::new(),
        };
        let mut enc = r.encoder(&settings, &MediaDevice::Host).unwrap();
        let mut dec = r.decoder(VideoCodec::H264, &MediaDevice::Host).unwrap();
        let src = HostFrame::solid(128, 72, 180, 100, 140).with_pts(4500);
        let coded = enc.encode(&VideoFrame::Host(src), false).unwrap();
        assert_eq!(coded.len(), 1);
        assert!(coded[0].keyframe);
        assert_eq!(coded[0].timestamp, 4500);
        assert!(is_raw(&coded[0].data));
        let out = dec.decode(&coded[0]).unwrap().unwrap();
        let host = out.as_host().unwrap();
        assert_eq!(host.resolution(), Resolution::new(64, 36));
        assert_eq!(host.pts, 4500);
        assert_eq!(host.luma(10, 10), 180);
        assert_eq!(host.chroma(5, 5), (100, 140));
        // Junk is rejected, the wrong codec is rejected.
        assert!(decode_raw(&Bytes::from_static(b"nope")).is_err());
        let wrong = EncoderSettings {
            codec: VideoCodec::VP9,
            ..settings
        };
        assert!(EncoderFactory::create(&RawFactory::for_codec(VideoCodec::VP8), &wrong).is_err());
    }
}
