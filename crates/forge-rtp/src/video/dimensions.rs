//! A keyframe's picture size, read from the bitstream without decoding.
//!
//! A simulcast sender sends the same camera at several sizes. A mixer that
//! decodes only one of them has to know each one's size before it decodes
//! it, to pick the smallest that fills the tile it draws. The size is in
//! every keyframe:
//!
//! - H.264: the sequence parameter set (ITU-T H.264 §7.3.2.1.1), its
//!   macroblock counts less the frame cropping.
//! - VP8: the key frame header (RFC 6386 §9.1), 14 bits each after the
//!   start code.
//!
//! Other codecs answer `None`; a caller ranks their layers some other way.

use forge_core::VideoCodec;

use super::payload::annexb_nal_units;

/// The width and height a keyframe declares, or `None` when the frame is
/// not a keyframe the codec's header can be read from.
pub fn keyframe_dimensions(codec: VideoCodec, frame: &[u8]) -> Option<(u32, u32)> {
    match codec {
        VideoCodec::H264 => annexb_nal_units(frame)
            .into_iter()
            .find(|nal| nal.first().is_some_and(|b| b & 0x1F == 7))
            .and_then(|sps| h264_sps_dimensions(&sps[1..])),
        VideoCodec::VP8 => vp8_keyframe_dimensions(frame),
        _ => None,
    }
}

/// A VP8 key frame: the 3-byte frame tag (bit 0 clear on a key frame),
/// the start code `9d 01 2a`, then width and height as 14 bits each (the
/// top two bits are a scaling hint).
fn vp8_keyframe_dimensions(frame: &[u8]) -> Option<(u32, u32)> {
    if frame.len() < 10 || frame[0] & 0x01 != 0 || frame[3..6] != [0x9d, 0x01, 0x2a] {
        return None;
    }
    let width = u16::from_le_bytes([frame[6], frame[7]]) & 0x3FFF;
    let height = u16::from_le_bytes([frame[8], frame[9]]) & 0x3FFF;
    (width > 0 && height > 0).then_some((width as u32, height as u32))
}

/// Bits of an RBSP: the NAL payload with emulation prevention bytes
/// (`00 00 03`) removed.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn new(nal_payload: &[u8]) -> Self {
        let mut data = Vec::with_capacity(nal_payload.len());
        let mut zeros = 0;
        for &b in nal_payload {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            data.push(b);
        }
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(bit as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    /// Unsigned Exp-Golomb, `ue(v)`.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        Some(((1u64 << zeros) - 1 + rest as u64).min(u32::MAX as u64) as u32)
    }

    /// Signed Exp-Golomb, `se(v)`.
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }
}

/// The profiles whose SPS carries chroma format, bit depths and scaling
/// matrices (H.264 §7.3.2.1.1).
fn has_chroma_info(profile_idc: u32) -> bool {
    matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

fn skip_scaling_list(b: &mut Bits, size: usize) -> Option<()> {
    let mut last = 8i32;
    let mut next = 8i32;
    for _ in 0..size {
        if next != 0 {
            let delta = b.se()?;
            next = (last + delta + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Some(())
}

/// The picture size of an SPS, given the NAL unit after its header byte.
fn h264_sps_dimensions(payload: &[u8]) -> Option<(u32, u32)> {
    let mut b = Bits::new(payload);
    let profile_idc = b.bits(8)?;
    b.bits(16)?; // constraint flags, level_idc
    b.ue()?; // seq_parameter_set_id
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = 0;
    if has_chroma_info(profile_idc) {
        chroma_format_idc = b.ue()?;
        if chroma_format_idc == 3 {
            separate_colour_plane = b.bit()?;
        }
        b.ue()?; // bit_depth_luma_minus8
        b.ue()?; // bit_depth_chroma_minus8
        b.bit()?; // qpprime_y_zero_transform_bypass_flag
        if b.bit()? == 1 {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if b.bit()? == 1 {
                    skip_scaling_list(&mut b, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    b.ue()?; // log2_max_frame_num_minus4
    match b.ue()? {
        0 => {
            b.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            b.bit()?; // delta_pic_order_always_zero_flag
            b.se()?; // offset_for_non_ref_pic
            b.se()?; // offset_for_top_to_bottom_field
            let cycle = b.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                b.se()?;
            }
        }
        _ => {}
    }
    b.ue()?; // max_num_ref_frames
    b.bit()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = b.ue()? as u64 + 1;
    let height_map_units = b.ue()? as u64 + 1;
    let frame_mbs_only = b.bit()? as u64;
    if frame_mbs_only == 0 {
        b.bit()?; // mb_adaptive_frame_field_flag
    }
    b.bit()?; // direct_8x8_inference_flag
    let (mut left, mut right, mut top, mut bottom) = (0u64, 0u64, 0u64, 0u64);
    if b.bit()? == 1 {
        left = b.ue()? as u64;
        right = b.ue()? as u64;
        top = b.ue()? as u64;
        bottom = b.ue()? as u64;
    }
    // ChromaArrayType is 0 with separate colour planes (H.264 §7.4.2.1.1).
    let chroma_array_type = if separate_colour_plane == 1 {
        0
    } else {
        chroma_format_idc
    };
    let (sub_w, sub_h) = match chroma_array_type {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let (crop_x, crop_y) = if chroma_array_type == 0 {
        (1, 2 - frame_mbs_only)
    } else {
        (sub_w, sub_h * (2 - frame_mbs_only))
    };
    let width = (width_mbs * 16).checked_sub((left + right) * crop_x)?;
    let height =
        ((2 - frame_mbs_only) * height_map_units * 16).checked_sub((top + bottom) * crop_y)?;
    (width > 0 && height > 0 && width <= 16_384 && height <= 16_384)
        .then_some((width as u32, height as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an SPS bit by bit, as an encoder would, to test the reader.
    struct Writer {
        bytes: Vec<u8>,
        bits: u32,
    }

    impl Writer {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bits: 0,
            }
        }
        fn bit(&mut self, v: u32) {
            if self.bits % 8 == 0 {
                self.bytes.push(0);
            }
            if v & 1 == 1 {
                let last = self.bytes.last_mut().unwrap();
                *last |= 1 << (7 - (self.bits % 8));
            }
            self.bits += 1;
        }
        fn bits(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                self.bit((v >> i) & 1);
            }
        }
        fn ue(&mut self, v: u32) {
            let x = v as u64 + 1;
            let len = 64 - x.leading_zeros();
            for _ in 0..len - 1 {
                self.bit(0);
            }
            for i in (0..len).rev() {
                self.bit(((x >> i) & 1) as u32);
            }
        }
        fn se(&mut self, v: i32) {
            self.ue(if v > 0 {
                (2 * v - 1) as u32
            } else {
                (-2 * v) as u32
            });
        }
        /// The NAL (header byte 0x67 included) with emulation prevention.
        fn nal(mut self) -> Vec<u8> {
            self.bit(1); // rbsp_stop_one_bit
            let mut out = vec![0x67];
            let mut zeros = 0;
            for b in self.bytes {
                if zeros >= 2 && b <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                zeros = if b == 0 { zeros + 1 } else { 0 };
                out.push(b);
            }
            out
        }
    }

    /// A baseline SPS for `w_mbs`×`h_mbs` macroblocks, cropped to
    /// `crop_bottom` luma rows less (in 4:2:0 units of two).
    fn baseline(w_mbs: u32, h_mbs: u32, crop_bottom: u32) -> Vec<u8> {
        let mut w = Writer::new();
        w.bits(66, 8); // baseline
        w.bits(0xC01F, 16); // constraints, level 3.1
        w.ue(0);
        w.ue(0); // log2_max_frame_num_minus4
        w.ue(2); // pic_order_cnt_type 2
        w.ue(1); // max_num_ref_frames
        w.bit(0);
        w.ue(w_mbs - 1);
        w.ue(h_mbs - 1);
        w.bit(1); // frame_mbs_only
        w.bit(1); // direct_8x8
        if crop_bottom > 0 {
            w.bit(1);
            w.ue(0);
            w.ue(0);
            w.ue(0);
            w.ue(crop_bottom / 2);
        } else {
            w.bit(0);
        }
        w.bit(0); // vui
        w.nal()
    }

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        out
    }

    #[test]
    fn an_h264_keyframe_says_its_size() {
        // 1280×720: 80×45 macroblocks, nothing cropped.
        let frame = annexb(&[
            &baseline(80, 45, 0),
            &[0x68, 0xCE, 0x3C, 0x80],
            &[0x65, 0x88, 0x84],
        ]);
        assert_eq!(
            keyframe_dimensions(VideoCodec::H264, &frame),
            Some((1280, 720))
        );
        // 640×360: 40×23 macroblocks (368 rows), 8 cropped.
        let frame = annexb(&[&baseline(40, 23, 8), &[0x65, 0x88]]);
        assert_eq!(
            keyframe_dimensions(VideoCodec::H264, &frame),
            Some((640, 360))
        );
        // 320×180: 20×12 macroblocks (192 rows), 12 cropped.
        let frame = annexb(&[&baseline(20, 12, 12), &[0x65]]);
        assert_eq!(
            keyframe_dimensions(VideoCodec::H264, &frame),
            Some((320, 180))
        );
    }

    #[test]
    fn a_high_profile_sps_with_scaling_lists_is_read() {
        let mut w = Writer::new();
        w.bits(100, 8); // high
        w.bits(0x0028, 16);
        w.ue(0);
        w.ue(1); // 4:2:0
        w.ue(0);
        w.ue(0);
        w.bit(0);
        w.bit(1); // seq_scaling_matrix_present
        for i in 0..8 {
            if i == 0 {
                w.bit(1);
                for _ in 0..16 {
                    w.se(1);
                }
            } else {
                w.bit(0);
            }
        }
        w.ue(0);
        w.ue(1); // pic_order_cnt_type 1
        w.bit(0);
        w.se(0);
        w.se(0);
        w.ue(2);
        w.se(-1);
        w.se(3);
        w.ue(4);
        w.bit(0);
        w.ue(119); // 1920
        w.ue(67); // 1088
        w.bit(1);
        w.bit(1);
        w.bit(1); // cropping
        w.ue(0);
        w.ue(0);
        w.ue(0);
        w.ue(4); // 8 rows: 1080
        w.bit(0);
        let frame = annexb(&[&w.nal(), &[0x65]]);
        assert_eq!(
            keyframe_dimensions(VideoCodec::H264, &frame),
            Some((1920, 1080))
        );
    }

    #[test]
    fn a_frame_without_an_sps_or_garbage_has_no_size() {
        assert_eq!(
            keyframe_dimensions(VideoCodec::H264, &annexb(&[&[0x41, 0x9a]])),
            None
        );
        assert_eq!(
            keyframe_dimensions(VideoCodec::H264, &annexb(&[&[0x67]])),
            None
        );
        assert_eq!(
            keyframe_dimensions(
                VideoCodec::H264,
                &annexb(&[&[0x67, 0xFF, 0xFF, 0xFF, 0xFF]])
            ),
            None
        );
        assert_eq!(keyframe_dimensions(VideoCodec::VP9, &[0x82, 0x49]), None);
    }

    #[test]
    fn a_vp8_key_frame_says_its_size() {
        let mut frame = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a];
        frame.extend_from_slice(&640u16.to_le_bytes());
        frame.extend_from_slice(&(360u16 | 0x4000).to_le_bytes()); // a scaling hint
        frame.extend_from_slice(&[0; 8]);
        assert_eq!(
            keyframe_dimensions(VideoCodec::VP8, &frame),
            Some((640, 360))
        );
        // An interframe (bit 0 set) has no size.
        frame[0] |= 1;
        assert_eq!(keyframe_dimensions(VideoCodec::VP8, &frame), None);
    }
}
