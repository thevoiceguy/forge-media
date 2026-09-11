//! Seed corpus for the `bfcp_parse` fuzz target (see `fuzz/README.md`).
//!
//! The messages a floor server exchanges — a hello and its answer, a
//! request and its statuses, a query's floor status, an error — written
//! by this crate's own encoder, so the fuzzer starts inside the grouped
//! attributes rather than at the version field. Nothing is written
//! unless `FORGE_FUZZ_SEED_DIR` names a directory.

use forge_bfcp::{
    Attribute, ErrorCode, FloorServer, Message, Primitive, Priority, Transport, VERSION_RELIABLE,
    VERSION_UNRELIABLE,
};
use std::path::PathBuf;

fn write_seed(name: &str, bytes: &[u8]) {
    let Some(root) = std::env::var_os("FORGE_FUZZ_SEED_DIR").map(PathBuf::from) else {
        return;
    };
    let dir = root.join("bfcp_parse");
    std::fs::create_dir_all(&dir).expect("create seed directory");
    std::fs::write(dir.join(name), bytes).expect("write seed");
}

/// Send `m` to the server; the message and its reply, if any, are seeds.
fn exchange(
    seeds: &mut Vec<(String, Vec<u8>)>,
    server: &mut FloorServer,
    name: &str,
    m: Message,
) -> forge_bfcp::Handled {
    let bytes = m.to_bytes().unwrap();
    let handled = server.handle(&bytes);
    seeds.push((name.to_string(), bytes));
    if let Some(reply) = &handled.reply {
        seeds.push((format!("{name}_reply"), reply.to_bytes().unwrap()));
    }
    handled
}

#[test]
fn bfcp_seeds() {
    let mut server = FloorServer::new(4321, 1, Transport::Unreliable);
    server.add_user(10, Some("Ann"), Some("sip:ann@example.com"));
    server.add_user(20, Some("Bob"), None);
    let mut seeds: Vec<(String, Vec<u8>)> = Vec::new();
    let s = &mut seeds;
    exchange(
        s,
        &mut server,
        "hello",
        Message::new(VERSION_UNRELIABLE, Primitive::Hello, 4321, 1, 10),
    );
    exchange(
        s,
        &mut server,
        "floor_query",
        Message::new(VERSION_UNRELIABLE, Primitive::FloorQuery, 4321, 1, 20)
            .with(Attribute::FloorId(1)),
    );
    let h = exchange(
        s,
        &mut server,
        "floor_request",
        Message::new(VERSION_UNRELIABLE, Primitive::FloorRequest, 4321, 2, 10)
            .with(Attribute::FloorId(1))
            .with(Attribute::Priority(Priority::High))
            .with(Attribute::ParticipantProvidedInfo("the slides".into())),
    );
    let (rid, _) = forge_bfcp::server::reported_status(h.reply.as_ref().unwrap()).unwrap();
    for (i, n) in server.grant(rid).into_iter().enumerate() {
        s.push((format!("granted_{i}"), n.message.to_bytes().unwrap()));
    }
    exchange(
        s,
        &mut server,
        "user_query",
        Message::new(VERSION_UNRELIABLE, Primitive::UserQuery, 4321, 3, 20)
            .with(Attribute::BeneficiaryId(10)),
    );
    exchange(
        s,
        &mut server,
        "floor_release",
        Message::new(VERSION_UNRELIABLE, Primitive::FloorRelease, 4321, 4, 10)
            .with(Attribute::FloorRequestId(rid)),
    );
    exchange(
        s,
        &mut server,
        "goodbye",
        Message::new(VERSION_UNRELIABLE, Primitive::Goodbye, 4321, 5, 10),
    );
    s.push((
        "error".into(),
        Message::error(
            Message::new(VERSION_RELIABLE, Primitive::Hello, 1, 9, 9).echo(),
            ErrorCode::UnknownMandatoryAttribute,
            Some("no"),
        )
        .to_bytes()
        .unwrap(),
    ));
    for (name, bytes) in &seeds {
        assert!(Message::parse(bytes).is_ok(), "{name}");
        write_seed(name, bytes);
    }
    assert!(seeds.len() >= 12);
}
