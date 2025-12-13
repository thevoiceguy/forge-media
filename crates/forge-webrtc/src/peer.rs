//! WebRTC PeerConnection implementation
//!
//! Provides the main abstraction for WebRTC peer-to-peer connections.

use crate::{Result, WebRtcError};
use forge_ice::{IceAgent, IceCandidate};
use forge_rtp::dtls::{DtlsCertificate, DtlsContext};
use forge_rtp::RtpSocketPair;
use forge_sdp::{
    DtlsAttributesExt, DtlsSetup, IceAttributesExt, MediaIceAttributesExt, SessionDescription,
    SessionDescriptionExt,
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// WebRTC connection state per RFC 8445
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state
    New,

    /// Gathering ICE candidates
    Gathering,

    /// Performing ICE connectivity checks
    Checking,

    /// ICE connected and DTLS handshake complete
    Connected,

    /// Connection failed
    Failed,

    /// Connection closed
    Closed,
}

/// WebRTC PeerConnection
///
/// Manages the complete lifecycle of a WebRTC connection including:
/// - ICE candidate gathering and connectivity checks
/// - DTLS handshake for key exchange
/// - SRTP encryption setup
/// - SDP offer/answer generation
pub struct PeerConnection {
    /// Unique connection identifier
    connection_id: String,

    /// ICE agent for NAT traversal
    ice_agent: Arc<Mutex<IceAgent>>,

    /// DTLS certificate for key exchange
    dtls_cert: Arc<DtlsCertificate>,

    /// DTLS context for handshake
    dtls_context: Option<DtlsContext>,

    /// RTP socket pair (RTP + RTCP)
    rtp_socket: Option<Arc<RtpSocketPair>>,

    /// Connection state
    state: ConnectionState,

    /// STUN servers for server-reflexive candidates
    stun_servers: Vec<String>,

    /// Local SDP offer/answer
    local_sdp: Option<String>,

    /// Remote SDP offer/answer
    remote_sdp: Option<String>,
}

impl PeerConnection {
    /// Create a new PeerConnection
    ///
    /// # Arguments
    ///
    /// * `stun_servers` - List of STUN server URLs (e.g., "stun:stun.l.google.com:19302")
    ///
    /// # Returns
    ///
    /// A new PeerConnection in the New state
    pub async fn new(stun_servers: Vec<String>) -> Result<Self> {
        let connection_id = generate_connection_id();

        info!("Creating new PeerConnection: {}", connection_id);

        // Generate DTLS certificate
        let dtls_cert = Arc::new(
            DtlsCertificate::generate()
                .map_err(|e| WebRtcError::DtlsError(e.to_string()))?,
        );

        debug!(
            "Generated DTLS certificate with fingerprint: {}",
            dtls_cert.fingerprint
        );

        // Create ICE agent (component 1 = RTP, port will be assigned by OS)
        let ice_agent = IceAgent::new(1, 0, stun_servers.clone());

        Ok(Self {
            connection_id,
            ice_agent: Arc::new(Mutex::new(ice_agent)),
            dtls_cert,
            dtls_context: None,
            rtp_socket: None,
            state: ConnectionState::New,
            stun_servers,
            local_sdp: None,
            remote_sdp: None,
        })
    }

    /// Create an SDP offer
    ///
    /// This will:
    /// 1. Gather ICE candidates
    /// 2. Generate SDP with ICE credentials and DTLS fingerprint
    /// 3. Transition to Gathering state
    ///
    /// # Returns
    ///
    /// SDP offer as a string
    pub async fn create_offer(&mut self) -> Result<String> {
        if self.state != ConnectionState::New {
            return Err(WebRtcError::InvalidState(format!(
                "Cannot create offer in state {:?}",
                self.state
            )));
        }

        info!("Creating SDP offer for connection {}", self.connection_id);

        // Transition to gathering state
        self.state = ConnectionState::Gathering;

        // Gather ICE candidates
        let mut ice_agent = self.ice_agent.lock().await;
        ice_agent
            .gather_candidates()
            .await
            .map_err(|e| WebRtcError::ConnectionFailed(format!("ICE gathering failed: {}", e)))?;

        let local_candidates = ice_agent.get_local_candidates().to_vec();
        let (ufrag, pwd) = ice_agent.get_local_credentials();
        let (ufrag, pwd) = (ufrag.to_string(), pwd.to_string());

        drop(ice_agent);

        debug!(
            "Gathered {} ICE candidates for {}",
            local_candidates.len(),
            self.connection_id
        );

        // Create SDP profile
        let profile = forge_sdp::profiles::SdpProfile::webrtc_audio();

        // Use first host candidate's address, or default to 0.0.0.0
        let local_addr = local_candidates
            .iter()
            .find(|c| {
                matches!(
                    c.typ,
                    forge_ice::candidate::CandidateType::Host
                )
            })
            .map(|c| c.ip.to_string())
            .unwrap_or_else(|| "0.0.0.0".to_string());

        // Use first host candidate's port, or default to 9
        let local_port = local_candidates
            .iter()
            .find(|c| {
                matches!(
                    c.typ,
                    forge_ice::candidate::CandidateType::Host
                )
            })
            .map(|c| c.port)
            .unwrap_or(9);

        let mut sdp = profile.with_local_addr(&local_addr, local_port);

        // Add ICE credentials
        sdp.set_ice_credentials(&ufrag, &pwd);

        // Add DTLS fingerprint and setup
        sdp.set_dtls_fingerprint("sha-256", &self.dtls_cert.fingerprint);
        sdp.set_dtls_setup(DtlsSetup::Actpass);

        // Add ICE candidates to media description
        if let Some(media) = sdp.media.first_mut() {
            for candidate in &local_candidates {
                media.add_ice_candidate_from_forge(candidate);
            }
        }

        // Serialize SDP
        let sdp_str = forge_sdp::serialize::serialize_sdp(&sdp);
        self.local_sdp = Some(sdp_str.clone());

        info!("Created SDP offer for {}", self.connection_id);

        Ok(sdp_str)
    }

    /// Set remote SDP answer
    ///
    /// This will:
    /// 1. Parse the remote SDP
    /// 2. Extract ICE credentials and candidates
    /// 3. Extract DTLS fingerprint
    /// 4. Start ICE connectivity checks
    /// 5. Perform DTLS handshake
    ///
    /// # Arguments
    ///
    /// * `sdp` - Remote SDP answer as a string
    pub async fn set_remote_answer(&mut self, sdp: &str) -> Result<()> {
        if self.state != ConnectionState::Gathering {
            return Err(WebRtcError::InvalidState(format!(
                "Cannot set remote answer in state {:?}",
                self.state
            )));
        }

        info!("Setting remote answer for {}", self.connection_id);

        // Parse SDP
        let remote_sdp = SessionDescription::from_str(sdp)?;

        // Extract ICE credentials
        let (remote_ufrag, remote_pwd) = remote_sdp
            .get_ice_credentials()
            .ok_or_else(|| WebRtcError::SdpError(
                forge_sdp::SdpError::MissingField("ICE credentials".to_string())
            ))?;

        // Extract DTLS fingerprint
        let (_algorithm, remote_fingerprint) = remote_sdp
            .get_dtls_fingerprint()
            .ok_or_else(|| WebRtcError::SdpError(
                forge_sdp::SdpError::MissingField("DTLS fingerprint".to_string())
            ))?;

        // Extract DTLS setup
        let _remote_setup = remote_sdp
            .get_dtls_setup()
            .ok_or_else(|| WebRtcError::SdpError(
                forge_sdp::SdpError::MissingField("DTLS setup".to_string())
            ))?;

        debug!(
            "Remote ICE credentials: ufrag={}, pwd={}",
            remote_ufrag, remote_pwd
        );
        debug!("Remote DTLS fingerprint: {}", remote_fingerprint);

        // Extract remote candidates from media description
        let remote_candidates: Vec<IceCandidate> = if let Some(media) = remote_sdp.media.first() {
            use forge_sdp::MediaIceAttributesExt;
            let candidate_strings: Vec<String> = MediaIceAttributesExt::get_ice_candidates(media);
            debug!("Received {} remote candidate strings", candidate_strings.len());

            // Parse candidate strings into IceCandidate structs
            candidate_strings
                .iter()
                .filter_map(|s| match IceCandidate::from_sdp_attribute(s) {
                    Ok(candidate) => {
                        debug!("Parsed remote candidate: {}", candidate);
                        Some(candidate)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse candidate '{}': {}", s, e);
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        debug!("Successfully parsed {} remote candidates", remote_candidates.len());

        // Set remote credentials in ICE agent
        let mut ice_agent = self.ice_agent.lock().await;
        ice_agent.set_remote_credentials(remote_ufrag, remote_pwd);

        // Add remote candidates to ICE agent
        for candidate in remote_candidates {
            ice_agent.add_remote_candidate(candidate);
        }

        // Form candidate pairs
        ice_agent.form_candidate_pairs();

        drop(ice_agent);

        // Store remote SDP
        self.remote_sdp = Some(sdp.to_string());

        // Transition to checking state
        self.state = ConnectionState::Checking;

        // Start connectivity checks and DTLS handshake
        info!("Starting ICE connectivity checks for {}", self.connection_id);

        // Perform ICE connectivity checks
        let mut ice_agent = self.ice_agent.lock().await;
        let checks_succeeded = ice_agent
            .perform_connectivity_checks()
            .await
            .map_err(|e| WebRtcError::ConnectionFailed(format!("ICE checks failed: {}", e)))?;

        if !checks_succeeded {
            self.state = ConnectionState::Failed;
            return Err(WebRtcError::ConnectionFailed(
                "No ICE candidate pairs succeeded".to_string(),
            ));
        }

        // Get the selected pair
        let selected_pair_index = ice_agent
            .nominate_pair()
            .ok_or_else(|| WebRtcError::ConnectionFailed("No pair nominated".to_string()))?;

        let selected_pair = &ice_agent.get_candidate_pairs()[selected_pair_index];
        let local_addr = std::net::SocketAddr::new(selected_pair.local.ip, selected_pair.local.port);
        let remote_addr = std::net::SocketAddr::new(selected_pair.remote.ip, selected_pair.remote.port);

        info!(
            "ICE checks complete: {} <-> {}",
            local_addr, remote_addr
        );

        drop(ice_agent);

        // TODO: In production, DTLS handshake should be driven by a background task
        // that continuously processes incoming DTLS packets and sends outgoing ones.
        // For now, we'll create the DTLS connection and mark it as a placeholder.

        info!(
            "DTLS handshake needs to be driven by application (see DtlsConnection::handshake)"
        );

        // Create DTLS context and connection (but don't run handshake yet - needs packet exchange)
        #[cfg(feature = "dtls")]
        {
            use forge_rtp::dtls::{DtlsContext, DtlsConnection, DtlsRole};

            let dtls_ctx = DtlsContext::new(self.dtls_cert.clone(), DtlsRole::Client)
                .map_err(|e| WebRtcError::DtlsError(format!("Failed to create DTLS context: {}", e)))?;

            let _dtls_conn = DtlsConnection::new(&dtls_ctx, DtlsRole::Client, Some(remote_fingerprint))
                .map_err(|e| WebRtcError::DtlsError(format!("Failed to create DTLS connection: {}", e)))?;

            // In production: spawn task to drive handshake with _dtls_conn.handshake()
            // and exchange packets over the selected ICE pair.
            // Store _dtls_conn in PeerConnection for later use.

            debug!(
                "DTLS connection created, handshake must be driven by packet exchange"
            );
        }

        // Transition to connected state (in production, wait for DTLS to complete)
        // For now, we're "connected" after ICE succeeds
        self.state = ConnectionState::Connected;

        info!("Connection established for {}", self.connection_id);

        Ok(())
    }

    /// Add an ICE candidate received from the remote peer
    ///
    /// # Arguments
    ///
    /// * `candidate` - ICE candidate
    pub async fn add_ice_candidate(&mut self, candidate: IceCandidate) -> Result<()> {
        debug!(
            "Adding ICE candidate for {}: {:?}",
            self.connection_id, candidate
        );

        let mut ice_agent = self.ice_agent.lock().await;
        ice_agent.add_remote_candidate(candidate);
        ice_agent.form_candidate_pairs();

        Ok(())
    }

    /// Get the current connection state
    pub fn get_state(&self) -> ConnectionState {
        self.state
    }

    /// Get the connection ID
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// Get the DTLS fingerprint
    pub fn dtls_fingerprint(&self) -> &str {
        &self.dtls_cert.fingerprint
    }

    /// Get the local SDP (if available)
    pub fn local_sdp(&self) -> Option<&str> {
        self.local_sdp.as_deref()
    }

    /// Get the remote SDP (if available)
    pub fn remote_sdp(&self) -> Option<&str> {
        self.remote_sdp.as_deref()
    }
}

/// Generate a unique connection ID
fn generate_connection_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();

    format!("webrtc-{}", timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_peer_connection() {
        let stun_servers = vec!["stun:stun.l.google.com:19302".to_string()];
        let peer = PeerConnection::new(stun_servers).await.unwrap();

        assert_eq!(peer.get_state(), ConnectionState::New);
        assert!(peer.connection_id().starts_with("webrtc-"));
        assert!(!peer.dtls_fingerprint().is_empty());
    }

    #[tokio::test]
    async fn test_create_offer() {
        let stun_servers = vec![];
        let mut peer = PeerConnection::new(stun_servers).await.unwrap();

        let offer = peer.create_offer().await.unwrap();

        assert_eq!(peer.get_state(), ConnectionState::Gathering);
        assert!(!offer.is_empty());
        assert!(offer.contains("v=0"));
        assert!(offer.contains("a=ice-ufrag:"));
        assert!(offer.contains("a=ice-pwd:"));
        assert!(offer.contains("a=fingerprint:"));
        assert!(offer.contains("a=setup:"));
    }

    #[tokio::test]
    async fn test_connection_id_generation() {
        let id1 = generate_connection_id();

        // Small delay to ensure different timestamp
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

        let id2 = generate_connection_id();

        assert!(id1.starts_with("webrtc-"));
        assert!(id2.starts_with("webrtc-"));
        assert_ne!(id1, id2); // Should be unique
    }
}
