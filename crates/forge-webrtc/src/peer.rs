//! WebRTC PeerConnection implementation
//!
//! Provides the main abstraction for WebRTC peer-to-peer connections.

use crate::{Result, WebRtcError};
use forge_ice::{IceAgent, IceCandidate};
use forge_rtp::dtls::DtlsCertificate;
use forge_rtp::RtpSocketPair;
use forge_sdp::{
    DtlsAttributesExt, DtlsSetup, IceAttributesExt, MediaIceAttributesExt, SessionDescription,
    SessionDescriptionExt,
};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

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

    /// DTLS connection (wrapped in Arc<Mutex> for background task access)
    #[cfg(feature = "dtls")]
    dtls_connection: Option<Arc<Mutex<forge_rtp::dtls::DtlsConnection>>>,

    /// Background task handle for DTLS handshake
    #[cfg(feature = "dtls")]
    dtls_task: Option<JoinHandle<()>>,

    /// RTP socket pair (RTP + RTCP)
    /// TODO: Implement RTP media flow using this socket
    #[allow(dead_code)]
    rtp_socket: Option<Arc<RtpSocketPair>>,

    /// Connection state shared with background tasks
    state: Arc<StdMutex<ConnectionState>>,

    /// STUN servers for server-reflexive candidates
    /// TODO: Use for ICE restart and re-negotiation
    #[allow(dead_code)]
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
            DtlsCertificate::generate().map_err(|e| WebRtcError::DtlsError(e.to_string()))?,
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
            #[cfg(feature = "dtls")]
            dtls_connection: None,
            #[cfg(feature = "dtls")]
            dtls_task: None,
            rtp_socket: None,
            state: Arc::new(StdMutex::new(ConnectionState::New)),
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
        if self.get_state() != ConnectionState::New {
            return Err(WebRtcError::InvalidState(format!(
                "Cannot create offer in state {:?}",
                self.get_state()
            )));
        }

        info!("Creating SDP offer for connection {}", self.connection_id);

        // Transition to gathering state
        self.set_state(ConnectionState::Gathering);

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
            .find(|c| matches!(c.typ, forge_ice::candidate::CandidateType::Host))
            .map(|c| c.ip.to_string())
            .unwrap_or_else(|| "0.0.0.0".to_string());

        // Use first host candidate's port, or default to 9
        let local_port = local_candidates
            .iter()
            .find(|c| matches!(c.typ, forge_ice::candidate::CandidateType::Host))
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
        if self.get_state() != ConnectionState::Gathering {
            return Err(WebRtcError::InvalidState(format!(
                "Cannot set remote answer in state {:?}",
                self.get_state()
            )));
        }

        info!("Setting remote answer for {}", self.connection_id);

        // Parse SDP
        let remote_sdp = SessionDescription::from_str(sdp)?;

        // Extract ICE credentials
        let (remote_ufrag, remote_pwd) = remote_sdp.get_ice_credentials().ok_or_else(|| {
            WebRtcError::SdpError(forge_sdp::SdpError::MissingField(
                "ICE credentials".to_string(),
            ))
        })?;

        // Extract DTLS fingerprint
        let (fingerprint_alg, remote_fingerprint) =
            remote_sdp.get_dtls_fingerprint().ok_or_else(|| {
                WebRtcError::SdpError(forge_sdp::SdpError::MissingField(
                    "DTLS fingerprint".to_string(),
                ))
            })?;

        // Extract DTLS setup
        let remote_setup = remote_sdp.get_dtls_setup().ok_or_else(|| {
            WebRtcError::SdpError(forge_sdp::SdpError::MissingField("DTLS setup".to_string()))
        })?;

        validate_dtls_fingerprint(&fingerprint_alg, &remote_fingerprint)?;

        debug!(
            "Remote ICE credentials: ufrag={}, pwd={}",
            remote_ufrag, remote_pwd
        );
        debug!("Remote DTLS fingerprint: {}", remote_fingerprint);

        // Extract remote candidates from media description
        let remote_candidates: Vec<IceCandidate> = if let Some(media) = remote_sdp.media.first() {
            use forge_sdp::MediaIceAttributesExt;
            let candidate_strings: Vec<String> = MediaIceAttributesExt::get_ice_candidates(media);
            debug!(
                "Received {} remote candidate strings",
                candidate_strings.len()
            );

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

        debug!(
            "Successfully parsed {} remote candidates",
            remote_candidates.len()
        );

        // Set remote credentials in ICE agent
        let mut ice_agent = self.ice_agent.lock().await;
        ice_agent
            .set_remote_credentials(remote_ufrag, remote_pwd)
            .map_err(|e| WebRtcError::IceError(format!("Failed to set remote credentials: {}", e)))?;

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
        self.set_state(ConnectionState::Checking);

        // Start connectivity checks and DTLS handshake
        info!(
            "Starting ICE connectivity checks for {}",
            self.connection_id
        );

        // Perform ICE connectivity checks
        let mut ice_agent = self.ice_agent.lock().await;
        let checks_succeeded = ice_agent
            .perform_connectivity_checks()
            .await
            .map_err(|e| WebRtcError::ConnectionFailed(format!("ICE checks failed: {}", e)))?;

        if !checks_succeeded {
            self.set_state(ConnectionState::Failed);
            return Err(WebRtcError::ConnectionFailed(
                "No ICE candidate pairs succeeded".to_string(),
            ));
        }

        // Get the selected pair
        let selected_pair_index = ice_agent
            .nominate_pair()
            .ok_or_else(|| WebRtcError::ConnectionFailed("No pair nominated".to_string()))?;

        let selected_pair = &ice_agent.get_candidate_pairs()[selected_pair_index];
        let local_addr =
            std::net::SocketAddr::new(selected_pair.local.ip, selected_pair.local.port);
        let remote_addr =
            std::net::SocketAddr::new(selected_pair.remote.ip, selected_pair.remote.port);

        info!("ICE checks complete: {} <-> {}", local_addr, remote_addr);

        // Get the socket from ICE agent for DTLS packet exchange
        let ice_agent = self.ice_agent.lock().await;
        let socket = ice_agent
            .get_socket()
            .ok_or_else(|| WebRtcError::ConnectionFailed("No ICE socket available".to_string()))?;
        drop(ice_agent);

        // Create DTLS context and connection, then spawn background task to drive handshake
        #[cfg(feature = "dtls")]
        {
            use forge_rtp::dtls::{DtlsConnection, DtlsContext, DtlsRole};

            let dtls_role = match remote_setup {
                DtlsSetup::Active => DtlsRole::Server,
                DtlsSetup::Passive => DtlsRole::Client,
                DtlsSetup::Actpass | DtlsSetup::Holdconn => {
                    return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
                        "Invalid DTLS setup value in answer".to_string(),
                    )))
                }
            };

            info!(
                "Starting DTLS handshake with remote fingerprint: {}",
                remote_fingerprint
            );

            let dtls_ctx = DtlsContext::new(self.dtls_cert.clone(), dtls_role).map_err(|e| {
                WebRtcError::DtlsError(format!("Failed to create DTLS context: {}", e))
            })?;

            let dtls_conn = DtlsConnection::new(&dtls_ctx, dtls_role, Some(remote_fingerprint))
                .map_err(|e| {
                    WebRtcError::DtlsError(format!("Failed to create DTLS connection: {}", e))
                })?;

            let dtls_conn = Arc::new(Mutex::new(dtls_conn));
            self.dtls_connection = Some(dtls_conn.clone());

            // Spawn background task to drive DTLS handshake
            let task_socket = socket.clone();
            let task_remote_addr = remote_addr;
            let task_connection_id = self.connection_id.clone();
            let task_state = self.state.clone();

            let dtls_task = tokio::spawn(async move {
                if let Err(e) = Self::drive_dtls_handshake(
                    task_socket,
                    task_remote_addr,
                    dtls_conn,
                    task_connection_id,
                    task_state,
                )
                .await
                {
                    error!("DTLS handshake task failed: {}", e);
                }
            });

            self.dtls_task = Some(dtls_task);

            debug!("DTLS handshake task spawned");
        }

        // Note: State will transition to Connected after DTLS handshake completes.
        #[cfg(not(feature = "dtls"))]
        {
            self.set_state(ConnectionState::Connected);
        }

        info!("ICE connectivity established for {}", self.connection_id);

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
        *self
            .state
            .lock()
            .expect("state mutex should not be poisoned")
    }

    fn set_state(&self, state: ConnectionState) {
        let mut guard = self
            .state
            .lock()
            .expect("state mutex should not be poisoned");
        *guard = state;
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

    /// Get the number of local ICE candidates gathered
    pub async fn local_candidate_count(&self) -> usize {
        self.ice_agent.lock().await.get_local_candidates().len()
    }

    /// Drive the DTLS handshake by exchanging packets over UDP
    ///
    /// This function runs in a background task and:
    /// 1. Initiates the DTLS handshake (client sends ClientHello)
    /// 2. Reads incoming packets and demultiplexes them
    /// 3. Feeds DTLS packets to the handshake state machine
    /// 4. Sends outgoing DTLS packets to the remote peer
    /// 5. Stops when handshake completes or fails
    ///
    /// Packet demultiplexing (RFC 5764):
    /// - 0-3: STUN/TURN (ignore, handled by ICE)
    /// - 20-63: DTLS
    /// - 64-79: TURN Channel (ignore)
    /// - 128-191: SRTP/SRTCP (ignore for now, handled after DTLS completes)
    #[cfg(feature = "dtls")]
    async fn drive_dtls_handshake(
        socket: Arc<tokio::net::UdpSocket>,
        remote_addr: std::net::SocketAddr,
        dtls_conn: Arc<Mutex<forge_rtp::dtls::DtlsConnection>>,
        connection_id: String,
        state: Arc<StdMutex<ConnectionState>>,
    ) -> Result<()> {
        use forge_rtp::dtls::DtlsState;

        info!("DTLS handshake task started for {}", connection_id);

        // Initiate handshake (client sends ClientHello)
        let mut dtls = dtls_conn.lock().await;
        let (complete, outgoing) = dtls
            .handshake(None)
            .map_err(|e| {
                {
                    let mut guard = state
                        .lock()
                        .expect("state mutex should not be poisoned");
                    *guard = ConnectionState::Failed;
                }
                WebRtcError::DtlsError(format!("Failed to initiate handshake: {}", e))
            })?;

        if !outgoing.is_empty() {
            debug!(
                "Sending initial DTLS ClientHello ({} bytes)",
                outgoing.len()
            );
            socket
                .send_to(&outgoing, remote_addr)
                .await
                .map_err(|e| {
                    {
                        let mut guard = state
                            .lock()
                            .expect("state mutex should not be poisoned");
                        *guard = ConnectionState::Failed;
                    }
                    WebRtcError::DtlsError(format!("Failed to send DTLS data: {}", e))
                })?;
        }

        if complete {
            info!("DTLS handshake completed immediately (unexpected for client)");
            drop(dtls);
            if let Err(err) = Self::extract_and_log_srtp_keys(dtls_conn, &connection_id).await {
                let mut guard = state
                    .lock()
                    .expect("state mutex should not be poisoned");
                *guard = ConnectionState::Failed;
                return Err(err);
            }
            return Ok(());
        }

        drop(dtls);

        // Loop to receive and process DTLS packets
        let mut buf = vec![0u8; 2048];
        let timeout_duration = tokio::time::Duration::from_secs(10);
        let start_time = tokio::time::Instant::now();

        loop {
            // Check for timeout
            if start_time.elapsed() > timeout_duration {
                error!("DTLS handshake timeout for {}", connection_id);
                {
                    let mut guard = state
                        .lock()
                        .expect("state mutex should not be poisoned");
                    *guard = ConnectionState::Failed;
                }
                return Err(WebRtcError::DtlsError("Handshake timeout".to_string()));
            }

            // Receive packet with timeout
            let (len, addr) = match tokio::time::timeout(
                tokio::time::Duration::from_secs(1),
                socket.recv_from(&mut buf),
            )
            .await
            {
                Ok(Ok((len, addr))) => (len, addr),
                Ok(Err(e)) => {
                    warn!("Socket recv error: {}", e);
                    continue;
                }
                Err(_) => {
                    // Timeout, check if handshake is still in progress
                    let dtls = dtls_conn.lock().await;
                    if dtls.state() == DtlsState::Connected {
                        info!("DTLS handshake completed");
                        drop(dtls);
                        {
                            let mut guard = state
                                .lock()
                                .expect("state mutex should not be poisoned");
                            *guard = ConnectionState::Connected;
                        }
                        if let Err(err) =
                            Self::extract_and_log_srtp_keys(dtls_conn, &connection_id).await
                        {
                            let mut guard = state
                                .lock()
                                .expect("state mutex should not be poisoned");
                            *guard = ConnectionState::Failed;
                            return Err(err);
                        }
                        return Ok(());
                    }
                    drop(dtls);
                    continue;
                }
            };

            // Demultiplex packet based on first byte
            if len == 0 {
                continue;
            }

            if addr != remote_addr {
                debug!("Ignoring DTLS packet from unexpected addr {}", addr);
                continue;
            }

            let first_byte = buf[0];
            let is_dtls = (20..=63).contains(&first_byte);

            if !is_dtls {
                // Not a DTLS packet, ignore (likely STUN or SRTP)
                debug!("Ignoring non-DTLS packet (first byte: {})", first_byte);
                continue;
            }

            debug!("Received DTLS packet ({} bytes)", len);

            // Process DTLS packet
            let mut dtls = dtls_conn.lock().await;
            let (complete, outgoing) = dtls.handshake(Some(&buf[..len])).map_err(|e| {
                {
                    let mut guard = state
                        .lock()
                        .expect("state mutex should not be poisoned");
                    *guard = ConnectionState::Failed;
                }
                WebRtcError::DtlsError(format!("Handshake processing failed: {}", e))
            })?;

            // Send any outgoing data
            if !outgoing.is_empty() {
                debug!("Sending DTLS response ({} bytes)", outgoing.len());
                socket.send_to(&outgoing, remote_addr).await.map_err(|e| {
                    {
                        let mut guard = state
                            .lock()
                            .expect("state mutex should not be poisoned");
                        *guard = ConnectionState::Failed;
                    }
                    WebRtcError::DtlsError(format!("Failed to send DTLS data: {}", e))
                })?;
            }

            if complete {
                info!("DTLS handshake completed for {}", connection_id);
                drop(dtls);
                {
                    let mut guard = state
                        .lock()
                        .expect("state mutex should not be poisoned");
                    *guard = ConnectionState::Connected;
                }
                if let Err(err) = Self::extract_and_log_srtp_keys(dtls_conn, &connection_id).await
                {
                    let mut guard = state
                        .lock()
                        .expect("state mutex should not be poisoned");
                    *guard = ConnectionState::Failed;
                    return Err(err);
                }
                return Ok(());
            }

            drop(dtls);
        }
    }

    /// Extract SRTP keys from completed DTLS handshake
    #[cfg(feature = "dtls")]
    async fn extract_and_log_srtp_keys(
        dtls_conn: Arc<Mutex<forge_rtp::dtls::DtlsConnection>>,
        connection_id: &str,
    ) -> Result<()> {
        let dtls = dtls_conn.lock().await;
        let (client_keys, server_keys) = dtls
            .export_srtp_keys()
            .map_err(|e| WebRtcError::DtlsError(format!("Failed to export SRTP keys: {}", e)))?;

        info!(
            "SRTP keys exported for {}: client_key={} bytes, server_key={} bytes",
            connection_id,
            client_keys.master_key.len(),
            server_keys.master_key.len()
        );

        // TODO: Store these keys for SRTP encryption/decryption

        Ok(())
    }
}

/// Generate a unique connection ID
fn generate_connection_id() -> String {
    format!("webrtc-{}", uuid::Uuid::new_v4())
}

fn validate_dtls_fingerprint(algorithm: &str, fingerprint: &str) -> Result<()> {
    if !algorithm.eq_ignore_ascii_case("sha-256") {
        return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
            "Unsupported DTLS fingerprint algorithm".to_string(),
        )));
    }

    if fingerprint.len() != 95 {
        return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
            "Invalid DTLS fingerprint length".to_string(),
        )));
    }

    if !fingerprint
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':')
    {
        return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
            "Invalid DTLS fingerprint format".to_string(),
        )));
    }

    Ok(())
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
