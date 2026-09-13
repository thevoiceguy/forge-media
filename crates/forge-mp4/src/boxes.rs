//! ISO base media boxes as bytes: a box is a 32-bit size, a four-character
//! type and a payload, a full box adds a version and flags. Everything
//! here builds into a `Vec<u8>` so a `moof` can be sized before its
//! `trun` data offsets are known.

/// A box type: four ASCII bytes.
pub type FourCc = [u8; 4];

/// A box under construction.
pub struct BoxBuf {
    buf: Vec<u8>,
}

impl BoxBuf {
    /// An empty box of `kind`; the size is patched by [`finish`](Self::finish).
    pub fn new(kind: FourCc) -> Self {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&kind);
        Self { buf }
    }

    /// A full box: version and 24-bit flags after the header.
    pub fn full(kind: FourCc, version: u8, flags: u32) -> Self {
        let mut b = Self::new(kind);
        b.u8(version);
        b.bytes(&flags.to_be_bytes()[1..]);
        b
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// A child box, finished.
    pub fn child(&mut self, child: BoxBuf) -> &mut Self {
        self.buf.extend_from_slice(&child.finish());
        self
    }

    /// The bytes so far, size patched in.
    pub fn finish(mut self) -> Vec<u8> {
        let size = self.buf.len() as u32;
        self.buf[..4].copy_from_slice(&size.to_be_bytes());
        self.buf
    }

    /// The position the next write lands at, relative to the box start.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Overwrite four bytes at `at` (relative to the box start).
    pub fn patch_u32(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
}

/// A box with a payload given whole.
pub fn simple(kind: FourCc, payload: &[u8]) -> Vec<u8> {
    let mut b = BoxBuf::new(kind);
    b.bytes(payload);
    b.finish()
}

/// A 16.16 fixed-point value.
pub fn fixed_16_16(v: u32) -> u32 {
    v << 16
}

/// The identity matrix `tkhd` and `mvhd` carry.
pub const IDENTITY_MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_is_sized_and_typed_and_nests() {
        let mut outer = BoxBuf::new(*b"moov");
        let mut inner = BoxBuf::full(*b"mvhd", 0, 0);
        inner.u32(1000);
        outer.child(inner);
        let bytes = outer.finish();
        assert_eq!(&bytes[..4], &(bytes.len() as u32).to_be_bytes());
        assert_eq!(&bytes[4..8], b"moov");
        assert_eq!(&bytes[12..16], b"mvhd");
        assert_eq!(bytes.len(), 8 + 8 + 4 + 4);
        let s = simple(*b"free", &[1, 2, 3]);
        assert_eq!(s, vec![0, 0, 0, 11, b'f', b'r', b'e', b'e', 1, 2, 3]);
        assert_eq!(fixed_16_16(1280), 1280 << 16);
    }
}
