//! Media session management for two-party calls

use forge_core::{CallId, ParticipantId, ForgeError, Result, ForgeEvent, EventBus};
use forge_rtp::{PortPool, PortPair, RtpSocketPair, RtpSocketConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

/// Configuration for a media session
#[derive(Debug, Clone)]
pub struct MediaSessionConfig {
    /// RTP socket configuration
    pub socket_config: RtpSocketConfig,
    /// Session timeout (idle duration before auto-termination)
    pub session_timeout: Duration,
}

impl Default for MediaSessionConfig {
    fn default() -> Self {
        Self {
            socket_config: RtpSocketConfig::default(),
            session_timeout: Duration::from_secs(300), // 5 minutes
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
    /// Session creation time
    created_at: Instant,
    /// Last activity time
    last_activity: Arc<RwLock<Instant>>,
    /// Configuration
    config: MediaSessionConfig,
    /// Event bus for publishing events
    event_bus: Option<Arc<EventBus>>,
    /// Forwarding task handles
    forwarding_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MediaSession {
    /// Create a new media session
    pub async fn new(
        call_id: CallId,
        participant_a_id: ParticipantId,
        participant_b_id: ParticipantId,
        port_pool: &PortPool,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
    ) -> Result<Self> {
        // Allocate ports
        let ports = port_pool.allocate().await?;
        tracing::info!(
            "Allocated ports for session {}: RTP={}, RTCP={}",
            call_id.0,
            ports.rtp_port,
            ports.rtcp_port
        );

        // Create socket pair
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;

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
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
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
        let forwarding_handle = crate::forwarding::ForwardingEngine::start_forwarding(Arc::clone(self)).await?;
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

        // Cancel all forwarding tasks
        let mut tasks = self.forwarding_tasks.lock().await;
        for task in tasks.drain(..) {
            task.abort();
        }

        *self.state.write().await = SessionState::Terminated;

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

    /// Update last activity timestamp
    async fn update_activity(&self) {
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
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        tracing::debug!("MediaSession {} dropped", self.call_id.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_rtp::PortPoolConfig;

    #[tokio::test]
    async fn test_session_creation() {
        let config = PortPoolConfig::new(30000, 31000).unwrap();
        let port_pool = PortPool::new(config);

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
        let port_pool = PortPool::new(config);

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
        let port_pool = PortPool::new(config);

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
}
