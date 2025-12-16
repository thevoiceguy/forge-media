//! Redis-based persistence backend
//!
//! Stores AI session state in Redis with TTL support.

#[cfg(feature = "persistence-redis")]
use super::{PersistenceBackend, PersistedAISession};
#[cfg(feature = "persistence-redis")]
use async_trait::async_trait;
#[cfg(feature = "persistence-redis")]
use forge_core::{CallId, ForgeError, Result};
#[cfg(feature = "persistence-redis")]
use redis::aio::ConnectionManager;
#[cfg(feature = "persistence-redis")]
use redis::AsyncCommands;
#[cfg(feature = "persistence-redis")]
use std::collections::HashMap;
#[cfg(feature = "persistence-redis")]
use tracing::{debug, error, info, warn};

#[cfg(feature = "persistence-redis")]
/// Redis-based persistence backend
pub struct RedisBackend {
    client: redis::Client,
    conn_manager: ConnectionManager,
    key_prefix: String,
    ttl_seconds: u64,
}

#[cfg(feature = "persistence-redis")]
impl RedisBackend {
    /// Create a new Redis backend
    pub async fn new(redis_url: &str) -> Result<Self> {
        Self::new_with_options(redis_url, "forge:ai:session:", 86400).await // 24h TTL default
    }

    /// Create a new Redis backend with custom options
    pub async fn new_with_options(
        redis_url: &str,
        key_prefix: &str,
        ttl_seconds: u64,
    ) -> Result<Self> {
        // Create Redis client
        let client = redis::Client::open(redis_url).map_err(|e| {
            ForgeError::Internal(format!("Failed to create Redis client: {}", e))
        })?;

        // Create connection manager for automatic reconnection
        let conn_manager = ConnectionManager::new(client.clone())
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to connect to Redis: {}", e)))?;

        info!("Connected to Redis at {}", redis_url);

        Ok(Self {
            client,
            conn_manager,
            key_prefix: key_prefix.to_string(),
            ttl_seconds,
        })
    }

    /// Get Redis key for a call ID
    fn redis_key(&self, call_id: &CallId) -> String {
        format!("{}{}", self.key_prefix, call_id.0)
    }

    /// Get all keys matching the prefix
    async fn get_all_keys(&self) -> Result<Vec<String>> {
        let pattern = format!("{}*", self.key_prefix);
        let mut conn = self.conn_manager.clone();

        let keys: Vec<String> = conn.keys(pattern).await.map_err(|e| {
            ForgeError::Internal(format!("Failed to list Redis keys: {}", e))
        })?;

        Ok(keys)
    }
}

#[cfg(feature = "persistence-redis")]
#[async_trait]
impl PersistenceBackend for RedisBackend {
    async fn save(&self, session: &PersistedAISession) -> Result<()> {
        let key = self.redis_key(&session.call_id);

        // Serialize to JSON
        let json = serde_json::to_string(session).map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to serialize AI session for call {}: {}",
                session.call_id.0, e
            ))
        })?;

        // Save to Redis with TTL
        let mut conn = self.conn_manager.clone();
        conn.set_ex::<_, _, ()>(&key, json, self.ttl_seconds)
            .await
            .map_err(|e| {
                ForgeError::Internal(format!(
                    "Failed to save AI session to Redis for call {}: {}",
                    session.call_id.0, e
                ))
            })?;

        debug!(
            "Saved AI session state for call {} to Redis (TTL: {}s)",
            session.call_id.0, self.ttl_seconds
        );

        Ok(())
    }

    async fn load(&self, call_id: &CallId) -> Result<Option<PersistedAISession>> {
        let key = self.redis_key(call_id);
        let mut conn = self.conn_manager.clone();

        // Get from Redis
        let json: Option<String> = conn.get(&key).await.map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to load AI session from Redis for call {}: {}",
                call_id.0, e
            ))
        })?;

        match json {
            Some(json) => {
                let session: PersistedAISession = serde_json::from_str(&json).map_err(|e| {
                    error!(
                        "Failed to deserialize AI session for call {}: {}. Data may be corrupted.",
                        call_id.0, e
                    );
                    ForgeError::Internal(format!(
                        "Failed to deserialize AI session for call {}: {}",
                        call_id.0, e
                    ))
                })?;

                debug!("Loaded AI session state for call {} from Redis", call_id.0);
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    async fn delete(&self, call_id: &CallId) -> Result<()> {
        let key = self.redis_key(call_id);
        let mut conn = self.conn_manager.clone();

        conn.del::<_, ()>(&key).await.map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to delete AI session from Redis for call {}: {}",
                call_id.0, e
            ))
        })?;

        debug!("Deleted AI session state for call {} from Redis", call_id.0);
        Ok(())
    }

    async fn list_all(&self) -> Result<HashMap<CallId, PersistedAISession>> {
        let keys = self.get_all_keys().await?;
        let mut sessions = HashMap::new();

        let mut conn = self.conn_manager.clone();

        for key in keys {
            // Get value
            match conn.get::<_, Option<String>>(&key).await {
                Ok(Some(json)) => {
                    match serde_json::from_str::<PersistedAISession>(&json) {
                        Ok(session) => {
                            sessions.insert(session.call_id.clone(), session);
                        }
                        Err(e) => {
                            warn!(
                                "Failed to deserialize AI session from Redis key {}: {}. Skipping.",
                                key, e
                            );
                        }
                    }
                }
                Ok(None) => {
                    // Key disappeared between listing and getting
                    continue;
                }
                Err(e) => {
                    warn!("Failed to get Redis key {}: {}. Skipping.", key, e);
                }
            }
        }

        info!(
            "Loaded {} AI sessions from Redis persistence",
            sessions.len()
        );

        Ok(sessions)
    }

    async fn health_check(&self) -> Result<bool> {
        let mut conn = self.conn_manager.clone();

        // Try a simple PING command
        match redis::cmd("PING")
            .query_async::<_, String>(&mut conn)
            .await
        {
            Ok(response) if response == "PONG" => Ok(true),
            Ok(_) => {
                error!("Redis health check got unexpected response");
                Ok(false)
            }
            Err(e) => {
                error!("Redis health check failed: {}", e);
                Ok(false)
            }
        }
    }
}

// Provide a stub implementation when Redis feature is not enabled
#[cfg(not(feature = "persistence-redis"))]
use forge_core::{ForgeError, Result};

#[cfg(not(feature = "persistence-redis"))]
pub struct RedisBackend;

#[cfg(not(feature = "persistence-redis"))]
impl RedisBackend {
    pub async fn new(_redis_url: &str) -> Result<Self> {
        Err(ForgeError::Internal(
            "Redis persistence backend not available. Compile with 'persistence-redis' feature."
                .to_string(),
        ))
    }
}
