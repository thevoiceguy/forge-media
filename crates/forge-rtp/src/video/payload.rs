//! RTP payload formats for video: coded frames to RTP payloads and back.
//!
//! One frame in, N payloads out ([`packetize`]); the N payloads of one
//! frame in, the frame out ([`depacketize`]). Grouping packets into frames,
//! ordering them and noticing loss is the assembler's job; this module
//! assumes it gets every payload of one frame, in order.
//!
//! What a "coded frame" is per codec — the form the decoders and encoders
//! forge binds work with:
//!
//! | Codec | Frame bytes | RFC |
//! |---|---|---|
//! | H.264 | Annex B byte stream (`00 00 00 01` before each NAL unit) | 6184 (single NAL, STAP-A, FU-A) |
//! | H.265 | Annex B byte stream | 7798 (single NAL, AP, FU) |
//! | VP8 | The raw VP8 frame | 7741 |
//! | VP9 | The raw VP9 frame | 9628 |
//! | AV1 | A temporal unit of OBUs, each with `obu_has_size_field = 1` | AV1 RTP payload format |

use bytes::{BufMut, Bytes, BytesMut};
use forge_core::VideoCodec;
use thiserror::Error;

/// One coded video frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodedFrame {
    /// RTP timestamp (90 kHz).
    pub timestamp: u32,
    /// Keyframe (IDR / IRAP / VP8 or VP9 key frame / AV1 new coded video
    /// sequence).
    pub keyframe: bool,
    pub data: Bytes,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PayloadError {
    #[error("payload too short")]
    Truncated,
    #[error("unsupported packetization: {0}")]
    Unsupported(&'static str),
    #[error("malformed payload: {0}")]
    Malformed(&'static str),
}

/// Rebuild the coded frame from the payloads of one frame, in order.
pub fn depacketize(codec: VideoCodec, payloads: &[&[u8]]) -> Result<Bytes, PayloadError> {
    match codec {
        VideoCodec::H264 => h264::depacketize(payloads),
        VideoCodec::H265 => h265::depacketize(payloads),
        VideoCodec::VP8 => vp8::depacketize(payloads),
        VideoCodec::VP9 => vp9::depacketize(payloads),
        VideoCodec::AV1 => av1::depacketize(payloads),
    }
}

/// Split a coded frame into RTP payloads of at most `max_payload` bytes.
/// The last payload of the frame is the one to send with the marker bit.
pub fn packetize(
    codec: VideoCodec,
    frame: &CodedFrame,
    max_payload: usize,
) -> Result<Vec<Bytes>, PayloadError> {
    let max_payload = max_payload.max(64);
    match codec {
        VideoCodec::H264 => h264::packetize(&frame.data, max_payload),
        VideoCodec::H265 => h265::packetize(&frame.data, max_payload),
        VideoCodec::VP8 => vp8::packetize(&frame.data, max_payload),
        VideoCodec::VP9 => vp9::packetize(&frame.data, frame.keyframe, max_payload),
        VideoCodec::AV1 => av1::packetize(&frame.data, frame.keyframe, max_payload),
    }
}

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Split an Annex B byte stream into NAL units (without start codes).
pub fn annexb_nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                // Trailing zero of a 4-byte start code belongs to the code.
                let mut end = i;
                if end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    units.push(&data[s..end]);
                }
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < data.len() {
            units.push(&data[s..]);
        }
    }
    units
}

/// Fragment `nal` into FU payloads. `header_len` is the NAL header size
/// (1 for H.264, 2 for H.265); `make_header` writes the FU indicator and
/// header for (start, end) into `out`.
fn fragment(
    nal: &[u8],
    header_len: usize,
    max_payload: usize,
    mut make_header: impl FnMut(&mut BytesMut, bool, bool),
) -> Vec<Bytes> {
    let body = &nal[header_len..];
    let fu_overhead = header_len + 1;
    let chunk = max_payload.saturating_sub(fu_overhead).max(1);
    let n = body.len().div_ceil(chunk).max(1);
    let mut out = Vec::with_capacity(n);
    for (i, part) in body.chunks(chunk).enumerate() {
        let mut b = BytesMut::with_capacity(fu_overhead + part.len());
        make_header(&mut b, i == 0, i == n - 1);
        b.put_slice(part);
        out.push(b.freeze());
    }
    out
}

pub mod h264 {
    //! RFC 6184: single NAL unit, STAP-A, FU-A.
    use super::*;

    const STAP_A: u8 = 24;
    const FU_A: u8 = 28;

    pub fn depacketize(payloads: &[&[u8]]) -> Result<Bytes, PayloadError> {
        let mut out = BytesMut::new();
        let mut fu_open = false;
        for p in payloads {
            let &first = p.first().ok_or(PayloadError::Truncated)?;
            match first & 0x1F {
                1..=23 => {
                    out.put_slice(&START_CODE);
                    out.put_slice(p);
                }
                STAP_A => {
                    let mut off = 1;
                    while off < p.len() {
                        if off + 2 > p.len() {
                            return Err(PayloadError::Truncated);
                        }
                        let size = u16::from_be_bytes([p[off], p[off + 1]]) as usize;
                        off += 2;
                        let nal = p.get(off..off + size).ok_or(PayloadError::Truncated)?;
                        out.put_slice(&START_CODE);
                        out.put_slice(nal);
                        off += size;
                    }
                }
                FU_A => {
                    let &fu = p.get(1).ok_or(PayloadError::Truncated)?;
                    let start = fu & 0x80 != 0;
                    let end = fu & 0x40 != 0;
                    if start {
                        out.put_slice(&START_CODE);
                        out.put_u8((first & 0xE0) | (fu & 0x1F));
                        fu_open = true;
                    } else if !fu_open {
                        return Err(PayloadError::Malformed("FU-A continuation without start"));
                    }
                    out.put_slice(&p[2..]);
                    if end {
                        fu_open = false;
                    }
                }
                25..=27 | 29 => return Err(PayloadError::Unsupported("STAP-B / MTAP / FU-B")),
                _ => return Err(PayloadError::Malformed("reserved NAL type")),
            }
        }
        if fu_open {
            return Err(PayloadError::Malformed("FU-A not ended"));
        }
        Ok(out.freeze())
    }

    pub fn packetize(annexb: &[u8], max_payload: usize) -> Result<Vec<Bytes>, PayloadError> {
        let nals = annexb_nal_units(annexb);
        if nals.is_empty() {
            return Err(PayloadError::Malformed("no NAL units"));
        }
        let mut out = Vec::new();
        for nal in nals {
            if nal.len() <= max_payload {
                out.push(Bytes::copy_from_slice(nal));
            } else {
                let indicator = (nal[0] & 0xE0) | FU_A;
                let nal_type = nal[0] & 0x1F;
                out.extend(fragment(nal, 1, max_payload, |b, start, end| {
                    b.put_u8(indicator);
                    b.put_u8((start as u8) << 7 | (end as u8) << 6 | nal_type);
                }));
            }
        }
        Ok(out)
    }
}

pub mod h265 {
    //! RFC 7798: single NAL unit, aggregation packet (AP), fragmentation
    //! unit (FU). NAL header is two bytes: `F(1) Type(6) LayerId(6) TID(3)`.
    use super::*;

    const AP: u8 = 48;
    const FU: u8 = 49;

    fn nal_type(b0: u8) -> u8 {
        (b0 >> 1) & 0x3F
    }

    pub fn depacketize(payloads: &[&[u8]]) -> Result<Bytes, PayloadError> {
        let mut out = BytesMut::new();
        let mut fu_open = false;
        for p in payloads {
            if p.len() < 2 {
                return Err(PayloadError::Truncated);
            }
            match nal_type(p[0]) {
                AP => {
                    let mut off = 2;
                    while off < p.len() {
                        if off + 2 > p.len() {
                            return Err(PayloadError::Truncated);
                        }
                        let size = u16::from_be_bytes([p[off], p[off + 1]]) as usize;
                        off += 2;
                        let nal = p.get(off..off + size).ok_or(PayloadError::Truncated)?;
                        out.put_slice(&START_CODE);
                        out.put_slice(nal);
                        off += size;
                    }
                }
                FU => {
                    let &fu = p.get(2).ok_or(PayloadError::Truncated)?;
                    let start = fu & 0x80 != 0;
                    let end = fu & 0x40 != 0;
                    if start {
                        out.put_slice(&START_CODE);
                        out.put_u8((p[0] & 0x81) | ((fu & 0x3F) << 1));
                        out.put_u8(p[1]);
                        fu_open = true;
                    } else if !fu_open {
                        return Err(PayloadError::Malformed("FU continuation without start"));
                    }
                    out.put_slice(&p[3..]);
                    if end {
                        fu_open = false;
                    }
                }
                50 => return Err(PayloadError::Unsupported("PACI")),
                _ => {
                    out.put_slice(&START_CODE);
                    out.put_slice(p);
                }
            }
        }
        if fu_open {
            return Err(PayloadError::Malformed("FU not ended"));
        }
        Ok(out.freeze())
    }

    pub fn packetize(annexb: &[u8], max_payload: usize) -> Result<Vec<Bytes>, PayloadError> {
        let nals = annexb_nal_units(annexb);
        if nals.is_empty() {
            return Err(PayloadError::Malformed("no NAL units"));
        }
        let mut out = Vec::new();
        for nal in nals {
            if nal.len() < 2 {
                return Err(PayloadError::Malformed("NAL unit shorter than its header"));
            }
            if nal.len() <= max_payload {
                out.push(Bytes::copy_from_slice(nal));
            } else {
                let hdr0 = (nal[0] & 0x81) | (FU << 1);
                let hdr1 = nal[1];
                let t = nal_type(nal[0]);
                out.extend(fragment(nal, 2, max_payload, |b, start, end| {
                    b.put_u8(hdr0);
                    b.put_u8(hdr1);
                    b.put_u8((start as u8) << 7 | (end as u8) << 6 | t);
                }));
            }
        }
        Ok(out)
    }
}

pub mod vp8 {
    //! RFC 7741: a payload descriptor per packet, then a slice of the frame.
    use super::*;
    use crate::video::vp8::descriptor_len;

    pub fn depacketize(payloads: &[&[u8]]) -> Result<Bytes, PayloadError> {
        let mut out = BytesMut::new();
        for p in payloads {
            let len = descriptor_len(p).ok_or(PayloadError::Truncated)?;
            out.put_slice(&p[len..]);
        }
        Ok(out.freeze())
    }

    /// Minimal descriptor: one byte, `S` on the first packet, partition
    /// index 0.
    pub fn packetize(frame: &[u8], max_payload: usize) -> Result<Vec<Bytes>, PayloadError> {
        if frame.is_empty() {
            return Err(PayloadError::Malformed("empty frame"));
        }
        let chunk = max_payload - 1;
        Ok(frame
            .chunks(chunk)
            .enumerate()
            .map(|(i, part)| {
                let mut b = BytesMut::with_capacity(part.len() + 1);
                b.put_u8(if i == 0 { 0x10 } else { 0x00 });
                b.put_slice(part);
                b.freeze()
            })
            .collect())
    }
}

pub mod vp9 {
    //! RFC 9628: descriptor `I P L F B E V Z`, optional picture id, layer
    //! indices, reference indices and scalability structure.
    use super::*;

    /// Length of the payload descriptor, or `None` when truncated.
    pub fn descriptor_len(p: &[u8]) -> Option<usize> {
        let b0 = *p.first()?;
        let (i, pp, l, f, v) = (
            b0 & 0x80 != 0,
            b0 & 0x40 != 0,
            b0 & 0x20 != 0,
            b0 & 0x10 != 0,
            b0 & 0x02 != 0,
        );
        let mut off = 1;
        if i {
            let m = *p.get(off)?;
            off += if m & 0x80 != 0 { 2 } else { 1 };
        }
        if l {
            off += 1;
            if !f {
                off += 1; // TL0PICIDX
            }
        }
        if f && pp {
            // Up to 3 reference indices, each with an N (more follows) bit.
            for _ in 0..3 {
                let r = *p.get(off)?;
                off += 1;
                if r & 0x01 == 0 {
                    break;
                }
            }
        }
        if v {
            let ss = *p.get(off)?;
            off += 1;
            let n_s = ((ss >> 5) & 0x07) as usize + 1;
            let y = ss & 0x10 != 0;
            let g = ss & 0x08 != 0;
            if y {
                off += 4 * n_s;
            }
            if g {
                let n_g = *p.get(off)? as usize;
                off += 1;
                for _ in 0..n_g {
                    let t = *p.get(off)?;
                    off += 1;
                    let r = ((t >> 2) & 0x03) as usize;
                    off += r;
                }
            }
        }
        (off <= p.len()).then_some(off)
    }

    pub fn depacketize(payloads: &[&[u8]]) -> Result<Bytes, PayloadError> {
        let mut out = BytesMut::new();
        for p in payloads {
            let len = descriptor_len(p).ok_or(PayloadError::Truncated)?;
            out.put_slice(&p[len..]);
        }
        Ok(out.freeze())
    }

    /// Minimal descriptor: one byte with `B` on the first packet, `E` on
    /// the last, `P` unless the frame is a keyframe.
    pub fn packetize(
        frame: &[u8],
        keyframe: bool,
        max_payload: usize,
    ) -> Result<Vec<Bytes>, PayloadError> {
        if frame.is_empty() {
            return Err(PayloadError::Malformed("empty frame"));
        }
        let chunk = max_payload - 1;
        let n = frame.len().div_ceil(chunk);
        Ok(frame
            .chunks(chunk)
            .enumerate()
            .map(|(i, part)| {
                let mut d = 0u8;
                if !keyframe {
                    d |= 0x40;
                }
                if i == 0 {
                    d |= 0x08;
                }
                if i == n - 1 {
                    d |= 0x04;
                }
                let mut b = BytesMut::with_capacity(part.len() + 1);
                b.put_u8(d);
                b.put_slice(part);
                b.freeze()
            })
            .collect())
    }
}

pub mod av1 {
    //! AV1 RTP payload format: aggregation header `Z Y W W N - - -`, then
    //! OBU elements, each LEB128-sized except (when `W > 0`) the last.
    //! OBUs travel without their size field; the temporal unit handed to
    //! the decoder gets size fields put back.
    use super::*;

    fn read_leb128(p: &[u8], off: &mut usize) -> Option<usize> {
        let mut value = 0usize;
        for i in 0..8 {
            let b = *p.get(*off)?;
            *off += 1;
            value |= ((b & 0x7F) as usize) << (7 * i);
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn write_leb128(out: &mut BytesMut, mut v: usize) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.put_u8(b);
                return;
            }
            out.put_u8(b | 0x80);
        }
    }

    const OBU_TEMPORAL_DELIMITER: u8 = 2;

    /// Header length of an OBU: 1, plus 1 with the extension flag.
    fn obu_header_len(b0: u8) -> usize {
        if b0 & 0x04 != 0 {
            2
        } else {
            1
        }
    }

    /// Emit one complete OBU (as carried in RTP, without size field) into
    /// the temporal unit with `obu_has_size_field` set.
    fn emit_obu(out: &mut BytesMut, obu: &[u8]) -> Result<(), PayloadError> {
        let &b0 = obu.first().ok_or(PayloadError::Truncated)?;
        let hl = obu_header_len(b0);
        if obu.len() < hl {
            return Err(PayloadError::Truncated);
        }
        out.put_u8(b0 | 0x02);
        if hl == 2 {
            out.put_u8(obu[1]);
        }
        write_leb128(out, obu.len() - hl);
        out.put_slice(&obu[hl..]);
        Ok(())
    }

    pub fn depacketize(payloads: &[&[u8]]) -> Result<Bytes, PayloadError> {
        let mut out = BytesMut::new();
        let mut partial: Option<BytesMut> = None;
        for p in payloads {
            let &agg = p.first().ok_or(PayloadError::Truncated)?;
            let z = agg & 0x80 != 0;
            let y = agg & 0x40 != 0;
            let w = ((agg >> 4) & 0x03) as usize;
            if z != partial.is_some() {
                return Err(PayloadError::Malformed("OBU fragment continuity"));
            }
            let mut off = 1;
            let mut idx = 0;
            while off < p.len() {
                let last = w > 0 && idx == w - 1;
                let size = if last {
                    p.len() - off
                } else {
                    read_leb128(p, &mut off).ok_or(PayloadError::Truncated)?
                };
                let elem = p.get(off..off + size).ok_or(PayloadError::Truncated)?;
                off += size;
                idx += 1;
                let continues = y && off >= p.len();
                match partial.take() {
                    Some(mut buf) => {
                        buf.put_slice(elem);
                        if continues {
                            partial = Some(buf);
                        } else {
                            emit_obu(&mut out, &buf)?;
                        }
                    }
                    None => {
                        if continues {
                            partial = Some(BytesMut::from(elem));
                        } else {
                            emit_obu(&mut out, elem)?;
                        }
                    }
                }
            }
        }
        if partial.is_some() {
            return Err(PayloadError::Malformed("OBU fragment not ended"));
        }
        Ok(out.freeze())
    }

    /// Split a temporal unit (OBUs with size fields) into packets, each
    /// with `W = 0` sizing. Temporal delimiters are dropped. `N` is set on
    /// the first packet of a keyframe.
    pub fn packetize(
        tu: &[u8],
        keyframe: bool,
        max_payload: usize,
    ) -> Result<Vec<Bytes>, PayloadError> {
        // Collect OBUs without size fields.
        let mut obus: Vec<Vec<u8>> = Vec::new();
        let mut off = 0;
        while off < tu.len() {
            let b0 = tu[off];
            let hl = obu_header_len(b0);
            let has_size = b0 & 0x02 != 0;
            if !has_size {
                return Err(PayloadError::Malformed(
                    "OBU without size field in temporal unit",
                ));
            }
            let mut o = off + hl;
            let size = read_leb128(tu, &mut o).ok_or(PayloadError::Truncated)?;
            let payload = tu.get(o..o + size).ok_or(PayloadError::Truncated)?;
            let obu_type = (b0 >> 3) & 0x0F;
            if obu_type != OBU_TEMPORAL_DELIMITER {
                let mut v = Vec::with_capacity(hl + size);
                v.push(b0 & !0x02);
                if hl == 2 {
                    v.push(tu[off + 1]);
                }
                v.extend_from_slice(payload);
                obus.push(v);
            }
            off = o + size;
        }
        if obus.is_empty() {
            return Err(PayloadError::Malformed("no OBUs"));
        }

        let mut packets: Vec<Bytes> = Vec::new();
        let mut cur = BytesMut::with_capacity(max_payload);
        cur.put_u8(0); // aggregation header, patched below
        let mut cur_z = false;
        let mut first_packet = true;
        let finish =
            |cur: &mut BytesMut, z: bool, y: bool, first: &mut bool, packets: &mut Vec<Bytes>| {
                let mut hdr = 0u8;
                if z {
                    hdr |= 0x80;
                }
                if y {
                    hdr |= 0x40;
                }
                if *first && keyframe {
                    hdr |= 0x08;
                }
                cur[0] = hdr;
                packets.push(std::mem::replace(cur, BytesMut::with_capacity(max_payload)).freeze());
                cur.put_u8(0);
                *first = false;
            };
        for obu in &obus {
            let mut rest: &[u8] = obu;
            loop {
                // Space left after a worst-case 2-byte LEB128 prefix.
                let room = max_payload.saturating_sub(cur.len() + 2);
                if room < 8 && cur.len() > 1 {
                    finish(&mut cur, cur_z, false, &mut first_packet, &mut packets);
                    cur_z = false;
                    continue;
                }
                if rest.len() <= room {
                    write_leb128(&mut cur, rest.len());
                    cur.put_slice(rest);
                    break;
                }
                // Fragment: fill this packet, continue in the next.
                let (part, tail) = rest.split_at(room);
                write_leb128(&mut cur, part.len());
                cur.put_slice(part);
                finish(&mut cur, cur_z, true, &mut first_packet, &mut packets);
                cur_z = true;
                rest = tail;
            }
        }
        if cur.len() > 1 {
            finish(&mut cur, cur_z, false, &mut first_packet, &mut packets);
        }
        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::inspect;

    fn frame(codec: VideoCodec, keyframe: bool, data: Vec<u8>) -> CodedFrame {
        let _ = codec;
        CodedFrame {
            timestamp: 9000,
            keyframe,
            data: Bytes::from(data),
        }
    }

    fn round_trip(codec: VideoCodec, f: &CodedFrame, mtu: usize) -> (Vec<Bytes>, Bytes) {
        let pk = packetize(codec, f, mtu).unwrap();
        assert!(pk.iter().all(|p| p.len() <= mtu), "payload over MTU");
        let refs: Vec<&[u8]> = pk.iter().map(|b| b.as_ref()).collect();
        let back = depacketize(codec, &refs).unwrap();
        (pk, back)
    }

    #[test]
    fn annexb_splits_three_and_four_byte_start_codes() {
        let s = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5,
        ];
        let nals = annexb_nal_units(&s);
        assert_eq!(
            nals,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]
        );
        assert!(annexb_nal_units(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn h264_small_nals_go_single_and_large_ones_fragment() {
        let mut data = vec![
            0, 0, 0, 1, 0x67, 0x42, 0xE0, 0x1F, 0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80,
        ];
        data.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        data.extend((0..3000u32).map(|i| (i % 251) as u8 + 1));
        let f = frame(VideoCodec::H264, true, data.clone());
        let (pk, back) = round_trip(VideoCodec::H264, &f, 1200);
        assert_eq!(back.as_ref(), data.as_slice());
        // SPS, PPS single; IDR in FU-A fragments (3 of them).
        assert_eq!(pk.len(), 5);
        assert_eq!(pk[0][0] & 0x1F, 7);
        assert_eq!(pk[2][0] & 0x1F, 28);
        assert_eq!(pk[2][1] & 0x80, 0x80, "FU start");
        assert_eq!(pk[4][1] & 0x40, 0x40, "FU end");
        // The inspector agrees on where the keyframe starts.
        assert!(inspect(VideoCodec::H264, &pk[0]).is_keyframe_start());
        assert!(inspect(VideoCodec::H264, &pk[2]).is_keyframe_start());
        assert!(!inspect(VideoCodec::H264, &pk[3]).frame_start);
    }

    #[test]
    fn h264_stap_a_and_bad_fragments_are_handled() {
        let stap = [0x78, 0, 2, 0x67, 0x42, 0, 3, 0x68, 0xCE, 0x38];
        let out = depacketize(VideoCodec::H264, &[&stap]).unwrap();
        assert_eq!(
            out.as_ref(),
            &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE, 0x38]
        );
        // Continuation without a start.
        assert_eq!(
            depacketize(VideoCodec::H264, &[&[0x7C, 0x05, 1, 2]]),
            Err(PayloadError::Malformed("FU-A continuation without start"))
        );
        assert_eq!(
            depacketize(VideoCodec::H264, &[&[0x79, 0]]),
            Err(PayloadError::Unsupported("STAP-B / MTAP / FU-B"))
        );
        assert_eq!(
            depacketize(VideoCodec::H264, &[&[]]),
            Err(PayloadError::Truncated)
        );
    }

    #[test]
    fn h265_round_trips_with_two_byte_headers() {
        // VPS(32), SPS(33), PPS(34) small; IDR_W_RADL(19) large.
        let mut data = Vec::new();
        for t in [32u8, 33, 34] {
            data.extend_from_slice(&[0, 0, 0, 1, t << 1, 1, 0xAA, 0xBB]);
        }
        data.extend_from_slice(&[0, 0, 0, 1, 19 << 1, 1]);
        data.extend((0..2500u32).map(|i| (i % 200) as u8 + 7));
        let f = frame(VideoCodec::H265, true, data.clone());
        let (pk, back) = round_trip(VideoCodec::H265, &f, 1000);
        assert_eq!(back.as_ref(), data.as_slice());
        assert_eq!((pk[3][0] >> 1) & 0x3F, 49, "FU");
        assert_eq!(pk[3][2] & 0x3F, 19, "FU type");
        assert!(inspect(VideoCodec::H265, &pk[3]).is_keyframe_start());
        // AP with two NALs.
        let ap = [48 << 1, 1, 0, 3, 32 << 1, 1, 9, 0, 3, 33 << 1, 1, 8];
        let out = depacketize(VideoCodec::H265, &[&ap]).unwrap();
        assert_eq!(
            out.as_ref(),
            &[0, 0, 0, 1, 32 << 1, 1, 9, 0, 0, 0, 1, 33 << 1, 1, 8]
        );
    }

    #[test]
    fn vp8_and_vp9_strip_descriptors_and_mark_frame_edges() {
        let data: Vec<u8> = (0..2600u32).map(|i| (i * 7 % 256) as u8).collect();
        // VP8 keyframe: payload header P bit 0 — first byte even.
        let mut key = data.clone();
        key[0] = 0x00;
        let f = frame(VideoCodec::VP8, true, key.clone());
        let (pk, back) = round_trip(VideoCodec::VP8, &f, 1000);
        assert_eq!(back.as_ref(), key.as_slice());
        assert_eq!(pk.len(), 3);
        assert!(inspect(VideoCodec::VP8, &pk[0]).is_keyframe_start());
        assert_eq!(
            inspect(VideoCodec::VP8, &pk[1]),
            crate::video::PayloadInfo {
                frame_start: false,
                keyframe: false
            }
        );
        // Extended descriptor on the way in is fine too.
        let ext = [0x90u8, 0xC0, 0x05, 0x01, 0xAB, 0xCD];
        assert_eq!(
            depacketize(VideoCodec::VP8, &[&ext]).unwrap().as_ref(),
            &[0xAB, 0xCD]
        );

        let f = frame(VideoCodec::VP9, false, data.clone());
        let (pk, back) = round_trip(VideoCodec::VP9, &f, 1000);
        assert_eq!(back.as_ref(), data.as_slice());
        assert_eq!(pk[0][0] & 0x08, 0x08, "B on first");
        assert_eq!(pk[2][0] & 0x04, 0x04, "E on last");
        assert_eq!(pk[1][0] & 0x0C, 0, "middle has neither");
        assert!(pk.iter().all(|p| p[0] & 0x40 != 0), "P on an inter frame");
        let kf = frame(VideoCodec::VP9, true, data.clone());
        let (pk, _) = round_trip(VideoCodec::VP9, &kf, 1000);
        assert!(inspect(VideoCodec::VP9, &pk[0]).is_keyframe_start());
    }

    #[test]
    fn vp9_descriptor_length_covers_every_optional_field() {
        // I (2-byte pid) + L + TL0PICIDX (no F) = 1 + 2 + 1 + 1.
        assert_eq!(
            vp9::descriptor_len(&[0xA0, 0x80, 0x01, 0x00, 0x05, 0xFF]),
            Some(5)
        );
        // F + P with two reference indices (first has N set).
        assert_eq!(vp9::descriptor_len(&[0x50, 0x03, 0x02, 0xFF]), Some(3));
        // V with SS: N_S=1 (2 layers), Y, G with 1 group of 2 refs.
        let ss = [
            0x02u8,
            0b0011_1000,
            0,
            1,
            0,
            2,
            0,
            3,
            0,
            4,
            1,
            0b0000_1000,
            9,
            9,
            0xEE,
        ];
        assert_eq!(vp9::descriptor_len(&ss), Some(14));
        assert_eq!(vp9::descriptor_len(&[0x80]), None);
    }

    #[test]
    fn av1_temporal_units_round_trip_with_size_fields_restored() {
        // OBUs with size fields: temporal delimiter, sequence header
        // (type 1), frame (type 6) large.
        let mut tu = vec![0x12, 0x00]; // TD, size 0
        tu.extend_from_slice(&[0x0A, 0x03, 0xAA, 0xBB, 0xCC]); // seq hdr, size 3
        let big: Vec<u8> = (0..2000u32).map(|i| (i % 253) as u8).collect();
        tu.push(0x32); // frame OBU, has_size
                       // LEB128 of 2000 = 0xD0 0x0F
        tu.extend_from_slice(&[0xD0, 0x0F]);
        tu.extend_from_slice(&big);
        let f = frame(VideoCodec::AV1, true, tu.clone());
        let pk = packetize(VideoCodec::AV1, &f, 1200).unwrap();
        assert!(pk.iter().all(|p| p.len() <= 1200));
        assert_eq!(pk[0][0] & 0x08, 0x08, "N on first packet of a keyframe");
        assert!(inspect(VideoCodec::AV1, &pk[0]).is_keyframe_start());
        assert_eq!(pk[0][0] & 0x80, 0, "first packet does not continue");
        assert_eq!(
            pk[pk.len() - 1][0] & 0x40,
            0,
            "last packet has no continuation"
        );
        assert!(pk.len() >= 2);
        assert_eq!(
            pk[1][0] & 0x80,
            0x80,
            "second packet continues the frame OBU"
        );
        let refs: Vec<&[u8]> = pk.iter().map(|b| b.as_ref()).collect();
        let back = depacketize(VideoCodec::AV1, &refs).unwrap();
        // Same as the input minus the temporal delimiter.
        assert_eq!(back.as_ref(), &tu[2..]);
    }

    #[test]
    fn av1_rejects_broken_fragment_chains() {
        // Z set on a first packet.
        assert_eq!(
            depacketize(VideoCodec::AV1, &[&[0x80, 0x01, 0x0A]]),
            Err(PayloadError::Malformed("OBU fragment continuity"))
        );
        // Y set on the last packet.
        assert_eq!(
            depacketize(VideoCodec::AV1, &[&[0x40, 0x01, 0x0A]]),
            Err(PayloadError::Malformed("OBU fragment not ended"))
        );
        // W = 1: last element unsized.
        let out = depacketize(VideoCodec::AV1, &[&[0x10, 0x0A, 0xAA]]).unwrap();
        assert_eq!(out.as_ref(), &[0x0A | 0x02, 0x01, 0xAA]);
    }

    #[test]
    fn packetize_refuses_empty_input() {
        for codec in VideoCodec::ALL {
            let f = frame(codec, true, Vec::new());
            assert!(packetize(codec, &f, 1200).is_err(), "{codec}");
        }
    }
}
