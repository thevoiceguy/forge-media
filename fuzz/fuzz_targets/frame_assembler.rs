//! The video frame assembler.
//!
//! It groups RTP packets into frames, orders them, notices loss and gives
//! up on gaps. It holds state across packets — a reorder buffer, a frame in
//! progress, a skipping flag — so the bug worth finding is not in one packet
//! but in a sequence: a wrapped sequence number, a gap that never fills, a
//! frame that grows without bound.
#![no_main]

use bytes::Bytes;
use forge_core::VideoCodec;
use forge_media_fuzz::frames;
use forge_rtp::rtp::RtpPacket;
use forge_rtp::video::assembler::FrameAssembler;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // First byte picks the codec, the rest is a run of framed RTP packets.
    let Some((&codec, rest)) = data.split_first() else {
        return;
    };
    let codec = match codec % 5 {
        0 => VideoCodec::H264,
        1 => VideoCodec::H265,
        2 => VideoCodec::VP8,
        3 => VideoCodec::VP9,
        _ => VideoCodec::AV1,
    };
    // Small limits on purpose: the default 4 MiB frame cap and 16-packet
    // reorder window are reachable only with inputs far larger than a fuzzer
    // will stumble on, and the boundary conditions are what matter.
    let mut assembler = FrameAssembler::with_limits(codec, 4, 64 * 1024);
    for bytes in frames(rest) {
        if let Ok(packet) = RtpPacket::parse(Bytes::copy_from_slice(bytes)) {
            let _ = assembler.push(packet);
        }
    }
});
