//! Session manager for coordinating active media sessions

use crate::session::{MediaSession, MediaSessionConfig};
use dashmap::DashMap;
use forge_core::{CallId, ParticipantId, ForgeError, Result, EventBus};
use forge_rtp::{PortPool, PortPoolConfig};
use metrics::gauge;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[cfg(all(target_os = "linux", feature = "xdp"))]
use forge_kernel::XdpManager;

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
                        tracing::warn!("Failed to initialize XDP, falling back to userspace: {}", e);
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
    ) -> Result<Arc<MediaSession>> {
        tracing::debug!("Creating new media session");

        // Check if session already exists
        if self.sessions.contains_key(&call_id) {
            return Err(ForgeError::Internal(format!(
                "Session {} already exists",
                call_id.0
            )));
        }

        // Create session
        #[cfg(all(target_os = "linux", feature = "xdp"))]
        let session = {
            Arc::new(
                MediaSession::new_with_xdp(
                    call_id.clone(),
                    participant_a,
                    participant_b,
                    &self.port_pool,
                    self.config.session_config.clone(),
                    self.event_bus.clone(),
                    self.xdp_manager.clone(),
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
                    self.config.session_config.clone(),
                    self.event_bus.clone(),
                )
                .await?,
            )
        };

        // Store session
        self.sessions.insert(call_id.clone(), Arc::clone(&session));

        let session_count = self.sessions.len();

        tracing::info!(
            "Created session {} with {} active sessions total",
            call_id.0,
            session_count
        );

        // Update metrics
        gauge!("forge_active_sessions", session_count as f64);

        Ok(session)
    }

    /// Get an existing session by call ID
    pub fn get_session(&self, call_id: &CallId) -> Option<Arc<MediaSession>> {
        self.sessions.get(call_id).map(|entry| Arc::clone(entry.value()))
    }

    /// List all active sessions
    pub fn list_sessions(&self) -> Vec<Arc<MediaSession>> {
        self.sessions
            .iter()
            .map(|entry| Arc::clone(entry.value()))
            .collect()
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
        gauge!("forge_active_sessions", session_count as f64);

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
            .create_session(call_id.clone(), participant_a, participant_b)
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
            .create_session(call_id.clone(), participant_a, participant_b)
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
            .create_session(call_id.clone(), participant_a, participant_b)
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
}
