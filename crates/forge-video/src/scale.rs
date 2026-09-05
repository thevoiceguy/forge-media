//! I420 scaling and blitting on the host: bilinear scaling of a frame into
//! a rectangle of another, letterbox fitting, and solid fills. Scalar
//! Rust; SIMD comes later if the numbers ask for it (phase 0 measured a
//! 3×3 composite of 720p sources at about one H.264 encode).

use crate::codec::CodecError;
use crate::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use crate::layout::Rect;

/// Scales whole frames on one device. The free functions in this module
/// are the host implementation; a device backend implements this trait
/// over its own frames.
pub trait Scaler: Send {
    fn device(&self) -> MediaDevice;
    /// Scale `src` to `to`, stretching; the caller picks `to` with
    /// [`fit`] when the aspect ratio must be kept. Fails when `src` is not
    /// on this scaler's device.
    fn scale(&mut self, src: &VideoFrame, to: Resolution) -> Result<VideoFrame, CodecError>;
}

/// The CPU scaler: [`resize`].
#[derive(Debug, Default, Clone, Copy)]
pub struct HostScaler;

impl Scaler for HostScaler {
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }

    fn scale(&mut self, src: &VideoFrame, to: Resolution) -> Result<VideoFrame, CodecError> {
        let host = src.as_host().ok_or_else(|| CodecError::WrongDevice {
            expected: MediaDevice::Host,
            actual: src.device(),
        })?;
        Ok(VideoFrame::Host(resize(host, to.width, to.height)))
    }
}

/// Bilinear scale of one plane into a rectangle of the destination plane.
/// `sw`/`sh` are the source region size at `sstride`; the destination
/// rectangle is `dx, dy, dw, dh` at `dstride`.
#[allow(clippy::too_many_arguments)]
pub fn scale_plane(
    src: &[u8],
    sstride: usize,
    sw: usize,
    sh: usize,
    dst: &mut [u8],
    dstride: usize,
    dx: usize,
    dy: usize,
    dw: usize,
    dh: usize,
) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    // 24.8 fixed-point steps.
    let fx = if dw > 1 {
        ((sw - 1) << 8) / (dw - 1)
    } else {
        0
    };
    let fy = if dh > 1 {
        ((sh - 1) << 8) / (dh - 1)
    } else {
        0
    };
    for j in 0..dh {
        let sy = j * fy;
        let y0 = sy >> 8;
        let wy = (sy & 0xFF) as u32;
        let y1 = (y0 + 1).min(sh - 1);
        let row0 = &src[y0 * sstride..y0 * sstride + sw];
        let row1 = &src[y1 * sstride..y1 * sstride + sw];
        let out = &mut dst[(dy + j) * dstride + dx..(dy + j) * dstride + dx + dw];
        for (i, o) in out.iter_mut().enumerate() {
            let sx = i * fx;
            let x0 = sx >> 8;
            let wx = (sx & 0xFF) as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let top = row0[x0] as u32 * (256 - wx) + row0[x1] as u32 * wx;
            let bot = row1[x0] as u32 * (256 - wx) + row1[x1] as u32 * wx;
            *o = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as u8;
        }
    }
}

/// Fill a rectangle of the canvas with one colour.
pub fn fill(dst: &mut HostFrame, r: Rect, y: u8, u: u8, v: u8) {
    let r = r.clip(dst.width, dst.height).even();
    for row in r.y..r.y + r.h {
        let start = row as usize * dst.y_stride + r.x as usize;
        dst.y[start..start + r.w as usize].fill(y);
    }
    for row in r.y / 2..(r.y + r.h) / 2 {
        let start = row as usize * dst.uv_stride + (r.x / 2) as usize;
        let end = start + (r.w / 2) as usize;
        dst.u[start..end].fill(u);
        dst.v[start..end].fill(v);
    }
}

/// Scale the whole source into `r` on the canvas (stretching).
pub fn scale_into(dst: &mut HostFrame, r: Rect, src: &HostFrame) {
    let r = r.clip(dst.width, dst.height).even();
    if r.w == 0 || r.h == 0 {
        return;
    }
    scale_plane(
        &src.y,
        src.y_stride,
        src.width as usize,
        src.height as usize,
        &mut dst.y,
        dst.y_stride,
        r.x as usize,
        r.y as usize,
        r.w as usize,
        r.h as usize,
    );
    for (sp, dp) in [(&src.u, &mut dst.u), (&src.v, &mut dst.v)] {
        scale_plane(
            sp,
            src.uv_stride,
            (src.width / 2) as usize,
            (src.height / 2) as usize,
            dp,
            dst.uv_stride,
            (r.x / 2) as usize,
            (r.y / 2) as usize,
            (r.w / 2) as usize,
            (r.h / 2) as usize,
        );
    }
}

/// The largest rectangle with the source's aspect ratio that fits in `r`,
/// centred: what letterboxing draws into.
pub fn fit(r: Rect, src_w: u32, src_h: u32) -> Rect {
    if src_w == 0 || src_h == 0 || r.w == 0 || r.h == 0 {
        return Rect::new(r.x, r.y, 0, 0);
    }
    // Compare r.w/r.h with src_w/src_h without floats.
    let (w, h) = if (r.w as u64) * (src_h as u64) <= (r.h as u64) * (src_w as u64) {
        // Width-bound.
        let h = ((r.w as u64 * src_h as u64) / src_w as u64) as u32;
        (r.w, h)
    } else {
        let w = ((r.h as u64 * src_w as u64) / src_h as u64) as u32;
        (w, r.h)
    };
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h).even()
}

/// Draw `src` letterboxed into `r`: bars in the given colour, the picture
/// scaled to fit with its aspect ratio kept.
pub fn letterbox(dst: &mut HostFrame, r: Rect, src: &HostFrame, bar: (u8, u8, u8)) {
    let inner = fit(r, src.width, src.height);
    if inner != r {
        fill(dst, r, bar.0, bar.1, bar.2);
    }
    scale_into(dst, inner, src);
}

/// Scale a whole frame to a new size.
pub fn resize(src: &HostFrame, width: u32, height: u32) -> HostFrame {
    let mut out = HostFrame::black(width, height).with_pts(src.pts);
    let full = Rect::new(0, 0, out.width, out.height);
    scale_into(&mut out, full, src);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_a_solid_frame_stays_solid_and_a_gradient_stays_monotonic() {
        let src = HostFrame::solid(64, 48, 200, 90, 160);
        let out = resize(&src, 20, 10);
        assert!(out.y.iter().all(|&p| p == 200));
        assert!(out.u.iter().all(|&p| p == 90) && out.v.iter().all(|&p| p == 160));

        let mut grad = HostFrame::black(64, 2);
        for x in 0..64 {
            grad.y[x] = (x * 4) as u8;
            grad.y[64 + x] = (x * 4) as u8;
        }
        let out = resize(&grad, 16, 2);
        let row: Vec<u8> = out.y[..16].to_vec();
        assert!(row.windows(2).all(|w| w[0] <= w[1]), "{row:?}");
        assert_eq!(row[0], 0);
        assert_eq!(row[15], 252, "last sample maps to the last source sample");
    }

    #[test]
    fn fit_keeps_aspect_and_centres() {
        // 16:9 source into a square: width-bound, bars top and bottom.
        let r = fit(Rect::new(0, 0, 100, 100), 1280, 720);
        assert_eq!(r, Rect::new(0, 22, 100, 56));
        // 4:3 source into a wide tile: height-bound, bars left and right.
        let r = fit(Rect::new(10, 10, 200, 50), 640, 480);
        assert_eq!(r, Rect::new(76, 10, 66, 50));
        // Same aspect: whole rectangle.
        assert_eq!(
            fit(Rect::new(0, 0, 320, 180), 1280, 720),
            Rect::new(0, 0, 320, 180)
        );
        assert_eq!(fit(Rect::new(4, 4, 10, 10), 0, 0).w, 0);
    }

    #[test]
    fn letterbox_paints_bars_and_the_picture() {
        let mut canvas = HostFrame::black(100, 100);
        let src = HostFrame::solid(160, 90, 200, 128, 128);
        letterbox(&mut canvas, Rect::new(0, 0, 100, 100), &src, (40, 128, 128));
        assert_eq!(canvas.luma(50, 5), 40, "bar");
        assert_eq!(canvas.luma(50, 50), 200, "picture");
        assert_eq!(canvas.luma(50, 95), 40, "bar");
    }

    #[test]
    fn host_scaler_resizes_host_frames_and_refuses_others() {
        let mut s = HostScaler;
        assert!(s.device().is_host());
        let src = VideoFrame::Host(HostFrame::solid(64, 48, 200, 90, 160).with_pts(9));
        let out = s.scale(&src, Resolution::new(32, 24)).unwrap();
        assert_eq!(out.resolution(), Resolution::new(32, 24));
        assert_eq!(out.pts(), 9);
        assert_eq!(out.as_host().unwrap().luma(5, 5), 200);
        let gpu = VideoFrame::Device(crate::frame::DeviceFrame {
            device: MediaDevice::parse("vaapi:/dev/dri/renderD128").unwrap(),
            width: 64,
            height: 48,
            pts: 0,
            handle: std::sync::Arc::new(()),
        });
        assert!(matches!(
            s.scale(&gpu, Resolution::new(32, 24)),
            Err(CodecError::WrongDevice { .. })
        ));
    }

    #[test]
    fn fill_respects_clipping_and_chroma() {
        let mut canvas = HostFrame::black(8, 8);
        fill(&mut canvas, Rect::new(4, 4, 100, 100), 210, 60, 70);
        assert_eq!(canvas.luma(3, 3), 16);
        assert_eq!(canvas.luma(4, 4), 210);
        assert_eq!(canvas.luma(7, 7), 210);
        assert_eq!(canvas.chroma(1, 1), (128, 128));
        assert_eq!(canvas.chroma(2, 2), (60, 70));
    }
}
