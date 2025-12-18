//! forge-ha - High Availability implementation for Forge Media Engine
//!
//! This crate provides distributed state management, failover orchestration,
//! and health monitoring for production deployments.

#![allow(dead_code, unused_variables, unused_imports)]

pub mod config;
pub mod types;
pub mod redis_client;
pub mod state_sync;
pub mod heartbeat;
pub mod election;
pub mod failover;
pub mod vip_manager;

// Re-export commonly used types
pub use config::{HAConfig, RedisConfig, CloudConfig, OnPremConfig, RoleConfig};
pub use types::{
    HARole, HAStatus, HealthState, InstanceId, InstanceHealth, PortRange, PortPair,
    SessionState, ConferenceState, DeploymentMode, CloudProvider,
    ParticipantState, ParticipantStats, CodecConfig,
    ConferenceParticipantState, AudioFormat, ConferenceRoomConfig, ConferenceSecurityConfig,
};
pub use redis_client::RedisHAClient;
pub use state_sync::{SessionStateSync, ConferenceStateSync, PortPoolStateSync, BatchUpdateCoordinator};
pub use heartbeat::{HeartbeatService, HeartbeatMonitor};
pub use election::{PrimaryElection, ElectionCoordinator};
pub use failover::{FailoverOrchestrator, FailoverState, RecoveryCallbacks, RecoveryStats};
pub use vip_manager::{VIPManager, CloudVIPManager, VRRPManager, VIPManagerFactory};

#[cfg(test)]
mod tests;
