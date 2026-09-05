//! The codec traits every binding implements, and the registry that
//! picks a binding for a codec on a device.
//!
//! Decoders take coded frames (as the frame assembler produces them) and
//! give frames resident on their device; encoders take frames resident on
//! their device and give coded frames. Both are synchronous and expected
//! to run on the codec thread pool, never on an async worker.

use crate::flavor::Flavor;
use crate::frame::{MediaDevice, Resolution, VideoFrame};
use forge_core::VideoCodec;
use forge_rtp::CodedFrame;
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("no {codec} {role} for device {device}")]
    Unavailable {
        codec: VideoCodec,
        role: &'static str,
        device: MediaDevice,
    },
    #[error("frame is on {actual}, this stage runs on {expected}")]
    WrongDevice {
        expected: MediaDevice,
        actual: MediaDevice,
    },
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("{0}")]
    Codec(String),
}

/// What an encoder is asked to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderSettings {
    pub codec: VideoCodec,
    pub resolution: Resolution,
    pub fps: u32,
    /// Target bitrate in kb/s.
    pub bitrate_kbps: u32,
    /// Maximum frames between keyframes.
    pub keyframe_interval: u32,
    /// Codec profile / fmtp as negotiated (normalised as in [`Flavor`]).
    pub profile: String,
}

impl EncoderSettings {
    pub fn for_flavor(flavor: &Flavor, keyframe_interval: u32) -> Self {
        Self {
            codec: flavor.codec,
            resolution: flavor.resolution,
            fps: flavor.fps,
            bitrate_kbps: flavor.max_kbps,
            keyframe_interval,
            profile: flavor.profile.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), CodecError> {
        if self.resolution.width < 16 || self.resolution.height < 16 {
            return Err(CodecError::InvalidConfig(format!(
                "resolution {} too small",
                self.resolution
            )));
        }
        if self.fps == 0 || self.fps > 120 {
            return Err(CodecError::InvalidConfig(format!(
                "fps {} out of range",
                self.fps
            )));
        }
        if self.bitrate_kbps == 0 {
            return Err(CodecError::InvalidConfig("bitrate is zero".into()));
        }
        Ok(())
    }
}

/// A video decoder for one stream.
pub trait VideoDecoder: Send {
    fn codec(&self) -> VideoCodec;
    fn device(&self) -> MediaDevice;
    /// Decode one coded frame. `None` when the decoder needs more input
    /// (or the frame was not displayable).
    fn decode(&mut self, frame: &CodedFrame) -> Result<Option<VideoFrame>, CodecError>;
    /// Forget reference state: call after loss, before the next keyframe.
    fn reset(&mut self);
}

/// A video encoder for one flavor.
pub trait VideoEncoder: Send {
    fn codec(&self) -> VideoCodec;
    fn device(&self) -> MediaDevice;
    fn settings(&self) -> &EncoderSettings;
    /// Encode one frame (resident on this encoder's device). `keyframe`
    /// forces an intra frame. May return no packet (encoder delay) or
    /// more than one.
    fn encode(&mut self, frame: &VideoFrame, keyframe: bool)
        -> Result<Vec<CodedFrame>, CodecError>;
    /// Retarget the bitrate in kb/s (from REMB / transport-cc).
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError>;
}

/// Builds decoders.
pub trait DecoderFactory: Send + Sync {
    fn codec(&self) -> VideoCodec;
    fn device(&self) -> MediaDevice;
    fn create(&self) -> Result<Box<dyn VideoDecoder>, CodecError>;
}

/// Builds encoders.
pub trait EncoderFactory: Send + Sync {
    fn codec(&self) -> VideoCodec;
    fn device(&self) -> MediaDevice;
    fn create(&self, settings: &EncoderSettings) -> Result<Box<dyn VideoEncoder>, CodecError>;
}

/// Which factory serves (codec, device). Filled by the bindings a build
/// includes; the raw codec is always there for the host.
#[derive(Default)]
pub struct CodecRegistry {
    decoders: HashMap<(VideoCodec, MediaDevice), Box<dyn DecoderFactory>>,
    encoders: HashMap<(VideoCodec, MediaDevice), Box<dyn EncoderFactory>>,
}

impl fmt::Debug for CodecRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodecRegistry")
            .field("decoders", &self.decoders.keys().collect::<Vec<_>>())
            .field("encoders", &self.encoders.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CodecRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_decoder(&mut self, factory: Box<dyn DecoderFactory>) {
        self.decoders
            .insert((factory.codec(), factory.device()), factory);
    }

    pub fn register_encoder(&mut self, factory: Box<dyn EncoderFactory>) {
        self.encoders
            .insert((factory.codec(), factory.device()), factory);
    }

    pub fn decoder(
        &self,
        codec: VideoCodec,
        device: &MediaDevice,
    ) -> Result<Box<dyn VideoDecoder>, CodecError> {
        self.decoders
            .get(&(codec, device.clone()))
            .ok_or_else(|| CodecError::Unavailable {
                codec,
                role: "decoder",
                device: device.clone(),
            })?
            .create()
    }

    pub fn encoder(
        &self,
        settings: &EncoderSettings,
        device: &MediaDevice,
    ) -> Result<Box<dyn VideoEncoder>, CodecError> {
        settings.validate()?;
        self.encoders
            .get(&(settings.codec, device.clone()))
            .ok_or_else(|| CodecError::Unavailable {
                codec: settings.codec,
                role: "encoder",
                device: device.clone(),
            })?
            .create(settings)
    }

    /// Codecs with both an encoder and a decoder on `device`.
    pub fn codecs_on(&self, device: &MediaDevice) -> Vec<VideoCodec> {
        VideoCodec::ALL
            .iter()
            .copied()
            .filter(|c| {
                self.decoders.contains_key(&(*c, device.clone()))
                    && self.encoders.contains_key(&(*c, device.clone()))
            })
            .collect()
    }

    pub fn can_decode(&self, codec: VideoCodec, device: &MediaDevice) -> bool {
        self.decoders.contains_key(&(codec, device.clone()))
    }

    pub fn can_encode(&self, codec: VideoCodec, device: &MediaDevice) -> bool {
        self.encoders.contains_key(&(codec, device.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validate() {
        let f = Flavor::new(VideoCodec::VP8, "", Resolution::new(640, 360), 15, 500);
        let s = EncoderSettings::for_flavor(&f, 300);
        assert!(s.validate().is_ok());
        assert_eq!(s.keyframe_interval, 300);
        let mut bad = s.clone();
        bad.fps = 0;
        assert!(bad.validate().is_err());
        bad = s.clone();
        bad.resolution = Resolution::new(8, 8);
        assert!(bad.validate().is_err());
        bad = s;
        bad.bitrate_kbps = 0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn empty_registry_reports_unavailable() {
        let r = CodecRegistry::new();
        let err = r
            .decoder(VideoCodec::VP8, &MediaDevice::Host)
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "no VP8 decoder for device host");
        assert!(r.codecs_on(&MediaDevice::Host).is_empty());
        assert!(!r.can_encode(VideoCodec::H264, &MediaDevice::Host));
    }
}
