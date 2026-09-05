//! Picture comparison for tests and parity checks: PSNR over the luma
//! plane. Two renderings of the same layout on different devices must
//! agree within a threshold.

use crate::frame::HostFrame;

/// Peak signal-to-noise ratio of the luma planes, in dB; `f64::INFINITY`
/// for identical planes, `None` when sizes differ.
pub fn psnr_luma(a: &HostFrame, b: &HostFrame) -> Option<f64> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let (w, h) = (a.width as usize, a.height as usize);
    let mut sq = 0u64;
    for row in 0..h {
        let ra = &a.y[row * a.y_stride..row * a.y_stride + w];
        let rb = &b.y[row * b.y_stride..row * b.y_stride + w];
        for (x, y) in ra.iter().zip(rb) {
            let d = *x as i64 - *y as i64;
            sq += (d * d) as u64;
        }
    }
    if sq == 0 {
        return Some(f64::INFINITY);
    }
    let mse = sq as f64 / (w * h) as f64;
    Some(10.0 * (255.0 * 255.0 / mse).log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_infinite_and_noise_lowers_it() {
        let a = HostFrame::solid(32, 32, 120, 128, 128);
        assert_eq!(psnr_luma(&a, &a), Some(f64::INFINITY));
        let mut b = a.clone();
        for (i, p) in b.y.iter_mut().enumerate() {
            *p = if i % 2 == 0 { 125 } else { 115 };
        }
        let p = psnr_luma(&a, &b).unwrap();
        assert!((p - 34.15).abs() < 0.1, "{p}");
        assert!(psnr_luma(&a, &HostFrame::black(16, 16)).is_none());
    }
}
