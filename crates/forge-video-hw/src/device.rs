//! A GPU as FFmpeg opens it, and the pools its frames come from.

use crate::ffi::{self, BufferRef};
use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use forge_video::frame::{MediaDevice, Resolution};
use std::sync::Mutex;

/// The software pixel format every frame on a device has here.
pub const SW_FORMAT: ff::AVPixelFormat = ff::AVPixelFormat::AV_PIX_FMT_NV12;

/// An open hardware device: an `AVHWDeviceContext`.
pub struct HwDevice {
    device: MediaDevice,
    kind: ff::AVHWDeviceType,
    ctx: BufferRef,
    /// The device's pixel format (`AV_PIX_FMT_CUDA`, `AV_PIX_FMT_VAAPI`…).
    pix_fmt: ff::AVPixelFormat,
    /// Frame pools by size, shared by uploads and encoders.
    pools: Mutex<Vec<(Resolution, BufferRef)>>,
}

fn kind_of(backend: &str) -> Option<(ff::AVHWDeviceType, ff::AVPixelFormat)> {
    use ff::AVHWDeviceType::*;
    use ff::AVPixelFormat::*;
    match backend {
        "cuda" => Some((AV_HWDEVICE_TYPE_CUDA, AV_PIX_FMT_CUDA)),
        "vaapi" => Some((AV_HWDEVICE_TYPE_VAAPI, AV_PIX_FMT_VAAPI)),
        "qsv" => Some((AV_HWDEVICE_TYPE_QSV, AV_PIX_FMT_QSV)),
        _ => None,
    }
}

impl HwDevice {
    /// Open `device` (`cuda:0`, `vaapi:/dev/dri/renderD128`, …).
    pub fn open(device: &MediaDevice) -> Result<HwDevice, CodecError> {
        let MediaDevice::Gpu { backend, address } = device else {
            return Err(CodecError::InvalidConfig(
                "the host is not a hardware device".into(),
            ));
        };
        let (kind, pix_fmt) = kind_of(backend).ok_or_else(|| {
            CodecError::InvalidConfig(format!("unknown device backend {backend}"))
        })?;
        let mut ctx: *mut ff::AVBufferRef = std::ptr::null_mut();
        let addr = ffi::cstr(address);
        let rc = unsafe {
            ff::av_hwdevice_ctx_create(&mut ctx, kind, addr.as_ptr(), std::ptr::null_mut(), 0)
        };
        ffi::check(&format!("open {device}"), rc)?;
        Ok(HwDevice {
            device: device.clone(),
            kind,
            ctx: BufferRef(ctx),
            pix_fmt,
            pools: Mutex::new(Vec::new()),
        })
    }

    pub fn device(&self) -> &MediaDevice {
        &self.device
    }

    pub fn kind(&self) -> ff::AVHWDeviceType {
        self.kind
    }

    pub fn pix_fmt(&self) -> ff::AVPixelFormat {
        self.pix_fmt
    }

    /// A new reference to the device context.
    pub fn context(&self) -> BufferRef {
        self.ctx.clone_ref()
    }

    /// The frame pool for `resolution`, made on first use.
    pub fn frames(&self, resolution: Resolution) -> Result<HwFrames, CodecError> {
        let mut pools = self.pools.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, r)) = pools.iter().find(|(res, _)| *res == resolution) {
            return Ok(HwFrames {
                ctx: r.clone_ref(),
                resolution,
                pix_fmt: self.pix_fmt,
            });
        }
        let frames = HwFrames::new(self, resolution)?;
        pools.push((resolution, frames.ctx.clone_ref()));
        Ok(frames)
    }
}

/// A pool of device frames of one size: an `AVHWFramesContext`.
pub struct HwFrames {
    ctx: BufferRef,
    resolution: Resolution,
    pix_fmt: ff::AVPixelFormat,
}

impl HwFrames {
    fn new(device: &HwDevice, resolution: Resolution) -> Result<HwFrames, CodecError> {
        let raw = unsafe { ff::av_hwframe_ctx_alloc(device.ctx.0) };
        if raw.is_null() {
            return Err(CodecError::Codec("av_hwframe_ctx_alloc failed".into()));
        }
        let ctx = BufferRef(raw);
        unsafe {
            let fc = (*raw).data as *mut ff::AVHWFramesContext;
            (*fc).format = device.pix_fmt;
            (*fc).sw_format = SW_FORMAT;
            (*fc).width = resolution.width as i32;
            (*fc).height = resolution.height as i32;
            // A ring for a room's tick and an encoder's queue; FFmpeg grows
            // it when it can (CUDA can).
            (*fc).initial_pool_size = 8;
        }
        ffi::check("av_hwframe_ctx_init", unsafe {
            ff::av_hwframe_ctx_init(raw)
        })?;
        Ok(HwFrames {
            ctx,
            resolution,
            pix_fmt: device.pix_fmt,
        })
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    pub fn pix_fmt(&self) -> ff::AVPixelFormat {
        self.pix_fmt
    }

    /// A new reference to the frames context.
    pub fn context(&self) -> BufferRef {
        self.ctx.clone_ref()
    }

    /// An empty frame from the pool.
    pub fn get(&self) -> Result<ffi::Frame, CodecError> {
        let f = ffi::Frame::new()?;
        ffi::check("av_hwframe_get_buffer", unsafe {
            ff::av_hwframe_get_buffer(self.ctx.0, f.0, 0)
        })?;
        Ok(f)
    }
}
