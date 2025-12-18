//! Failover orchestration and state machine

use crate::election::{ElectionCoordinator, PrimaryElection};
use crate::heartbeat::HeartbeatMonitor;
use crate::redis_client::RedisHAClient;
use crate::state_sync::{ConferenceStateSync, SessionStateSync};
use crate::types::{ConferenceState, HARole, InstanceId, SessionState};
use chrono::Utc;
use forge_core::{ForgeError, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Failover state machine states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverState {
    /// Normal operation (no failover in progress)
    Normal,
    /// Detecting primary failure
    Detecting,
    /// Running primary election
    Electing,
    /// Promoting to primary
    Promoting,
    /// Recovering sessions and state
    Recovering,
    /// Failover complete
    Complete,
}

/// Callback trait for session and conference recovery
#[async_trait::async_trait]
pub trait RecoveryCallbacks: Send + Sync {
    /// Recover a session from state
    async fn recover_session(&self, state: SessionState) -> Result<()>;

    /// Recover a conference from state
    async fn recover_conference(&self, state: ConferenceState) -> Result<()>;

    /// Get count of active sessions
    async fn get_session_count(&self) -> usize;

    /// Get count of active conferences
    async fn get_conference_count(&self) -> usize;
}

/// Failover orchestrator that coordinates the entire failover process
pub struct FailoverOrchestrator {
    instance_id: InstanceId,
    redis: RedisHAClient,
    election_coordinator: Arc<ElectionCoordinator>,
    heartbeat_monitor: Arc<HeartbeatMonitor>,
    current_state: Arc<RwLock<FailoverState>>,
    failover_count: Arc<RwLock<u64>>,
    last_failover: Arc<RwLock<Option<chrono::DateTime<Utc>>>>,
}

impl FailoverOrchestrator {
    /// Create a new failover orchestrator
    pub fn new(
        instance_id: InstanceId,
        redis: RedisHAClient,
        election_coordinator: Arc<ElectionCoordinator>,
        heartbeat_monitor: Arc<HeartbeatMonitor>,
    ) -> Self {
        Self {
            instance_id,
            redis,
            election_coordinator,
            heartbeat_monitor,
            current_state: Arc::new(RwLock::new(FailoverState::Normal)),
            failover_count: Arc::new(RwLock::new(0)),
            last_failover: Arc::new(RwLock::new(None)),
        }
    }

    /// Get current failover state
    pub async fn get_state(&self) -> FailoverState {
        *self.current_state.read().await
    }

    /// Get failover count
    pub async fn get_failover_count(&self) -> u64 {
        *self.failover_count.read().await
    }

    /// Get last failover timestamp
    pub async fn get_last_failover(&self) -> Option<chrono::DateTime<Utc>> {
        *self.last_failover.read().await
    }

    /// Execute the full failover process
    pub async fn execute_failover<C: RecoveryCallbacks>(
        &self,
        callbacks: Arc<C>,
    ) -> Result<()> {
        info!("Starting failover process");

        // Phase 1: Detecting
        self.set_state(FailoverState::Detecting).await;
        info!("Phase 1: Detecting primary failure");
        // Wait for primary failure detection (handled by heartbeat monitor)

        // Phase 2: Electing
        self.set_state(FailoverState::Electing).await;
        info!("Phase 2: Running primary election");

        match self.election_coordinator.handle_primary_failure().await {
            Ok(()) => {
                info!("Successfully elected as new primary");
            }
            Err(e) => {
                error!("Failed to win primary election: {}", e);
                self.set_state(FailoverState::Normal).await;
                return Err(e);
            }
        }

        // Phase 3: Promoting
        self.set_state(FailoverState::Promoting).await;
        info!("Phase 3: Promoting to primary role");
        // Role has been updated by election coordinator

        // Phase 4: Recovering
        self.set_state(FailoverState::Recovering).await;
        info!("Phase 4: Recovering sessions and conferences");

        match self.recover_state(callbacks).await {
            Ok(stats) => {
                info!(
                    "State recovery complete: {} sessions, {} conferences",
                    stats.sessions_recovered, stats.conferences_recovered
                );
            }
            Err(e) => {
                error!("Failed to recover state: {}", e);
                // Continue anyway - some state is better than no state
            }
        }

        // Phase 5: Complete
        self.set_state(FailoverState::Complete).await;
        *self.failover_count.write().await += 1;
        *self.last_failover.write().await = Some(Utc::now());
        info!("Failover process complete");

        // Return to normal state
        self.set_state(FailoverState::Normal).await;

        Ok(())
    }

    /// Recover all sessions and conferences from Redis
    async fn recover_state<C: RecoveryCallbacks>(
        &self,
        callbacks: Arc<C>,
    ) -> Result<RecoveryStats> {
        info!("Beginning state recovery from Redis");

        let mut stats = RecoveryStats::default();

        // Recover sessions
        info!("Loading sessions from Redis...");
        match SessionStateSync::load_all(&self.redis).await {
            Ok(sessions) => {
                info!("Found {} sessions to recover", sessions.len());
                for session_state in sessions {
                    match callbacks.recover_session(session_state.clone()).await {
                        Ok(()) => {
                            stats.sessions_recovered += 1;
                            debug!("Recovered session: {}", session_state.call_id);
                        }
                        Err(e) => {
                            error!(
                                "Failed to recover session {}: {}",
                                session_state.call_id, e
                            );
                            stats.sessions_failed += 1;
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to load sessions from Redis: {}", e);
                return Err(e);
            }
        }

        // Recover conferences
        info!("Loading conferences from Redis...");
        match ConferenceStateSync::load_all(&self.redis).await {
            Ok(conferences) => {
                info!("Found {} conferences to recover", conferences.len());
                for conference_state in conferences {
                    match callbacks.recover_conference(conference_state.clone()).await {
                        Ok(()) => {
                            stats.conferences_recovered += 1;
                            debug!("Recovered conference: {}", conference_state.room_id);
                        }
                        Err(e) => {
                            error!(
                                "Failed to recover conference {}: {}",
                                conference_state.room_id, e
                            );
                            stats.conferences_failed += 1;
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to load conferences from Redis: {}", e);
                return Err(e);
            }
        }

        info!(
            "State recovery summary: {} sessions recovered ({} failed), {} conferences recovered ({} failed)",
            stats.sessions_recovered,
            stats.sessions_failed,
            stats.conferences_recovered,
            stats.conferences_failed
        );

        Ok(stats)
    }

    /// Set failover state
    async fn set_state(&self, state: FailoverState) {
        *self.current_state.write().await = state;
        info!("Failover state transition: {:?}", state);
    }

    /// Start the failover orchestrator (spawns background task)
    ///
    /// This continuously monitors for primary failures and orchestrates failover when detected.
    /// After a successful failover (when this standby becomes primary), it stops monitoring
    /// and switches to publishing heartbeats as the new primary.
    pub fn start<C: RecoveryCallbacks + 'static>(
        self: Arc<Self>,
        callbacks: Arc<C>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            info!("Failover orchestrator started, monitoring for failures");

            loop {
                // Make sure we know who the primary is before monitoring its health
                if let Err(e) = self.heartbeat_monitor.detect_primary().await {
                    warn!("Could not detect current primary: {}", e);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                // Wait for primary failure detection
                if let Err(e) = self.heartbeat_monitor.wait_for_primary_failure().await {
                    error!("Heartbeat monitor error: {}", e);
                    // Wait before retrying to avoid tight loop on persistent errors
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }

                info!("Primary failure detected! Initiating failover");

                // Execute failover
                match self.execute_failover(callbacks.clone()).await {
                    Ok(_) => {
                        info!("Failover completed successfully - this instance is now primary");
                        // After becoming primary, we stop monitoring for failures
                        // The HeartbeatService will publish this instance's heartbeats
                        break;
                    }
                    Err(e) => {
                        error!("Failover failed: {} - will retry on next failure detection", e);
                        // Continue monitoring - another standby may have won the election
                        // or the failure may be transient
                    }
                }
            }

            info!("Failover orchestrator stopped (now primary)");
        })
    }

    /// Manual failover (graceful transfer)
    pub async fn manual_failover<C: RecoveryCallbacks>(
        &self,
        callbacks: Arc<C>,
    ) -> Result<()> {
        info!("Initiating manual failover");

        // Check current state
        let current_state = self.get_state().await;
        if current_state != FailoverState::Normal {
            return Err(ForgeError::Internal(format!(
                "Cannot initiate manual failover, current state: {:?}",
                current_state
            )));
        }

        // Execute failover process
        self.execute_failover(callbacks).await
    }
}

/// Recovery statistics
#[derive(Debug, Default)]
pub struct RecoveryStats {
    pub sessions_recovered: usize,
    pub sessions_failed: usize,
    pub conferences_recovered: usize,
    pub conferences_failed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct MockCallbacks;

    #[async_trait::async_trait]
    impl RecoveryCallbacks for MockCallbacks {
        async fn recover_session(&self, _state: SessionState) -> Result<()> {
            Ok(())
        }

        async fn recover_conference(&self, _state: ConferenceState) -> Result<()> {
            Ok(())
        }

        async fn get_session_count(&self) -> usize {
            0
        }

        async fn get_conference_count(&self) -> usize {
            0
        }
    }

    #[test]
    fn test_failover_state_transitions() {
        assert_eq!(FailoverState::Normal, FailoverState::Normal);
        assert_ne!(FailoverState::Detecting, FailoverState::Electing);
    }

    #[test]
    fn test_recovery_stats_default() {
        let stats = RecoveryStats::default();
        assert_eq!(stats.sessions_recovered, 0);
        assert_eq!(stats.sessions_failed, 0);
        assert_eq!(stats.conferences_recovered, 0);
        assert_eq!(stats.conferences_failed, 0);
    }

    #[tokio::test]
    async fn test_mock_callbacks() {
        let callbacks = MockCallbacks;
        assert_eq!(callbacks.get_session_count().await, 0);
        assert_eq!(callbacks.get_conference_count().await, 0);
    }
}
