//! Session manager for coordinating active media sessions

use crate::session::{MediaSession, MediaSessionConfig};
use dashmap::DashMap;
use forge_core::{CallId, ParticipantId, ForgeError, Result, EventBus};
use forge_rtp::{PortPool, PortPoolConfig};
use std::sync::Arc;

/// Session manager configuration
#[derive(Debug, Clone)]
pub struct SessionManagerConfig {
    /// Port pool configuration
    pub port_pool_config: PortPoolConfig,
    /// Default session configuration
    pub session_config: MediaSessionConfig,
}

impl Default for SessionManagerConfig {
    fn default() -> Self {
        Self {
            port_pool_config: PortPoolConfig::default(),
            session_config: MediaSessionConfig::default(),
        }
    }
}

/// Manages all active media sessions
pub struct SessionManager {
    /// Active sessions indexed by call ID
    sessions: DashMap<CallId, Arc<MediaSession>>,
    /// Port pool for allocating RTP/RTCP ports
    port_pool: PortPool,
    /// Configuration
    config: SessionManagerConfig,
    /// Event bus for publishing events
    event_bus: Option<Arc<EventBus>>,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(config: SessionManagerConfig, event_bus: Option<Arc<EventBus>>) -> Self {
        let port_pool = PortPool::new(config.port_pool_config.clone());

        Self {
            sessions: DashMap::new(),
            port_pool,
            config,
            event_bus,
        }
    }

    /// Create a new media session
    pub async fn create_session(
        &self,
        call_id: CallId,
        participant_a: ParticipantId,
        participant_b: ParticipantId,
    ) -> Result<Arc<MediaSession>> {
        // Check if session already exists
        if self.sessions.contains_key(&call_id) {
            return Err(ForgeError::Internal(format!(
                "Session {} already exists",
                call_id.0
            )));
        }

        // Create session
        let session = Arc::new(
            MediaSession::new(
                call_id.clone(),
                participant_a,
                participant_b,
                &self.port_pool,
                self.config.session_config.clone(),
                self.event_bus.clone(),
            )
            .await?,
        );

        // Store session
        self.sessions.insert(call_id.clone(), Arc::clone(&session));

        tracing::info!(
            "Created session {} with {} active sessions total",
            call_id.0,
            self.sessions.len()
        );

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
    pub async fn start_session(&self, call_id: &CallId) -> Result<()> {
        let session = self
            .get_session(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        session.start_forwarding().await?;
        Ok(())
    }

    /// Stop a session and deallocate resources
    pub async fn stop_session(&self, call_id: &CallId) -> Result<()> {
        let session = self
            .sessions
            .remove(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        let session = session.1; // Extract value from (K, V) tuple

        // Stop forwarding
        session.stop_forwarding().await?;

        // Deallocate ports
        self.port_pool.deallocate(session.ports()).await;

        tracing::info!(
            "Stopped session {} with {} active sessions remaining",
            call_id.0,
            self.sessions.len()
        );

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
