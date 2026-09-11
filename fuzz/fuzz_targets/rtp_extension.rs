//! RTP header extensions (RFC 8285).
//!
//! A browser stamps every packet with a `mid` (and more) in a one-byte or
//! two-byte header extension, and the transport reads it to sort the
//! packet into a section. The container is length-checked by the packet
//! parser; the elements inside it are walked here, id by id, and that walk
//! must never read past the body whatever the lengths say.
#![no_main]

use bytes::Bytes;
use forge_rtp::rtp::RtpPacket;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(packet) = RtpPacket::parse(Bytes::copy_from_slice(data)) {
        if let Some(ext) = &packet.extension {
            for (id, element) in ext.elements() {
                let _ = ext.element(id);
                let _ = element.len();
            }
        }
    }
});
