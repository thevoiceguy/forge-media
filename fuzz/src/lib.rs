//! Shared framing for the targets that take a *sequence* of packets.
//!
//! A depacketizer is handed the payloads of one frame, and the assembler a
//! run of RTP packets; neither is interesting when fed a single blob. The
//! fuzzer's input is therefore read as length-prefixed records: a `u16`
//! big-endian length, then that many bytes, repeated until the input runs
//! out. A truncated final record is taken as far as it goes rather than
//! discarded, so a mutation that shortens the input still produces a
//! well-formed sequence to test with.
//!
//! `cargo-fuzz` could derive this from `arbitrary` instead, but its encoding
//! is an implementation detail of that crate, and the seed corpora are
//! written by a stable-toolchain test that has to produce exactly this
//! layout (see `crates/forge-rtp/tests/fuzz_seeds.rs`).

/// Split `data` into length-prefixed records.
pub fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while rest.len() >= 2 {
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        let take = len.min(rest.len());
        out.push(&rest[..take]);
        rest = &rest[take..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::frames;

    #[test]
    fn splits_on_the_prefix_and_tolerates_truncation() {
        assert_eq!(frames(&[0, 2, 1, 2, 0, 1, 3]), vec![&[1, 2][..], &[3][..]]);
        // A length longer than what is left takes what is there.
        assert_eq!(frames(&[0, 9, 1, 2]), vec![&[1, 2][..]]);
        assert!(frames(&[0]).is_empty());
    }
}
