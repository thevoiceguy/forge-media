//! State synchronization for HA
//!
//! This module handles serialization and synchronization of session and conference
//! state to Redis for failover recovery.

use crate::redis_client::RedisHAClient;
use crate::types::{ConferenceState, SessionState};
use forge_core::Result;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Trait for types that can be synchronized to Redis
pub trait StateSync {
    /// Serialize and store state to Redis
    async fn sync_to_redis(&self, redis: &RedisHAClient) -> Result<()>;

    /// Load and deserialize state from Redis
    async fn load_from_redis(redis: &RedisHAClient, key: &str) -> Result<Option<Self>>
    where
        Self: Sized;

    /// Delete state from Redis
    async fn delete_from_redis(redis: &RedisHAClient, key: &str) -> Result<()>;
}

/// Session state synchronization
pub struct SessionStateSync;

impl SessionStateSync {
    /// Sync session state to Redis with TTL
    pub async fn sync(
        redis: &RedisHAClient,
        call_id: &str,
        state: &SessionState,
        ttl: Duration,
    ) -> Result<()> {
        let key = format!("sessions:{}", call_id);
        debug!("Syncing session {} to Redis (TTL: {:?})", call_id, ttl);

        redis.set_ex(&key, state, ttl).await?;

        debug!("Session {} synced successfully", call_id);
        Ok(())
    }

    /// Load session state from Redis
    pub async fn load(redis: &RedisHAClient, call_id: &str) -> Result<Option<SessionState>> {
        let key = format!("sessions:{}", call_id);
        debug!("Loading session {} from Redis", call_id);

        let state: Option<SessionState> = redis.get(&key).await?;

        if state.is_some() {
            debug!("Session {} loaded successfully", call_id);
        } else {
            debug!("Session {} not found in Redis", call_id);
        }

        Ok(state)
    }

    /// Load all session states from Redis
    pub async fn load_all(redis: &RedisHAClient) -> Result<Vec<SessionState>> {
        debug!("Loading all sessions from Redis");

        let keys = redis.scan_match("sessions:*").await?;
        let mut sessions = Vec::new();

        for key in keys {
            match redis.get::<SessionState>(&key).await? {
                Some(state) => sessions.push(state),
                None => {
                    warn!("Session key {} existed but value is gone", key);
                }
            }
        }

        info!("Loaded {} sessions from Redis", sessions.len());
        Ok(sessions)
    }

    /// Delete session state from Redis
    pub async fn delete(redis: &RedisHAClient, call_id: &str) -> Result<()> {
        let key = format!("sessions:{}", call_id);
        debug!("Deleting session {} from Redis", call_id);

        redis.del(&key).await?;

        debug!("Session {} deleted successfully", call_id);
        Ok(())
    }

    /// Update session statistics (batched, non-critical)
    pub async fn update_stats(
        redis: &RedisHAClient,
        call_id: &str,
        state: &SessionState,
        ttl: Duration,
    ) -> Result<()> {
        let key = format!("sessions:{}", call_id);
        debug!("Updating session {} statistics", call_id);

        // For stats updates, we just overwrite the entire state
        // In a future optimization, we could use HSET to update only stats fields
        redis.set_ex(&key, state, ttl).await?;

        Ok(())
    }

    /// Refresh TTL for a session without changing its value
    pub async fn refresh_ttl(redis: &RedisHAClient, call_id: &str, ttl: Duration) -> Result<()> {
        let key = format!("sessions:{}", call_id);
        redis.expire(&key, ttl).await?;
        Ok(())
    }
}

/// Conference state synchronization
pub struct ConferenceStateSync;

impl ConferenceStateSync {
    /// Sync conference state to Redis with TTL
    pub async fn sync(
        redis: &RedisHAClient,
        room_id: &str,
        state: &ConferenceState,
        ttl: Duration,
    ) -> Result<()> {
        let key = format!("conferences:{}", room_id);
        debug!("Syncing conference {} to Redis (TTL: {:?})", room_id, ttl);

        redis.set_ex(&key, state, ttl).await?;

        debug!("Conference {} synced successfully", room_id);
        Ok(())
    }

    /// Load conference state from Redis
    pub async fn load(redis: &RedisHAClient, room_id: &str) -> Result<Option<ConferenceState>> {
        let key = format!("conferences:{}", room_id);
        debug!("Loading conference {} from Redis", room_id);

        let state: Option<ConferenceState> = redis.get(&key).await?;

        if state.is_some() {
            debug!("Conference {} loaded successfully", room_id);
        } else {
            debug!("Conference {} not found in Redis", room_id);
        }

        Ok(state)
    }

    /// Load all conference states from Redis
    pub async fn load_all(redis: &RedisHAClient) -> Result<Vec<ConferenceState>> {
        debug!("Loading all conferences from Redis");

        let keys = redis.scan_match("conferences:*").await?;
        let mut conferences = Vec::new();

        for key in keys {
            match redis.get::<ConferenceState>(&key).await? {
                Some(state) => conferences.push(state),
                None => {
                    warn!("Conference key {} existed but value is gone", key);
                }
            }
        }

        info!("Loaded {} conferences from Redis", conferences.len());
        Ok(conferences)
    }

    /// Delete conference state from Redis
    pub async fn delete(redis: &RedisHAClient, room_id: &str) -> Result<()> {
        let key = format!("conferences:{}", room_id);
        debug!("Deleting conference {} from Redis", room_id);

        redis.del(&key).await?;

        debug!("Conference {} deleted successfully", room_id);
        Ok(())
    }

    /// Refresh TTL for a conference without changing its value
    pub async fn refresh_ttl(redis: &RedisHAClient, room_id: &str, ttl: Duration) -> Result<()> {
        let key = format!("conferences:{}", room_id);
        redis.expire(&key, ttl).await?;
        Ok(())
    }
}

/// Port pool state synchronization
pub struct PortPoolStateSync;

impl PortPoolStateSync {
    /// Sync allocated ports to Redis
    pub async fn sync(
        redis: &RedisHAClient,
        instance_id: &str,
        allocated_ports: &Vec<u16>,
        ttl: Duration,
    ) -> Result<()> {
        let key = format!("ports:{}", instance_id);
        debug!(
            "Syncing {} allocated ports for instance {} to Redis",
            allocated_ports.len(),
            instance_id
        );

        // Store as JSON array
        redis.set_ex(&key, allocated_ports, ttl).await?;

        debug!("Port allocations synced successfully");
        Ok(())
    }

    /// Load allocated ports from Redis
    pub async fn load(redis: &RedisHAClient, instance_id: &str) -> Result<Option<Vec<u16>>> {
        let key = format!("ports:{}", instance_id);
        debug!(
            "Loading allocated ports for instance {} from Redis",
            instance_id
        );

        let ports: Option<Vec<u16>> = redis.get(&key).await?;

        if let Some(ref p) = ports {
            debug!("Loaded {} allocated ports", p.len());
        } else {
            debug!("No port allocations found in Redis");
        }

        Ok(ports)
    }

    /// Delete port allocations from Redis
    pub async fn delete(redis: &RedisHAClient, instance_id: &str) -> Result<()> {
        let key = format!("ports:{}", instance_id);
        debug!(
            "Deleting port allocations for instance {} from Redis",
            instance_id
        );

        redis.del(&key).await?;

        debug!("Port allocations deleted successfully");
        Ok(())
    }
}

/// Batch update coordinator for non-critical state updates
pub struct BatchUpdateCoordinator {
    redis: RedisHAClient,
    session_ttl: Duration,
    conference_ttl: Duration,
}

impl BatchUpdateCoordinator {
    /// Create a new batch update coordinator
    pub fn new(redis: RedisHAClient, session_ttl: Duration, conference_ttl: Duration) -> Self {
        Self {
            redis,
            session_ttl,
            conference_ttl,
        }
    }

    /// Batch update session statistics
    pub async fn update_session_stats(
        &self,
        updates: Vec<(String, SessionState)>,
    ) -> Result<usize> {
        let mut success_count = 0;

        for (call_id, state) in updates {
            if let Err(e) =
                SessionStateSync::update_stats(&self.redis, &call_id, &state, self.session_ttl)
                    .await
            {
                error!("Failed to update stats for session {}: {}", call_id, e);
            } else {
                success_count += 1;
            }
        }

        debug!("Batch updated statistics for {} sessions", success_count);
        Ok(success_count)
    }

    /// Batch refresh TTLs for active sessions
    pub async fn refresh_session_ttls(&self, call_ids: Vec<String>) -> Result<usize> {
        let mut success_count = 0;

        for call_id in call_ids {
            if let Err(e) =
                SessionStateSync::refresh_ttl(&self.redis, &call_id, self.session_ttl).await
            {
                error!("Failed to refresh TTL for session {}: {}", call_id, e);
            } else {
                success_count += 1;
            }
        }

        debug!("Batch refreshed TTL for {} sessions", success_count);
        Ok(success_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AudioFormat, CodecConfig, ConferenceParticipantState, ConferenceRoomConfig,
        ConferenceSecurityConfig, ParticipantState, ParticipantStats, PortPair,
    };
    use chrono::Utc;

    fn create_test_session_state() -> SessionState {
        SessionState {
            call_id: "test-call-123".to_string(),
            state: "Active".to_string(),
            participant_a: ParticipantState {
                id: "participant-a".to_string(),
                remote_addr: Some("192.168.1.100:5060".parse().unwrap()),
                codec: CodecConfig {
                    payload_type: 0,
                    codec: "PCMU".to_string(),
                    clock_rate: 8000,
                },
                stats: ParticipantStats {
                    packets_received: 1000,
                    bytes_received: 160000,
                    packets_sent: 1000,
                    bytes_sent: 160000,
                    packets_lost: 5,
                },
            },
            participant_b: ParticipantState {
                id: "participant-b".to_string(),
                remote_addr: Some("192.168.1.101:5060".parse().unwrap()),
                codec: CodecConfig {
                    payload_type: 0,
                    codec: "PCMU".to_string(),
                    clock_rate: 8000,
                },
                stats: ParticipantStats {
                    packets_received: 1000,
                    bytes_received: 160000,
                    packets_sent: 1000,
                    bytes_sent: 160000,
                    packets_lost: 3,
                },
            },
            ports: PortPair {
                rtp_port: 30000,
                rtcp_port: 30001,
            },
            created_at: Utc::now(),
            last_activity: Utc::now(),
            sdp: Some("v=0...".to_string()),
            from_tag: Some("tag1".to_string()),
            to_tag: Some("tag2".to_string()),
            transcoder_state: None,
            xdp_active: false,
            ai_session_id: None,
            version: 1,
            instance_id: "primary-instance".to_string(),
        }
    }

    fn create_test_conference_state() -> ConferenceState {
        ConferenceState {
            room_id: "test-room-456".to_string(),
            format: AudioFormat {
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: 16,
            },
            frame_size: 480,
            participants: vec![ConferenceParticipantState {
                id: "participant-1".to_string(),
                call_id: "call-1".to_string(),
                role: "Host".to_string(),
                state: "Active".to_string(),
                gain: 1.0,
                join_time: Utc::now(),
                is_recording: false,
                packets_received: 5000,
            }],
            is_locked: false,
            recording_active: false,
            recording_path: None,
            room_config: ConferenceRoomConfig {
                security: ConferenceSecurityConfig {
                    guest_pin: Some("1234".to_string()),
                    host_pin: Some("5678".to_string()),
                    require_guest_pin: true,
                    max_pin_attempts: 3,
                    default_locked: false,
                },
                max_channels: 100,
                wait_for_moderator: false,
            },
            ai_active: false,
            version: 1,
            instance_id: "primary-instance".to_string(),
        }
    }

    #[test]
    fn test_session_state_serialization() {
        let state = create_test_session_state();
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: SessionState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.call_id, state.call_id);
        assert_eq!(deserialized.state, state.state);
        assert_eq!(deserialized.ports.rtp_port, state.ports.rtp_port);
    }

    #[test]
    fn test_conference_state_serialization() {
        let state = create_test_conference_state();
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: ConferenceState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.room_id, state.room_id);
        assert_eq!(deserialized.format.sample_rate, state.format.sample_rate);
        assert_eq!(deserialized.participants.len(), state.participants.len());
    }
}
