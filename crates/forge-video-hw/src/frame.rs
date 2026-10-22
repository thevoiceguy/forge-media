//! Frames on the device, and the copies across the bus.
//!
//! A [`DeviceFrame`]'s handle is an [`HwFrame`]: an owned `AVFrame` in
//! device memory (NV12) with its frames context. Host frames are I420;
//! [`upload`] interleaves the chroma into NV12 on the way up and
//! [`download`] splits it on the way down.

use crate::device::{HwDevice, HwFrames, SW_FORMAT};
use crate::ffi::{self, Frame};
use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use forge_video::frame::{DeviceFrame, HostFrame, MediaDevice, Resolution, VideoFrame};
use std::sync::Arc;

/// The handle behind a [`DeviceFrame`] on a device this crate opened.
pub struct HwFrame {
    pub(crate) frame: Frame,
}

impl HwFrame {
    /// The raw frame, for the codecs and filters.
    pub(crate) fn raw(&self) -> *mut ff::AVFrame {
        self.frame.0
    }

    pub fn width(&self) -> u32 {
        unsafe { (*self.frame.0).width as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { (*self.frame.0).height as u32 }
    }
}

/// Wrap an owned device `AVFrame` as a [`DeviceFrame`] on `device`.
pub(crate) fn wrap(device: &MediaDevice, frame: Frame, pts: u32) -> VideoFrame {
    let (width, height) = unsafe { ((*frame.0).width as u32, (*frame.0).height as u32) };
    VideoFrame::Device(DeviceFrame {
        device: device.clone(),
        width,
        height,
        pts,
        handle: Arc::new(HwFrame { frame }),
    })
}

/// The [`HwFrame`] behind a frame, when it is one of ours on `device`.
pub fn hw_frame<'a>(
    frame: &'a VideoFrame,
    device: &MediaDevice,
) -> Result<&'a HwFrame, CodecError> {
    match frame {
        VideoFrame::Device(d) if &d.device == device => d
            .handle
            .downcast_ref::<HwFrame>()
            .ok_or_else(|| CodecError::Codec("device frame is not an FFmpeg frame".into())),
        other => Err(CodecError::WrongDevice {
            expected: device.clone(),
            actual: other.device(),
        }),
    }
}

/// Copy a host frame up to `pool`'s device.
pub fn upload(hw: &HwDevice, pool: &HwFrames, host: &HostFrame) -> Result<VideoFrame, CodecError> {
    if host.resolution() != pool.resolution() {
        return Err(CodecError::InvalidConfig(format!(
            "upload of a {}x{} frame into a {} pool",
            host.width,
            host.height,
            pool.resolution()
        )));
    }
    // I420 → NV12 on the host, then one transfer.
    let sw = Frame::new()?;
    unsafe {
        (*sw.0).format = SW_FORMAT as i32;
        (*sw.0).width = host.width as i32;
        (*sw.0).height = host.height as i32;
    }
    ffi::check("av_frame_get_buffer", unsafe {
        ff::av_frame_get_buffer(sw.0, 0)
    })?;
    unsafe {
        let f = &*sw.0;
        let (w, h) = (host.width as usize, host.height as usize);
        let ys = f.linesize[0] as usize;
        for row in 0..h {
            let dst = std::slice::from_raw_parts_mut(f.data[0].add(row * ys), w);
            dst.copy_from_slice(&host.y[row * host.y_stride..row * host.y_stride + w]);
        }
        let uvs = f.linesize[1] as usize;
        let (cw, ch) = (w / 2, h / 2);
        for row in 0..ch {
            let dst = std::slice::from_raw_parts_mut(f.data[1].add(row * uvs), cw * 2);
            let u = &host.u[row * host.uv_stride..row * host.uv_stride + cw];
            let v = &host.v[row * host.uv_stride..row * host.uv_stride + cw];
            for x in 0..cw {
                dst[2 * x] = u[x];
                dst[2 * x + 1] = v[x];
            }
        }
    }
    let dev = pool.get()?;
    ffi::check("av_hwframe_transfer_data (upload)", unsafe {
        ff::av_hwframe_transfer_data(dev.0, sw.0, 0)
    })?;
    unsafe {
        (*dev.0).pts = host.pts as i64;
    }
    Ok(wrap(hw.device(), dev, host.pts))
}

/// Copy a device frame down to the host as I420.
pub fn download(frame: &VideoFrame, device: &MediaDevice) -> Result<HostFrame, CodecError> {
    let hw = hw_frame(frame, device)?;
    let pts = match frame {
        VideoFrame::Device(d) => d.pts,
        VideoFrame::Host(h) => h.pts,
    };
    let sw = Frame::new()?;
    ffi::check("av_hwframe_transfer_data (download)", unsafe {
        ff::av_hwframe_transfer_data(sw.0, hw.raw(), 0)
    })?;
    host_from_sw(&sw, pts)
}

/// An I420 host frame from a software `AVFrame` in NV12 or I420.
pub(crate) fn host_from_sw(sw: &Frame, pts: u32) -> Result<HostFrame, CodecError> {
    unsafe {
        let f = &*sw.0;
        let (w, h) = (f.width as usize, f.height as usize);
        let res = Resolution::new(w as u32, h as u32);
        let mut out = HostFrame::black(res.width, res.height).with_pts(pts);
        let ys = f.linesize[0] as usize;
        for row in 0..h {
            let src = std::slice::from_raw_parts(f.data[0].add(row * ys), w);
            out.y[row * out.y_stride..row * out.y_stride + w].copy_from_slice(src);
        }
        let (cw, ch) = (w / 2, h / 2);
        match f.format {
            x if x == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 => {
                let uvs = f.linesize[1] as usize;
                for row in 0..ch {
                    let src = std::slice::from_raw_parts(f.data[1].add(row * uvs), cw * 2);
                    for x in 0..cw {
                        out.u[row * out.uv_stride + x] = src[2 * x];
                        out.v[row * out.uv_stride + x] = src[2 * x + 1];
                    }
                }
            }
            x if x == ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32 => {
                let us = f.linesize[1] as usize;
                let vs = f.linesize[2] as usize;
                for row in 0..ch {
                    let u = std::slice::from_raw_parts(f.data[1].add(row * us), cw);
                    let v = std::slice::from_raw_parts(f.data[2].add(row * vs), cw);
                    out.u[row * out.uv_stride..row * out.uv_stride + cw].copy_from_slice(u);
                    out.v[row * out.uv_stride..row * out.uv_stride + cw].copy_from_slice(v);
                }
            }
            other => {
                return Err(CodecError::Codec(format!(
                    "downloaded frame in pixel format {other}, not NV12 or I420"
                )))
            }
        }
        Ok(out)
    }
}
