//! Media forced through a live TURN relay (coturn), relay↔relay — the path that
//! survives symmetric NAT on both ends. Ignored by default; needs a TURN server
//! that permits loopback peers:
//!
//! ```text
//! docker run -d --network host coturn/coturn -n --lt-cred-mech --realm=forge.test \
//!   --user=test:test --listening-ip=127.0.0.1 --relay-ip=127.0.0.1 \
//!   --min-port=49160 --max-port=49250 --no-tls --allow-loopback-peers
//! FORGE_TURN_URI=turn:127.0.0.1:3478 FORGE_TURN_USER=test FORGE_TURN_PASS=test \
//!   cargo test -p forge-webrtc --test turn_relay -- --ignored --nocapture
//! ```

use std::time::Duration;

use bytes::Bytes;
use forge_webrtc::{
    ConnectionState, IceCandidate, PeerConfig, PeerConnection, PeerEvent, TransportConfig,
    TurnServer,
};
use tokio::sync::mpsc;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge_webrtc=debug,forge_ice=debug".into()),
        )
        .with_test_writer()
        .try_init();
}

/// Drop inlined `a=candidate` lines so every candidate is trickled — letting the
/// test choose which ones the peer ever sees.
fn strip_candidates(sdp: &str) -> String {
    let mut out: String = sdp
        .lines()
        .filter(|l| !l.starts_with("a=candidate"))
        .collect::<Vec<_>>()
        .join("\r\n");
    out.push_str("\r\n");
    out
}

fn is_relay(c: &IceCandidate) -> bool {
    c.to_sdp_attribute().contains(" typ relay")
}

fn pump(
    mut events: mpsc::Receiver<PeerEvent>,
) -> (
    mpsc::UnboundedReceiver<IceCandidate>,
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
                    let _ = ev_tx.send(other);
                }
            }
        }
    });
    (cand_rx, ev_rx)
}

fn turn_cfg(turn: &TurnServer) -> PeerConfig {
    PeerConfig {
        transport: TransportConfig {
            turn_servers: vec![turn.clone()],
            ..TransportConfig::default()
        },
        ..PeerConfig::default()
    }
}

#[tokio::test]
#[ignore = "requires a live TURN server; set FORGE_TURN_URI/USER/PASS"]
async fn media_flows_relay_to_relay() {
    init_tracing();
    let uri = std::env::var("FORGE_TURN_URI").expect("FORGE_TURN_URI");
    let user = std::env::var("FORGE_TURN_USER").unwrap_or_default();
    let pass = std::env::var("FORGE_TURN_PASS").unwrap_or_default();
    let turn = TurnServer::new(uri, user, pass);

    let mut caller = PeerConnection::with_config(turn_cfg(&turn)).await.unwrap();
    let mut callee = PeerConnection::with_config(turn_cfg(&turn)).await.unwrap();

    let offer = strip_candidates(&caller.create_offer().await.unwrap());
    let (mut caller_cands, mut caller_events) = pump(caller.take_events().unwrap());
    callee.set_remote_offer(&offer).await.unwrap();
    let answer = strip_candidates(&callee.create_answer().await.unwrap());
    let (mut callee_cands, mut callee_events) = pump(callee.take_events().unwrap());
    caller.set_remote_answer(&answer).await.unwrap();

    // Trickle ONLY relay candidates → the sole pairable path is relay↔relay.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let (mut c_relay, mut e_relay) = (0u32, 0u32);
    loop {
        tokio::select! {
            Some(c) = caller_cands.recv() => if is_relay(&c) { c_relay += 1; callee.add_ice_candidate(c).await.unwrap(); },
            Some(c) = callee_cands.recv() => if is_relay(&c) { e_relay += 1; caller.add_ice_candidate(c).await.unwrap(); },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        if caller.get_state() == ConnectionState::Connected
            && callee.get_state() == ConnectionState::Connected
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "not connected over relay: caller={:?} callee={:?} (relay cands fwd: caller={c_relay} callee={e_relay})",
            caller.get_state(),
            callee.get_state()
        );
        assert_ne!(caller.get_state(), ConnectionState::Failed, "caller failed");
        assert_ne!(callee.get_state(), ConnectionState::Failed, "callee failed");
    }
    assert!(
        c_relay > 0 && e_relay > 0,
        "both sides must have offered a relay candidate"
    );
    eprintln!("connected relay↔relay (caller fwd {c_relay}, callee fwd {e_relay})");

    // Media both ways over the relay.
    let a = caller.sender().unwrap();
    let b = callee.sender().unwrap();
    for i in 0..10u8 {
        a.send_audio(Bytes::from(vec![0xf8, i, 1, 2, 3]), 960)
            .await
            .unwrap();
        b.send_audio(Bytes::from(vec![0xf9, i, 4, 5, 6]), 960)
            .await
            .unwrap();
    }
    expect_rtp(&mut callee_events, &[0xf8, 4, 1, 2, 3]).await;
    expect_rtp(&mut caller_events, &[0xf9, 4, 4, 5, 6]).await;
    let _ = (&mut e_relay, &mut c_relay);
    eprintln!("media crossed the relay both ways");
}

async fn expect_rtp(events: &mut mpsc::UnboundedReceiver<PeerEvent>, payload: &[u8]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for relayed RTP")
            .expect("events closed");
        if let PeerEvent::Rtp(pkt) = ev {
            if pkt.payload.as_ref() == payload {
                return;
            }
        }
    }
}

/// Renomination: a session that connected relay↔relay moves onto a direct pair once
/// one succeeds, on both sides, and media keeps flowing. The direct candidates are
/// held back until the relayed session is up — the order a slow trickle or a late
/// hole punch produces on a real network (DSIP WAN Run 3b).
#[tokio::test]
#[ignore = "requires a live TURN server; set FORGE_TURN_URI/USER/PASS"]
async fn relayed_session_moves_to_a_direct_pair() {
    init_tracing();
    let uri = std::env::var("FORGE_TURN_URI").expect("FORGE_TURN_URI");
    let user = std::env::var("FORGE_TURN_USER").unwrap_or_default();
    let pass = std::env::var("FORGE_TURN_PASS").unwrap_or_default();
    let turn = TurnServer::new(uri, user, pass);

    let mut caller = PeerConnection::with_config(turn_cfg(&turn)).await.unwrap();
    let mut callee = PeerConnection::with_config(turn_cfg(&turn)).await.unwrap();
    let offer = strip_candidates(&caller.create_offer().await.unwrap());
    let (mut caller_cands, mut caller_events) = pump(caller.take_events().unwrap());
    callee.set_remote_offer(&offer).await.unwrap();
    let answer = strip_candidates(&callee.create_answer().await.unwrap());
    let (mut callee_cands, mut callee_events) = pump(callee.take_events().unwrap());
    caller.set_remote_answer(&answer).await.unwrap();

    // Phase 1: relay candidates only, direct ones held back.
    let (mut caller_direct, mut callee_direct) = (Vec::new(), Vec::new());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while !(caller.get_state() == ConnectionState::Connected
        && callee.get_state() == ConnectionState::Connected)
    {
        tokio::select! {
            Some(c) = caller_cands.recv() => if is_relay(&c) { callee.add_ice_candidate(c).await.unwrap() } else { caller_direct.push(c) },
            Some(c) = callee_cands.recv() => if is_relay(&c) { caller.add_ice_candidate(c).await.unwrap() } else { callee_direct.push(c) },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "not connected over the relay"
        );
    }
    let relay_caller = ice_remote(&mut caller_events).await;
    let relay_callee = ice_remote(&mut callee_events).await;
    eprintln!("connected relay↔relay: caller → {relay_caller}, callee → {relay_callee}");

    // Phase 2: the direct candidates arrive; both sides must move off the relay.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(c) = caller_cands.try_recv() {
        if !is_relay(&c) {
            caller_direct.push(c)
        }
    }
    while let Ok(c) = callee_cands.try_recv() {
        if !is_relay(&c) {
            callee_direct.push(c)
        }
    }
    assert!(
        !caller_direct.is_empty() && !callee_direct.is_empty(),
        "no direct candidates gathered"
    );
    for c in caller_direct {
        callee.add_ice_candidate(c).await.unwrap();
    }
    for c in callee_direct {
        caller.add_ice_candidate(c).await.unwrap();
    }
    let direct_caller = ice_remote(&mut caller_events).await;
    let direct_callee = ice_remote(&mut callee_events).await;
    assert_ne!(direct_caller, relay_caller, "caller stayed on the relay");
    assert_ne!(direct_callee, relay_callee, "callee stayed on the relay");
    eprintln!("renominated: caller → {direct_caller}, callee → {direct_callee}");

    // Media both ways on the new pair.
    let a = caller.sender().unwrap();
    let b = callee.sender().unwrap();
    for i in 0..10u8 {
        a.send_audio(Bytes::from(vec![0xe8, i, 1, 2, 3]), 960)
            .await
            .unwrap();
        b.send_audio(Bytes::from(vec![0xe9, i, 4, 5, 6]), 960)
            .await
            .unwrap();
    }
    expect_rtp(&mut callee_events, &[0xe8, 4, 1, 2, 3]).await;
    expect_rtp(&mut caller_events, &[0xe9, 4, 4, 5, 6]).await;
    eprintln!("media crossed the direct pair both ways");
}

/// The remote address of the next `IceConnected` (a first nomination or a renomination).
async fn ice_remote(events: &mut mpsc::UnboundedReceiver<PeerEvent>) -> std::net::SocketAddr {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for IceConnected")
            .expect("events closed");
        if let PeerEvent::IceConnected { remote, .. } = ev {
            return remote;
        }
    }
}
