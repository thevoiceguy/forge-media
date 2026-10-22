//! EBML primitives: element ids, variable-length sizes and the typed
//! elements Matroska is built from (RFC 8794 §4, Matroska §5).
//!
//! Everything here writes to a `Write`; the writer patches sizes and
//! positions afterwards through `Seek`, which is why a recording goes to
//! a file rather than a stream.

use std::io::{self, Write};

/// EBML element ids, each the byte sequence it is written as.
pub mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;

    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const VOID: u32 = 0xEC;

    pub const INFO: u32 = 0x1549_A966;
    pub const TIMECODE_SCALE: u32 = 0x002A_D7B1;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const DURATION: u32 = 0x4489;

    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_LACING: u32 = 0x9C;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const SEEK_PRE_ROLL: u32 = 0x56BB;
    pub const DEFAULT_DURATION: u32 = 0x0023_E383;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;

    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMECODE: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;

    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
}

/// The bytes an element id is written as: its significant bytes, which
/// already carry EBML's length marker.
pub fn id_bytes(id: u32) -> Vec<u8> {
    let be = id.to_be_bytes();
    let first = be.iter().position(|b| *b != 0).unwrap_or(3);
    be[first..].to_vec()
}

/// How many bytes a size needs as an EBML variable-length integer. The
/// all-ones value of each length is reserved for "unknown", so a size
/// that would encode as all ones takes one byte more.
pub fn vint_len(value: u64) -> usize {
    for len in 1..=8usize {
        let bits = 7 * len as u32;
        let max = (1u64 << bits) - 1; // reserved: unknown size
        if value < max {
            return len;
        }
    }
    8
}

/// Write `value` as a variable-length integer of exactly `len` bytes.
pub fn write_vint_len<W: Write>(w: &mut W, value: u64, len: usize) -> io::Result<()> {
    debug_assert!((1..=8).contains(&len));
    let mut buf = [0u8; 8];
    for (i, b) in buf.iter_mut().enumerate().take(len) {
        let shift = 8 * (len - 1 - i);
        *b = (value >> shift) as u8;
    }
    // The length marker: a single bit set in the first byte.
    buf[0] |= 1u8 << (8 - len);
    w.write_all(&buf[..len])
}

/// Write `value` as the shortest variable-length integer that holds it.
pub fn write_vint<W: Write>(w: &mut W, value: u64) -> io::Result<()> {
    write_vint_len(w, value, vint_len(value))
}

/// An unsigned integer's shortest big-endian form (at least one byte).
pub fn uint_bytes(value: u64) -> Vec<u8> {
    let be = value.to_be_bytes();
    let first = be.iter().position(|b| *b != 0).unwrap_or(7);
    be[first..].to_vec()
}

/// Write an element id and the size of the payload that follows.
pub fn write_header<W: Write>(w: &mut W, id: u32, size: u64) -> io::Result<()> {
    w.write_all(&id_bytes(id))?;
    write_vint(w, size)
}

/// Write a complete element with a byte payload.
pub fn write_binary<W: Write>(w: &mut W, id: u32, data: &[u8]) -> io::Result<()> {
    write_header(w, id, data.len() as u64)?;
    w.write_all(data)
}

/// Write an unsigned-integer element in its shortest form.
pub fn write_uint<W: Write>(w: &mut W, id: u32, value: u64) -> io::Result<()> {
    write_binary(w, id, &uint_bytes(value))
}

/// Write an unsigned-integer element padded to `len` bytes, so its value
/// can be patched later without moving anything (EBML allows leading
/// zeros in an unsigned integer).
pub fn write_uint_padded<W: Write>(w: &mut W, id: u32, value: u64, len: usize) -> io::Result<()> {
    let be = value.to_be_bytes();
    write_binary(w, id, &be[8 - len..])
}

/// Write a 64-bit float element.
pub fn write_f64<W: Write>(w: &mut W, id: u32, value: f64) -> io::Result<()> {
    write_binary(w, id, &value.to_be_bytes())
}

/// Write a UTF-8 string element.
pub fn write_str<W: Write>(w: &mut W, id: u32, value: &str) -> io::Result<()> {
    write_binary(w, id, value.as_bytes())
}

/// Write a Void element of exactly `total` bytes, header included.
/// Fails below 2 bytes, the smallest a Void can be.
pub fn write_void<W: Write>(w: &mut W, total: usize) -> io::Result<()> {
    assert!(total >= 2, "a Void needs at least 2 bytes");
    // One id byte, then a size varint sized so id + size + payload == total.
    let mut size_len = 1;
    loop {
        let payload = total as i64 - 1 - size_len as i64;
        if payload < 0 {
            return Err(io::Error::other("Void too small"));
        }
        if vint_len(payload as u64) <= size_len {
            w.write_all(&id_bytes(id::VOID))?;
            write_vint_len(w, payload as u64, size_len)?;
            w.write_all(&vec![0u8; payload as usize])?;
            return Ok(());
        }
        size_len += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keep_their_length_marker() {
        assert_eq!(id_bytes(id::EBML), vec![0x1A, 0x45, 0xDF, 0xA3]);
        assert_eq!(id_bytes(id::SEGMENT), vec![0x18, 0x53, 0x80, 0x67]);
        assert_eq!(id_bytes(id::TIMECODE_SCALE), vec![0x2A, 0xD7, 0xB1]);
        assert_eq!(id_bytes(id::TRACK_NUMBER), vec![0xD7]);
        assert_eq!(id_bytes(id::VOID), vec![0xEC]);
    }

    #[test]
    fn sizes_take_the_shortest_form_and_avoid_the_unknown_marker() {
        assert_eq!(vint_len(0), 1);
        assert_eq!(vint_len(126), 1);
        // 127 is 1-byte all-ones, which means "unknown".
        assert_eq!(vint_len(127), 2);
        assert_eq!(vint_len(128), 2);
        assert_eq!(vint_len((1 << 14) - 2), 2);
        assert_eq!(vint_len((1 << 14) - 1), 3);

        let mut out = Vec::new();
        write_vint(&mut out, 0).unwrap();
        assert_eq!(out, vec![0x80]);
        out.clear();
        write_vint(&mut out, 1).unwrap();
        assert_eq!(out, vec![0x81]);
        out.clear();
        write_vint(&mut out, 127).unwrap();
        assert_eq!(out, vec![0x40, 0x7F]);
        out.clear();
        write_vint_len(&mut out, 5, 8).unwrap();
        assert_eq!(out, vec![0x01, 0, 0, 0, 0, 0, 0, 5]);
    }

    #[test]
    fn integers_and_voids_are_the_length_they_claim() {
        assert_eq!(uint_bytes(0), vec![0]);
        assert_eq!(uint_bytes(255), vec![255]);
        assert_eq!(uint_bytes(256), vec![1, 0]);

        for total in 2..40usize {
            let mut out = Vec::new();
            write_void(&mut out, total).unwrap();
            assert_eq!(out.len(), total, "void of {total} bytes");
            assert_eq!(out[0], 0xEC);
        }

        let mut out = Vec::new();
        write_uint_padded(&mut out, id::SEEK_POSITION, 7, 8).unwrap();
        assert_eq!(out.len(), 2 + 1 + 8, "id, size and an 8-byte payload");
        assert_eq!(&out[out.len() - 8..], &[0, 0, 0, 0, 0, 0, 0, 7]);
    }
}
