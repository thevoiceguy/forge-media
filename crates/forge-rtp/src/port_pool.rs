//! Port pool management for RTP/RTCP port allocation
//!
//! This module provides thread-safe allocation of UDP port pairs for RTP/RTCP sessions.
//! Ports are allocated in pairs (even for RTP, odd for RTCP) as per RFC 3550.

use forge_core::{ForgeError, Result};
use rand::{seq::SliceRandom, Rng};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "ha")]
use forge_ha::{PortPoolStateSync, RedisHAClient};

/// Configuration for port pool
#[derive(Debug, Clone)]
pub struct PortPoolConfig {
    /// Minimum port number (must be even)
    pub min_port: u16,
    /// Maximum port number
    pub max_port: u16,
}

impl Default for PortPoolConfig {
    fn default() -> Self {
        Self {
            min_port: 10000,
            max_port: 20000,
        }
    }
}

impl PortPoolConfig {
    /// Create a new port pool configuration
    pub fn new(min_port: u16, max_port: u16) -> Result<Self> {
        // Ensure min_port is even (RTP requires even ports)
        if min_port % 2 != 0 {
            return Err(ForgeError::InvalidConfig(
                "min_port must be even for RTP/RTCP pairs".to_string(),
            ));
        }

        if min_port >= max_port {
            return Err(ForgeError::InvalidConfig(
                "min_port must be less than max_port".to_string(),
            ));
        }

        if max_port - min_port < 100 {
            return Err(ForgeError::InvalidConfig(
                "Port range must be at least 100 ports".to_string(),
            ));
        }

        Ok(Self { min_port, max_port })
    }

    /// Get the total number of available port pairs
    pub fn capacity(&self) -> usize {
        ((self.max_port - self.min_port) / 2) as usize
    }
}

/// A pair of ports for RTP and RTCP
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortPair {
    /// RTP port (always even)
    pub rtp_port: u16,
    /// RTCP port (always odd, rtp_port + 1)
    pub rtcp_port: u16,
}

impl PortPair {
    /// Create a new port pair from an RTP port
    pub fn new(rtp_port: u16) -> Result<Self> {
        if rtp_port % 2 != 0 {
            return Err(ForgeError::Rtp("RTP port must be even".to_string()));
        }

        Ok(Self {
            rtp_port,
            rtcp_port: rtp_port + 1,
        })
    }
}

/// Thread-safe port pool for allocating RTP/RTCP port pairs
#[derive(Clone)]
pub struct PortPool {
    config: PortPoolConfig,
    state: Arc<Mutex<PoolState>>,
}

#[derive(Debug)]
struct PoolState {
    available: Vec<u16>,
    allocated: HashSet<u16>,
}

impl PortPool {
    /// Create a new port pool with the given configuration.
    /// Ports are pre-shuffled to reduce predictability of allocations.
    pub fn new(config: PortPoolConfig) -> Self {
        let mut ports: Vec<u16> = (config.min_port..config.max_port)
            .filter(|p| p % 2 == 0)
            .collect();
        ports.shuffle(&mut rand::rng());

        Self {
            config,
            state: Arc::new(Mutex::new(PoolState {
                available: ports,
                allocated: HashSet::new(),
            })),
        }
    }

    /// Create a new port pool with pre-allocated ports (for HA recovery)
    ///
    /// This constructor is used during failover to restore the port pool state
    /// with previously allocated ports marked as unavailable.
    #[cfg(feature = "ha")]
    pub fn new_with_allocated(
        config: PortPoolConfig,
        allocated_ports: HashSet<u16>,
    ) -> Result<Self> {
        // Validate that all allocated ports are within range and even
        for &port in &allocated_ports {
            if port < config.min_port || port >= config.max_port {
                return Err(ForgeError::InvalidConfig(format!(
                    "Allocated port {} is outside configured range {}-{}",
                    port, config.min_port, config.max_port
                )));
            }
            if port % 2 != 0 {
                return Err(ForgeError::InvalidConfig(format!(
                    "Allocated port {} must be even (RTP ports only)",
                    port
                )));
            }
        }

        // Create list of available ports (all ports in range except allocated ones)
        let mut available: Vec<u16> = (config.min_port..config.max_port)
            .filter(|p| p % 2 == 0 && !allocated_ports.contains(p))
            .collect();
        available.shuffle(&mut rand::rng());

        tracing::info!(
            "Created port pool with {} pre-allocated ports, {} available",
            allocated_ports.len(),
            available.len()
        );

        Ok(Self {
            config,
            state: Arc::new(Mutex::new(PoolState {
                available,
                allocated: allocated_ports,
            })),
        })
    }

    /// Allocate a new port pair
    ///
    /// Returns the next available RTP/RTCP port pair, or an error if the pool is exhausted.
    pub async fn allocate(&self) -> Result<PortPair> {
        let mut state = self.state.lock().await;

        if state.available.is_empty() {
            return Err(ForgeError::ResourceLimit(
                "No available ports in pool".to_string(),
            ));
        }

        // Choose a random available port to reduce predictability
        let idx = rand::rng().random_range(0..state.available.len());
        let rtp_port = state.available.swap_remove(idx);
        state.allocated.insert(rtp_port);

        PortPair::new(rtp_port)
    }

    /// Allocate a specific port pair (if available)
    ///
    /// This is useful for scenarios where a specific port is required.
    pub async fn allocate_specific(&self, rtp_port: u16) -> Result<PortPair> {
        if rtp_port < self.config.min_port || rtp_port >= self.config.max_port - 1 {
            return Err(ForgeError::InvalidConfig(
                "Port outside of configured range".to_string(),
            ));
        }

        if rtp_port % 2 != 0 {
            return Err(ForgeError::Rtp("RTP port must be even".to_string()));
        }

        let mut state = self.state.lock().await;

        if state.allocated.contains(&rtp_port) {
            return Err(ForgeError::ResourceLimit(format!(
                "Port {} already allocated",
                rtp_port
            )));
        }

        if let Some(pos) = state.available.iter().position(|p| *p == rtp_port) {
            state.available.swap_remove(pos);
        } else {
            return Err(ForgeError::ResourceLimit(format!(
                "Port {} already allocated",
                rtp_port
            )));
        }

        state.allocated.insert(rtp_port);
        PortPair::new(rtp_port)
    }

    /// Deallocate a port pair
    ///
    /// Returns the port pair to the pool for reuse.
    pub async fn deallocate(&self, pair: PortPair) {
        let mut state = self.state.lock().await;
        if state.allocated.remove(&pair.rtp_port) {
            state.available.push(pair.rtp_port);
        }
    }

    /// Get the number of currently allocated port pairs
    pub async fn allocated_count(&self) -> usize {
        let state = self.state.lock().await;
        state.allocated.len()
    }

    /// Get the number of available port pairs
    pub async fn available_count(&self) -> usize {
        let state = self.state.lock().await;
        state.available.len()
    }

    /// Check if the pool is exhausted
    pub async fn is_exhausted(&self) -> bool {
        self.available_count().await == 0
    }

    /// Get the pool configuration
    pub fn config(&self) -> &PortPoolConfig {
        &self.config
    }

    /// Get list of currently allocated ports (for HA state sync)
    #[cfg(feature = "ha")]
    pub async fn get_allocated_ports(&self) -> Vec<u16> {
        let state = self.state.lock().await;
        state.allocated.iter().copied().collect()
    }

    /// Sync port pool state to Redis (for HA replication)
    #[cfg(feature = "ha")]
    pub async fn sync_state(
        &self,
        redis: &RedisHAClient,
        instance_id: &str,
        ttl: std::time::Duration,
    ) -> Result<()> {
        let allocated_ports = self.get_allocated_ports().await;

        PortPoolStateSync::sync(redis, instance_id, &allocated_ports, ttl)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to sync port pool state: {}", e)))?;

        tracing::debug!(
            "Synced {} allocated ports to Redis for instance {}",
            allocated_ports.len(),
            instance_id
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_pool_config() {
        let config = PortPoolConfig::new(10000, 20000).unwrap();
        assert_eq!(config.min_port, 10000);
        assert_eq!(config.max_port, 20000);
        assert_eq!(config.capacity(), 5000);
    }

    #[test]
    fn test_port_pool_config_validation() {
        // min_port must be even
        assert!(PortPoolConfig::new(10001, 20000).is_err());

        // min_port must be less than max_port
        assert!(PortPoolConfig::new(20000, 10000).is_err());

        // Range must be at least 100 ports
        assert!(PortPoolConfig::new(10000, 10050).is_err());
    }

    #[test]
    fn test_port_pair() {
        let pair = PortPair::new(10000).unwrap();
        assert_eq!(pair.rtp_port, 10000);
        assert_eq!(pair.rtcp_port, 10001);

        // RTP port must be even
        assert!(PortPair::new(10001).is_err());
    }

    #[tokio::test]
    async fn test_port_allocation() {
        let config = PortPoolConfig::new(10000, 10100).unwrap();
        let pool = PortPool::new(config);

        // Allocate first pair
        let pair1 = pool.allocate().await.unwrap();
        assert_eq!(pair1.rtcp_port, pair1.rtp_port + 1);
        assert!(pair1.rtp_port % 2 == 0);
        assert!((10000..10100).contains(&pair1.rtp_port));

        // Allocate second pair (should be different and still valid)
        let pair2 = pool.allocate().await.unwrap();
        assert_ne!(pair1.rtp_port, pair2.rtp_port);
        assert_eq!(pair2.rtcp_port, pair2.rtp_port + 1);
        assert!(pair2.rtp_port % 2 == 0);
        assert!((10000..10100).contains(&pair2.rtp_port));

        // Check counts
        assert_eq!(pool.allocated_count().await, 2);
        assert_eq!(pool.available_count().await, 48);
    }

    #[tokio::test]
    async fn test_port_deallocation() {
        let config = PortPoolConfig::new(10000, 10100).unwrap();
        let pool = PortPool::new(config);

        let pair = pool.allocate().await.unwrap();
        assert_eq!(pool.allocated_count().await, 1);
        let available_after_alloc = pool.available_count().await;

        pool.deallocate(pair).await;
        assert_eq!(pool.allocated_count().await, 0);
        assert_eq!(pool.available_count().await, available_after_alloc + 1);

        // Should be able to allocate again
        let pair2 = pool.allocate().await.unwrap();
        assert_eq!(pair2.rtcp_port, pair2.rtp_port + 1);
    }

    #[tokio::test]
    async fn test_specific_port_allocation() {
        let config = PortPoolConfig::new(10000, 10100).unwrap();
        let pool = PortPool::new(config);

        // Allocate specific port
        let pair = pool.allocate_specific(10020).await.unwrap();
        assert_eq!(pair.rtp_port, 10020);
        assert_eq!(pair.rtcp_port, 10021);

        // Try to allocate the same port again (should fail)
        assert!(pool.allocate_specific(10020).await.is_err());

        // Try to allocate odd port (should fail)
        assert!(pool.allocate_specific(10021).await.is_err());

        // Try to allocate port outside range (should fail)
        assert!(pool.allocate_specific(9000).await.is_err());
    }

    #[tokio::test]
    async fn test_pool_exhaustion() {
        let config = PortPoolConfig::new(10000, 10100).unwrap();
        let pool = PortPool::new(config);

        // Allocate all available ports (50 pairs)
        let mut pairs = vec![];
        for _ in 0..50 {
            let pair = pool.allocate().await.unwrap();
            pairs.push(pair);
        }

        assert!(pool.is_exhausted().await);

        // Next allocation should fail
        assert!(pool.allocate().await.is_err());

        // Deallocate one pair
        pool.deallocate(pairs[0]).await;

        // Should be able to allocate again
        assert!(pool.allocate().await.is_ok());
    }
}
