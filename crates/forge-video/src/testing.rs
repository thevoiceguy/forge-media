//! A device that is not there: host frames behind device handles, so
//! the placement and fallback decisions of a room on a device can be
//! tested on a machine without one. The fake device composites and
//! scales with the host code and counts its copies across the "bus".

use crate::codec::{CodecError, CodecRegistry};
use crate::compose::{Compositor, HostCompositor, TileSource};
use crate::device::DeviceBackend;
use crate::frame::{DeviceFrame, HostFrame, MediaDevice, Resolution, VideoFrame};
use crate::layout::Layout;
use crate::raw::RawFactory;
use crate::scale::{self, Scaler};
use forge_core::VideoCodec;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The backend name of a fake device: `fake:0`, `fake:left`, …
pub const FAKE_BACKEND: &str = "fake";

/// A fake device at `address`.
pub fn fake_device(address: &str) -> MediaDevice {
    MediaDevice::Gpu {
        backend: FAKE_BACKEND.to_string(),
        address: address.to_string(),
    }
}

/// Whether `device` is a fake one.
pub fn is_fake(device: &MediaDevice) -> bool {
    matches!(device, MediaDevice::Gpu { backend, .. } if backend == FAKE_BACKEND)
}

/// What a fake device frame holds: the pixels, on the host after all.
pub struct FakeFrame(pub HostFrame);

/// `host` as a frame on the fake `device`.
pub fn wrap(device: &MediaDevice, host: HostFrame) -> VideoFrame {
    VideoFrame::Device(DeviceFrame {
        device: device.clone(),
        width: host.width,
        height: host.height,
        pts: host.pts,
        handle: Arc::new(FakeFrame(host)),
    })
}

/// The pixels behind a fake device frame on `device`.
pub fn unwrap<'a>(
    frame: &'a VideoFrame,
    device: &MediaDevice,
) -> Result<&'a HostFrame, CodecError> {
    match frame {
        VideoFrame::Device(d) if &d.device == device => d
            .handle
            .downcast_ref::<FakeFrame>()
            .map(|f| &f.0)
            .ok_or_else(|| CodecError::Codec("device frame is not a fake frame".into())),
        other => Err(CodecError::WrongDevice {
            expected: device.clone(),
            actual: other.device(),
        }),
    }
}

/// The fake device's backend.
pub struct FakeBackend {
    device: MediaDevice,
    /// Refuse to build a compositor, as a device whose filter graph
    /// does not come up would.
    no_compositor: AtomicBool,
    uploads: AtomicU64,
    downloads: AtomicU64,
}

impl FakeBackend {
    pub fn new(address: &str) -> Self {
        Self {
            device: fake_device(address),
            no_compositor: AtomicBool::new(false),
            uploads: AtomicU64::new(0),
            downloads: AtomicU64::new(0),
        }
    }

    /// Make every compositor request fail from now on.
    pub fn set_no_compositor(&self, no: bool) {
        self.no_compositor.store(no, Ordering::Release);
    }

    /// Host frames copied up so far.
    pub fn uploads(&self) -> u64 {
        self.uploads.load(Ordering::Acquire)
    }

    /// Device frames copied down so far.
    pub fn downloads(&self) -> u64 {
        self.downloads.load(Ordering::Acquire)
    }
}

impl DeviceBackend for FakeBackend {
    fn device(&self) -> MediaDevice {
        self.device.clone()
    }

    fn compositor(
        &self,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<Box<dyn Compositor>, CodecError> {
        if self.no_compositor.load(Ordering::Acquire) {
            return Err(CodecError::Unavailable {
                codec: VideoCodec::H264,
                role: "compositor",
                device: self.device.clone(),
            });
        }
        Ok(Box::new(FakeCompositor {
            device: self.device.clone(),
            inner: HostCompositor::new(width, height, layout),
            canvas: None,
        }))
    }

    fn scaler(&self) -> Result<Box<dyn Scaler>, CodecError> {
        Ok(Box::new(FakeScaler {
            device: self.device.clone(),
        }))
    }

    fn upload(&self, frame: &HostFrame) -> Result<VideoFrame, CodecError> {
        self.uploads.fetch_add(1, Ordering::AcqRel);
        Ok(wrap(&self.device, frame.clone()))
    }

    fn download(&self, frame: &VideoFrame) -> Result<HostFrame, CodecError> {
        if let VideoFrame::Host(h) = frame {
            return Ok(h.clone());
        }
        self.downloads.fetch_add(1, Ordering::AcqRel);
        unwrap(frame, &self.device).cloned()
    }
}

/// The host compositor behind fake device frames.
struct FakeCompositor {
    device: MediaDevice,
    inner: HostCompositor,
    canvas: Option<VideoFrame>,
}

impl Compositor for FakeCompositor {
    fn device(&self) -> MediaDevice {
        self.device.clone()
    }
    fn layout(&self) -> Layout {
        self.inner.layout()
    }
    fn set_layout(&mut self, layout: Layout) {
        self.inner.set_layout(layout)
    }
    fn resolution(&self) -> Resolution {
        self.inner.resolution()
    }
    fn render(&mut self, sources: &[TileSource<'_>], pts: u32) -> Result<(), CodecError> {
        let mut hosts: Vec<Option<VideoFrame>> = Vec::with_capacity(sources.len());
        for s in sources {
            hosts.push(match s.frame {
                None => None,
                Some(f) => Some(VideoFrame::Host(unwrap(f, &self.device)?.clone())),
            });
        }
        let host_sources: Vec<TileSource<'_>> = sources
            .iter()
            .zip(&hosts)
            .map(|(s, f)| TileSource {
                frame: f.as_ref(),
                ..s.clone()
            })
            .collect();
        self.inner.render(&host_sources, pts)?;
        self.canvas = Some(wrap(&self.device, self.inner.host_canvas().clone()));
        Ok(())
    }
    fn canvas(&self) -> &VideoFrame {
        self.canvas.as_ref().unwrap_or_else(|| self.inner.canvas())
    }
}

struct FakeScaler {
    device: MediaDevice,
}

impl Scaler for FakeScaler {
    fn device(&self) -> MediaDevice {
        self.device.clone()
    }
    fn scale(&mut self, src: &VideoFrame, to: Resolution) -> Result<VideoFrame, CodecError> {
        let host = unwrap(src, &self.device)?;
        Ok(wrap(&self.device, scale::resize(host, to.width, to.height)))
    }
}

/// The raw codec on the host for every codec, and on the fake `device`
/// for the codecs listed: what a room on a device that decodes some
/// codecs and encodes fewer looks like to the registry.
pub fn fake_registry(
    device: &MediaDevice,
    decoders: &[VideoCodec],
    encoders: &[VideoCodec],
) -> CodecRegistry {
    let mut r = crate::raw::raw_registry();
    for codec in decoders {
        r.register_decoder(Box::new(RawFactory::for_codec(*codec).on(device.clone())));
    }
    for codec in encoders {
        r.register_encoder(Box::new(RawFactory::for_codec(*codec).on(device.clone())));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::EncoderSettings;
    use crate::compose::TileKind;
    use crate::flavor::Flavor;

    #[test]
    fn the_fake_device_composites_scales_and_counts_its_copies() {
        let b = FakeBackend::new("0");
        let d = b.device();
        assert!(is_fake(&d));
        assert_eq!(d.to_string(), "fake:0");
        let host = HostFrame::solid(64, 36, 200, 128, 128);
        let up = b.upload(&host).unwrap();
        assert_eq!(up.device(), d);
        assert_eq!(b.uploads(), 1);
        assert_eq!(b.download(&up).unwrap(), host);
        assert_eq!(b.downloads(), 1);
        // A host frame "downloads" without a copy.
        assert_eq!(b.download(&VideoFrame::Host(host.clone())).unwrap(), host);
        assert_eq!(b.downloads(), 1);

        let mut c = b.compositor(128, 72, Layout::Grid).unwrap();
        assert_eq!(c.device(), d);
        c.render(
            &[TileSource {
                id: "a",
                name: "a",
                frame: Some(&up),
                speaking: false,
                muted: false,
                kind: TileKind::Camera,
            }],
            9,
        )
        .unwrap();
        let canvas = c.canvas().clone();
        assert_eq!(canvas.device(), d);
        assert_eq!(canvas.pts(), 9);
        let shown = b.download(&canvas).unwrap();
        assert_eq!(shown.luma(64, 36), 200);
        // A host frame on a fake compositor is refused like any other
        // device's would be.
        let err = c
            .render(
                &[TileSource {
                    id: "h",
                    name: "h",
                    frame: Some(&VideoFrame::Host(host.clone())),
                    speaking: false,
                    muted: false,
                    kind: TileKind::Camera,
                }],
                10,
            )
            .unwrap_err();
        assert!(matches!(err, CodecError::WrongDevice { .. }));

        let mut s = b.scaler().unwrap();
        let small = s.scale(&up, Resolution::new(32, 18)).unwrap();
        assert_eq!(small.resolution(), Resolution::new(32, 18));
        assert_eq!(small.device(), d);

        b.set_no_compositor(true);
        assert!(b.compositor(64, 36, Layout::Grid).is_err());
    }

    #[test]
    fn the_fake_registry_puts_the_raw_codec_on_the_device() {
        let d = fake_device("0");
        let r = fake_registry(
            &d,
            &[VideoCodec::VP8, VideoCodec::H264],
            &[VideoCodec::H264],
        );
        assert_eq!(r.decodable_on(&d), vec![VideoCodec::H264, VideoCodec::VP8]);
        assert_eq!(r.encodable_on(&d), vec![VideoCodec::H264]);
        assert_eq!(r.codecs_on(&d), vec![VideoCodec::H264]);
        assert_eq!(r.codecs_on(&MediaDevice::Host).len(), 5);
        // Encode on the device from a device frame, decode back to one.
        let f = Flavor::new(VideoCodec::H264, "", Resolution::new(64, 36), 15, 500);
        let mut enc = r.encoder(&EncoderSettings::for_flavor(&f, 30), &d).unwrap();
        assert_eq!(enc.device(), d);
        let frame = wrap(&d, HostFrame::solid(64, 36, 90, 128, 128));
        let coded = enc.encode(&frame, true).unwrap();
        assert_eq!(coded.len(), 1);
        assert!(enc
            .encode(&VideoFrame::Host(HostFrame::black(64, 36)), true)
            .is_err());
        let mut dec = r.decoder(VideoCodec::H264, &d).unwrap();
        assert_eq!(dec.device(), d);
        let back = dec.decode(&coded[0]).unwrap().unwrap();
        assert_eq!(back.device(), d);
        assert_eq!(unwrap(&back, &d).unwrap().luma(1, 1), 90);
    }
}
