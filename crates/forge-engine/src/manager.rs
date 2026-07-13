//! Session manager for coordinating active media sessions

use crate::session::{MediaSession, MediaSessionConfig};
use dashmap::DashMap;
use forge_core::{CallId, EventBus, ForgeError, ParticipantId, Result};
use forge_rtp::{PortPool, PortPoolConfig};
use metrics::gauge;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[cfg(all(target_os = "linux", feature = "xdp"))]
use forge_kernel::XdpManager;

#[cfg(feature = "ha")]
use forge_ha::{ConferenceState, RecoveryCallbacks, RedisHAClient, SessionState, SessionStateSync};

/// High Availability backend for session state replication
#[cfg(feature = "ha")]
#[derive(Clone)]
pub struct HABackend {
    /// Redis client for state synchronization
    pub redis: Arc<RedisHAClient>,
    /// Instance ID for this server
    pub instance_id: String,
    /// TTL for session state in Redis
    pub session_ttl: Duration,
    /// TTL for conference state in Redis
    pub conference_ttl: Duration,
}

#[cfg(feature = "ha")]
impl HABackend {
    /// Create a new HA backend
    pub fn new(
        redis: Arc<RedisHAClient>,
        instance_id: String,
        session_ttl: Duration,
        conference_ttl: Duration,
    ) -> Self {
        Self {
            redis,
            instance_id,
            session_ttl,
            conference_ttl,
        }
    }
}

/// Session manager configuration
#[derive(Debug, Clone)]
pub struct SessionManagerConfig {
    /// Port pool configuration
    pub port_pool_config: PortPoolConfig,
    /// Default session configuration
    pub session_config: MediaSessionConfig,
    /// Interval for checking and cleaning up timed-out sessions
    pub cleanup_interval: Duration,
}

impl Default for SessionManagerConfig {
    fn default() -> Self {
        Self {
            port_pool_config: PortPoolConfig::default(),
            session_config: MediaSessionConfig::default(),
            cleanup_interval: Duration::from_secs(30), // Check every 30 seconds
        }
    }
}

/// Manages all active media sessions
pub struct SessionManager {
    /// Active sessions indexed by call ID
    sessions: DashMap<CallId, Arc<MediaSession>>,
    /// Port pool for allocating RTP/RTCP ports
    port_pool: Arc<PortPool>,
    /// Configuration
    config: SessionManagerConfig,
    /// Event bus for publishing events
    event_bus: Option<Arc<EventBus>>,
    /// Timeout monitoring task handle
    monitoring_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Shutdown flag for monitoring task
    shutdown: Arc<AtomicBool>,
    /// XDP manager for kernel-level packet forwarding (Linux only)
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    xdp_manager: Option<Arc<XdpManager>>,
    /// HA backend for state replication
    #[cfg(feature = "ha")]
    ha_backend: Option<Arc<HABackend>>,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(config: SessionManagerConfig, event_bus: Option<Arc<EventBus>>) -> Arc<Self> {
        let port_pool = Arc::new(PortPool::new(config.port_pool_config.clone()));

        let manager = Arc::new(Self {
            sessions: DashMap::new(),
            port_pool,
            config,
            event_bus,
            monitoring_task: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_manager: None,
            #[cfg(feature = "ha")]
            ha_backend: None,
        });

        manager
    }

    /// Create a new session manager with HA backend
    #[cfg(feature = "ha")]
    pub fn new_with_ha(
        config: SessionManagerConfig,
        event_bus: Option<Arc<EventBus>>,
        ha_backend: Arc<HABackend>,
    ) -> Arc<Self> {
        let port_pool = Arc::new(PortPool::new(config.port_pool_config.clone()));

        let manager = Arc::new(Self {
            sessions: DashMap::new(),
            port_pool,
            config,
            event_bus,
            monitoring_task: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_manager: None,
            ha_backend: Some(ha_backend),
        });

        manager
    }

    /// Create a new session manager with XDP support
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn new_with_xdp(
        config: SessionManagerConfig,
        xdp_config: forge_core::config::XdpConfig,
        event_bus: Option<Arc<EventBus>>,
    ) -> Arc<Self> {
        let port_pool = Arc::new(PortPool::new(config.port_pool_config.clone()));

        // Try to initialize XDP if enabled
        let xdp_manager = if xdp_config.enabled {
            tracing::info!(
                "Initializing XDP on interface {} with mode {:?}",
                xdp_config.interface,
                xdp_config.mode
            );

            // Convert config XdpMode to kernel XdpMode
            let xdp_mode = match xdp_config.mode {
                forge_core::config::XdpMode::Native => forge_kernel::XdpMode::Native,
                forge_core::config::XdpMode::Generic => forge_kernel::XdpMode::Generic,
            };

            match XdpManager::new(&xdp_config.interface, xdp_mode).await {
                Ok(manager) => {
                    tracing::info!("XDP manager initialized successfully");
                    Some(Arc::new(manager))
                }
                Err(e) => {
                    if xdp_config.fallback {
                        tracing::warn!(
                            "Failed to initialize XDP, falling back to userspace: {}",
                            e
                        );
                        None
                    } else {
                        tracing::error!("Failed to initialize XDP: {}", e);
                        None
                    }
                }
            }
        } else {
            tracing::info!("XDP disabled in configuration");
            None
        };

        let manager = Arc::new(Self {
            sessions: DashMap::new(),
            port_pool,
            config,
            event_bus,
            monitoring_task: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            xdp_manager,
            #[cfg(feature = "ha")]
            ha_backend: None,
        });

        manager
    }

    /// Create a new session manager with XDP and HA support
    #[cfg(all(target_os = "linux", feature = "xdp", feature = "ha"))]
    pub async fn new_with_xdp_and_ha(
        config: SessionManagerConfig,
        xdp_config: forge_core::config::XdpConfig,
        event_bus: Option<Arc<EventBus>>,
        ha_backend: Arc<HABackend>,
    ) -> Arc<Self> {
        let port_pool = Arc::new(PortPool::new(config.port_pool_config.clone()));

        // Try to initialize XDP if enabled
        let xdp_manager = if xdp_config.enabled {
            tracing::info!(
                "Initializing XDP on interface {} with mode {:?}",
                xdp_config.interface,
                xdp_config.mode
            );

            // Convert config XdpMode to kernel XdpMode
            let xdp_mode = match xdp_config.mode {
                forge_core::config::XdpMode::Native => forge_kernel::XdpMode::Native,
                forge_core::config::XdpMode::Generic => forge_kernel::XdpMode::Generic,
            };

            match XdpManager::new(&xdp_config.interface, xdp_mode).await {
                Ok(manager) => {
                    tracing::info!("XDP manager initialized successfully");
                    Some(Arc::new(manager))
                }
                Err(e) => {
                    if xdp_config.fallback {
                        tracing::warn!(
                            "Failed to initialize XDP, falling back to userspace: {}",
                            e
                        );
                        None
                    } else {
                        tracing::error!("Failed to initialize XDP: {}", e);
                        None
                    }
                }
            }
        } else {
            tracing::info!("XDP disabled in configuration");
            None
        };

        let manager = Arc::new(Self {
            sessions: DashMap::new(),
            port_pool,
            config,
            event_bus,
            monitoring_task: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            xdp_manager,
            ha_backend: Some(ha_backend),
        });

        manager
    }

    /// Create a new media session
    #[tracing::instrument(skip(self), fields(call_id = %call_id.0))]
    pub async fn create_session(
        &self,
        call_id: CallId,
        participant_a: ParticipantId,
        participant_b: ParticipantId,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
    ) -> Result<Arc<MediaSession>> {
        self.create_session_with_config(
            call_id,
            participant_a,
            participant_b,
            sdp,
            from_tag,
            to_tag,
            None, // Use default config
        )
        .await
    }

    /// Create a new media session with custom configuration
    #[tracing::instrument(skip(self, custom_config), fields(call_id = %call_id.0))]
    pub async fn create_session_with_config(
        &self,
        call_id: CallId,
        participant_a: ParticipantId,
        participant_b: ParticipantId,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
        custom_config: Option<MediaSessionConfig>,
    ) -> Result<Arc<MediaSession>> {
        tracing::debug!("Creating new media session");

        // Check if session already exists
        if self.sessions.contains_key(&call_id) {
            return Err(ForgeError::Internal(format!(
                "Session {} already exists",
                call_id.0
            )));
        }

        // Use custom config or default
        let session_config = custom_config.unwrap_or_else(|| self.config.session_config.clone());

        // Create session
        #[cfg(all(target_os = "linux", feature = "xdp"))]
        let session = {
            Arc::new(
                MediaSession::new_with_xdp(
                    call_id.clone(),
                    participant_a,
                    participant_b,
                    &self.port_pool,
                    session_config,
                    self.event_bus.clone(),
                    self.xdp_manager.clone(),
                    sdp,
                    from_tag,
                    to_tag,
                )
                .await?,
            )
        };

        #[cfg(not(all(target_os = "linux", feature = "xdp")))]
        let session = {
            Arc::new(
                MediaSession::new(
                    call_id.clone(),
                    participant_a,
                    participant_b,
                    &self.port_pool,
                    session_config,
                    self.event_bus.clone(),
                    sdp,
                    from_tag,
                    to_tag,
                )
                .await?,
            )
        };

        // Store session
        self.sessions.insert(call_id.clone(), Arc::clone(&session));

        // Sync to Redis if HA is enabled
        #[cfg(feature = "ha")]
        if let Some(ha) = &self.ha_backend {
            if let Err(e) = session
                .sync_to_redis(&ha.redis, &ha.instance_id, ha.session_ttl)
                .await
            {
                tracing::error!("Failed to sync session {} to Redis: {}", call_id.0, e);
                // Don't fail the session creation, but log the error
                // The session can still operate locally
            } else {
                tracing::debug!("Synced session {} to Redis for HA", call_id.0);
            }
        }

        let session_count = self.sessions.len();

        tracing::info!(
            "Created session {} with {} active sessions total",
            call_id.0,
            session_count
        );

        // Update metrics
        gauge!("forge_active_sessions").set(session_count as f64);

        Ok(session)
    }

    /// Create a new media session with specific codec configurations
    #[tracing::instrument(skip(self, codec_a, codec_b, custom_config), fields(call_id = %call_id.0))]
    pub async fn create_session_with_codecs(
        &self,
        call_id: CallId,
        participant_a: ParticipantId,
        participant_b: ParticipantId,
        codec_a: crate::session::ParticipantCodecConfig,
        codec_b: crate::session::ParticipantCodecConfig,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
        custom_config: Option<MediaSessionConfig>,
    ) -> Result<Arc<MediaSession>> {
        tracing::debug!(
            "Creating new media session with codecs: A={:?}@{}Hz, B={:?}@{}Hz",
            codec_a.codec,
            codec_a.clock_rate,
            codec_b.codec,
            codec_b.clock_rate
        );

        // Check if session already exists
        if self.sessions.contains_key(&call_id) {
            return Err(ForgeError::Internal(format!(
                "Session {} already exists",
                call_id.0
            )));
        }

        // Use custom config or default
        let session_config = custom_config.unwrap_or_else(|| self.config.session_config.clone());

        // Create session with codec configurations
        let session = Arc::new(
            MediaSession::new_with_codecs(
                call_id.clone(),
                participant_a,
                participant_b,
                codec_a,
                codec_b,
                &self.port_pool,
                session_config,
                self.event_bus.clone(),
                sdp,
                from_tag,
                to_tag,
            )
            .await?,
        );

        // Store session
        self.sessions.insert(call_id.clone(), Arc::clone(&session));

        // Sync to Redis if HA is enabled
        #[cfg(feature = "ha")]
        if let Some(ha) = &self.ha_backend {
            if let Err(e) = session
                .sync_to_redis(&ha.redis, &ha.instance_id, ha.session_ttl)
                .await
            {
                tracing::error!("Failed to sync session {} to Redis: {}", call_id.0, e);
                // Don't fail the session creation, but log the error
                // The session can still operate locally
            } else {
                tracing::debug!("Synced session {} to Redis for HA", call_id.0);
            }
        }

        let session_count = self.sessions.len();

        tracing::info!(
            "Created session {} with {} active sessions total",
            call_id.0,
            session_count
        );

        // Update metrics
        gauge!("forge_active_sessions").set(session_count as f64);

        Ok(session)
    }

    /// Get an existing session by call ID
    pub fn get_session(&self, call_id: &CallId) -> Option<Arc<MediaSession>> {
        self.sessions
            .get(call_id)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// List all active sessions
    pub fn list_sessions(&self) -> Vec<Arc<MediaSession>> {
        self.sessions
            .iter()
            .map(|entry| Arc::clone(entry.value()))
            .collect()
    }

    /// Get the runtime media configuration for a specific participant leg.
    pub async fn participant_media_state(
        &self,
        call_id: &CallId,
        leg: crate::session::ParticipantLabel,
    ) -> Result<crate::session::ParticipantMediaState> {
        let session = self
            .get_session(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        Ok(session.participant_media_state(leg).await)
    }

    /// Update the runtime media configuration for a specific participant leg.
    pub async fn update_participant_media(
        &self,
        call_id: &CallId,
        leg: crate::session::ParticipantLabel,
        update: crate::session::ParticipantMediaUpdate,
    ) -> Result<crate::session::ParticipantMediaState> {
        let session = self
            .get_session(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        session.update_participant_media(leg, update).await
    }

    /// Start forwarding for a session
    #[tracing::instrument(skip(self), fields(call_id = %call_id.0))]
    pub async fn start_session(&self, call_id: &CallId) -> Result<()> {
        tracing::debug!("Starting session forwarding");

        let session = self
            .get_session(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        session.start_forwarding().await?;
        tracing::info!("Session forwarding started successfully");
        Ok(())
    }

    /// Stop a session and deallocate resources
    #[tracing::instrument(skip(self), fields(call_id = %call_id.0))]
    pub async fn stop_session(&self, call_id: &CallId) -> Result<()> {
        tracing::debug!("Stopping session");

        let session = self
            .sessions
            .remove(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        let session = session.1; // Extract value from (K, V) tuple

        // Stop forwarding (this will also deallocate ports automatically)
        session.stop_forwarding().await?;

        let session_count = self.sessions.len();

        tracing::info!(
            "Stopped session {} with {} active sessions remaining",
            call_id.0,
            session_count
        );

        // Update metrics
        gauge!("forge_active_sessions").set(session_count as f64);

        Ok(())
    }

    /// Get the number of active sessions
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Get port pool statistics
    pub async fn port_pool_stats(&self) -> (usize, usize) {
        let allocated = self.port_pool.allocated_count().await;
        let available = self.port_pool.available_count().await;
        (allocated, available)
    }

    /// Cleanup timed-out sessions
    pub async fn cleanup_timedout_sessions(&self) -> usize {
        let mut removed = 0;

        // Find timed-out sessions
        let timedout: Vec<CallId> = {
            let mut timedout_sessions = Vec::new();
            for entry in self.sessions.iter() {
                if entry.value().is_timed_out().await {
                    timedout_sessions.push(entry.key().clone());
                }
            }
            timedout_sessions
        };

        // Remove them
        for call_id in timedout {
            if let Err(e) = self.stop_session(&call_id).await {
                tracing::error!("Failed to stop timed-out session {}: {}", call_id.0, e);
            } else {
                removed += 1;
            }
        }

        if removed > 0 {
            tracing::info!("Cleaned up {} timed-out sessions", removed);
        }

        removed
    }

    /// Start the timeout monitoring task
    pub async fn start_monitoring(self: &Arc<Self>) {
        let mut task = self.monitoring_task.lock().await;

        if task.is_some() {
            tracing::warn!("Timeout monitoring task already running");
            return;
        }

        // Reset shutdown flag in case monitoring was stopped previously
        self.shutdown.store(false, Ordering::Relaxed);

        tracing::info!(
            "Starting timeout monitoring with interval of {:?}",
            self.config.cleanup_interval
        );

        let manager = Arc::clone(self);
        let interval = self.config.cleanup_interval;
        let shutdown = Arc::clone(&self.shutdown);

        *task = Some(tokio::spawn(async move {
            loop {
                // Check shutdown flag
                if shutdown.load(Ordering::Relaxed) {
                    tracing::info!("Timeout monitoring task shutting down");
                    break;
                }

                // Sleep for the cleanup interval
                tokio::time::sleep(interval).await;

                // Check again after sleep in case shutdown was signaled
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                // Cleanup timed-out sessions
                manager.cleanup_timedout_sessions().await;
            }

            tracing::info!("Timeout monitoring task stopped");
        }));
    }

    /// Stop the timeout monitoring task
    pub async fn stop_monitoring(&self) {
        tracing::info!("Stopping timeout monitoring");

        // Signal shutdown
        self.shutdown.store(true, Ordering::Relaxed);

        // Cancel the task
        let mut task = self.monitoring_task.lock().await;
        if let Some(handle) = task.take() {
            handle.abort();
        }

        tracing::info!("Timeout monitoring stopped");
    }

    /// Recover all sessions from Redis (called during failover)
    #[cfg(feature = "ha")]
    pub async fn recover_sessions_from_redis(&self) -> Result<usize> {
        let ha = self
            .ha_backend
            .as_ref()
            .ok_or_else(|| ForgeError::Internal("HA backend not configured".to_string()))?;

        tracing::info!("Starting session recovery from Redis");

        // Load all sessions from Redis
        let states = SessionStateSync::load_all(&ha.redis).await.map_err(|e| {
            ForgeError::Internal(format!("Failed to load sessions from Redis: {}", e))
        })?;

        let mut recovered = 0;
        let mut failed = 0;

        for state in states {
            let call_id = CallId(state.call_id.clone());

            // Skip if session already exists (shouldn't happen, but be defensive)
            if self.sessions.contains_key(&call_id) {
                tracing::warn!(
                    "Session {} already exists during recovery, skipping",
                    call_id.0
                );
                continue;
            }

            // Recover the session from state
            match MediaSession::from_state(
                state,
                &self.port_pool,
                self.config.session_config.clone(),
                self.event_bus.clone(),
            )
            .await
            {
                Ok(session) => {
                    let session = Arc::new(session);

                    // Start forwarding immediately
                    if let Err(e) = session.start_forwarding().await {
                        tracing::error!(
                            "Failed to start forwarding for recovered session {}: {}",
                            call_id.0,
                            e
                        );
                        failed += 1;
                        continue;
                    }

                    // Store the recovered session
                    self.sessions.insert(call_id.clone(), session);
                    recovered += 1;

                    tracing::info!("Successfully recovered session {}", call_id.0);
                }
                Err(e) => {
                    tracing::error!("Failed to recover session {}: {}", call_id.0, e);
                    failed += 1;
                }
            }
        }

        let session_count = self.sessions.len();
        tracing::info!(
            "Session recovery complete: {} recovered, {} failed, {} total active",
            recovered,
            failed,
            session_count
        );

        // Update metrics
        gauge!("forge_active_sessions").set(session_count as f64);

        Ok(recovered)
    }

    /// Recover a single session from state (used by RecoveryCallbacks)
    #[cfg(feature = "ha")]
    async fn recover_session_from_state(&self, state: SessionState) -> Result<()> {
        let call_id = CallId(state.call_id.clone());

        // Skip if session already exists
        if self.sessions.contains_key(&call_id) {
            tracing::debug!("Session {} already exists, skipping recovery", call_id.0);
            return Ok(());
        }

        // Recover the session
        let session = MediaSession::from_state(
            state,
            &self.port_pool,
            self.config.session_config.clone(),
            self.event_bus.clone(),
        )
        .await?;

        let session = Arc::new(session);

        // Start forwarding
        session.start_forwarding().await?;

        // Store the session
        self.sessions.insert(call_id.clone(), session);

        tracing::info!("Recovered session {}", call_id.0);

        Ok(())
    }
}

/// Implement RecoveryCallbacks trait for SessionManager
#[cfg(feature = "ha")]
#[async_trait::async_trait]
impl RecoveryCallbacks for SessionManager {
    async fn recover_session(&self, state: SessionState) -> Result<()> {
        self.recover_session_from_state(state).await
    }

    async fn recover_conference(&self, state: ConferenceState) -> Result<()> {
        // TODO: Conference recovery will be implemented when ConferenceManager is integrated
        tracing::warn!("Conference recovery not yet implemented: {}", state.room_id);
        Ok(())
    }

    async fn get_session_count(&self) -> usize {
        self.sessions.len()
    }

    async fn get_conference_count(&self) -> usize {
        // TODO: Return actual conference count when ConferenceManager is integrated
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_manager_create() {
        let config = SessionManagerConfig {
            port_pool_config: PortPoolConfig::new(50000, 51000).unwrap(),
            ..Default::default()
        };

        let manager = SessionManager::new(config, None);

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session = manager
            .create_session(
                call_id.clone(),
                participant_a,
                participant_b,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(session.call_id(), &call_id);
        assert_eq!(manager.session_count(), 1);

        // Verify we can retrieve it
        let retrieved = manager.get_session(&call_id).unwrap();
        assert_eq!(retrieved.call_id(), &call_id);
    }

    #[tokio::test]
    async fn test_session_manager_lifecycle() {
        let config = SessionManagerConfig {
            port_pool_config: PortPoolConfig::new(51000, 52000).unwrap(),
            ..Default::default()
        };

        let manager = SessionManager::new(config, None);

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        // Create session
        manager
            .create_session(
                call_id.clone(),
                participant_a,
                participant_b,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(manager.session_count(), 1);

        // Start session
        manager.start_session(&call_id).await.unwrap();

        // Stop session
        manager.stop_session(&call_id).await.unwrap();
        assert_eq!(manager.session_count(), 0);
    }

    #[tokio::test]
    async fn test_session_manager_cleanup_timeout() {
        let config = SessionManagerConfig {
            port_pool_config: PortPoolConfig::new(52000, 53000).unwrap(),
            session_config: MediaSessionConfig {
                session_timeout: std::time::Duration::from_millis(50),
                ..Default::default()
            },
            cleanup_interval: std::time::Duration::from_millis(10),
        };

        let manager = SessionManager::new(config, None);

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        // Create session with short timeout
        manager
            .create_session(
                call_id.clone(),
                participant_a,
                participant_b,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(manager.session_count(), 1);

        // Wait for timeout
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Cleanup
        let removed = manager.cleanup_timedout_sessions().await;
        assert_eq!(removed, 1);
        assert_eq!(manager.session_count(), 0);
    }

    #[tokio::test]
    async fn test_monitoring_restart_after_stop() {
        let config = SessionManagerConfig {
            port_pool_config: PortPoolConfig::new(53000, 54000).unwrap(),
            session_config: MediaSessionConfig {
                session_timeout: std::time::Duration::from_millis(30),
                ..Default::default()
            },
            cleanup_interval: std::time::Duration::from_millis(10),
        };

        let manager = SessionManager::new(config, None);

        // First run
        manager.start_monitoring().await;
        let call_id = CallId::generate();
        manager
            .create_session(
                call_id.clone(),
                ParticipantId::generate(),
                ParticipantId::generate(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(manager.session_count(), 0, "session should be cleaned up");
        manager.stop_monitoring().await;

        // Second run after stop should still work
        manager.start_monitoring().await;
        let call_id2 = CallId::generate();
        manager
            .create_session(
                call_id2.clone(),
                ParticipantId::generate(),
                ParticipantId::generate(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(
            manager.session_count(),
            0,
            "session should be cleaned up after restart"
        );
        manager.stop_monitoring().await;
    }
}
