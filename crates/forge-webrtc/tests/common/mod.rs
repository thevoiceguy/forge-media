//! Shared harness for the in-process two-peer tests.
#![allow(dead_code)]

use std::time::Duration;

use forge_webrtc::{ConnectionState, PeerConfig, PeerConnection, PeerEvent, SignalingState};
use tokio::sync::mpsc;

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge_webrtc=debug,forge_ice=info".into()),
        )
        .with_test_writer()
        .try_init();
}

/// Forward trickled candidates from `events` into `peer_tx` and report the
/// first `Connected`/`Failed`; keep forwarding media afterwards.
pub fn pump(
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

/// Two connected peers: `caller` offered with `caller_cfg`, `callee`
/// answered with `callee_cfg`; candidates trickled both ways until both
/// report `Connected`.
pub async fn connect_pair_with(
    caller_cfg: PeerConfig,
    callee_cfg: PeerConfig,
) -> (
    PeerConnection,
    PeerConnection,
    mpsc::UnboundedReceiver<PeerEvent>,
    mpsc::UnboundedReceiver<PeerEvent>,
) {
    let mut caller = PeerConnection::with_config(caller_cfg).await.unwrap();
    let mut callee = PeerConnection::with_config(callee_cfg).await.unwrap();

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

    trickle_until_connected(
        &mut caller,
        &mut callee,
        &mut caller_cands,
        &mut callee_cands,
    )
    .await;
    (caller, callee, caller_events, callee_events)
}

/// Trickle candidates between two peers until both are connected.
pub async fn trickle_until_connected(
    caller: &mut PeerConnection,
    callee: &mut PeerConnection,
    caller_cands: &mut mpsc::UnboundedReceiver<forge_webrtc::IceCandidate>,
    callee_cands: &mut mpsc::UnboundedReceiver<forge_webrtc::IceCandidate>,
) {
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
}

/// The first audio packet carrying `payload`.
pub async fn expect_rtp(
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

/// The next `n` video packets.
pub async fn expect_video(
    events: &mut mpsc::UnboundedReceiver<PeerEvent>,
    n: usize,
) -> Vec<forge_rtp::RtpPacket> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for video RTP")
            .expect("events closed");
        if let PeerEvent::VideoRtp(pkt) = ev {
            out.push(pkt);
        }
    }
    out
}

/// The first RTCP compound in which `pred` matches a sub-packet, within
/// `timeout`.
pub async fn expect_rtcp(
    events: &mut mpsc::UnboundedReceiver<PeerEvent>,
    timeout: Duration,
    mut pred: impl FnMut(&forge_rtp::RtcpPacket) -> bool,
) -> Vec<forge_rtp::RtcpPacket> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for RTCP")
            .expect("events closed");
        if let PeerEvent::Rtcp(packets) = ev {
            if packets.iter().any(&mut pred) {
                return packets;
            }
        }
    }
}
