//! `TransportConfig::local_port` — pinning the ICE socket's port.
//!
//! A server bridging WebRTC to something else usually has a media port
//! range: opened in a firewall, sized as a capacity budget, and drawn
//! from a pool it accounts for. A connection that binds an ephemeral
//! port sits outside all three. These tests cover the contract that
//! lets such a server place the socket deliberately.

use forge_webrtc::{PeerConfig, PeerConnection, TransportConfig};
use tokio::net::UdpSocket;

/// Every host candidate in the SDP, as (ip, port).
fn host_candidates(sdp: &str) -> Vec<(String, u16)> {
    sdp.lines()
        .filter(|l| l.starts_with("a=candidate:") && l.contains("typ host"))
        .filter_map(|l| {
            // a=candidate:<foundation> <component> udp <prio> <ip> <port> typ host
            let f: Vec<&str> = l.split_whitespace().collect();
            Some((f.get(4)?.to_string(), f.get(5)?.parse().ok()?))
        })
        .collect()
}

async fn free_even_port() -> u16 {
    // Bind ephemeral, note the port, drop the socket. A racing bind is
    // possible in principle; the range walk keeps it from mattering.
    for _ in 0..64 {
        let s = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let p = s.local_addr().unwrap().port();
        drop(s);
        if p % 2 == 0 {
            return p;
        }
    }
    panic!("no even ephemeral port found");
}

fn config_on(port: u16) -> PeerConfig {
    PeerConfig {
        transport: TransportConfig {
            local_port: port,
            ..TransportConfig::default()
        },
        ..PeerConfig::default()
    }
}

#[tokio::test]
async fn host_candidates_use_the_requested_port() {
    let port = free_even_port().await;
    let mut pc = PeerConnection::with_config(config_on(port)).await.unwrap();
    let offer = pc.create_offer().await.unwrap();

    let cands = host_candidates(&offer);
    assert!(!cands.is_empty(), "no host candidate in offer:\n{offer}");
    for (ip, got) in &cands {
        assert_eq!(
            *got, port,
            "host candidate {ip}:{got} is not on the requested port {port}"
        );
    }
    pc.close();
}

/// `0` is the documented "let the OS choose" value and stays the
/// default, so existing callers are unaffected.
#[tokio::test]
async fn zero_still_means_ephemeral() {
    assert_eq!(TransportConfig::default().local_port, 0);

    let mut pc = PeerConnection::new(vec![]).await.unwrap();
    let offer = pc.create_offer().await.unwrap();
    let cands = host_candidates(&offer);
    assert!(!cands.is_empty(), "no host candidate in offer:\n{offer}");
    assert!(
        cands.iter().all(|(_, p)| *p != 0),
        "an OS-assigned port must still be a real port"
    );
    pc.close();
}

/// The failure is loud on purpose: falling back to an ephemeral port
/// would put media outside the range the caller asked for, which is
/// the one thing asking was meant to prevent.
#[tokio::test]
async fn an_occupied_port_fails_rather_than_falling_back() {
    let squatter = UdpSocket::bind("0.0.0.0:0").await.unwrap();
    let port = squatter.local_addr().unwrap().port();

    // The socket is bound lazily, so the failure may surface either at
    // construction or at the first offer. Either is fine; silently
    // succeeding on some *other* port is not.
    let outcome = match PeerConnection::with_config(config_on(port)).await {
        Err(e) => Err(e),
        Ok(mut pc) => match pc.create_offer().await {
            Err(e) => Err(e),
            Ok(offer) => Ok((pc, offer)),
        },
    };

    match outcome {
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            assert!(
                msg.contains("bind") || msg.contains("address"),
                "error should name the bind failure, got: {e}"
            );
        }
        Ok((mut pc, offer)) => {
            // Some platforms permit the second bind; if so, the
            // contract that matters is still that the port is the one
            // asked for, never a silent substitute.
            for (_, got) in host_candidates(&offer) {
                assert_eq!(got, port, "silently moved off the requested port");
            }
            pc.close();
        }
    }
    drop(squatter);
}
