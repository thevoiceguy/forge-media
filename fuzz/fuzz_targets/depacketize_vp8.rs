//! The VP8 depacketizer (RFC 7741).
//!
//! Every byte here arrives from the network. The payloads of one frame go
//! in and the coded frame comes out; a malformed set must be an error, not
//! a panic, an unbounded allocation or a slice out of bounds.
#![no_main]

use forge_media_fuzz::frames;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The assembler hands the depacketizer one frame's payloads in order,
    // so the interesting input is a sequence of them rather than one blob.
    let packets = frames(data);
    let _ = forge_rtp::video::payload::vp8::depacketize(&packets);
});
