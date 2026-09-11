//! The transports' bookkeeping: frames cut from a TCP stream, and one
//! UDP client's transactions — replies replayed, notifications sent one
//! at a time and retransmitted on T1 until acknowledged.

use std::time::{Duration, Instant};

use forge_bfcp::{
    Attribute, Message, ParseErrorKind, Primitive, TcpFramer, UdpInbound, UdpPeer, MAX_RETRANSMITS,
    T1, VERSION_RELIABLE, VERSION_UNRELIABLE,
};

#[test]
fn a_tcp_stream_is_cut_into_messages_however_it_arrives() {
    let a = Message::new(VERSION_RELIABLE, Primitive::Hello, 1, 1, 10);
    let b = Message::new(VERSION_RELIABLE, Primitive::FloorRequest, 1, 2, 10)
        .with(Attribute::FloorId(1))
        .with(Attribute::ParticipantProvidedInfo("slides".into()));
    let mut stream = a.to_bytes().unwrap();
    stream.extend(b.to_bytes().unwrap());
    let mut framer = TcpFramer::new();
    // Byte by byte: nothing until a whole message is in.
    let mut got = Vec::new();
    for byte in &stream {
        framer.push(&[*byte]);
        if let Some(m) = framer.next_message().unwrap() {
            got.push(m);
        }
    }
    assert_eq!(got, vec![a.clone(), b.clone()]);
    assert_eq!(framer.pending(), 0);
    // All at once: both.
    framer.push(&stream);
    assert_eq!(framer.next_message().unwrap(), Some(a.clone()));
    assert_eq!(framer.next_message().unwrap(), Some(b.clone()));
    assert_eq!(framer.next_message().unwrap(), None);
    // An unknown primitive is dropped with its length, and the stream goes on.
    let mut bad = a.to_bytes().unwrap();
    bad[1] = 200;
    framer.push(&bad);
    framer.push(&b.to_bytes().unwrap());
    let err = framer.next_message().unwrap_err();
    assert_eq!(err.kind, ParseErrorKind::UnknownPrimitive(200));
    assert_eq!(framer.next_message().unwrap(), Some(b));
}

#[test]
fn a_udp_peer_replays_answers_and_retransmits_notifications() {
    let t0 = Instant::now();
    let mut peer = UdpPeer::new();
    let request = Message::new(VERSION_UNRELIABLE, Primitive::Hello, 1, 5, 10)
        .to_bytes()
        .unwrap();
    assert_eq!(peer.inbound(&request, t0), UdpInbound::Request);
    let reply = Message::response(
        Message::parse(&request).unwrap().echo(),
        Primitive::HelloAck,
    )
    .to_bytes()
    .unwrap();
    peer.cache_response(5, reply.clone(), t0);
    // The same transaction again: the cached reply, not a new request.
    assert_eq!(
        peer.inbound(&request, t0 + Duration::from_millis(600)),
        UdpInbound::Replay(reply.clone())
    );
    // A new transaction is a new request.
    let next = Message::new(VERSION_UNRELIABLE, Primitive::Hello, 1, 6, 10)
        .to_bytes()
        .unwrap();
    assert_eq!(
        peer.inbound(&next, t0 + Duration::from_secs(1)),
        UdpInbound::Request
    );
    // After T2 the old reply is forgotten: a stale retransmission is a request.
    let _ = peer.tick(t0 + Duration::from_secs(11));
    assert_eq!(
        peer.inbound(&request, t0 + Duration::from_secs(11)),
        UdpInbound::Request
    );

    // Two notifications: the first goes now, the second waits its turn.
    let n1 = Message::new(
        VERSION_UNRELIABLE,
        Primitive::FloorRequestStatus,
        1,
        100,
        10,
    );
    let n2 = Message::new(VERSION_UNRELIABLE, Primitive::FloorStatus, 1, 101, 10);
    let t1 = t0 + Duration::from_secs(20);
    let sent = peer.notify(&n1, t1).expect("sent at once");
    assert_eq!(sent, n1.to_bytes().unwrap());
    assert!(peer.notify(&n2, t1).is_none(), "one outstanding at a time");
    assert!(peer.is_busy());
    assert_eq!(peer.queued(), 1);
    // Not yet due.
    assert!(peer.tick(t1 + Duration::from_millis(100)).send.is_empty());
    // Due: retransmitted, and the interval doubles.
    let tick = peer.tick(t1 + T1);
    assert_eq!(tick.send, vec![sent.clone()]);
    assert!(peer
        .tick(t1 + T1 + Duration::from_millis(600))
        .send
        .is_empty());
    assert_eq!(peer.tick(t1 + T1 * 3).send, vec![sent.clone()]);
    // Acknowledged (R set, the same transaction): the next one goes out on
    // the following tick.
    let mut ack = Message::new(
        VERSION_UNRELIABLE,
        Primitive::FloorRequestStatusAck,
        1,
        100,
        10,
    );
    ack.responder = true;
    assert_eq!(
        peer.inbound(&ack.to_bytes().unwrap(), t1 + T1 * 3),
        UdpInbound::Acked {
            transaction_id: 100
        }
    );
    assert!(!peer.is_busy());
    let tick = peer.tick(t1 + T1 * 3);
    assert_eq!(tick.send, vec![n2.to_bytes().unwrap()]);
    assert!(peer.is_busy());
    // An ack for something else is stray.
    assert_eq!(
        peer.inbound(&ack.to_bytes().unwrap(), t1 + T1 * 4),
        UdpInbound::Stray
    );
    // Never acknowledged: after the retransmissions the peer is broken.
    let mut now = t1 + T1 * 3;
    let mut sends = 0;
    for _ in 0..(MAX_RETRANSMITS + 2) {
        now += Duration::from_secs(10);
        let tick = peer.tick(now);
        sends += tick.send.len();
        if tick.broken {
            break;
        }
    }
    assert_eq!(sends as u32, MAX_RETRANSMITS);
    assert!(!peer.is_busy());
    assert_eq!(peer.queued(), 0);
    // Garbage is left to the floor server to answer, if it can.
    assert_eq!(peer.inbound(&[1, 2], now), UdpInbound::Unreadable);
}
