//! Primary election using Redis distributed locking

use crate::redis_client::RedisHAClient;
use crate::types::{HARole, InstanceId};
use forge_core::{ForgeError, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, warn};

/// Primary election key in Redis
const PRIMARY_ELECTION_KEY: &str = "election:primary";

/// Lock TTL for primary election (15 seconds)
const PRIMARY_LOCK_TTL_SECS: u64 = 15;

/// Lock renewal interval (5 seconds - must be less than TTL)
const LOCK_RENEWAL_INTERVAL_SECS: u64 = 5;

/// Primary election manager
pub struct PrimaryElection {
    instance_id: InstanceId,
    redis: RedisHAClient,
    role: Arc<RwLock<HARole>>,
    is_primary: Arc<RwLock<bool>>,
    lock_renewal_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl PrimaryElection {
    /// Create a new primary election manager
    pub fn new(
        instance_id: InstanceId,
        redis: RedisHAClient,
        role: Arc<RwLock<HARole>>,
    ) -> Self {
        Self {
            instance_id,
            redis,
            role,
            is_primary: Arc::new(RwLock::new(false)),
            lock_renewal_handle: Arc::new(RwLock::new(None)),
        }
    }

    /// Attempt to elect this instance as primary
    pub async fn elect_primary(&self) -> Result<bool> {
        info!(
            "Attempting primary election for instance {}",
            self.instance_id
        );

        let instance_id_str = self.instance_id.to_string();
        let ttl = Duration::from_secs(PRIMARY_LOCK_TTL_SECS);

        // Try to acquire the primary lock using SET NX EX
        let acquired = self
            .redis
            .set_nx_ex(PRIMARY_ELECTION_KEY, &instance_id_str, ttl)
            .await?;

        if acquired {
            info!(
                "Successfully elected as primary: {}",
                self.instance_id
            );
            *self.is_primary.write().await = true;
            *self.role.write().await = HARole::Primary;
            // Note: Lock renewal must be started by caller (needs Arc<Self>)
            Ok(true)
        } else {
            // Check who owns the lock
            if let Some(current_primary) = self.redis.get_raw(PRIMARY_ELECTION_KEY).await? {
                info!(
                    "Primary election failed, current primary: {}",
                    current_primary
                );
            } else {
                warn!("Primary election failed, but no current primary found");
            }

            *self.is_primary.write().await = false;
            *self.role.write().await = HARole::Standby;
            Ok(false)
        }
    }

    /// Renew primary lock (called periodically by primary)
    pub async fn renew_primary_lock(&self) -> Result<()> {
        let is_primary = *self.is_primary.read().await;
        if !is_primary {
            return Err(ForgeError::Internal(
                "Cannot renew lock, not primary".to_string(),
            ));
        }

        debug!("Renewing primary lock");

        #[cfg(feature = "metrics")]
        let _timer = crate::metrics::LOCK_RENEWAL_DURATION.start_timer();

        #[cfg(feature = "metrics")]
        crate::metrics::LOCK_RENEWALS_TOTAL.inc();

        // Check if we still own the lock
        let ttl = Duration::from_secs(PRIMARY_LOCK_TTL_SECS);
        let renewed = self
            .redis
            .compare_and_expire(PRIMARY_ELECTION_KEY, &self.instance_id.to_string(), ttl)
            .await?;

        if renewed {
            debug!("Primary lock renewed successfully");
            Ok(())
        } else {
            error!(
                "Failed to renew primary lock (value no longer matches {}). Stepping down.",
                self.instance_id
            );

            #[cfg(feature = "metrics")]
            crate::metrics::LOCK_RENEWAL_FAILURES.inc();

            *self.is_primary.write().await = false;
            *self.role.write().await = HARole::Standby;
            Err(ForgeError::Internal(
                "Failed to renew primary lock".to_string(),
            ))
        }
    }

    /// Step down from primary role (voluntary)
    pub async fn step_down(&self) -> Result<()> {
        let is_primary = *self.is_primary.read().await;
        if !is_primary {
            warn!("Attempted to step down, but not primary");
            return Ok(());
        }

        info!("Stepping down from primary role");

        // Stop lock renewal task
        self.stop_lock_renewal().await;

        // Delete the primary lock
        self.redis.del(PRIMARY_ELECTION_KEY).await?;

        *self.is_primary.write().await = false;
        *self.role.write().await = HARole::Standby;

        info!("Successfully stepped down from primary");
        Ok(())
    }

    /// Stop lock renewal task (called during step down)
    async fn stop_lock_renewal(&self) {
        if let Some(handle) = self.lock_renewal_handle.write().await.take() {
            info!("Stopping lock renewal task");
            handle.abort();
        }
    }

    /// Check if this instance is primary
    pub async fn is_primary(&self) -> bool {
        *self.is_primary.read().await
    }

    /// Get current primary instance ID from Redis
    pub async fn get_current_primary(&self) -> Result<Option<InstanceId>> {
        if let Some(primary_id_str) = self.redis.get_raw(PRIMARY_ELECTION_KEY).await? {
            match InstanceId::from_string(&primary_id_str) {
                Ok(instance_id) => Ok(Some(instance_id)),
                Err(e) => {
                    error!("Invalid primary instance ID in Redis: {}", e);
                    Ok(None)
                }
            }
        } else {
            Ok(None)
        }
    }

    /// Start primary lock renewal service (for primary instances)
    async fn start_lock_renewal_if_needed(self: &Arc<Self>) {
        let mut handle = self.lock_renewal_handle.write().await;
        if handle.is_some() {
            return;
        }
        *handle = Some(self.clone().spawn_lock_renewal());
    }

    fn spawn_lock_renewal(self: Arc<Self>) -> JoinHandle<()> {
        let renewal_interval = Duration::from_secs(LOCK_RENEWAL_INTERVAL_SECS);

        tokio::spawn(async move {
            info!(
                "Starting primary lock renewal service (interval: {:?})",
                renewal_interval
            );

            let mut ticker = time::interval(renewal_interval);

            loop {
                ticker.tick().await;

                // Check if we're still primary
                if !self.is_primary().await {
                    info!("No longer primary, stopping lock renewal");
                    break;
                }

                // Attempt to renew the lock
                if let Err(e) = self.renew_primary_lock().await {
                    error!("Failed to renew primary lock: {}", e);
                    error!("Stopping lock renewal service");
                    break;
                }
            }

            // Clear the handle when task exits
            *self.lock_renewal_handle.write().await = None;
            info!("Primary lock renewal service stopped");
        })
    }

    /// Start primary election process (for standby instances)
    pub async fn run_election_on_failure(self: Arc<Self>) -> Result<()> {
        info!("Starting election process on primary failure");

        // Wait a brief moment to ensure all standbys detect the failure
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Attempt to elect self as primary
        let elected = self.elect_primary().await?;

        if elected {
            info!("Won primary election, now primary");
            self.start_lock_renewal_if_needed().await;
            Ok(())
        } else {
            info!("Lost primary election, remaining standby");
            Err(ForgeError::Internal(
                "Failed to win primary election".to_string(),
            ))
        }
    }
}

/// Election coordinator that manages the entire election process
pub struct ElectionCoordinator {
    election: Arc<PrimaryElection>,
}

impl ElectionCoordinator {
    /// Create a new election coordinator
    pub fn new(election: Arc<PrimaryElection>) -> Self {
        Self { election }
    }

    /// Initialize role (either become primary or standby)
    pub async fn initialize(&self) -> Result<HARole> {
        info!("Initializing instance role");

        // Check if there's already a primary
        match self.election.get_current_primary().await? {
            Some(current_primary) => {
                if current_primary == self.election.instance_id {
                    info!("This instance is already registered as primary");
                    *self.election.is_primary.write().await = true;
                    *self.election.role.write().await = HARole::Primary;
                    self.election.start_lock_renewal_if_needed().await;
                    Ok(HARole::Primary)
                } else {
                    info!(
                        "Another instance is primary: {}, becoming standby",
                        current_primary
                    );
                    *self.election.is_primary.write().await = false;
                    *self.election.role.write().await = HARole::Standby;
                    Ok(HARole::Standby)
                }
            }
            None => {
                info!("No primary found, attempting election");
                let elected = self.election.elect_primary().await?;
                if elected {
                    self.election.start_lock_renewal_if_needed().await;
                    Ok(HARole::Primary)
                } else {
                    Ok(HARole::Standby)
                }
            }
        }
    }

    /// Handle primary failure and trigger election
    pub async fn handle_primary_failure(&self) -> Result<()> {
        info!("Handling primary failure");

        // Attempt to become primary
        self.election
            .clone()
            .run_election_on_failure()
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RedisConfig;

    #[test]
    fn test_election_constants() {
        assert!(LOCK_RENEWAL_INTERVAL_SECS < PRIMARY_LOCK_TTL_SECS);
        assert_eq!(PRIMARY_ELECTION_KEY, "election:primary");
    }

    #[tokio::test]
    async fn test_election_creation() {
        let instance_id = InstanceId::new();
        let role = Arc::new(RwLock::new(HARole::Unknown));

        // Would need Redis client for full test
        // This demonstrates API usage
        assert!(!instance_id.to_string().is_empty());
    }

    #[tokio::test]
    async fn test_role_tracking() {
        let role = Arc::new(RwLock::new(HARole::Unknown));

        *role.write().await = HARole::Primary;
        assert_eq!(*role.read().await, HARole::Primary);

        *role.write().await = HARole::Standby;
        assert_eq!(*role.read().await, HARole::Standby);
    }
}
