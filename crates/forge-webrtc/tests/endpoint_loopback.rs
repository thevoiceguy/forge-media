//! Two peer connections in one process: offer/answer, trickle ICE in both
//! directions, DTLS in both roles, SRTP both ways, then a re-offer on the
//! same transport. This is the contract the DSIP native endpoint relies on.

use std::time::Duration;

use bytes::Bytes;
use forge_webrtc::{
    ConnectionState, Direction, PeerConfig, PeerConnection, PeerEvent, SignalingState, WebRtcError,
};
use tokio::sync::mpsc;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge_webrtc=debug,forge_ice=info".into()),
        )
        .with_test_writer()
        .try_init();
}

/// Forward trickled candidates from `events` into `peer_tx` and report the
/// first `Connected`/`Failed`; keep forwarding RTP afterwards.
fn pump(
    mut events: mpsc::Receiver<PeerEvent>,
    name: &'static str,
) -> (
    mpsc::UnboundedReceiver<forge_webrtc::IceCandidate>,
    mpsc::UnboundedReceiver<PeerEvent>,
) {
    let (cand_tx, cand_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            match ev {
                PeerEvent::LocalCandidate(c) => {
                    let _ = cand_tx.send(c);
                }
                other => {
                    if matches!(other, PeerEvent::Failed(_)) {
                        eprintln!("{name}: {other:?}");
                    }
                    let _ = ev_tx.send(other);
                }
            }
        }
    });
    (cand_rx, ev_rx)
}

async fn connect_pair() -> (
    PeerConnection,
    PeerConnection,
    mpsc::UnboundedReceiver<PeerEvent>,
    mpsc::UnboundedReceiver<PeerEvent>,
) {
    let mut caller = PeerConnection::new(vec![]).await.unwrap();
    let mut callee = PeerConnection::with_config(PeerConfig {
        direction: Direction::SendRecv,
        ..PeerConfig::default()
    })
    .await
    .unwrap();

    let offer = caller.create_offer().await.unwrap();
    assert!(offer.contains("a=setup:actpass"));
    let (mut caller_cands, caller_events) = pump(caller.take_events().unwrap(), "caller");

    callee.set_remote_offer(&offer).await.unwrap();
    assert_eq!(callee.signaling_state(), SignalingState::HaveRemoteOffer);
    let answer = callee.create_answer().await.unwrap();
    assert!(answer.contains("a=setup:active"), "{answer}");
    assert_eq!(callee.signaling_state(), SignalingState::Stable);
    let (mut callee_cands, callee_events) = pump(callee.take_events().unwrap(), "callee");

    caller.set_remote_answer(&answer).await.unwrap();
    assert_eq!(caller.signaling_state(), SignalingState::Stable);

    // Trickle until both are connected.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        tokio::select! {
            Some(c) = caller_cands.recv() => callee.add_ice_candidate(c).await.unwrap(),
            Some(c) = callee_cands.recv() => caller.add_ice_candidate(c).await.unwrap(),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        if caller.get_state() == ConnectionState::Connected
            && callee.get_state() == ConnectionState::Connected
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "not connected: caller={:?} callee={:?}",
            caller.get_state(),
            callee.get_state()
        );
        assert_ne!(caller.get_state(), ConnectionState::Failed);
        assert_ne!(callee.get_state(), ConnectionState::Failed);
    }
    (caller, callee, caller_events, callee_events)
}

async fn expect_rtp(
    events: &mut mpsc::UnboundedReceiver<PeerEvent>,
    payload: &[u8],
) -> forge_rtp::RtpPacket {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for RTP")
            .expect("events closed");
        if let PeerEvent::Rtp(pkt) = ev {
            if pkt.payload.as_ref() == payload {
                return pkt;
            }
        }
    }
}

#[tokio::test]
async fn offer_answer_trickle_dtls_srtp_both_ways() {
    init_tracing();
    let (caller, callee, mut caller_events, mut callee_events) = connect_pair().await;

    // Both sides negotiated Opus on PT 111 (answer mirrors the offer).
    assert_eq!(caller.negotiated_opus_pt(), 111);
    assert_eq!(callee.negotiated_opus_pt(), 111);

    let a = caller.sender().unwrap();
    let b = callee.sender().unwrap();
    // Pretend Opus frames (the SRTP path does not care about the codec).
    for i in 0..5u8 {
        a.send_audio(Bytes::from(vec![0xf8, i, 1, 2, 3]), 960)
            .await
            .unwrap();
        b.send_audio(Bytes::from(vec![0xf9, i, 4, 5, 6]), 960)
            .await
            .unwrap();
    }
    let at_callee = expect_rtp(&mut callee_events, &[0xf8, 4, 1, 2, 3]).await;
    let at_caller = expect_rtp(&mut caller_events, &[0xf9, 4, 4, 5, 6]).await;
    assert_eq!(at_callee.header.payload_type(), 111);
    let (ssrc_at_callee, ssrc_at_caller) = (at_callee.header.ssrc, at_caller.header.ssrc);
    assert_eq!(ssrc_at_callee, caller.ssrc());
    assert_eq!(ssrc_at_caller, callee.ssrc());
    // The 5th frame carries base+4*960; after five sends the next timestamp is base+5*960.
    let ts = at_callee.header.timestamp;
    assert_eq!(a.timestamp().wrapping_sub(ts), 960);
}

#[tokio::test]
async fn screening_answer_is_recvonly_then_reoffer_escalates() {
    init_tracing();
    let mut caller = PeerConnection::new(vec![]).await.unwrap();
    let mut callee = PeerConnection::with_config(PeerConfig {
        direction: Direction::RecvOnly,
        ..PeerConfig::default()
    })
    .await
    .unwrap();
    let offer = caller.create_offer().await.unwrap();
    let (mut caller_cands, _caller_events) = pump(caller.take_events().unwrap(), "caller");
    callee.set_remote_offer(&offer).await.unwrap();
    let answer = callee.create_answer().await.unwrap();
    assert!(answer.contains("a=recvonly\r\n"), "{answer}");
    let (mut callee_cands, mut callee_events) = pump(callee.take_events().unwrap(), "callee");
    caller.set_remote_answer(&answer).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !(caller.get_state() == ConnectionState::Connected
        && callee.get_state() == ConnectionState::Connected)
    {
        tokio::select! {
            Some(c) = caller_cands.recv() => callee.add_ice_candidate(c).await.unwrap(),
            Some(c) = callee_cands.recv() => caller.add_ice_candidate(c).await.unwrap(),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        assert!(tokio::time::Instant::now() < deadline);
    }

    // Escalation: the screener re-offers sendrecv on the same transport.
    let creds_before = {
        let r = forge_webrtc::sdp::parse_remote(caller.remote_sdp().unwrap()).unwrap();
        (r.ufrag, r.pwd)
    };
    callee.set_direction(Direction::SendRecv);
    let reoffer = callee.create_offer().await.unwrap();
    assert!(reoffer.contains("a=sendrecv\r\n"));
    assert!(
        reoffer.contains(&format!("a=ice-ufrag:{}\r\n", creds_before.0)),
        "re-offer must keep ICE credentials"
    );
    caller.set_remote_offer(&reoffer).await.unwrap();
    let reanswer = caller.create_answer().await.unwrap();
    assert!(reanswer.contains("a=sendrecv\r\n"), "{reanswer}");
    callee.set_remote_answer(&reanswer).await.unwrap();
    assert_eq!(caller.get_state(), ConnectionState::Connected);
    assert_eq!(callee.get_state(), ConnectionState::Connected);

    // The existing SRTP association carries media immediately after.
    let s = caller.sender().unwrap();
    s.send_audio(Bytes::from_static(&[9, 9, 9]), 960)
        .await
        .unwrap();
    expect_rtp(&mut callee_events, &[9, 9, 9]).await;
}

#[tokio::test]
async fn ice_restart_is_refused() {
    init_tracing();
    let (mut caller, mut callee, _a, _b) = connect_pair().await;
    let reoffer = callee.create_offer().await.unwrap();
    let restarted = reoffer
        .replace("a=ice-ufrag:", "a=ice-ufrag:x")
        .replace("a=ice-pwd:", "a=ice-pwd:x");
    match caller.set_remote_offer(&restarted).await {
        Err(WebRtcError::IceRestartUnsupported) => {}
        other => panic!("expected IceRestartUnsupported, got {other:?}"),
    }
    // The connection is untouched.
    assert_eq!(caller.get_state(), ConnectionState::Connected);
    callee.rollback_local_offer().unwrap();
}

#[tokio::test]
async fn rejected_reoffer_rolls_back_cleanly() {
    init_tracing();
    let (mut caller, mut callee, _a, mut callee_events) = connect_pair().await;
    callee.set_direction(Direction::SendRecv);
    let _reoffer = callee.create_offer().await.unwrap();
    // Peer rejects the update (DSIP `reject media.unsupported`): roll back.
    callee.rollback_local_offer().unwrap();
    assert_eq!(callee.signaling_state(), SignalingState::Stable);
    let s = caller.sender().unwrap();
    s.send_audio(Bytes::from_static(&[7, 7]), 960)
        .await
        .unwrap();
    expect_rtp(&mut callee_events, &[7, 7]).await;
    caller.close();
    assert_eq!(caller.get_state(), ConnectionState::Closed);
}
