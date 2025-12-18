//! Heartbeat service for health monitoring and failure detection

use crate::config::RedisConfig;
use crate::redis_client::RedisHAClient;
use crate::types::{HARole, HealthState, InstanceHealth, InstanceId, PortRange};
use chrono::Utc;
use forge_core::{ForgeError, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, warn};

/// Heartbeat service that publishes instance health to Redis
pub struct HeartbeatService {
    instance_id: InstanceId,
    redis: RedisHAClient,
    config: RedisConfig,
    role: Arc<RwLock<HARole>>,
    health_state: Arc<RwLock<HealthState>>,
    port_range: PortRange,
    session_count: Arc<RwLock<usize>>,
    conference_count: Arc<RwLock<usize>>,
    start_time: std::time::Instant,
    version: String,
    ip_address: Arc<RwLock<String>>,
}

impl HeartbeatService {
    /// Create a new heartbeat service
    pub fn new(
        instance_id: InstanceId,
        redis: RedisHAClient,
        config: RedisConfig,
        role: Arc<RwLock<HARole>>,
        port_range: PortRange,
        version: String,
    ) -> Self {
        Self {
            instance_id,
            redis,
            config,
            role,
            health_state: Arc::new(RwLock::new(HealthState::Healthy)),
            port_range,
            session_count: Arc::new(RwLock::new(0)),
            conference_count: Arc::new(RwLock::new(0)),
            start_time: std::time::Instant::now(),
            version,
            ip_address: Arc::new(RwLock::new(Self::resolve_local_ip())),
        }
    }

    /// Update session count
    pub async fn update_session_count(&self, count: usize) {
        *self.session_count.write().await = count;
    }

    /// Update conference count
    pub async fn update_conference_count(&self, count: usize) {
        *self.conference_count.write().await = count;
    }

    /// Update health state
    pub async fn update_health_state(&self, state: HealthState) {
        *self.health_state.write().await = state;
    }

    /// Get current instance health
    pub async fn get_instance_health(&self) -> InstanceHealth {
        let role = *self.role.read().await;
        let state = *self.health_state.read().await;
        let session_count = *self.session_count.read().await;
        let conference_count = *self.conference_count.read().await;
        let uptime_seconds = self.start_time.elapsed().as_secs();

        InstanceHealth {
            instance_id: self.instance_id.clone(),
            role,
            state,
            ip_address: self.ip_address.read().await.clone(),
            advertised_address: None, // Could be configured separately
            port_range: self.port_range,
            last_heartbeat: Utc::now(),
            session_count,
            conference_count,
            uptime_seconds,
            version: self.version.clone(),
        }
    }

    /// Resolve local IP address once to avoid repeated external calls
    ///
    /// Uses a non-routable destination to let the OS pick the outbound interface
    /// without requiring real egress, then falls back to hostname resolution.
    fn resolve_local_ip() -> String {
        use std::net::{ToSocketAddrs, UdpSocket};

        // Use a non-routable address to avoid external dependency
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            if socket.connect("192.0.2.1:9").is_ok() {
                if let Ok(local_addr) = socket.local_addr() {
                    let ip = local_addr.ip();
                    if !ip.is_loopback() {
                        return ip.to_string();
                    }
                }
            }
        }

        // Fallback: try to get IP from hostname resolution
        if let Ok(hostname) = hostname::get() {
            if let Some(hostname_str) = hostname.to_str() {
                let addr = format!("{}:0", hostname_str);
                if let Ok(mut addrs) = addr.to_socket_addrs() {
                    if let Some(socket_addr) = addrs.next() {
                        let ip = socket_addr.ip();
                        if !ip.is_loopback() {
                            return ip.to_string();
                        }
                    }
                }
            }
        }

        // Last resort fallback
        warn!("Could not determine local IP address, using 127.0.0.1");
        "127.0.0.1".to_string()
    }

    /// Publish heartbeat to Redis
    async fn publish_heartbeat(&self) -> Result<()> {
        let health = self.get_instance_health().await;
        let key = format!("instance:{}", self.instance_id);

        debug!(
            "Publishing heartbeat: role={}, state={}, sessions={}, conferences={}",
            health.role, health.state, health.session_count, health.conference_count
        );

        // Publish with TTL (expires if heartbeat stops)
        // Use 2x interval to allow for one missed heartbeat plus network jitter
        // This enables faster failure detection (target: 30-40s total)
        let ttl = Duration::from_secs(self.config.heartbeat_interval_secs * 2);
        self.redis.set_ex(&key, &health, ttl).await?;

        Ok(())
    }

    /// Publish a heartbeat immediately (useful after promotion)
    pub async fn publish_now(&self) -> Result<()> {
        self.publish_heartbeat().await
    }

    /// Refresh the cached IP address (e.g., after VIP activation)
    pub async fn refresh_ip(&self) {
        let resolved = Self::resolve_local_ip();
        *self.ip_address.write().await = resolved;
    }

    /// Explicitly set the IP address (useful for VIPs)
    pub async fn set_ip_address(&self, ip: String) {
        *self.ip_address.write().await = ip;
    }

    /// Start the heartbeat service (spawns background task)
    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        let interval = self.config.heartbeat_interval();

        tokio::spawn(async move {
            info!(
                "Starting heartbeat service (interval: {:?})",
                interval
            );

            let mut ticker = time::interval(interval);

            loop {
                ticker.tick().await;

                if let Err(e) = self.publish_heartbeat().await {
                    error!("Failed to publish heartbeat: {}", e);
                    // Update health state to degraded on Redis failure
                    self.update_health_state(HealthState::Degraded).await;
                } else {
                    // Ensure health state is healthy if we can reach Redis
                    let current_state = *self.health_state.read().await;
                    if current_state == HealthState::Degraded {
                        self.update_health_state(HealthState::Healthy).await;
                    }
                }
            }
        })
    }
}

/// Heartbeat monitor that watches other instances and detects failures
pub struct HeartbeatMonitor {
    instance_id: InstanceId,
    redis: RedisHAClient,
    config: RedisConfig,
    primary_instance: Arc<RwLock<Option<InstanceId>>>,
}

impl HeartbeatMonitor {
    /// Create a new heartbeat monitor
    pub fn new(
        instance_id: InstanceId,
        redis: RedisHAClient,
        config: RedisConfig,
    ) -> Self {
        Self {
            instance_id,
            redis,
            config,
            primary_instance: Arc::new(RwLock::new(None)),
        }
    }

    /// Get health of a specific instance
    pub async fn get_instance_health(
        &self,
        instance_id: &InstanceId,
    ) -> Result<Option<InstanceHealth>> {
        let key = format!("instance:{}", instance_id);
        self.redis.get(&key).await
    }

    /// Get all known instances
    pub async fn get_all_instances(&self) -> Result<Vec<InstanceHealth>> {
        let keys = self.redis.scan_match("instance:*").await?;
        let mut instances = Vec::new();

        for key in keys {
            match self.redis.get::<InstanceHealth>(&key).await? {
                Some(health) => instances.push(health),
                None => {
                    warn!("Instance key {} existed but value is gone", key);
                }
            }
        }

        Ok(instances)
    }

    /// Check if primary instance is alive
    pub async fn is_primary_alive(&self) -> Result<bool> {
        let primary_id = self.primary_instance.read().await;

        if let Some(ref id) = *primary_id {
            // Check if primary's heartbeat exists and is fresh
            let key = format!("instance:{}", id);
            let exists = self.redis.exists(&key).await?;

            if exists {
                // Check TTL to ensure it's fresh
                if let Some(ttl) = self.redis.ttl(&key).await? {
                    if ttl > 0 {
                        return Ok(true);
                    }
                }
            }

            // Primary heartbeat is stale or missing
            Ok(false)
        } else {
            // No primary known
            Ok(false)
        }
    }

    /// Detect primary instance from Redis
    pub async fn detect_primary(&self) -> Result<Option<InstanceId>> {
        let instances = self.get_all_instances().await?;

        for instance in instances {
            if instance.role == HARole::Primary {
                info!("Detected primary instance: {}", instance.instance_id);
                *self.primary_instance.write().await = Some(instance.instance_id.clone());
                return Ok(Some(instance.instance_id));
            }
        }

        warn!("No primary instance detected");
        Ok(None)
    }

    /// Wait for primary failure detection
    pub async fn wait_for_primary_failure(&self) -> Result<()> {
        let check_interval = self.config.heartbeat_interval();
        let mut consecutive_failures = 0;
        // 2 consecutive failures to declare primary dead
        // With TTL=2x interval, this gives us ~30s detection time:
        // T+0: crash, T+20: TTL expires, T+20: check #1, T+30: check #2 → failure
        let required_failures = 2;

        info!("Starting primary failure detection (check interval: {:?})", check_interval);

        let mut ticker = time::interval(check_interval);

        loop {
            ticker.tick().await;

            // If we don't know who the primary is, try to detect before counting failures
            if self.primary_instance.read().await.is_none() {
                if let Err(e) = self.detect_primary().await {
                    warn!("Failed to detect primary instance: {}", e);
                }
                // After detection attempt, if still unknown, skip this tick to avoid false alarms
                if self.primary_instance.read().await.is_none() {
                    debug!("Primary instance unknown; skipping failure counting this interval");
                    continue;
                }
            }

            match self.is_primary_alive().await {
                Ok(true) => {
                    // Primary is alive, reset failure counter
                    if consecutive_failures > 0 {
                        debug!("Primary recovered, resetting failure counter");
                        consecutive_failures = 0;
                    }
                }
                Ok(false) => {
                    consecutive_failures += 1;
                    warn!(
                        "Primary heartbeat missing ({}/{} failures)",
                        consecutive_failures, required_failures
                    );

                    if consecutive_failures >= required_failures {
                        error!(
                            "Primary instance has failed! ({} consecutive failures)",
                            consecutive_failures
                        );
                        return Ok(());
                    }
                }
                Err(e) => {
                    error!("Failed to check primary health: {}", e);
                    // Don't count Redis errors as primary failures
                    // The issue might be with our connection, not the primary
                }
            }
        }
    }

    /// Start monitoring (spawns background task)
    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            info!("Starting heartbeat monitor");

            // First, detect the primary instance
            if let Err(e) = self.detect_primary().await {
                error!("Failed to detect primary instance: {}", e);
            }

            // Then wait for primary failure
            if let Err(e) = self.wait_for_primary_failure().await {
                error!("Heartbeat monitor error: {}", e);
            }

            info!("Heartbeat monitor stopped");
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RedisConfig;

    fn create_test_config() -> RedisConfig {
        RedisConfig {
            url: "redis://localhost:6379".to_string(),
            sentinels: None,
            master_name: None,
            key_prefix: "test:ha:".to_string(),
            heartbeat_interval_secs: 1,
            failover_timeout_secs: 5,
            session_ttl_secs: 3600,
            conference_ttl_secs: 7200,
        }
    }

    #[test]
    fn test_get_local_ip() {
        let ip = HeartbeatService::resolve_local_ip();
        assert!(!ip.is_empty());
    }

    #[tokio::test]
    async fn test_heartbeat_service_creation() {
        let instance_id = InstanceId::new();
        let config = create_test_config();

        // Note: This would require a Redis connection, so we skip actual Redis operations
        // In real tests, we'd use a mock or test Redis instance
    }

    #[tokio::test]
    async fn test_update_counts() {
        let instance_id = InstanceId::new();
        let config = create_test_config();
        let role = Arc::new(RwLock::new(HARole::Primary));
        let port_range = PortRange::new(30000, 35000);

        // Create service without Redis (for testing count updates only)
        // In real implementation, we'd need a Redis client
        // This demonstrates the API usage
    }
}
