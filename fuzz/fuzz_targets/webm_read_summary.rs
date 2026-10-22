//! The WebM (Matroska) summary reader.
//!
//! It reads back recordings this workspace writes, but a recording on disk
//! is a file like any other: truncated by a crash mid-write, or simply not
//! what it claims to be. EBML is nested and length-prefixed throughout, so a
//! declared element size larger than what is left is the shape of the bug.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = forge_webm::read_summary(data);
});
