//! forge-ha - High Availability implementation for Forge Media Engine
//!
//! This crate provides distributed state management, failover orchestration,
//! and health monitoring for production deployments.

#![allow(dead_code, unused_variables, unused_imports)]

pub mod config;
pub mod election;
pub mod failover;
pub mod heartbeat;
pub mod metrics;
pub mod redis_client;
pub mod state_sync;
pub mod types;
pub mod vip_manager;

// Re-export commonly used types
pub use config::{CloudConfig, HAConfig, OnPremConfig, RedisConfig, RoleConfig};
pub use election::{ElectionCoordinator, PrimaryElection};
pub use failover::{FailoverOrchestrator, FailoverState, RecoveryCallbacks, RecoveryStats};
pub use heartbeat::{HeartbeatMonitor, HeartbeatService};
pub use redis_client::RedisHAClient;
pub use state_sync::{
    BatchUpdateCoordinator, ConferenceStateSync, PortPoolStateSync, SessionStateSync,
};
pub use types::{
    AudioFormat, CloudProvider, CodecConfig, ConferenceParticipantState, ConferenceRoomConfig,
    ConferenceSecurityConfig, ConferenceState, DeploymentMode, HARole, HAStatus, HealthState,
    InstanceHealth, InstanceId, ParticipantState, ParticipantStats, PortPair, PortRange,
    SessionState,
};
pub use vip_manager::{CloudVIPManager, VIPManager, VIPManagerFactory, VRRPManager};

#[cfg(test)]
mod tests;
