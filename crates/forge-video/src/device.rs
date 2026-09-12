//! A device as the pipeline uses it: where frames live and how they get
//! there, the compositor and scaler that run on it, and the copies
//! across the bus for the stages that must run somewhere else.
//!
//! [`DeviceBackend`] is what a room is placed on. The host is
//! [`HostBackend`]; a GPU is `forge-video-hw`'s backend; tests use the
//! fake device in [`crate::testing`]. The codecs are not here — they
//! come from the [`crate::codec::CodecRegistry`], keyed by device — so
//! the same registry serves a host room and a device room, and a stage
//! the device lacks falls back to the host with one copy per frame
//! (design §5.5: the copy the design allows and the scheduler avoids).

use crate::codec::CodecError;
use crate::compose::{Compositor, HostCompositor};
use crate::frame::{HostFrame, MediaDevice, VideoFrame};
use crate::layout::Layout;
use crate::scale::{HostScaler, Scaler};

/// The stages of the pipeline a device supplies besides its codecs.
pub trait DeviceBackend: Send + Sync {
    /// The device: what its frames, codecs and stages are keyed by.
    fn device(&self) -> MediaDevice;
    /// A compositor with a canvas of this size on the device.
    fn compositor(
        &self,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<Box<dyn Compositor>, CodecError>;
    /// A scaler for frames on the device.
    fn scaler(&self) -> Result<Box<dyn Scaler>, CodecError>;
    /// A host frame copied onto the device.
    fn upload(&self, frame: &HostFrame) -> Result<VideoFrame, CodecError>;
    /// A device frame copied to the host. A host frame is returned as
    /// it is, so a stage can ask for "this frame, on the host" without
    /// caring where it was.
    fn download(&self, frame: &VideoFrame) -> Result<HostFrame, CodecError>;

    /// `frame` on this device: as it is when it is already here,
    /// uploaded when it is a host frame. A frame on another device is
    /// refused.
    fn resident(&self, frame: &VideoFrame) -> Result<VideoFrame, CodecError> {
        match frame {
            VideoFrame::Host(h) if self.device().is_host() => Ok(VideoFrame::Host(h.clone())),
            VideoFrame::Host(h) => self.upload(h),
            VideoFrame::Device(d) if d.device == self.device() => Ok(frame.clone()),
            other => Err(CodecError::WrongDevice {
                expected: self.device(),
                actual: other.device(),
            }),
        }
    }
}

/// The CPU: frames are host frames and the copies are clones.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostBackend;

impl DeviceBackend for HostBackend {
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }

    fn compositor(
        &self,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<Box<dyn Compositor>, CodecError> {
        Ok(Box::new(HostCompositor::new(width, height, layout)))
    }

    fn scaler(&self) -> Result<Box<dyn Scaler>, CodecError> {
        Ok(Box::new(HostScaler))
    }

    fn upload(&self, frame: &HostFrame) -> Result<VideoFrame, CodecError> {
        Ok(VideoFrame::Host(frame.clone()))
    }

    fn download(&self, frame: &VideoFrame) -> Result<HostFrame, CodecError> {
        match frame {
            VideoFrame::Host(h) => Ok(h.clone()),
            VideoFrame::Device(d) => Err(CodecError::WrongDevice {
                expected: MediaDevice::Host,
                actual: d.device.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::DeviceFrame;
    use std::sync::Arc;

    #[test]
    fn the_host_backend_copies_nothing_and_refuses_other_devices() {
        let b = HostBackend;
        assert!(b.device().is_host());
        let f = HostFrame::solid(16, 16, 100, 128, 128);
        let up = b.upload(&f).unwrap();
        assert_eq!(up.as_host(), Some(&f));
        assert_eq!(b.download(&up).unwrap(), f);
        assert_eq!(b.resident(&up).unwrap().as_host(), Some(&f));
        let gpu = VideoFrame::Device(DeviceFrame {
            device: MediaDevice::parse("cuda:0").unwrap(),
            width: 16,
            height: 16,
            pts: 0,
            handle: Arc::new(()),
        });
        assert!(b.download(&gpu).is_err());
        assert!(b.resident(&gpu).is_err());
        let mut c = b.compositor(64, 36, Layout::Grid).unwrap();
        assert!(c.device().is_host());
        c.render(&[], 1).unwrap();
        assert_eq!(c.canvas().pts(), 1);
        assert!(b.scaler().unwrap().device().is_host());
    }
}
