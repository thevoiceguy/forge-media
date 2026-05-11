// forge-media
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
//! Unit-level tests for `ForgeHepEmitter`. A capturing sink lets us
//! assert every HEP packet field without binding sockets or
//! reaching for tokio.

use std::sync::{Arc, Mutex};

use forge_hep::{Direction, ForgeHepEmitter, HepProtocol, HepSink, IpProto, RtpQosReport};
use hep_rs::HepPacket;

#[derive(Default)]
struct CapturingSink {
    received: Mutex<Vec<HepPacket>>,
}

impl HepSink for CapturingSink {
    fn send(&self, packet: HepPacket) {
        self.received.lock().unwrap().push(packet);
    }
}

#[test]
fn emit_rtcp_builds_a_well_formed_packet() {
    let sink = Arc::new(CapturingSink::default());
    let emitter =
        ForgeHepEmitter::new(sink.clone() as Arc<dyn HepSink>, 2002).with_password("homer-secret");

    // Minimal valid RTCP Receiver Report (PT=201, length=1, no
    // blocks). Real wire packets are bigger but the codec doesn't
    // care for our purposes — we're testing that the emitter wraps
    // the bytes in a HEP envelope.
    let rtcp = b"\x81\xc9\x00\x07\xde\xad\xbe\xef";
    let src = "10.0.0.1:5001".parse().unwrap();
    let dst = "10.0.0.2:5003".parse().unwrap();

    emitter.emit_rtcp(
        Direction::Inbound,
        IpProto::Udp,
        src,
        dst,
        rtcp,
        Some("call-abc-123"),
    );

    let pkts = sink.received.lock().unwrap();
    assert_eq!(pkts.len(), 1);
    let pkt = &pkts[0];
    assert_eq!(pkt.capture_id, 2002);
    assert_eq!(pkt.capture_password.as_deref(), Some("homer-secret"));
    assert_eq!(pkt.protocol, HepProtocol::Rtcp);
    assert_eq!(pkt.transport, IpProto::Udp);
    assert_eq!(pkt.src, src);
    assert_eq!(pkt.dst, dst);
    assert_eq!(pkt.correlation_id.as_deref(), Some("call-abc-123"));
    assert_eq!(pkt.payload, rtcp);
}

#[test]
fn emit_rtp_qos_serializes_report_as_json_payload() {
    let sink = Arc::new(CapturingSink::default());
    let emitter = ForgeHepEmitter::new(sink.clone() as Arc<dyn HepSink>, 1);

    let report = RtpQosReport {
        ssrc: Some(0x1234_5678),
        fraction_lost: Some(0.039),
        packets_lost: Some(10),
        jitter: Some(85),
        ..Default::default()
    };
    let addr = "127.0.0.1:5000".parse().unwrap();

    emitter.emit_rtp_qos(IpProto::Udp, addr, addr, &report, Some("call-x"));

    let pkts = sink.received.lock().unwrap();
    assert_eq!(pkts.len(), 1);
    assert_eq!(pkts[0].protocol, HepProtocol::RtpQos);

    // Payload round-trips through JSON identically.
    let decoded: RtpQosReport = serde_json::from_slice(&pkts[0].payload).expect("json");
    assert_eq!(decoded.ssrc, Some(0x1234_5678));
    assert_eq!(decoded.packets_lost, Some(10));
    assert_eq!(decoded.jitter, Some(85));
    assert_eq!(decoded.fraction_lost, Some(0.039));
}

#[test]
fn rtcp_payload_oversize_is_truncated_not_dropped() {
    let sink = Arc::new(CapturingSink::default());
    let emitter = ForgeHepEmitter::new(sink.clone() as Arc<dyn HepSink>, 1);

    let huge = vec![b'X'; 128 * 1024];
    let addr = "127.0.0.1:5000".parse().unwrap();
    emitter.emit_rtcp(Direction::Inbound, IpProto::Udp, addr, addr, &huge, None);

    let pkts = sink.received.lock().unwrap();
    assert_eq!(pkts.len(), 1);
    assert!(pkts[0].payload.len() <= 60 * 1024);
}
