//! WebRTC Integration Tests
//!
//! These tests validate the complete WebRTC connection flow including:
//! - Peer connection creation
//! - SDP offer generation
//! - ICE candidate gathering
//! - Connection state management

use forge_webrtc::{ConnectionState, PeerConnection};

/// Test basic peer connection creation
#[tokio::test]
async fn test_peer_connection_creation() {
    let stun_servers = vec!["stun:stun.l.google.com:19302".to_string()];
    let peer = PeerConnection::new(stun_servers).await.unwrap();

    assert_eq!(peer.get_state(), ConnectionState::New);
    assert!(!peer.connection_id().is_empty());
    assert!(!peer.dtls_fingerprint().is_empty());
}

/// Test SDP offer generation
#[tokio::test]
async fn test_sdp_offer_generation() {
    let stun_servers = vec![]; // No STUN for this test
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    let offer = peer.create_offer().await.unwrap();

    // Verify SDP offer structure
    assert!(offer.contains("v=0")); // Version
    assert!(offer.contains("m=audio")); // Media type
    assert!(offer.contains("a=ice-ufrag:")); // ICE credentials
    assert!(offer.contains("a=ice-pwd:")); // ICE password
    assert!(offer.contains("a=fingerprint:")); // DTLS fingerprint
    assert!(offer.contains("a=setup:")); // DTLS setup

    // State should transition to Gathering
    assert_eq!(peer.get_state(), ConnectionState::Gathering);

    // Local SDP should be set
    assert!(peer.local_sdp().is_some());
}

/// Test ICE candidate gathering
#[tokio::test]
async fn test_ice_candidate_gathering() {
    let stun_servers = vec![]; // No STUN - just host candidates
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    peer.create_offer().await.unwrap();

    // Get candidate count
    let candidate_count = peer.local_candidate_count().await;

    // Should have at least one host candidate (loopback)
    assert!(
        candidate_count > 0,
        "Expected at least one ICE candidate, got {}",
        candidate_count
    );
}

/// Test connection ID uniqueness
#[tokio::test]
async fn test_connection_id_uniqueness() {
    let stun_servers = vec![];

    let peer1 = PeerConnection::new(stun_servers.clone()).await.unwrap();

    // Small delay to ensure different timestamp
    tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;

    let peer2 = PeerConnection::new(stun_servers).await.unwrap();

    assert_ne!(
        peer1.connection_id(),
        peer2.connection_id(),
        "Connection IDs should be unique"
    );
}

/// Test DTLS fingerprint format
#[tokio::test]
async fn test_dtls_fingerprint_format() {
    let stun_servers = vec![];
    let peer = PeerConnection::new(stun_servers).await.unwrap();

    let fingerprint = peer.dtls_fingerprint();

    // SHA-256 fingerprint format: XX:XX:XX:... (95 characters total)
    assert_eq!(
        fingerprint.len(),
        95,
        "Expected 95 characters for SHA-256 fingerprint, got {}",
        fingerprint.len()
    );

    // Should contain colons
    assert!(
        fingerprint.contains(':'),
        "Fingerprint should contain colon separators"
    );

    // Should be uppercase hex
    assert!(
        fingerprint
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':'),
        "Fingerprint should only contain hex digits and colons"
    );
}

/// Test SDP offer contains valid ICE credentials
#[tokio::test]
async fn test_sdp_ice_credentials() {
    let stun_servers = vec![];
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    let offer = peer.create_offer().await.unwrap();

    // Extract ICE ufrag (RFC 8445 requires at least 4 characters, 24 recommended)
    let ufrag_line = offer
        .lines()
        .find(|line| line.starts_with("a=ice-ufrag:"))
        .expect("SDP should contain ice-ufrag");
    let ufrag = ufrag_line.strip_prefix("a=ice-ufrag:").unwrap();
    assert!(
        ufrag.len() >= 4,
        "ICE ufrag must be at least 4 characters (RFC 8445), got {}",
        ufrag.len()
    );

    // Extract ICE password (RFC 8445 requires at least 22 characters, 24 recommended)
    let pwd_line = offer
        .lines()
        .find(|line| line.starts_with("a=ice-pwd:"))
        .expect("SDP should contain ice-pwd");
    let pwd = pwd_line.strip_prefix("a=ice-pwd:").unwrap();
    assert!(
        pwd.len() >= 22,
        "ICE password must be at least 22 characters (RFC 8445), got {}",
        pwd.len()
    );
}

/// Test SDP offer contains DTLS setup attribute
#[tokio::test]
async fn test_sdp_dtls_setup() {
    let stun_servers = vec![];
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    let offer = peer.create_offer().await.unwrap();

    // Should contain DTLS setup
    assert!(
        offer.contains("a=setup:actpass"),
        "SDP should contain DTLS setup:actpass"
    );
}

/// Test multiple offer generations
#[tokio::test]
async fn test_multiple_offers_forbidden() {
    let stun_servers = vec![];
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    // First offer should succeed
    peer.create_offer().await.unwrap();

    // Second offer should fail (already in Gathering state)
    let result = peer.create_offer().await;
    assert!(
        result.is_err(),
        "Creating second offer should fail when not in New state"
    );
}

/// Test ICE candidate addition
#[tokio::test]
async fn test_add_ice_candidate() {
    let stun_servers = vec![];
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    peer.create_offer().await.unwrap();

    // Create a mock ICE candidate
    use forge_ice::candidate::{CandidateType, IceCandidate, Protocol};
    use std::net::IpAddr;

    let candidate = IceCandidate {
        foundation: "1".to_string(),
        component: 1,
        protocol: Protocol::Udp,
        priority: 2130706431,
        ip: IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 100)),
        port: 54321,
        typ: CandidateType::Host,
        rel_addr: None,
        rel_port: None,
    };

    // Adding candidate should succeed
    let result = peer.add_ice_candidate(candidate).await;
    assert!(result.is_ok(), "Adding ICE candidate should succeed");
}

/// Test connection state getters
#[tokio::test]
async fn test_connection_state_getters() {
    let stun_servers = vec![];
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    // Initially no SDPs
    assert!(peer.local_sdp().is_none());
    assert!(peer.remote_sdp().is_none());

    // After creating offer, local SDP should be set
    peer.create_offer().await.unwrap();
    assert!(peer.local_sdp().is_some());
    assert!(peer.remote_sdp().is_none());
}

/// Benchmark ICE candidate gathering performance
#[tokio::test]
async fn test_ice_gathering_performance() {
    let stun_servers = vec![];
    let mut peer = PeerConnection::new(stun_servers).await.unwrap();

    let start = std::time::Instant::now();
    peer.create_offer().await.unwrap();
    let duration = start.elapsed();

    // ICE gathering should complete quickly for host candidates
    assert!(
        duration.as_millis() < 1000,
        "ICE gathering took too long: {}ms",
        duration.as_millis()
    );

    println!("ICE gathering completed in {}ms", duration.as_millis());
}
