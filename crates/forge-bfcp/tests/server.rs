//! The floor server as RFC 8855 §13 has it: a scripted client says
//! hello, asks, is granted, is told, gives back, is refused, is revoked,
//! and every mistake draws the right `Error`.

use forge_bfcp::server::reported_status;
use forge_bfcp::{
    Attribute, ErrorCode, Event, FloorServer, Handled, Message, Primitive, Priority, RequestStatus,
    Transport, VERSION_RELIABLE, VERSION_UNRELIABLE,
};

const CONF: u32 = 4321;
const FLOOR: u16 = 1;

fn server(transport: Transport) -> FloorServer {
    let mut s = FloorServer::new(CONF, FLOOR, transport);
    s.add_user(10, Some("Ann"), Some("sip:ann@example.com"));
    s.add_user(20, Some("Bob"), None);
    s
}

fn send(s: &mut FloorServer, m: Message) -> Handled {
    s.handle(&m.to_bytes().unwrap())
}

fn msg(version: u8, primitive: Primitive, txn: u16, user: u16) -> Message {
    Message::new(version, primitive, CONF, txn, user)
}

fn error_of(h: &Handled) -> ErrorCode {
    let reply = h.reply.as_ref().expect("a reply");
    assert_eq!(reply.primitive, Primitive::Error);
    reply.error_code().expect("an error code")
}

#[test]
fn hello_request_grant_release_over_tcp() {
    let mut s = server(Transport::Reliable);
    let v = VERSION_RELIABLE;

    // Hello → HelloAck listing what the server speaks, in Ann's transaction.
    let h = send(&mut s, msg(v, Primitive::Hello, 1, 10));
    let ack = h.reply.unwrap();
    assert_eq!(ack.primitive, Primitive::HelloAck);
    assert_eq!(
        (ack.conference_id, ack.transaction_id, ack.user_id),
        (CONF, 1, 10)
    );
    assert!(!ack.responder, "no R flag over TCP");
    assert!(matches!(
        ack.attributes[0],
        Attribute::SupportedPrimitives(_)
    ));
    assert!(matches!(
        ack.attributes[1],
        Attribute::SupportedAttributes(_)
    ));
    assert_eq!(h.events, vec![Event::Hello { user_id: 10 }]);

    // Bob subscribes to the floor.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorQuery, 1, 20).with(Attribute::FloorId(FLOOR)),
    );
    let status = h.reply.unwrap();
    assert_eq!(status.primitive, Primitive::FloorStatus);
    assert_eq!(status.floor_ids(), vec![FLOOR]);
    assert!(
        status.floor_request_information().is_empty(),
        "nobody has asked yet"
    );

    // Ann asks: pending, and the owner is told.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 2, 10)
            .with(Attribute::FloorId(FLOOR))
            .with(Attribute::Priority(Priority::High))
            .with(Attribute::ParticipantProvidedInfo("the slides".into())),
    );
    let pending = h.reply.unwrap();
    assert_eq!(pending.primitive, Primitive::FloorRequestStatus);
    assert_eq!(pending.transaction_id, 2);
    let (rid, st) = reported_status(&pending).unwrap();
    assert_eq!(st, RequestStatus::Pending);
    assert_eq!(
        h.events,
        vec![Event::Requested {
            user_id: 10,
            request_id: rid,
            priority: Priority::High,
            info: Some("the slides".into()),
        }]
    );
    assert!(
        h.notifications.is_empty(),
        "nothing changed on the floor yet"
    );
    assert!(s.holder().is_none());
    // The information names the beneficiary and carries the priority.
    let (_, info) = pending.floor_request_information()[0];
    assert!(info.iter().any(|a| matches!(a, Attribute::BeneficiaryInformation { beneficiary_id: 10, attributes } if attributes.iter().any(|b| matches!(b, Attribute::UserDisplayName(n) if n == "Ann")))));
    assert!(info.contains(&Attribute::Priority(Priority::High)));

    // The owner grants: Ann hears Granted in a server transaction (id 0
    // over TCP), Bob hears the floor's state.
    let n = s.grant(rid);
    assert_eq!(n.len(), 2);
    assert_eq!(n[0].user_id, 10);
    assert_eq!(n[0].message.primitive, Primitive::FloorRequestStatus);
    assert_eq!(n[0].message.transaction_id, 0);
    assert_eq!(
        reported_status(&n[0].message),
        Some((rid, RequestStatus::Granted))
    );
    assert_eq!(n[1].user_id, 20);
    assert_eq!(n[1].message.primitive, Primitive::FloorStatus);
    assert_eq!(n[1].message.transaction_id, 0);
    let (_, bobs_view) = n[1].message.floor_request_information()[0];
    assert_eq!(
        forge_bfcp::message::overall_status(bobs_view),
        Some(RequestStatus::Granted)
    );
    assert_eq!(s.holder(), Some((10, rid)));

    // Ann asks again while holding: one ongoing request per user.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 3, 10).with(Attribute::FloorId(FLOOR)),
    );
    assert_eq!(error_of(&h), ErrorCode::MaximumFloorRequestsReached);

    // A query about the request says Granted.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequestQuery, 4, 20).with(Attribute::FloorRequestId(rid)),
    );
    assert_eq!(
        reported_status(&h.reply.unwrap()),
        Some((rid, RequestStatus::Granted))
    );

    // Bob tries to release Ann's: not his.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRelease, 2, 20).with(Attribute::FloorRequestId(rid)),
    );
    assert_eq!(error_of(&h), ErrorCode::UnauthorizedOperation);

    // Ann releases: Released to her, the floor's state to Bob, the owner told.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRelease, 5, 10).with(Attribute::FloorRequestId(rid)),
    );
    assert_eq!(
        reported_status(&h.reply.unwrap()),
        Some((rid, RequestStatus::Released))
    );
    assert_eq!(
        h.events,
        vec![Event::Released {
            user_id: 10,
            request_id: rid,
            was_granted: true
        }]
    );
    assert_eq!(h.notifications.len(), 1);
    assert_eq!(h.notifications[0].user_id, 20);
    assert!(h.notifications[0]
        .message
        .floor_request_information()
        .is_empty());
    assert!(s.holder().is_none());

    // Releasing it again: gone.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRelease, 6, 10).with(Attribute::FloorRequestId(rid)),
    );
    assert_eq!(error_of(&h), ErrorCode::FloorRequestIdDoesNotExist);
}

#[test]
fn deny_revoke_cancel_and_goodbye() {
    let mut s = server(Transport::Unreliable);
    let v = VERSION_UNRELIABLE;
    send(
        &mut s,
        msg(v, Primitive::FloorQuery, 1, 20).with(Attribute::FloorId(FLOOR)),
    );

    // Denied with a reason.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 2, 10).with(Attribute::FloorId(FLOOR)),
    );
    let (rid, _) = reported_status(h.reply.as_ref().unwrap()).unwrap();
    let n = s.deny(rid, Some("someone else is sharing"));
    assert_eq!(
        reported_status(&n[0].message),
        Some((rid, RequestStatus::Denied))
    );
    let (_, info) = n[0].message.floor_request_information()[0];
    let overall = info
        .iter()
        .find_map(|a| match a {
            Attribute::OverallRequestStatus { attributes, .. } => Some(attributes),
            _ => None,
        })
        .unwrap();
    assert!(overall.contains(&Attribute::StatusInfo("someone else is sharing".into())));
    // Over UDP a server-initiated message has its own non-zero transaction.
    assert_ne!(n[0].message.transaction_id, 0);
    assert!(!n[0].message.responder);
    assert!(s.request(rid).is_none(), "a denied request is forgotten");

    // Granted, then revoked by the host.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 3, 10).with(Attribute::FloorId(FLOOR)),
    );
    let (rid, _) = reported_status(h.reply.as_ref().unwrap()).unwrap();
    s.grant(rid);
    let n = s.revoke(rid, Some("stopped by the host"));
    assert_eq!(
        reported_status(&n[0].message),
        Some((rid, RequestStatus::Revoked))
    );
    assert_eq!(n[1].user_id, 20);
    assert!(s.holder().is_none());

    // A pending request withdrawn is Cancelled, not Released.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 4, 10).with(Attribute::FloorId(FLOOR)),
    );
    let (rid, _) = reported_status(h.reply.as_ref().unwrap()).unwrap();
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRelease, 5, 10).with(Attribute::FloorRequestId(rid)),
    );
    assert_eq!(
        reported_status(h.reply.as_ref().unwrap()),
        Some((rid, RequestStatus::Cancelled))
    );
    assert!(h.notifications.is_empty(), "the floor did not change hands");

    // Goodbye while holding the floor frees it and tells the subscriber.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 6, 10).with(Attribute::FloorId(FLOOR)),
    );
    let (rid, _) = reported_status(h.reply.as_ref().unwrap()).unwrap();
    s.grant(rid);
    let h = send(&mut s, msg(v, Primitive::Goodbye, 7, 10));
    assert_eq!(h.reply.unwrap().primitive, Primitive::GoodbyeAck);
    assert_eq!(
        h.events,
        vec![Event::Goodbye {
            user_id: 10,
            released: Some(rid)
        }]
    );
    assert_eq!(h.notifications.len(), 1);
    assert!(s.holder().is_none());
    // Gone: the next message from that user is refused.
    let h = send(&mut s, msg(v, Primitive::Hello, 8, 10));
    assert_eq!(error_of(&h), ErrorCode::UserDoesNotExist);

    // An acknowledgement is reported, not answered.
    let mut ack = msg(v, Primitive::FloorStatusAck, 9, 20);
    ack.responder = true;
    let h = send(&mut s, ack);
    assert!(h.reply.is_none());
    assert_eq!(
        h.events,
        vec![Event::Acked {
            user_id: 20,
            transaction_id: 9
        }]
    );
}

#[test]
fn every_mistake_draws_the_error_the_rfc_names() {
    let mut s = server(Transport::Reliable);
    let v = VERSION_RELIABLE;
    // The wrong version for the transport.
    let h = send(&mut s, msg(VERSION_UNRELIABLE, Primitive::Hello, 1, 10));
    assert_eq!(error_of(&h), ErrorCode::UnsupportedVersion);
    // Another conference.
    let h = send(&mut s, Message::new(v, Primitive::Hello, 99, 1, 10));
    assert_eq!(error_of(&h), ErrorCode::ConferenceDoesNotExist);
    // An unknown user.
    let h = send(&mut s, msg(v, Primitive::Hello, 1, 77));
    assert_eq!(error_of(&h), ErrorCode::UserDoesNotExist);
    // Another floor.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 2, 10).with(Attribute::FloorId(9)),
    );
    assert_eq!(error_of(&h), ErrorCode::InvalidFloorId);
    let h = send(
        &mut s,
        msg(v, Primitive::FloorQuery, 2, 10).with(Attribute::FloorId(9)),
    );
    assert_eq!(error_of(&h), ErrorCode::InvalidFloorId);
    // A request on someone else's behalf.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 3, 10)
            .with(Attribute::FloorId(FLOOR))
            .with(Attribute::BeneficiaryId(20)),
    );
    assert_eq!(error_of(&h), ErrorCode::UnauthorizedOperation);
    // A chair's action.
    let h = send(&mut s, msg(v, Primitive::ChairAction, 4, 10));
    assert_eq!(error_of(&h), ErrorCode::UnauthorizedOperation);
    // A request id nobody has.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequestQuery, 5, 10).with(Attribute::FloorRequestId(42)),
    );
    assert_eq!(error_of(&h), ErrorCode::FloorRequestIdDoesNotExist);
    // An unknown primitive, in the sender's transaction.
    let mut bytes = msg(v, Primitive::Hello, 6, 10).to_bytes().unwrap();
    bytes[1] = 250;
    let h = s.handle(&bytes);
    assert_eq!(error_of(&h), ErrorCode::UnknownPrimitive);
    assert_eq!(h.reply.as_ref().unwrap().transaction_id, 6);
    // An unknown mandatory attribute, listed in the details.
    let bytes = msg(v, Primitive::Hello, 7, 10)
        .with(Attribute::Unknown {
            attr_type: 100,
            value: vec![0, 0],
        })
        .to_bytes()
        .unwrap();
    let mut mandatory = bytes.clone();
    mandatory[12] |= 1;
    let h = s.handle(&mandatory);
    assert_eq!(error_of(&h), ErrorCode::UnknownMandatoryAttribute);
    assert!(
        matches!(&h.reply.unwrap().attributes[0], Attribute::ErrorCode { details, .. } if *details == vec![100 << 1])
    );
    // The same attribute without the bit is ignored and the Hello answered.
    let h = s.handle(&bytes);
    assert_eq!(h.reply.unwrap().primitive, Primitive::HelloAck);
    // Truncated: the length is wrong, and the header says whose fault.
    let mut short = msg(v, Primitive::Hello, 8, 10).to_bytes().unwrap();
    short[3] = 2;
    let h = s.handle(&short);
    assert_eq!(error_of(&h), ErrorCode::IncorrectMessageLength);
    // Nothing a header can be read from: nothing to answer.
    assert!(s.handle(&[1, 2, 3]).reply.is_none());

    // A user query says what a user holds.
    let h = send(
        &mut s,
        msg(v, Primitive::FloorRequest, 9, 10).with(Attribute::FloorId(FLOOR)),
    );
    let (rid, _) = reported_status(h.reply.as_ref().unwrap()).unwrap();
    let h = send(
        &mut s,
        msg(v, Primitive::UserQuery, 10, 20).with(Attribute::BeneficiaryId(10)),
    );
    let status = h.reply.unwrap();
    assert_eq!(status.primitive, Primitive::UserStatus);
    assert_eq!(status.floor_request_information()[0].0, rid);
    // A FloorQuery with no floor unsubscribes and answers bare.
    let h = send(&mut s, msg(v, Primitive::FloorQuery, 11, 20));
    assert!(h.reply.unwrap().attributes.is_empty());
    s.grant(rid);
    assert!(
        s.grant(rid).iter().all(|n| n.user_id != 20),
        "Bob unsubscribed"
    );

    // The user leaving frees the floor.
    let (n, released) = s.remove_user(10);
    assert_eq!(released, Some(rid));
    assert!(n.is_empty(), "nobody left to tell");
    assert!(s.holder().is_none());
}
