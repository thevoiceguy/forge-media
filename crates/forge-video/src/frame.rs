//! Frames and where they live.
//!
//! Every stage of the pipeline is bound to a [`MediaDevice`], and a frame
//! is either host memory (I420 planes) or a handle to memory on one
//! device. A stage only accepts frames resident where it runs; a room's
//! pipeline is placed on one device end to end so nothing but RTP crosses
//! the bus on a GPU node. The host is the reference implementation.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

/// Where media is processed: the CPU, or an accelerator identified by a
/// backend name and an address (`vaapi:/dev/dri/renderD128`, `cuda:0`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaDevice {
    Host,
    Gpu { backend: String, address: String },
}

impl MediaDevice {
    pub fn is_host(&self) -> bool {
        matches!(self, MediaDevice::Host)
    }

    /// Parse `host`, or `backend:address`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("host") || s.eq_ignore_ascii_case("cpu") {
            return Some(MediaDevice::Host);
        }
        let (backend, address) = s.split_once(':')?;
        if backend.is_empty() || address.is_empty() {
            return None;
        }
        Some(MediaDevice::Gpu {
            backend: backend.to_ascii_lowercase(),
            address: address.to_string(),
        })
    }
}

impl fmt::Display for MediaDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaDevice::Host => f.write_str("host"),
            MediaDevice::Gpu { backend, address } => write!(f, "{backend}:{address}"),
        }
    }
}

/// Width and height in pixels. Always even, since I420 chroma is
/// subsampled by two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width: width & !1,
            height: height & !1,
        }
    }

    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// The standard rung this resolution belongs to, by height:
    /// `"180p"`, `"360p"`, `"720p"`, `"1080p"`, else the exact size.
    pub fn label(&self) -> String {
        match self.height {
            180 => "180p".into(),
            360 => "360p".into(),
            720 => "720p".into(),
            1080 => "1080p".into(),
            _ => format!("{}x{}", self.width, self.height),
        }
    }

    /// Parse `WxH` or a rung name (`720p` is 1280×720).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        match s {
            "180p" => return Some(Self::new(320, 180)),
            "360p" => return Some(Self::new(640, 360)),
            "720p" => return Some(Self::new(1280, 720)),
            "1080p" => return Some(Self::new(1920, 1080)),
            _ => {}
        }
        let (w, h) = s.split_once('x')?;
        Some(Self::new(w.parse().ok()?, h.parse().ok()?))
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// An I420 frame in host memory: three planes, each with its own stride,
/// limited range (Y 16–235, U/V 16–240).
#[derive(Clone, PartialEq, Eq)]
pub struct HostFrame {
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp in 90 kHz RTP units.
    pub pts: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
}

impl fmt::Debug for HostFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HostFrame({}x{} pts {}, strides {}/{})",
            self.width, self.height, self.pts, self.y_stride, self.uv_stride
        )
    }
}

impl HostFrame {
    /// A black frame (Y 16, U/V 128) with tight strides.
    pub fn black(width: u32, height: u32) -> Self {
        let width = width & !1;
        let height = height & !1;
        let (w, h) = (width as usize, height as usize);
        Self {
            width,
            height,
            pts: 0,
            y: vec![16; w * h],
            u: vec![128; (w / 2) * (h / 2)],
            v: vec![128; (w / 2) * (h / 2)],
            y_stride: w,
            uv_stride: w / 2,
        }
    }

    /// A frame filled with one colour, in Y/U/V.
    pub fn solid(width: u32, height: u32, y: u8, u: u8, v: u8) -> Self {
        let mut f = Self::black(width, height);
        f.y.fill(y);
        f.u.fill(u);
        f.v.fill(v);
        f
    }

    /// Wrap tightly packed I420 bytes (Y, then U, then V).
    pub fn from_i420(width: u32, height: u32, data: &[u8]) -> Option<Self> {
        let (w, h) = ((width & !1) as usize, (height & !1) as usize);
        let y_len = w * h;
        let c_len = (w / 2) * (h / 2);
        if data.len() < y_len + 2 * c_len {
            return None;
        }
        Some(Self {
            width: w as u32,
            height: h as u32,
            pts: 0,
            y: data[..y_len].to_vec(),
            u: data[y_len..y_len + c_len].to_vec(),
            v: data[y_len + c_len..y_len + 2 * c_len].to_vec(),
            y_stride: w,
            uv_stride: w / 2,
        })
    }

    /// Tightly packed I420 bytes (Y, then U, then V), whatever the strides.
    pub fn to_i420(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = Vec::with_capacity(w * h * 3 / 2);
        for row in 0..h {
            out.extend_from_slice(&self.y[row * self.y_stride..row * self.y_stride + w]);
        }
        for plane in [&self.u, &self.v] {
            for row in 0..h / 2 {
                out.extend_from_slice(&plane[row * self.uv_stride..row * self.uv_stride + w / 2]);
            }
        }
        out
    }

    pub fn resolution(&self) -> Resolution {
        Resolution::new(self.width, self.height)
    }

    /// The luma sample at (x, y).
    pub fn luma(&self, x: u32, y: u32) -> u8 {
        self.y[y as usize * self.y_stride + x as usize]
    }

    /// Chroma samples at chroma coordinates (x/2, y/2).
    pub fn chroma(&self, cx: u32, cy: u32) -> (u8, u8) {
        let i = cy as usize * self.uv_stride + cx as usize;
        (self.u[i], self.v[i])
    }

    pub fn with_pts(mut self, pts: u32) -> Self {
        self.pts = pts;
        self
    }
}

/// A frame resident on a device: an opaque handle the device's backend
/// understands, plus what any stage may ask without touching the memory.
#[derive(Clone)]
pub struct DeviceFrame {
    pub device: MediaDevice,
    pub width: u32,
    pub height: u32,
    pub pts: u32,
    pub handle: Arc<dyn Any + Send + Sync>,
}

impl fmt::Debug for DeviceFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DeviceFrame({}x{} pts {} on {})",
            self.width, self.height, self.pts, self.device
        )
    }
}

/// A frame, wherever it lives.
#[derive(Debug, Clone)]
pub enum VideoFrame {
    Host(HostFrame),
    Device(DeviceFrame),
}

impl VideoFrame {
    pub fn device(&self) -> MediaDevice {
        match self {
            VideoFrame::Host(_) => MediaDevice::Host,
            VideoFrame::Device(d) => d.device.clone(),
        }
    }

    pub fn resolution(&self) -> Resolution {
        match self {
            VideoFrame::Host(h) => h.resolution(),
            VideoFrame::Device(d) => Resolution::new(d.width, d.height),
        }
    }

    pub fn pts(&self) -> u32 {
        match self {
            VideoFrame::Host(h) => h.pts,
            VideoFrame::Device(d) => d.pts,
        }
    }

    /// The host frame, if this is one.
    pub fn as_host(&self) -> Option<&HostFrame> {
        match self {
            VideoFrame::Host(h) => Some(h),
            VideoFrame::Device(_) => None,
        }
    }

    pub fn into_host(self) -> Option<HostFrame> {
        match self {
            VideoFrame::Host(h) => Some(h),
            VideoFrame::Device(_) => None,
        }
    }
}

impl From<HostFrame> for VideoFrame {
    fn from(h: HostFrame) -> Self {
        VideoFrame::Host(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_parse_and_print() {
        assert_eq!(MediaDevice::parse("host"), Some(MediaDevice::Host));
        assert_eq!(MediaDevice::parse("CPU"), Some(MediaDevice::Host));
        let g = MediaDevice::parse("VAAPI:/dev/dri/renderD128").unwrap();
        assert_eq!(g.to_string(), "vaapi:/dev/dri/renderD128");
        assert!(!g.is_host());
        assert_eq!(MediaDevice::parse("cuda:"), None);
        assert_eq!(MediaDevice::parse("nonsense"), None);
    }

    #[test]
    fn resolutions_are_even_and_have_rung_names() {
        let r = Resolution::new(1281, 721);
        assert_eq!((r.width, r.height), (1280, 720));
        assert_eq!(r.label(), "720p");
        assert_eq!(Resolution::parse("360p"), Some(Resolution::new(640, 360)));
        assert_eq!(Resolution::parse("400x300").unwrap().label(), "400x300");
        assert_eq!(Resolution::parse("bad"), None);
        assert!(Resolution::new(640, 360) < Resolution::new(1280, 720));
    }

    #[test]
    fn host_frames_pack_and_unpack_with_strides() {
        let mut f = HostFrame::solid(4, 2, 100, 110, 120);
        // Give the luma plane a wider stride with padding.
        f.y = vec![100, 100, 100, 100, 0, 0, 100, 100, 100, 100, 0, 0];
        f.y_stride = 6;
        let packed = f.to_i420();
        assert_eq!(packed.len(), 4 * 2 * 3 / 2);
        assert!(packed[..8].iter().all(|&b| b == 100));
        assert_eq!(&packed[8..10], &[110, 110]);
        assert_eq!(&packed[10..12], &[120, 120]);
        let back = HostFrame::from_i420(4, 2, &packed).unwrap();
        assert_eq!(back.luma(3, 1), 100);
        assert_eq!(back.chroma(1, 0), (110, 120));
        assert!(HostFrame::from_i420(4, 2, &packed[..5]).is_none());
        let vf: VideoFrame = back.with_pts(90).into();
        assert_eq!(vf.pts(), 90);
        assert!(vf.device().is_host());
        assert_eq!(vf.resolution(), Resolution::new(4, 2));
    }
}
