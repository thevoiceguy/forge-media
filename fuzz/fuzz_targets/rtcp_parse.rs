//! The RTCP parser.
//!
//! Compound RTCP packets carry counted, length-prefixed sub-packets — report
//! blocks, SDES chunks, feedback control information — and a parser that
//! trusts any of those counts against a short buffer reads past its end. The
//! packets arrive from the far end of a call, which is to say from anywhere.
#![no_main]

use forge_rtp::rtcp::RtcpPacket;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = RtcpPacket::parse(data);
});
