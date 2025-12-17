//! Redis client for HA state management

use forge_core::{ForgeError, Result};
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, FromRedisValue, ToRedisArgs};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Redis client for HA operations
#[derive(Clone)]
pub struct RedisHAClient {
    client: redis::Client,
    conn_manager: ConnectionManager,
    key_prefix: String,
}

impl RedisHAClient {
    /// Create a new Redis HA client
    pub async fn new(redis_url: &str, key_prefix: &str) -> Result<Self> {
        info!("Connecting to Redis at {}", redis_url);

        // Create Redis client
        let client = redis::Client::open(redis_url).map_err(|e| {
            ForgeError::Internal(format!("Failed to create Redis client: {}", e))
        })?;

        // Create connection manager for automatic reconnection
        let conn_manager = ConnectionManager::new(client.clone())
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to connect to Redis: {}", e)))?;

        info!("Successfully connected to Redis");

        Ok(Self {
            client,
            conn_manager,
            key_prefix: key_prefix.to_string(),
        })
    }

    /// Build a full Redis key with prefix
    fn build_key(&self, suffix: &str) -> String {
        format!("{}{}", self.key_prefix, suffix)
    }

    /// Get a value from Redis and deserialize it
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let full_key = self.build_key(key);
        debug!("GET {}", full_key);

        let mut conn = self.conn_manager.clone();
        let value: Option<String> = conn
            .get(&full_key)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis GET failed: {}", e)))?;

        match value {
            Some(json) => {
                let deserialized = serde_json::from_str(&json).map_err(|e| {
                    ForgeError::Internal(format!("Failed to deserialize from Redis: {}", e))
                })?;
                Ok(Some(deserialized))
            }
            None => Ok(None),
        }
    }

    /// Set a value in Redis with serialization
    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let full_key = self.build_key(key);
        debug!("SET {}", full_key);

        let json = serde_json::to_string(value)
            .map_err(|e| ForgeError::Internal(format!("Failed to serialize for Redis: {}", e)))?;

        let mut conn = self.conn_manager.clone();
        conn.set::<_, _, ()>(&full_key, json)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis SET failed: {}", e)))?;

        Ok(())
    }

    /// Set a value with TTL
    pub async fn set_ex<T: Serialize>(&self, key: &str, value: &T, ttl: Duration) -> Result<()> {
        let full_key = self.build_key(key);
        debug!("SETEX {} (TTL: {:?})", full_key, ttl);

        let json = serde_json::to_string(value)
            .map_err(|e| ForgeError::Internal(format!("Failed to serialize for Redis: {}", e)))?;

        let mut conn = self.conn_manager.clone();
        conn.set_ex::<_, _, ()>(&full_key, json, ttl.as_secs())
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis SETEX failed: {}", e)))?;

        Ok(())
    }

    /// Set a value only if it doesn't exist (NX) with TTL
    pub async fn set_nx_ex(&self, key: &str, value: &str, ttl: Duration) -> Result<bool> {
        let full_key = self.build_key(key);
        debug!("SET {} NX EX {} (TTL: {:?})", full_key, value, ttl);

        let mut conn = self.conn_manager.clone();
        let result: Option<String> = redis::cmd("SET")
            .arg(&full_key)
            .arg(value)
            .arg("NX")
            .arg("EX")
            .arg(ttl.as_secs())
            .query_async(&mut conn)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis SET NX EX failed: {}", e)))?;

        Ok(result.is_some())
    }

    /// Delete a key
    pub async fn del(&self, key: &str) -> Result<bool> {
        let full_key = self.build_key(key);
        debug!("DEL {}", full_key);

        let mut conn = self.conn_manager.clone();
        let deleted: i32 = conn
            .del(&full_key)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis DEL failed: {}", e)))?;

        Ok(deleted > 0)
    }

    /// Scan for keys matching a pattern
    pub async fn scan_match(&self, pattern: &str) -> Result<Vec<String>> {
        let full_pattern = self.build_key(pattern);
        debug!("SCAN with pattern {}", full_pattern);

        let mut conn = self.conn_manager.clone();
        let mut cursor = 0;
        let mut keys = Vec::new();

        loop {
            let (new_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&full_pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(|e| ForgeError::Internal(format!("Redis SCAN failed: {}", e)))?;

            keys.extend(batch);
            cursor = new_cursor;

            if cursor == 0 {
                break;
            }
        }

        // Strip prefix from returned keys
        let prefix_len = self.key_prefix.len();
        let keys: Vec<String> = keys
            .into_iter()
            .map(|k| k[prefix_len..].to_string())
            .collect();

        debug!("SCAN found {} keys", keys.len());
        Ok(keys)
    }

    /// Get TTL for a key (in seconds)
    pub async fn ttl(&self, key: &str) -> Result<Option<i64>> {
        let full_key = self.build_key(key);
        debug!("TTL {}", full_key);

        let mut conn = self.conn_manager.clone();
        let ttl: i64 = conn
            .ttl(&full_key)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis TTL failed: {}", e)))?;

        if ttl == -2 {
            // Key does not exist
            Ok(None)
        } else if ttl == -1 {
            // Key exists but has no TTL
            Ok(Some(-1))
        } else {
            Ok(Some(ttl))
        }
    }

    /// Set TTL for an existing key
    pub async fn expire(&self, key: &str, ttl: Duration) -> Result<bool> {
        let full_key = self.build_key(key);
        debug!("EXPIRE {} {}", full_key, ttl.as_secs());

        let mut conn = self.conn_manager.clone();
        let result: bool = conn
            .expire(&full_key, ttl.as_secs() as i64)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis EXPIRE failed: {}", e)))?;

        Ok(result)
    }

    /// Check if key exists
    pub async fn exists(&self, key: &str) -> Result<bool> {
        let full_key = self.build_key(key);
        debug!("EXISTS {}", full_key);

        let mut conn = self.conn_manager.clone();
        let exists: bool = conn
            .exists(&full_key)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis EXISTS failed: {}", e)))?;

        Ok(exists)
    }

    /// Increment a counter
    pub async fn incr(&self, key: &str) -> Result<i64> {
        let full_key = self.build_key(key);
        debug!("INCR {}", full_key);

        let mut conn = self.conn_manager.clone();
        let value: i64 = conn
            .incr(&full_key, 1)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis INCR failed: {}", e)))?;

        Ok(value)
    }

    /// Get a raw string value (without deserialization)
    pub async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        let full_key = self.build_key(key);
        debug!("GET (raw) {}", full_key);

        let mut conn = self.conn_manager.clone();
        let value: Option<String> = conn
            .get(&full_key)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis GET failed: {}", e)))?;

        Ok(value)
    }

    /// Set a raw string value (without serialization)
    pub async fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        let full_key = self.build_key(key);
        debug!("SET (raw) {}", full_key);

        let mut conn = self.conn_manager.clone();
        conn.set::<_, _, ()>(&full_key, value)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis SET failed: {}", e)))?;

        Ok(())
    }

    /// Hash set field
    pub async fn hset(&self, key: &str, field: &str, value: &str) -> Result<()> {
        let full_key = self.build_key(key);
        debug!("HSET {} {} {}", full_key, field, value);

        let mut conn = self.conn_manager.clone();
        conn.hset::<_, _, _, ()>(&full_key, field, value)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis HSET failed: {}", e)))?;

        Ok(())
    }

    /// Hash get field
    pub async fn hget(&self, key: &str, field: &str) -> Result<Option<String>> {
        let full_key = self.build_key(key);
        debug!("HGET {} {}", full_key, field);

        let mut conn = self.conn_manager.clone();
        let value: Option<String> = conn
            .hget(&full_key, field)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis HGET failed: {}", e)))?;

        Ok(value)
    }

    /// Hash get all fields
    pub async fn hgetall(&self, key: &str) -> Result<std::collections::HashMap<String, String>> {
        let full_key = self.build_key(key);
        debug!("HGETALL {}", full_key);

        let mut conn = self.conn_manager.clone();
        let values: std::collections::HashMap<String, String> = conn
            .hgetall(&full_key)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis HGETALL failed: {}", e)))?;

        Ok(values)
    }

    /// Test connection to Redis
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.conn_manager.clone();
        let response: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| ForgeError::Internal(format!("Redis PING failed: {}", e)))?;

        if response == "PONG" {
            Ok(())
        } else {
            Err(ForgeError::Internal(format!(
                "Unexpected PING response: {}",
                response
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires running Redis instance
    async fn test_redis_connection() {
        let client = RedisHAClient::new("redis://localhost:6379", "test:ha:")
            .await
            .expect("Failed to connect to Redis");

        client.ping().await.expect("Ping failed");
    }

    #[tokio::test]
    #[ignore] // Requires running Redis instance
    async fn test_set_and_get() {
        let client = RedisHAClient::new("redis://localhost:6379", "test:ha:")
            .await
            .expect("Failed to connect to Redis");

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct TestData {
            name: String,
            value: i32,
        }

        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };

        client
            .set("test_key", &data)
            .await
            .expect("Set failed");

        let retrieved: Option<TestData> = client
            .get("test_key")
            .await
            .expect("Get failed");

        assert_eq!(retrieved, Some(data));

        // Clean up
        client.del("test_key").await.expect("Del failed");
    }

    #[tokio::test]
    #[ignore] // Requires running Redis instance
    async fn test_set_nx_ex() {
        let client = RedisHAClient::new("redis://localhost:6379", "test:ha:")
            .await
            .expect("Failed to connect to Redis");

        let key = "test_lock";
        let value = "instance-123";

        // First set should succeed
        let result = client
            .set_nx_ex(key, value, Duration::from_secs(10))
            .await
            .expect("Set NX EX failed");
        assert!(result);

        // Second set should fail (key exists)
        let result = client
            .set_nx_ex(key, "instance-456", Duration::from_secs(10))
            .await
            .expect("Set NX EX failed");
        assert!(!result);

        // Clean up
        client.del(key).await.expect("Del failed");
    }
}
