//! The MP4 summary reader.
//!
//! It reads back recordings this workspace writes, but a recording on disk
//! is a file like any other: cut short by a crash mid-fragment, or not what
//! it claims to be. Boxes are nested and length-prefixed throughout, so a
//! declared size larger than what is left, or a `trun` claiming more
//! samples than its bytes hold, is the shape of the bug.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = forge_mp4::read_summary(data);
});
