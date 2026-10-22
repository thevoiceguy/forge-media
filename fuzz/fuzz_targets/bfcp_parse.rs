//! BFCP messages (RFC 8855).
//!
//! A floor server reads whatever a room system — or anyone who can reach
//! its port — sends it. The header's payload length, each attribute's
//! length and the grouped attributes' nesting are all attacker-controlled,
//! and none of them may take the parser past the datagram. Whatever
//! parses is written back and read again, which must give the same
//! message; whatever does not must still yield the header a reply needs
//! when the header was readable.
#![no_main]

use forge_bfcp::{Message, TcpFramer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match Message::parse(data) {
        Ok(m) => {
            let bytes = m.to_bytes().expect("a parsed message can be written");
            let back = Message::parse(&bytes).expect("a written message can be read");
            assert_eq!(back, m);
            let _ = (m.floor_ids(), m.floor_request_id(), m.priority(), m.error_code());
            for (_, info) in m.floor_request_information() {
                let _ = forge_bfcp::message::overall_status(info);
            }
        }
        Err(e) => {
            let _ = e.error_code();
        }
    }
    let mut framer = TcpFramer::new();
    framer.push(data);
    for _ in 0..8 {
        match framer.next_message() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
});
