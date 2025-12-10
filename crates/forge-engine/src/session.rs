//! Media session management for two-party calls

use forge_core::{CallId, EventBus, ForgeError, ForgeEvent, ParticipantId, Result};
use forge_rtp::{PortPair, PortPool, RtpSocketConfig, RtpSocketPair};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

#[cfg(all(target_os = "linux", feature = "xdp"))]
use forge_kernel::{ForwardKey, ForwardValue, XdpManager};

/// DTMF detection configuration
#[derive(Debug, Clone)]
pub struct DtmfConfig {
    /// Enable RFC 2833 (telephone-event) detection
    pub enable_rfc2833: bool,
    /// Enable inband (Goertzel) detection
    pub enable_inband: bool,
    /// Enable deduplication (recommended when multiple methods enabled)
    pub enable_dedup: bool,
    /// Opus payload type for inband detection (dynamic, typically 111)
    pub opus_payload_type: u8,
}

/// Transcoding configuration
#[derive(Debug, Clone)]
pub struct TranscodingConfig {
    /// Enable automatic transcoding when participants use different codecs
    pub enable_transcoding: bool,
    /// RTP payload type mapping for codec detection
    pub payload_type_map: forge_transcoder::rtp::PayloadTypeMap,
}

impl Default for DtmfConfig {
    fn default() -> Self {
        Self {
            enable_rfc2833: true,
            enable_inband: true,
            enable_dedup: true,
            opus_payload_type: 111, // Common dynamic payload type for Opus
        }
    }
}

impl Default for TranscodingConfig {
    fn default() -> Self {
        Self {
            enable_transcoding: true,
            payload_type_map: forge_transcoder::rtp::PayloadTypeMap::default(),
        }
    }
}

/// Configuration for a media session
#[derive(Debug, Clone)]
pub struct MediaSessionConfig {
    /// RTP socket configuration
    pub socket_config: RtpSocketConfig,
    /// Session timeout (idle duration before auto-termination)
    pub session_timeout: Duration,
    /// DTMF detection configuration
    pub dtmf_config: DtmfConfig,
    /// Transcoding configuration
    pub transcoding_config: TranscodingConfig,
}

impl Default for MediaSessionConfig {
    fn default() -> Self {
        Self {
            socket_config: RtpSocketConfig::default(),
            session_timeout: Duration::from_secs(300), // 5 minutes
            dtmf_config: DtmfConfig::default(),
            transcoding_config: TranscodingConfig::default(),
        }
    }
}

/// Participant in a media session
#[derive(Debug, Clone)]
pub struct Participant {
    /// Participant ID
    pub id: ParticipantId,
    /// Remote RTP endpoint (learned via symmetric RTP)
    pub remote_addr: Option<SocketAddr>,
    /// Codec payload type
    pub payload_type: u8,
    /// Statistics
    pub stats: ParticipantStats,
}

/// Statistics for a participant
#[derive(Debug, Clone, Default)]
pub struct ParticipantStats {
    /// Total packets received
    pub packets_received: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Total packets sent
    pub packets_sent: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Packets lost
    pub packets_lost: u64,
    /// Last packet received timestamp
    pub last_packet_at: Option<Instant>,
}

/// State of a media session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Session is being initialized
    Initializing,
    /// Session is active and forwarding media
    Active,
    /// Session is on hold
    OnHold,
    /// Session is terminating
    Terminating,
    /// Session has terminated
    Terminated,
}

/// A two-party media session
pub struct MediaSession {
    /// Unique session/call ID
    call_id: CallId,
    /// Session state
    state: Arc<RwLock<SessionState>>,
    /// Participant A
    participant_a: Arc<RwLock<Participant>>,
    /// Participant B
    participant_b: Arc<RwLock<Participant>>,
    /// RTP/RTCP socket pair
    sockets: Arc<RtpSocketPair>,
    /// Port pair allocation
    ports: PortPair,
    /// Port pool reference for cleanup
    port_pool: Arc<PortPool>,
    /// Track if ports have been deallocated
    ports_deallocated: Arc<AtomicBool>,
    /// Session creation time
    created_at: Instant,
    /// Last activity time
    last_activity: Arc<RwLock<Instant>>,
    /// Configuration
    config: MediaSessionConfig,
    /// Event bus for publishing events
    event_bus: Option<Arc<EventBus>>,
    /// RFC 2833 (telephone-event) DTMF detector
    dtmf_detector: Arc<Mutex<forge_dtmf::Rfc2833Detector>>,
    /// Inband DTMF detector (Goertzel algorithm)
    inband_detector: Arc<Mutex<forge_dtmf::GoertzelDetector>>,
    /// DTMF event deduplicator
    dtmf_dedup: Arc<Mutex<forge_dtmf::DtmfDeduplicator>>,
    /// Opus decoder for inband DTMF detection
    #[cfg(feature = "opus")]
    opus_decoder: Arc<Mutex<forge_codecs::opus::OpusCodec>>,
    /// Transcoder for A → B direction (optional, created when needed)
    transcoder_a_to_b: Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>>,
    /// Transcoder for B → A direction (optional, created when needed)
    transcoder_b_to_a: Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>>,
    /// Forwarding task handles
    forwarding_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// Optional offer/answer SDP associated with the session
    sdp: Option<String>,
    /// SIP/SDP from-tag if provided
    from_tag: Option<String>,
    /// SIP/SDP to-tag if provided
    to_tag: Option<String>,
    /// XDP manager for kernel-level packet forwarding (Linux only)
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    xdp_manager: Option<Arc<XdpManager>>,
    /// Track if XDP fast path is active
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    xdp_active: Arc<AtomicBool>,
}

impl MediaSession {
    /// Create a new media session
    pub async fn new(
        call_id: CallId,
        participant_a_id: ParticipantId,
        participant_b_id: ParticipantId,
        port_pool: &Arc<PortPool>,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
    ) -> Result<Self> {
        // Allocate ports
        let ports = port_pool.allocate().await?;
        let mut port_guard = PortAllocationGuard::new(Arc::clone(port_pool), ports);
        tracing::info!(
            "Allocated ports for session {}: RTP={}, RTCP={}",
            call_id.0,
            ports.rtp_port,
            ports.rtcp_port
        );

        // Create socket pair
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;
        port_guard.disarm();

        let participant_a = Participant {
            id: participant_a_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU
            stats: ParticipantStats::default(),
        };

        let participant_b = Participant {
            id: participant_b_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU
            stats: ParticipantStats::default(),
        };

        let now = Instant::now();

        let session = Self {
            call_id: call_id.clone(),
            state: Arc::new(RwLock::new(SessionState::Initializing)),
            participant_a: Arc::new(RwLock::new(participant_a)),
            participant_b: Arc::new(RwLock::new(participant_b)),
            sockets: Arc::new(sockets),
            ports,
            port_pool: Arc::clone(port_pool),
            ports_deallocated: Arc::new(AtomicBool::new(false)),
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            dtmf_detector: Arc::new(Mutex::new(forge_dtmf::Rfc2833Detector::new(8000))),
            inband_detector: Arc::new(Mutex::new(forge_dtmf::GoertzelDetector::new(8000, 160))),
            dtmf_dedup: Arc::new(Mutex::new(forge_dtmf::DtmfDeduplicator::new())),
            #[cfg(feature = "opus")]
            opus_decoder: Arc::new(Mutex::new({
                let opus_config = forge_codecs::opus::OpusConfig {
                    sample_rate: 48000,
                    channels: 1,
                    application: forge_codecs::opus::OpusApplication::Voip,
                    bitrate: 24000,
                    frame_duration_ms: 20,
                };
                forge_codecs::opus::OpusCodec::with_config(opus_config)
                    .expect("Failed to create Opus decoder for DTMF detection")
            })),
            transcoder_a_to_b: Arc::new(Mutex::new(None)),
            transcoder_b_to_a: Arc::new(Mutex::new(None)),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
            sdp,
            from_tag,
            to_tag,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_manager: None,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_active: Arc::new(AtomicBool::new(false)),
        };

        // Publish session created event
        if let Some(bus) = &event_bus {
            bus.publish(ForgeEvent::SessionCreated {
                call_id,
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(session)
    }

    /// Create a new media session with XDP support
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn new_with_xdp(
        call_id: CallId,
        participant_a_id: ParticipantId,
        participant_b_id: ParticipantId,
        port_pool: &Arc<PortPool>,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
        xdp_manager: Option<Arc<XdpManager>>,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
    ) -> Result<Self> {
        // Allocate ports
        let ports = port_pool.allocate().await?;
        let mut port_guard = PortAllocationGuard::new(Arc::clone(port_pool), ports);
        tracing::info!(
            "Allocated ports for session {}: RTP={}, RTCP={}",
            call_id.0,
            ports.rtp_port,
            ports.rtcp_port
        );

        // Create socket pair
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;
        port_guard.disarm();

        let participant_a = Participant {
            id: participant_a_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU
            stats: ParticipantStats::default(),
        };

        let participant_b = Participant {
            id: participant_b_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU
            stats: ParticipantStats::default(),
        };

        let now = Instant::now();

        let session = Self {
            call_id: call_id.clone(),
            state: Arc::new(RwLock::new(SessionState::Initializing)),
            participant_a: Arc::new(RwLock::new(participant_a)),
            participant_b: Arc::new(RwLock::new(participant_b)),
            sockets: Arc::new(sockets),
            ports,
            port_pool: Arc::clone(port_pool),
            ports_deallocated: Arc::new(AtomicBool::new(false)),
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            dtmf_detector: Arc::new(Mutex::new(forge_dtmf::Rfc2833Detector::new(8000))),
            inband_detector: Arc::new(Mutex::new(forge_dtmf::GoertzelDetector::new(8000, 160))),
            dtmf_dedup: Arc::new(Mutex::new(forge_dtmf::DtmfDeduplicator::new())),
            #[cfg(feature = "opus")]
            opus_decoder: Arc::new(Mutex::new({
                let opus_config = forge_codecs::opus::OpusConfig {
                    sample_rate: 48000,
                    channels: 1,
                    application: forge_codecs::opus::OpusApplication::Voip,
                    bitrate: 24000,
                    frame_duration_ms: 20,
                };
                forge_codecs::opus::OpusCodec::with_config(opus_config)
                    .expect("Failed to create Opus decoder for DTMF detection")
            })),
            transcoder_a_to_b: Arc::new(Mutex::new(None)),
            transcoder_b_to_a: Arc::new(Mutex::new(None)),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
            sdp,
            from_tag,
            to_tag,
            xdp_manager,
            xdp_active: Arc::new(AtomicBool::new(false)),
        };

        // Publish session created event
        if let Some(bus) = &event_bus {
            bus.publish(ForgeEvent::SessionCreated {
                call_id,
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(session)
    }

    /// Get the call ID
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }

    /// Get the current session state
    pub async fn state(&self) -> SessionState {
        *self.state.read().await
    }

    /// Get the allocated port pair
    pub fn ports(&self) -> PortPair {
        self.ports
    }

    /// Get participant A statistics
    pub async fn participant_a_stats(&self) -> ParticipantStats {
        self.participant_a.read().await.stats.clone()
    }

    /// Get participant B statistics
    pub async fn participant_b_stats(&self) -> ParticipantStats {
        self.participant_b.read().await.stats.clone()
    }

    /// Get session uptime
    pub fn uptime(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Get time since last activity
    pub async fn idle_time(&self) -> Duration {
        self.last_activity.read().await.elapsed()
    }

    /// Check if session has timed out
    pub async fn is_timed_out(&self) -> bool {
        self.idle_time().await > self.config.session_timeout
    }

    /// Get the event bus
    pub fn event_bus(&self) -> Option<&Arc<EventBus>> {
        self.event_bus.as_ref()
    }

    /// Get the DTMF detector
    pub fn dtmf_detector(&self) -> &Arc<Mutex<forge_dtmf::Rfc2833Detector>> {
        &self.dtmf_detector
    }

    /// Get the inband DTMF detector
    pub fn inband_detector(&self) -> &Arc<Mutex<forge_dtmf::GoertzelDetector>> {
        &self.inband_detector
    }

    /// Get the DTMF deduplicator
    pub fn dtmf_dedup(&self) -> &Arc<Mutex<forge_dtmf::DtmfDeduplicator>> {
        &self.dtmf_dedup
    }

    /// Get the DTMF configuration
    pub fn dtmf_config(&self) -> &DtmfConfig {
        &self.config.dtmf_config
    }

    /// Get the Opus decoder for inband DTMF detection
    #[cfg(feature = "opus")]
    pub fn opus_decoder(&self) -> &Arc<Mutex<forge_codecs::opus::OpusCodec>> {
        &self.opus_decoder
    }

    /// Get the transcoding configuration
    pub fn transcoding_config(&self) -> &TranscodingConfig {
        &self.config.transcoding_config
    }

    /// Get transcoder for A → B direction
    pub fn transcoder_a_to_b(&self) -> &Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>> {
        &self.transcoder_a_to_b
    }

    /// Get transcoder for B → A direction
    pub fn transcoder_b_to_a(&self) -> &Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>> {
        &self.transcoder_b_to_a
    }

    /// Initialize transcoder for A → B if needed
    pub async fn ensure_transcoder_a_to_b(
        &self,
        src_codec: forge_codecs::AudioCodecType,
        dst_codec: forge_codecs::AudioCodecType,
    ) -> Result<()> {
        if !self.config.transcoding_config.enable_transcoding {
            return Ok(());
        }

        if src_codec == dst_codec {
            return Ok(()); // No transcoding needed
        }

        let mut transcoder = self.transcoder_a_to_b.lock().await;
        if transcoder.is_none() {
            tracing::info!(
                "Initializing transcoder for session {} A→B: {} → {}",
                self.call_id.0,
                codec_name(src_codec),
                codec_name(dst_codec)
            );

            let pt_map = self.config.transcoding_config.payload_type_map;
            let new_transcoder = forge_transcoder::RtpTranscoder::new(src_codec, dst_codec, pt_map)
                .map_err(|e| ForgeError::Internal(format!("Failed to create transcoder: {}", e)))?;

            *transcoder = Some(new_transcoder);
        }

        Ok(())
    }

    /// Initialize transcoder for B → A if needed
    pub async fn ensure_transcoder_b_to_a(
        &self,
        src_codec: forge_codecs::AudioCodecType,
        dst_codec: forge_codecs::AudioCodecType,
    ) -> Result<()> {
        if !self.config.transcoding_config.enable_transcoding {
            return Ok(());
        }

        if src_codec == dst_codec {
            return Ok(()); // No transcoding needed
        }

        let mut transcoder = self.transcoder_b_to_a.lock().await;
        if transcoder.is_none() {
            tracing::info!(
                "Initializing transcoder for session {} B→A: {} → {}",
                self.call_id.0,
                codec_name(src_codec),
                codec_name(dst_codec)
            );

            let pt_map = self.config.transcoding_config.payload_type_map;
            let new_transcoder = forge_transcoder::RtpTranscoder::new(src_codec, dst_codec, pt_map)
                .map_err(|e| ForgeError::Internal(format!("Failed to create transcoder: {}", e)))?;

            *transcoder = Some(new_transcoder);
        }

        Ok(())
    }

    /// Activate XDP fast path for this session
    /// Should be called after both participants' endpoints are learned
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn activate_xdp_fast_path(&self) -> Result<()> {
        // Check if XDP is available
        let xdp_manager = match &self.xdp_manager {
            Some(mgr) => mgr,
            None => {
                tracing::debug!("XDP not available for session {}", self.call_id.0);
                return Ok(());
            }
        };

        // Check if already active
        if self.xdp_active.load(Ordering::Relaxed) {
            tracing::debug!(
                "XDP fast path already active for session {}",
                self.call_id.0
            );
            return Ok(());
        }

        // Get participant addresses
        let (a_addr, b_addr) = {
            let a = self.participant_a.read().await;
            let b = self.participant_b.read().await;

            match (a.remote_addr, b.remote_addr) {
                (Some(a_addr), Some(b_addr)) => (a_addr, b_addr),
                _ => {
                    tracing::warn!(
                        "Cannot activate XDP fast path - not all endpoints learned for session {}",
                        self.call_id.0
                    );
                    return Ok(());
                }
            }
        };

        tracing::info!(
            "Activating XDP fast path for session {} (A: {} <-> B: {})",
            self.call_id.0,
            a_addr,
            b_addr
        );

        // Helper to convert SocketAddr to network byte order
        fn addr_to_network_bytes(addr: SocketAddr) -> (u32, u16) {
            let ip_bytes = match addr.ip() {
                std::net::IpAddr::V4(ipv4) => ipv4.octets(),
                std::net::IpAddr::V6(_) => {
                    // XDP currently only supports IPv4
                    return (0, 0);
                }
            };
            let ip_u32 = u32::from_ne_bytes(ip_bytes);
            let port_be = addr.port().to_be();
            (ip_u32, port_be)
        }

        let (a_ip, a_port) = addr_to_network_bytes(a_addr);
        let (b_ip, b_port) = addr_to_network_bytes(b_addr);
        let rtp_port_be = self.ports.rtp_port.to_be();

        // Insert bidirectional forwarding rules
        // Rule 1: A -> B (packets from A forwarded to B)
        let key_a_to_b = ForwardKey {
            src_ip: a_ip,
            src_port: a_port,
            dst_port: rtp_port_be,
            dst_ip: 0,    // Will be filled by XDP program (our local IP)
            protocol: 17, // UDP
            _padding: [0; 3],
        };

        let value_a_to_b = ForwardValue {
            dest_ip: b_ip,
            dest_port: b_port,
            src_ip: 0, // Our IP for reply
            src_port: rtp_port_be,
            last_seen: 0,
        };

        xdp_manager
            .insert_forward_rule(key_a_to_b, value_a_to_b)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP insert forward rule failed: {}", e)))?;

        // Rule 2: B -> A (packets from B forwarded to A)
        let key_b_to_a = ForwardKey {
            src_ip: b_ip,
            src_port: b_port,
            dst_port: rtp_port_be,
            dst_ip: 0,
            protocol: 17,
            _padding: [0; 3],
        };

        let value_b_to_a = ForwardValue {
            dest_ip: a_ip,
            dest_port: a_port,
            src_ip: 0,
            src_port: rtp_port_be,
            last_seen: 0,
        };

        xdp_manager
            .insert_forward_rule(key_b_to_a, value_b_to_a)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP insert forward rule failed: {}", e)))?;

        self.xdp_active.store(true, Ordering::Relaxed);

        tracing::info!("XDP fast path activated for session {}", self.call_id.0);

        Ok(())
    }

    /// Deactivate XDP fast path for this session
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn deactivate_xdp_fast_path(&self) -> Result<()> {
        // Check if XDP is available and active
        let xdp_manager = match &self.xdp_manager {
            Some(mgr) => mgr,
            None => return Ok(()),
        };

        if !self.xdp_active.load(Ordering::Relaxed) {
            return Ok(());
        }

        tracing::info!("Deactivating XDP fast path for session {}", self.call_id.0);

        // Get participant addresses
        let (a_addr, b_addr) = {
            let a = self.participant_a.read().await;
            let b = self.participant_b.read().await;

            match (a.remote_addr, b.remote_addr) {
                (Some(a_addr), Some(b_addr)) => (a_addr, b_addr),
                _ => {
                    self.xdp_active.store(false, Ordering::Relaxed);
                    return Ok(());
                }
            }
        };

        // Helper to convert SocketAddr to network byte order
        fn addr_to_network_bytes(addr: SocketAddr) -> (u32, u16) {
            let ip_bytes = match addr.ip() {
                std::net::IpAddr::V4(ipv4) => ipv4.octets(),
                std::net::IpAddr::V6(_) => return (0, 0),
            };
            let ip_u32 = u32::from_ne_bytes(ip_bytes);
            let port_be = addr.port().to_be();
            (ip_u32, port_be)
        }

        let (a_ip, a_port) = addr_to_network_bytes(a_addr);
        let (b_ip, b_port) = addr_to_network_bytes(b_addr);
        let rtp_port_be = self.ports.rtp_port.to_be();

        // Remove bidirectional forwarding rules
        let key_a_to_b = ForwardKey {
            src_ip: a_ip,
            src_port: a_port,
            dst_port: rtp_port_be,
            dst_ip: 0,
            protocol: 17,
            _padding: [0; 3],
        };

        let key_b_to_a = ForwardKey {
            src_ip: b_ip,
            src_port: b_port,
            dst_port: rtp_port_be,
            dst_ip: 0,
            protocol: 17,
            _padding: [0; 3],
        };

        xdp_manager
            .remove_forward_rule(&key_a_to_b)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP remove forward rule failed: {}", e)))?;
        xdp_manager
            .remove_forward_rule(&key_b_to_a)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP remove forward rule failed: {}", e)))?;

        self.xdp_active.store(false, Ordering::Relaxed);

        tracing::info!("XDP fast path deactivated for session {}", self.call_id.0);

        Ok(())
    }

    /// Start the RTP forwarding loop
    pub async fn start_forwarding(self: &Arc<Self>) -> Result<()> {
        let mut state = self.state.write().await;
        if *state != SessionState::Initializing {
            return Err(ForgeError::Internal(
                "Session must be in Initializing state to start forwarding".to_string(),
            ));
        }

        *state = SessionState::Active;
        drop(state);

        tracing::info!("Starting RTP forwarding for session {}", self.call_id.0);

        // Publish state change event
        if let Some(bus) = &self.event_bus {
            bus.publish(ForgeEvent::SessionActive {
                call_id: self.call_id.clone(),
                timestamp: chrono::Utc::now(),
            });
        }

        // Start forwarding task
        let forwarding_handle =
            crate::forwarding::ForwardingEngine::start_forwarding(Arc::clone(self)).await?;
        self.forwarding_tasks.lock().await.push(forwarding_handle);

        Ok(())
    }

    /// Stop the RTP forwarding loop
    pub async fn stop_forwarding(&self) -> Result<()> {
        let mut state = self.state.write().await;
        if *state == SessionState::Terminated {
            return Ok(());
        }

        *state = SessionState::Terminating;
        drop(state);

        tracing::info!("Stopping RTP forwarding for session {}", self.call_id.0);

        // Deactivate XDP fast path if active
        #[cfg(all(target_os = "linux", feature = "xdp"))]
        {
            if let Err(e) = self.deactivate_xdp_fast_path().await {
                tracing::error!("Failed to deactivate XDP fast path: {}", e);
            }
        }

        // Cancel all forwarding tasks
        let mut tasks = self.forwarding_tasks.lock().await;
        for task in tasks.drain(..) {
            task.abort();
        }

        *self.state.write().await = SessionState::Terminated;

        // Deallocate ports - guaranteed cleanup
        self.deallocate_ports().await;

        // Publish termination event
        if let Some(bus) = &self.event_bus {
            bus.publish(ForgeEvent::SessionTerminated {
                call_id: self.call_id.clone(),
                reason: "Stopped by request".to_string(),
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(())
    }

    /// Deallocate ports (idempotent)
    async fn deallocate_ports(&self) {
        // Use compare_exchange to ensure we only deallocate once
        if self
            .ports_deallocated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tracing::debug!(
                "Deallocating ports for session {}: RTP={}, RTCP={}",
                self.call_id.0,
                self.ports.rtp_port,
                self.ports.rtcp_port
            );
            self.port_pool.deallocate(self.ports).await;
        }
    }

    /// Update last activity timestamp
    /// This should be called whenever RTP packets are received/forwarded
    pub async fn update_activity(&self) {
        *self.last_activity.write().await = Instant::now();
    }

    /// Get the socket pair (for forwarding implementation)
    pub fn sockets(&self) -> &Arc<RtpSocketPair> {
        &self.sockets
    }

    /// Get mutable reference to participant A
    pub fn participant_a(&self) -> &Arc<RwLock<Participant>> {
        &self.participant_a
    }

    /// Get mutable reference to participant B
    pub fn participant_b(&self) -> &Arc<RwLock<Participant>> {
        &self.participant_b
    }

    /// Get associated SDP (if any)
    pub fn sdp(&self) -> Option<&str> {
        self.sdp.as_deref()
    }

    /// Get from-tag (if any)
    pub fn from_tag(&self) -> Option<&str> {
        self.from_tag.as_deref()
    }

    /// Get to-tag (if any)
    pub fn to_tag(&self) -> Option<&str> {
        self.to_tag.as_deref()
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        tracing::debug!("MediaSession {} dropped", self.call_id.0);

        // Ensure ports are deallocated even if stop_forwarding was never called
        // Check if ports have already been deallocated
        if !self.ports_deallocated.load(Ordering::SeqCst) {
            tracing::warn!(
                "Session {} dropped without cleanup - spawning port deallocation task",
                self.call_id.0
            );

            // Spawn a detached task to deallocate ports asynchronously
            let port_pool = Arc::clone(&self.port_pool);
            let ports = self.ports;
            let ports_deallocated = Arc::clone(&self.ports_deallocated);
            let call_id = self.call_id.0.clone();

            tokio::spawn(async move {
                // Double-check to avoid race condition
                if ports_deallocated
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    tracing::debug!(
                        "Drop cleanup: Deallocating ports for session {}: RTP={}, RTCP={}",
                        call_id,
                        ports.rtp_port,
                        ports.rtcp_port
                    );
                    port_pool.deallocate(ports).await;
                }
            });
        }
    }
}

/// Helper function to get codec name for logging
fn codec_name(codec: forge_codecs::AudioCodecType) -> &'static str {
    match codec {
        forge_codecs::AudioCodecType::PCMU => "G.711 µ-law",
        forge_codecs::AudioCodecType::PCMA => "G.711 A-law",
        forge_codecs::AudioCodecType::Opus => "Opus",
        forge_codecs::AudioCodecType::PCM => "PCM",
    }
}

/// Guard that returns allocated ports if construction fails before the session owns them
struct PortAllocationGuard {
    port_pool: Arc<PortPool>,
    ports: PortPair,
    active: bool,
}

impl PortAllocationGuard {
    fn new(port_pool: Arc<PortPool>, ports: PortPair) -> Self {
        Self {
            port_pool,
            ports,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for PortAllocationGuard {
    fn drop(&mut self) {
        if self.active {
            let pool = Arc::clone(&self.port_pool);
            let ports = self.ports;
            tokio::spawn(async move {
                pool.deallocate(ports).await;
                tracing::debug!(
                    "PortAllocationGuard cleaned up ports after construction failure: RTP={}, RTCP={}",
                    ports.rtp_port,
                    ports.rtcp_port
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_rtp::PortPoolConfig;
    use std::net::{SocketAddr, UdpSocket};

    #[tokio::test]
    async fn test_session_creation() {
        let config = PortPoolConfig::new(30000, 31000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session = MediaSession::new(
            call_id.clone(),
            participant_a,
            participant_b,
            &port_pool,
            MediaSessionConfig::default(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(session.call_id(), &call_id);
        assert_eq!(session.state().await, SessionState::Initializing);
        assert!(session.uptime() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_session_lifecycle() {
        let config = PortPoolConfig::new(31000, 32000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session = Arc::new(
            MediaSession::new(
                call_id,
                participant_a,
                participant_b,
                &port_pool,
                MediaSessionConfig::default(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        );

        // Start forwarding
        session.start_forwarding().await.unwrap();
        assert_eq!(session.state().await, SessionState::Active);

        // Stop forwarding
        session.stop_forwarding().await.unwrap();
        assert_eq!(session.state().await, SessionState::Terminated);
    }

    #[tokio::test]
    async fn test_session_timeout() {
        let config = PortPoolConfig::new(32000, 33000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session_config = MediaSessionConfig {
            session_timeout: Duration::from_millis(50),
            ..Default::default()
        };

        let session = MediaSession::new(
            call_id,
            participant_a,
            participant_b,
            &port_pool,
            session_config,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // Should not be timed out initially
        assert!(!session.is_timed_out().await);

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should be timed out now
        assert!(session.is_timed_out().await);
    }

    #[tokio::test]
    async fn test_ports_released_on_socket_failure() {
        let config = PortPoolConfig::new(20000, 20200).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        // Pre-bind RTP port so socket creation fails with EADDRINUSE when reuse is disabled
        let prebind_addr: SocketAddr = "0.0.0.0:20000".parse().unwrap();
        let _sock = UdpSocket::bind(prebind_addr).expect("failed to prebind test socket");

        let mut session_config = MediaSessionConfig::default();
        session_config.socket_config.reuse_address = false;

        let result = MediaSession::new(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            &port_pool,
            session_config,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(
            result.is_err(),
            "socket creation should fail when port is already bound"
        );

        // Give the guard's spawned deallocation task time to complete
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(
            port_pool.allocated_count().await,
            0,
            "ports should be returned to pool"
        );
    }
}
