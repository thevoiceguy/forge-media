//! The device as a room is placed on it: [`forge_video::DeviceBackend`]
//! over an open [`HwDevice`].

use crate::compose::DeviceCompositor;
use crate::device::HwDevice;
use crate::frame::{download, upload};
use crate::scale::DeviceScaler;
use forge_video::codec::{CodecError, CodecRegistry};
use forge_video::compose::Compositor;
use forge_video::device::DeviceBackend;
use forge_video::frame::{HostFrame, MediaDevice, VideoFrame};
use forge_video::layout::Layout;
use forge_video::scale::Scaler;
use std::sync::Arc;

/// An open device with its compositor, scaler and copies.
pub struct HwBackend {
    device: Arc<HwDevice>,
}

impl HwBackend {
    /// Open `device` (`cuda:0`, …).
    pub fn open(device: &MediaDevice) -> Result<HwBackend, CodecError> {
        Ok(HwBackend {
            device: Arc::new(HwDevice::open(device)?),
        })
    }

    pub fn from_device(device: Arc<HwDevice>) -> HwBackend {
        HwBackend { device }
    }

    /// The open device, shared with the codecs registered on it.
    pub fn hw(&self) -> &Arc<HwDevice> {
        &self.device
    }

    /// Register this device's codecs in `registry`, on the same open
    /// device the backend uses.
    pub fn register(
        &self,
        registry: &mut CodecRegistry,
    ) -> Result<crate::Capabilities, CodecError> {
        crate::register_on(registry, &self.device)
    }
}

impl DeviceBackend for HwBackend {
    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn compositor(
        &self,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<Box<dyn Compositor>, CodecError> {
        Ok(Box::new(DeviceCompositor::new(
            Arc::clone(&self.device),
            width,
            height,
            layout,
        )?))
    }

    fn scaler(&self) -> Result<Box<dyn Scaler>, CodecError> {
        Ok(Box::new(DeviceScaler::new(Arc::clone(&self.device))))
    }

    fn upload(&self, frame: &HostFrame) -> Result<VideoFrame, CodecError> {
        let pool = self.device.frames(frame.resolution())?;
        upload(&self.device, &pool, frame)
    }

    fn download(&self, frame: &VideoFrame) -> Result<HostFrame, CodecError> {
        match frame {
            VideoFrame::Host(h) => Ok(h.clone()),
            VideoFrame::Device(_) => download(frame, self.device.device()),
        }
    }
}
