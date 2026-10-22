//! Inputs the nightly fuzzer found, kept so they stay fixed. Each must do
//! what the `bfcp_parse` target demands: parse and round-trip, or be
//! refused — never be read and then be impossible to write.

use forge_bfcp::Message;

fn holds(data: &[u8]) {
    if let Ok(message) = Message::parse(data) {
        let bytes = message.to_bytes().expect("a parsed message can be written");
        assert_eq!(Message::parse(&bytes).expect("and read again"), message);
    }
}

/// 2026-09-27: a grouped attribute whose last child's padding was cut
/// short, which written back padded no longer fitted its length octet.
#[test]
fn a_group_with_its_last_padding_cut_short() {
    holds(include_bytes!("regressions/bfcp_parse-2359cecb.bin"));
}
