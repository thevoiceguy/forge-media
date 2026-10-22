//! GPU video for forge-video, over FFmpeg's hardware device contexts
//! (FCP video conferencing design §5.5, §9, §15.7).
//!
//! A [`MediaDevice::Gpu`] such as `cuda:0` opens as an [`HwDevice`]; a
//! [`DeviceFrame`](forge_video::frame::DeviceFrame) on it is an
//! `AVFrame` whose pixels live in device memory, wrapped so that every
//! stage of forge-video's pipeline can hold and pass it without knowing
//! FFmpeg. Around that: decoders that leave their output on the device
//! (NVDEC through FFmpeg's hardware acceleration), encoders that take
//! frames from it (`h264_nvenc`, `hevc_nvenc`, `av1_nvenc`), a scaler
//! (`scale_cuda`), a compositor ([`DeviceCompositor`], a filter graph
//! per output that scales and overlays the pictures onto an uploaded
//! underlay), and [`upload`](frame::upload) / [`download`](frame::download)
//! for the copies the design allows but the scheduler avoids.
//! [`HwBackend`] is all of that as the [`forge_video::DeviceBackend`] a
//! room is placed on.
//!
//! The device's software pixel format is NV12 throughout — what the
//! decoders produce and the encoders take — and a host frame crosses the
//! bus as one plane conversion each way. The device type is FFmpeg's, so
//! `vaapi` and `qsv` open the same way; only CUDA is built and measured
//! (§15.7, decision 3).
//!
//! Nothing here runs without the FFmpeg the machine has: the bindings
//! are generated against it at build time, and [`probe`] says whether
//! a device opens at all, so a test on a machine without one skips.

pub mod backend;
pub mod bench;
pub mod compose;
pub mod decoder;
pub mod device;
pub mod encoder;
pub mod frame;
pub mod scale;

mod ffi;
mod graph;

pub use backend::HwBackend;
pub use compose::DeviceCompositor;
pub use decoder::HwDecoder;
pub use device::{HwDevice, HwFrames};
pub use encoder::NvEncoder;
pub use scale::DeviceScaler;

use forge_core::VideoCodec;
use forge_video::codec::{CodecError, CodecRegistry};
use forge_video::frame::MediaDevice;
use std::sync::Arc;

/// What a device can do, as FFmpeg's build and the driver report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub device: MediaDevice,
    /// Codecs with a hardware decoder.
    pub decoders: Vec<VideoCodec>,
    /// Codecs with a hardware encoder.
    pub encoders: Vec<VideoCodec>,
}

/// Open `device` and see what it offers, or `None` when it does not
/// open (no such GPU, no driver, an FFmpeg without the backend).
pub fn probe(device: &MediaDevice) -> Option<Capabilities> {
    let hw = HwDevice::open(device).ok()?;
    Some(Capabilities {
        device: device.clone(),
        decoders: VideoCodec::ALL
            .iter()
            .copied()
            .filter(|c| decoder::available(&hw, *c))
            .collect(),
        encoders: VideoCodec::ALL
            .iter()
            .copied()
            .filter(|c| encoder::available(*c))
            .collect(),
    })
}

/// Register every hardware decoder and encoder `device` offers, on a
/// fresh open of it. A room's backend and its codecs should share one
/// open device: see [`HwBackend::register`].
pub fn register(
    registry: &mut CodecRegistry,
    device: &MediaDevice,
) -> Result<Capabilities, CodecError> {
    let hw = Arc::new(HwDevice::open(device)?);
    register_on(registry, &hw)
}

/// Register every hardware decoder and encoder an open device offers.
pub fn register_on(
    registry: &mut CodecRegistry,
    hw: &Arc<HwDevice>,
) -> Result<Capabilities, CodecError> {
    let hw = Arc::clone(hw);
    let device = hw.device().clone();
    let caps = Capabilities {
        device: device.clone(),
        decoders: VideoCodec::ALL
            .iter()
            .copied()
            .filter(|c| decoder::available(&hw, *c))
            .collect(),
        encoders: VideoCodec::ALL
            .iter()
            .copied()
            .filter(|c| encoder::available(*c))
            .collect(),
    };
    for codec in &caps.decoders {
        registry.register_decoder(Box::new(decoder::Factory {
            device: Arc::clone(&hw),
            codec: *codec,
        }));
    }
    for codec in &caps.encoders {
        registry.register_encoder(Box::new(encoder::Factory {
            device: Arc::clone(&hw),
            codec: *codec,
        }));
    }
    Ok(caps)
}

/// The device a build tries by default: the first CUDA GPU.
pub const DEFAULT_DEVICE: &str = "cuda:0";

/// Register the default device if it opens; `None` when it does not.
pub fn register_default(registry: &mut CodecRegistry) -> Option<Capabilities> {
    let device = MediaDevice::parse(DEFAULT_DEVICE)?;
    register(registry, &device).ok()
}
