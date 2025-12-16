//! AI session persistence and recovery
//!
//! This module provides state persistence for AI sessions to enable:
//! - Survival of connection drops
//! - Server restart recovery
//! - Automatic reconnection with exponential backoff
//! - Session state tracking

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use forge_core::{CallId, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod disk;
pub mod redis;

// Re-export backend implementations
pub use disk::DiskBackend;
#[cfg(feature = "persistence-redis")]
pub use redis::RedisBackend;

/// AI session persistent state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedAISession {
    /// Call ID
    pub call_id: CallId,
    /// AI configuration
    pub config: crate::ai_integration::AISessionConfig,
    /// Connection state
    pub connection_state: ConnectionState,
    /// When the session was created
    pub created_at: DateTime<Utc>,
    /// Last successful connection time
    pub last_connected: Option<DateTime<Utc>>,
    /// Number of reconnection attempts
    pub reconnect_attempts: u32,
    /// Maximum reconnection attempts before giving up
    pub max_reconnect_attempts: u32,
    /// Conversation context (for future use)
    pub conversation_context: Option<String>,
}

/// Connection state for AI sessions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    /// Session is connected and active
    Connected,
    /// Session is disconnected, ready for reconnection
    Disconnected,
    /// Session is actively reconnecting
    Reconnecting,
    /// Session has failed after max retries
    Failed,
    /// Session has been terminated (won't reconnect)
    Terminated,
}

impl PersistedAISession {
    /// Create a new persisted session
    pub fn new(call_id: CallId, config: crate::ai_integration::AISessionConfig) -> Self {
        Self {
            call_id,
            config,
            connection_state: ConnectionState::Disconnected,
            created_at: Utc::now(),
            last_connected: None,
            reconnect_attempts: 0,
            max_reconnect_attempts: 10, // Default: 10 attempts
            conversation_context: None,
        }
    }

    /// Calculate backoff duration for reconnection attempts
    ///
    /// Uses exponential backoff: 1s, 2s, 4s, 8s, 16s, 32s, 60s (max)
    pub fn backoff_duration(&self) -> std::time::Duration {
        let base_ms = 1000u64; // 1 second
        let max_ms = 60000u64; // 60 seconds max

        let exponential = base_ms * 2u64.pow(self.reconnect_attempts.min(6));
        let duration_ms = exponential.min(max_ms);

        std::time::Duration::from_millis(duration_ms)
    }

    /// Check if session should attempt reconnection
    pub fn should_reconnect(&self) -> bool {
        matches!(
            self.connection_state,
            ConnectionState::Disconnected | ConnectionState::Reconnecting
        ) && self.reconnect_attempts < self.max_reconnect_attempts
    }

    /// Mark session as connected
    pub fn mark_connected(&mut self) {
        self.connection_state = ConnectionState::Connected;
        self.last_connected = Some(Utc::now());
        self.reconnect_attempts = 0; // Reset on successful connection
    }

    /// Mark session as disconnected
    pub fn mark_disconnected(&mut self) {
        self.connection_state = ConnectionState::Disconnected;
    }

    /// Increment reconnection attempt
    pub fn increment_reconnect_attempt(&mut self) {
        self.reconnect_attempts += 1;
        self.connection_state = ConnectionState::Reconnecting;

        // Mark as failed if exceeded max attempts
        if self.reconnect_attempts >= self.max_reconnect_attempts {
            self.connection_state = ConnectionState::Failed;
        }
    }

    /// Mark session as terminated (won't reconnect)
    pub fn mark_terminated(&mut self) {
        self.connection_state = ConnectionState::Terminated;
    }
}

/// Trait for AI session persistence backends
#[async_trait]
pub trait PersistenceBackend: Send + Sync {
    /// Save a session state
    async fn save(&self, session: &PersistedAISession) -> Result<()>;

    /// Load a session state by call ID
    async fn load(&self, call_id: &CallId) -> Result<Option<PersistedAISession>>;

    /// Delete a session state
    async fn delete(&self, call_id: &CallId) -> Result<()>;

    /// List all persisted sessions
    async fn list_all(&self) -> Result<HashMap<CallId, PersistedAISession>>;

    /// Check if backend is healthy
    async fn health_check(&self) -> Result<bool>;
}

/// Persistence configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceConfig {
    /// Backend type (disk or redis)
    pub backend: PersistenceBackendType,
    /// Base directory for disk backend
    pub disk_base_dir: std::path::PathBuf,
    /// Redis connection URL (if using Redis)
    pub redis_url: Option<String>,
    /// Enable persistence (can be disabled for testing)
    pub enabled: bool,
    /// Health check interval in seconds
    pub health_check_interval_secs: u64,
    /// Maximum reconnection attempts
    pub max_reconnect_attempts: u32,
}

/// Persistence backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersistenceBackendType {
    /// Disk-based JSON persistence
    Disk,
    /// Redis-based persistence
    Redis,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            backend: PersistenceBackendType::Disk,
            disk_base_dir: std::path::PathBuf::from("/var/lib/forge/ai-sessions"),
            redis_url: None,
            enabled: true,
            health_check_interval_secs: 30,
            max_reconnect_attempts: 10,
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Connected => write!(f, "Connected"),
            ConnectionState::Disconnected => write!(f, "Disconnected"),
            ConnectionState::Reconnecting => write!(f, "Reconnecting"),
            ConnectionState::Failed => write!(f, "Failed"),
            ConnectionState::Terminated => write!(f, "Terminated"),
        }
    }
}
