//! The codec side of the container: the parameter sets a keyframe
//! carries and the configuration record built from them (`avcC`,
//! `hvcC`, `av1C`, `dOps`), and the samples as MP4 wants them — H.264
//! and HEVC as length-prefixed NAL units rather than the Annex B byte
//! stream the depacketizers produce, AV1 temporal units as they are.

use crate::{Mp4Error, Mp4VideoCodec};

/// Split an Annex B byte stream into NAL units, without start codes.
/// A three- or four-byte start code (`00 00 01` or `00 00 00 01`)
/// begins each unit; bytes before the first are ignored.
pub fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = data.len();
    let mut start: Option<usize> = None;
    while i + 2 < n {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            // The start code; a fourth zero before it belongs to the code.
            let mut end = i;
            if let Some(s) = start {
                if end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    out.push(&data[s..end]);
                }
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < n {
            out.push(&data[s..]);
        }
    }
    out
}

/// The NAL units of an Annex B frame as one AVCC sample: each with a
/// four-byte big-endian length, parameter sets left out (they live in
/// the configuration record) as `avc1` / `hvc1` samples require.
pub fn annex_b_to_sample(codec: Mp4VideoCodec, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in annex_b_nals(data) {
        if nal.is_empty() || is_parameter_set(codec, nal) {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

fn nal_type(codec: Mp4VideoCodec, nal: &[u8]) -> u8 {
    match codec {
        Mp4VideoCodec::H264 => nal[0] & 0x1F,
        Mp4VideoCodec::Hevc => (nal[0] >> 1) & 0x3F,
        Mp4VideoCodec::Av1 => 0,
    }
}

fn is_parameter_set(codec: Mp4VideoCodec, nal: &[u8]) -> bool {
    match codec {
        Mp4VideoCodec::H264 => matches!(nal_type(codec, nal), 7 | 8),
        Mp4VideoCodec::Hevc => matches!(nal_type(codec, nal), 32..=34),
        Mp4VideoCodec::Av1 => false,
    }
}

/// The parameter sets of a keyframe, by kind, in order of appearance.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    /// H.264 SPS, or HEVC VPS.
    pub first: Vec<Vec<u8>>,
    /// H.264 PPS, or HEVC SPS.
    pub second: Vec<Vec<u8>>,
    /// HEVC PPS.
    pub third: Vec<Vec<u8>>,
}

/// What the configuration record needs from the first keyframe.
pub fn parameter_sets(codec: Mp4VideoCodec, data: &[u8]) -> ParameterSets {
    let mut ps = ParameterSets::default();
    if codec == Mp4VideoCodec::Av1 {
        for obu in av1_obus(data) {
            if obu.kind == 1 {
                ps.first.push(obu.whole.to_vec());
            }
        }
        return ps;
    }
    for nal in annex_b_nals(data) {
        if nal.is_empty() {
            continue;
        }
        match (codec, nal_type(codec, nal)) {
            (Mp4VideoCodec::H264, 7) | (Mp4VideoCodec::Hevc, 32) => ps.first.push(nal.to_vec()),
            (Mp4VideoCodec::H264, 8) | (Mp4VideoCodec::Hevc, 33) => ps.second.push(nal.to_vec()),
            (Mp4VideoCodec::Hevc, 34) => ps.third.push(nal.to_vec()),
            _ => {}
        }
    }
    ps
}

/// The `avcC` payload (ISO/IEC 14496-15 §5.3.3.1).
pub fn avcc(ps: &ParameterSets) -> Result<Vec<u8>, Mp4Error> {
    let sps = ps.first.first().ok_or(Mp4Error::NoParameterSets("SPS"))?;
    if sps.len() < 4 {
        return Err(Mp4Error::NoParameterSets("SPS"));
    }
    if ps.second.is_empty() {
        return Err(Mp4Error::NoParameterSets("PPS"));
    }
    let mut out = Vec::with_capacity(32);
    out.push(1); // configurationVersion
    out.push(sps[1]); // AVCProfileIndication
    out.push(sps[2]); // profile_compatibility
    out.push(sps[3]); // AVCLevelIndication
    out.push(0xFC | 3); // lengthSizeMinusOne = 3: four-byte lengths
    out.push(0xE0 | (ps.first.len().min(31) as u8));
    for s in ps.first.iter().take(31) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    }
    out.push(ps.second.len().min(255) as u8);
    for p in ps.second.iter().take(255) {
        out.extend_from_slice(&(p.len() as u16).to_be_bytes());
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// The `hvcC` payload (ISO/IEC 14496-15 §8.3.3.1). The profile, tier
/// and level come from the VPS's `profile_tier_level`, which sits at a
/// fixed offset after the two-byte NAL header; the format fields say
/// 4:2:0 at 8 bits, what every encoder here produces.
pub fn hvcc(ps: &ParameterSets) -> Result<Vec<u8>, Mp4Error> {
    let vps = ps.first.first().ok_or(Mp4Error::NoParameterSets("VPS"))?;
    if ps.second.is_empty() {
        return Err(Mp4Error::NoParameterSets("SPS"));
    }
    if ps.third.is_empty() {
        return Err(Mp4Error::NoParameterSets("PPS"));
    }
    // NAL header (2) + vps ids/layers/nesting (2) + reserved 0xffff (2),
    // then profile_tier_level: 1 + 4 + 6 + 1 bytes.
    let ptl = vps.get(6..18).ok_or(Mp4Error::NoParameterSets("VPS"))?;
    let mut out = Vec::with_capacity(64);
    out.push(1); // configurationVersion
    out.extend_from_slice(ptl); // profile space/tier/idc, compat flags, constraints, level
    out.extend_from_slice(&0xF000u16.to_be_bytes()); // min_spatial_segmentation_idc
    out.push(0xFC); // parallelismType
    out.push(0xFC | 1); // chromaFormat 4:2:0
    out.push(0xF8); // bitDepthLumaMinus8
    out.push(0xF8); // bitDepthChromaMinus8
    out.extend_from_slice(&0u16.to_be_bytes()); // avgFrameRate
                                                // constantFrameRate 0, numTemporalLayers 1, temporalIdNested 1,
                                                // lengthSizeMinusOne 3.
    out.push((1 << 3) | (1 << 2) | 3);
    let arrays: [(u8, &Vec<Vec<u8>>); 3] = [(32, &ps.first), (33, &ps.second), (34, &ps.third)];
    out.push(arrays.len() as u8);
    for (kind, nals) in arrays {
        out.push(0x80 | kind); // array_completeness = 1
        out.extend_from_slice(&(nals.len().min(65535) as u16).to_be_bytes());
        for n in nals.iter().take(65535) {
            out.extend_from_slice(&(n.len() as u16).to_be_bytes());
            out.extend_from_slice(n);
        }
    }
    Ok(out)
}

/// One OBU of a temporal unit.
pub struct Obu<'a> {
    pub kind: u8,
    /// The OBU with its header and size field, as it sits in the unit.
    pub whole: &'a [u8],
    /// The payload after the header and size.
    pub payload: &'a [u8],
}

/// The OBUs of an AV1 temporal unit (each with `obu_has_size_field`).
pub fn av1_obus(data: &[u8]) -> Vec<Obu<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let start = i;
        let h = data[i];
        let kind = (h >> 3) & 0x0F;
        let has_ext = h & 0x04 != 0;
        let has_size = h & 0x02 != 0;
        i += 1;
        if has_ext {
            i += 1;
        }
        if !has_size {
            // Without a size the OBU runs to the end of the unit.
            out.push(Obu {
                kind,
                whole: &data[start..],
                payload: &data[i.min(data.len())..],
            });
            break;
        }
        // leb128
        let mut size = 0usize;
        let mut shift = 0u32;
        let mut ok = false;
        while i < data.len() && shift < 64 {
            let b = data[i];
            i += 1;
            size |= ((b & 0x7F) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                ok = true;
                break;
            }
        }
        if !ok || i + size > data.len() {
            break;
        }
        out.push(Obu {
            kind,
            whole: &data[start..i + size],
            payload: &data[i..i + size],
        });
        i += size;
    }
    out
}

/// A big-endian bit reader over a sequence header.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(bit as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    fn uvlc(&mut self) -> Option<u32> {
        let mut leading = 0u32;
        while self.bit()? == 0 {
            leading += 1;
            if leading >= 32 {
                return None;
            }
        }
        if leading == 0 {
            return Some(0);
        }
        Some(self.bits(leading)? + (1 << leading) - 1)
    }
}

/// The `av1C` payload (AV1 ISOBMFF binding §2.3): profile, level and
/// tier of the first operating point from the sequence header, the
/// colour fields assumed 8-bit 4:2:0, and the sequence header OBU as the
/// configuration OBU.
pub fn av1c(ps: &ParameterSets) -> Result<Vec<u8>, Mp4Error> {
    let seq = ps
        .first
        .first()
        .ok_or(Mp4Error::NoParameterSets("sequence header"))?;
    let obus = av1_obus(seq);
    let hdr = obus
        .first()
        .ok_or(Mp4Error::NoParameterSets("sequence header"))?;
    let mut b = Bits {
        data: hdr.payload,
        pos: 0,
    };
    let (profile, level, tier) = (|| -> Option<(u32, u32, u32)> {
        let profile = b.bits(3)?;
        let _still_picture = b.bit()?;
        let reduced = b.bit()?;
        if reduced == 1 {
            return Some((profile, b.bits(5)?, 0));
        }
        let timing_info_present = b.bit()?;
        let mut decoder_model_info_present = 0;
        let mut buffer_delay_length = 0;
        if timing_info_present == 1 {
            b.bits(32)?; // num_units_in_display_tick
            b.bits(32)?; // time_scale
            if b.bit()? == 1 {
                b.uvlc()?; // num_ticks_per_picture_minus_1
            }
            decoder_model_info_present = b.bit()?;
            if decoder_model_info_present == 1 {
                buffer_delay_length = b.bits(5)? + 1;
                b.bits(32)?; // num_units_in_decoding_tick
                b.bits(5)?; // buffer_removal_time_length_minus_1
                b.bits(5)?; // frame_presentation_time_length_minus_1
            }
        }
        let initial_display_delay_present = b.bit()?;
        let _op_count = b.bits(5)?;
        // The first operating point.
        b.bits(12)?; // operating_point_idc
        let level = b.bits(5)?;
        let tier = if level > 7 { b.bit()? } else { 0 };
        if decoder_model_info_present == 1 && b.bit()? == 1 {
            b.bits(buffer_delay_length)?; // decoder_buffer_delay
            b.bits(buffer_delay_length)?; // encoder_buffer_delay
            b.bit()?; // low_delay_mode_flag
        }
        if initial_display_delay_present == 1 && b.bit()? == 1 {
            b.bits(4)?;
        }
        Some((profile, level, tier))
    })()
    .ok_or(Mp4Error::NoParameterSets("sequence header"))?;
    let mut out = Vec::with_capacity(4 + seq.len());
    out.push(0x81); // marker, version 1
    out.push(((profile & 7) << 5) as u8 | (level & 0x1F) as u8);
    // tier, high_bitdepth 0, twelve_bit 0, monochrome 0, subsampling 1/1,
    // chroma_sample_position 0.
    out.push(((tier & 1) << 7) as u8 | (1 << 3) | (1 << 2));
    out.push(0); // no initial_presentation_delay
    out.extend_from_slice(seq);
    Ok(out)
}

/// The `dOps` payload (Opus in ISOBMFF §4.3.2).
pub fn dops(channels: u8, pre_skip: u16, input_sample_rate: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.push(0); // Version
    out.push(channels);
    out.extend_from_slice(&pre_skip.to_be_bytes());
    out.extend_from_slice(&input_sample_rate.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes()); // OutputGain
    out.push(0); // ChannelMappingFamily
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An H.264 keyframe as the depacketizers deliver one: SPS, PPS, IDR.
    pub fn h264_keyframe() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x02]); // SPS
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80]); // PPS
        v.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33]); // IDR
        v
    }

    #[test]
    fn annex_b_splits_on_either_start_code_and_drops_parameter_sets_from_samples() {
        let f = h264_keyframe();
        let nals = annex_b_nals(&f);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0][0], 0x67);
        assert_eq!(nals[2], &[0x65, 0x88, 0x84, 0x00, 0x33]);
        let sample = annex_b_to_sample(Mp4VideoCodec::H264, &f);
        assert_eq!(sample, vec![0, 0, 0, 5, 0x65, 0x88, 0x84, 0x00, 0x33]);
        let ps = parameter_sets(Mp4VideoCodec::H264, &f);
        assert_eq!(ps.first.len(), 1);
        assert_eq!(ps.second.len(), 1);
        let rec = avcc(&ps).unwrap();
        assert_eq!(&rec[..5], &[1, 0x42, 0xC0, 0x1E, 0xFF]);
        assert_eq!(rec[5], 0xE1);
        assert!(avcc(&ParameterSets::default()).is_err());
    }

    #[test]
    fn hevc_takes_its_profile_from_the_vps_and_lists_three_arrays() {
        let mut vps = vec![0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF];
        vps.extend_from_slice(&[0x01, 0x60, 0, 0, 0x03, 0, 0, 0, 0, 0, 0, 0x5D]); // ptl
        let sps = vec![0x42, 0x01, 0x01];
        let pps = vec![0x44, 0x01, 0xC1];
        let mut f = Vec::new();
        for n in [&vps, &sps, &pps, &vec![0x26, 0x01, 0xAF]] {
            f.extend_from_slice(&[0, 0, 0, 1]);
            f.extend_from_slice(n);
        }
        let ps = parameter_sets(Mp4VideoCodec::Hevc, &f);
        assert_eq!((ps.first.len(), ps.second.len(), ps.third.len()), (1, 1, 1));
        let rec = hvcc(&ps).unwrap();
        assert_eq!(rec[0], 1);
        assert_eq!(rec[1], 0x01, "profile space/tier/idc from the VPS");
        assert_eq!(rec[12], 0x5D, "level");
        assert_eq!(rec[22], 3, "three arrays");
        let sample = annex_b_to_sample(Mp4VideoCodec::Hevc, &f);
        assert_eq!(sample, vec![0, 0, 0, 3, 0x26, 0x01, 0xAF]);
    }

    /// A sequence header OBU (reduced still picture header off, no
    /// timing info): profile 0, level 8 (4.0), tier 0.
    pub fn av1_keyframe() -> Vec<u8> {
        // seq_profile 000, still 0, reduced 0, timing 0, initial_display 0,
        // op_count 00000, op_idc 000000000000 (bits 12..24), level 01000 (bits 24..29), tier 0.
        let payload: Vec<u8> = vec![0b0000_0000, 0b0000_0000, 0b0000_0000, 0b0100_0000, 0];
        let mut tu = vec![0x0A, payload.len() as u8]; // OBU_SEQUENCE_HEADER, has_size
        tu.extend_from_slice(&payload);
        tu.extend_from_slice(&[0x32, 3, 0xAA, 0xBB, 0xCC]); // frame OBU
        tu
    }

    #[test]
    fn av1_reads_profile_level_and_tier_and_keeps_the_sequence_header() {
        let tu = av1_keyframe();
        let obus = av1_obus(&tu);
        assert_eq!(obus.len(), 2);
        assert_eq!(obus[0].kind, 1);
        assert_eq!(obus[1].kind, 6);
        let ps = parameter_sets(Mp4VideoCodec::Av1, &tu);
        assert_eq!(ps.first.len(), 1);
        let rec = av1c(&ps).unwrap();
        assert_eq!(rec[0], 0x81);
        assert_eq!(rec[1], 8, "profile 0, level 8");
        assert_eq!(rec[2], 0x0C, "4:2:0");
        assert_eq!(&rec[4..], &tu[..7]);
        assert_eq!(annex_b_to_sample(Mp4VideoCodec::Av1, &tu), Vec::<u8>::new());
    }

    #[test]
    fn dops_carries_the_opus_head_fields_big_endian() {
        let d = dops(2, 3840, 48_000);
        assert_eq!(d, vec![0, 2, 0x0F, 0x00, 0, 0, 0xBB, 0x80, 0, 0, 0]);
    }
}
