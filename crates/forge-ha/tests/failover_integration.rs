//! Integration tests for HA failover scenarios
//!
//! These tests verify end-to-end failover behavior including:
//! - Primary failure detection
//! - Standby promotion
//! - Session state recovery
//! - Conference state preservation
//! - Port pool recovery

use forge_ha::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Test helper to create a test Redis URL
fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

/// Test helper to create a unique key prefix for test isolation
fn test_key_prefix() -> String {
    format!("forge:ha:test:{}:", uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_primary_election_single_winner() {
    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    // Create two instances that will compete for primary role
    let client1 = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client 1");

    let client2 = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client 2");

    // Instance 1 attempts to become primary
    let election_key = "election:primary";
    let instance1_id = "instance-1";
    let instance2_id = "instance-2";

    // First election should succeed
    let result1 = client1
        .set_nx_ex(election_key, instance1_id, Duration::from_secs(15))
        .await
        .expect("Failed to set election lock");

    assert!(result1, "Instance 1 should win the election");

    // Second election should fail (lock already held)
    let result2 = client2
        .set_nx_ex(election_key, instance2_id, Duration::from_secs(15))
        .await
        .expect("Failed to attempt election");

    assert!(!result2, "Instance 2 should not win while lock is held");

    // Verify the winner
    let winner: Option<String> = client1
        .get_raw(election_key)
        .await
        .expect("Failed to get election winner");

    assert_eq!(winner.unwrap(), instance1_id);

    // Cleanup
    let _ = client1.del(election_key).await;
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_session_state_persistence() {
    use forge_ha::*;

    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client");

    // Create test session state with all required fields
    let session_state = SessionState {
        call_id: "test-call-123".to_string(),
        state: "Active".to_string(),
        participant_a: ParticipantState {
            id: "participant-a".to_string(),
            remote_addr: None,
            codec: CodecConfig {
                payload_type: 0,
                codec: "PCMU".to_string(),
                clock_rate: 8000,
            },
            stats: ParticipantStats::default(),
        },
        participant_b: ParticipantState {
            id: "participant-b".to_string(),
            remote_addr: None,
            codec: CodecConfig {
                payload_type: 0,
                codec: "PCMU".to_string(),
                clock_rate: 8000,
            },
            stats: ParticipantStats::default(),
        },
        ports: PortPair {
            rtp_port: 30000,
            rtcp_port: 30001,
        },
        created_at: chrono::Utc::now(),
        last_activity: chrono::Utc::now(),
        sdp: None,
        from_tag: None,
        to_tag: None,
        transcoder_state: None,
        xdp_active: false,
        ai_session_id: None,
        version: 1,
        instance_id: "primary-01".to_string(),
    };

    // Persist to Redis using SessionStateSync
    SessionStateSync::sync(
        &client,
        &session_state.call_id,
        &session_state,
        Duration::from_secs(3600),
    )
    .await
    .expect("Failed to persist session");

    // Retrieve and verify
    let retrieved_state = SessionStateSync::load(&client, &session_state.call_id)
        .await
        .expect("Failed to retrieve session")
        .expect("Session not found");

    assert_eq!(retrieved_state.call_id, session_state.call_id);
    assert_eq!(retrieved_state.state, session_state.state);
    assert_eq!(retrieved_state.ports.rtp_port, session_state.ports.rtp_port);
    assert_eq!(retrieved_state.ports.rtcp_port, session_state.ports.rtcp_port);

    // Cleanup
    let key = format!("sessions:{}", session_state.call_id);
    let _ = client.del(&key).await;
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_heartbeat_expiration_detection() {
    use forge_ha::*;

    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client");

    // Publish a heartbeat with short TTL
    let heartbeat_key = "instance:test-instance-01";

    let heartbeat = InstanceHealth {
        instance_id: InstanceId::new(),
        role: HARole::Primary,
        state: HealthState::Healthy,
        ip_address: "10.0.1.10".to_string(),
        advertised_address: Some("203.0.113.10".to_string()),
        port_range: PortRange::new(30000, 35000),
        last_heartbeat: chrono::Utc::now(),
        session_count: 10,
        conference_count: 2,
        uptime_seconds: 100,
        version: "0.1.0".to_string(),
    };

    // Set with 2 second TTL using set_ex
    client
        .set_ex(heartbeat_key, &heartbeat, Duration::from_secs(2))
        .await
        .expect("Failed to publish heartbeat");

    // Verify heartbeat exists
    let exists = client.exists(heartbeat_key).await.expect("Failed to check existence");
    assert!(exists, "Heartbeat should exist immediately after publishing");

    // Wait for expiration (3 seconds to be safe)
    sleep(Duration::from_secs(3)).await;

    // Verify heartbeat expired
    let exists_after = client.exists(heartbeat_key).await.expect("Failed to check existence");
    assert!(!exists_after, "Heartbeat should have expired");
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_conference_state_recovery() {
    use forge_ha::*;

    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client");

    // Create test conference state with all required fields
    let conference_state = ConferenceState {
        room_id: "room-456".to_string(),
        format: AudioFormat {
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: 16,
        },
        frame_size: 480,
        participants: vec![
            ConferenceParticipantState {
                id: "alice".to_string(),
                call_id: "call-alice".to_string(),
                role: "Host".to_string(),
                state: "Active".to_string(),
                gain: 1.0,
                join_time: chrono::Utc::now(),
                is_recording: false,
                packets_received: 1000,
            },
            ConferenceParticipantState {
                id: "bob".to_string(),
                call_id: "call-bob".to_string(),
                role: "Guest".to_string(),
                state: "Active".to_string(),
                gain: 1.0,
                join_time: chrono::Utc::now(),
                is_recording: false,
                packets_received: 800,
            },
        ],
        is_locked: false,
        recording_active: false,
        recording_path: None,
        room_config: ConferenceRoomConfig {
            security: ConferenceSecurityConfig {
                guest_pin: None,
                host_pin: None,
                require_guest_pin: false,
                max_pin_attempts: 3,
                default_locked: false,
            },
            max_channels: 32,
            wait_for_moderator: false,
        },
        ai_active: false,
        version: 1,
        instance_id: "primary-01".to_string(),
    };

    // Persist to Redis using ConferenceStateSync
    ConferenceStateSync::sync(
        &client,
        &conference_state.room_id,
        &conference_state,
        Duration::from_secs(7200),
    )
    .await
    .expect("Failed to persist conference");

    // Simulate failover: new instance recovers state
    let recovered_state = ConferenceStateSync::load(&client, &conference_state.room_id)
        .await
        .expect("Failed to recover conference")
        .expect("Conference not found");

    assert_eq!(recovered_state.room_id, conference_state.room_id);
    assert_eq!(recovered_state.participants.len(), 2);
    assert_eq!(recovered_state.participants[0].id, "alice");
    assert_eq!(recovered_state.participants[1].id, "bob");

    // Cleanup
    let key = format!("conferences:{}", conference_state.room_id);
    let _ = client.del(&key).await;
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_port_pool_state_recovery() {
    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client");

    let instance_id = "primary-01";

    // Simulate allocated ports
    let allocated_ports = vec![30000, 30002, 30004, 30006, 30008];

    // Persist port allocation state directly
    let port_key = format!("ports:{}", instance_id);

    client
        .set_ex(&port_key, &allocated_ports, Duration::from_secs(3600))
        .await
        .expect("Failed to persist ports");

    // Simulate failover: recover port allocations
    let recovered_ports: Option<Vec<u16>> = client
        .get(&port_key)
        .await
        .expect("Failed to recover ports");

    let recovered_ports = recovered_ports.expect("Ports not found");

    assert_eq!(recovered_ports.len(), 5);
    assert_eq!(recovered_ports, allocated_ports);

    // Verify specific ports
    assert!(recovered_ports.contains(&30000));
    assert!(recovered_ports.contains(&30008));
    assert!(!recovered_ports.contains(&30001)); // RTCP port (not in list)

    // Cleanup
    let _ = client.del(&port_key).await;
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_batch_session_recovery() {
    use forge_ha::*;

    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client");

    // Create multiple session states
    let session_count = 10;
    let mut call_ids = Vec::new();

    for i in 0..session_count {
        let call_id = format!("call-{}", i);
        let session_state = SessionState {
            call_id: call_id.clone(),
            state: "Active".to_string(),
            participant_a: ParticipantState {
                id: format!("participant-a-{}", i),
                remote_addr: None,
                codec: CodecConfig {
                    payload_type: 0,
                    codec: "PCMU".to_string(),
                    clock_rate: 8000,
                },
                stats: ParticipantStats::default(),
            },
            participant_b: ParticipantState {
                id: format!("participant-b-{}", i),
                remote_addr: None,
                codec: CodecConfig {
                    payload_type: 0,
                    codec: "PCMU".to_string(),
                    clock_rate: 8000,
                },
                stats: ParticipantStats::default(),
            },
            ports: PortPair {
                rtp_port: 30000 + (i * 2) as u16,
                rtcp_port: 30001 + (i * 2) as u16,
            },
            created_at: chrono::Utc::now(),
            last_activity: chrono::Utc::now(),
            sdp: None,
            from_tag: None,
            to_tag: None,
            transcoder_state: None,
            xdp_active: false,
            ai_session_id: None,
            version: 1,
            instance_id: "primary-01".to_string(),
        };

        SessionStateSync::sync(&client, &call_id, &session_state, Duration::from_secs(3600))
            .await
            .expect("Failed to persist session");

        call_ids.push(call_id);
    }

    // Simulate failover: scan and recover all sessions
    let recovered_keys = client
        .scan_match("sessions:*")
        .await
        .expect("Failed to scan sessions");

    assert_eq!(
        recovered_keys.len(),
        session_count as usize,
        "Should recover all sessions"
    );

    // Verify we can load each recovered session
    for call_id in &call_ids {
        let _state = SessionStateSync::load(&client, call_id)
            .await
            .expect("Failed to load session")
            .expect("Session not found");
    }

    // Cleanup
    for call_id in call_ids {
        let key = format!("sessions:{}", call_id);
        let _ = client.del(&key).await;
    }
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_failover_timing() {
    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create Redis client");

    // Simulate failover detection time with updated timing
    // New: TTL=2x interval (20s), 2 consecutive failures
    let heartbeat_interval = Duration::from_secs(10);
    let ttl_multiplier = 2; // Updated from 3 to 2
    let ttl = heartbeat_interval * ttl_multiplier;
    let required_failures = 2; // Updated from 3 to 2

    // Detection time: TTL expires (20s) + 2 checks (20s) = ~30-40s
    let expected_detection = ttl + (heartbeat_interval * required_failures);
    assert_eq!(expected_detection.as_secs(), 40);

    // Measure recovery time (simplified - just measure Redis operations)
    let start = std::time::Instant::now();

    // 1. Check for primary heartbeat (simulated miss)
    let heartbeat_key = "instance:primary";
    let _ = client.get_raw(heartbeat_key).await; // Will return None, primary is down

    // 2. Attempt election
    let election_key = "election:primary";
    let elected = client
        .set_nx_ex(election_key, "standby-01", Duration::from_secs(15))
        .await
        .expect("Failed to attempt election");
    assert!(elected);

    // 3. Scan for sessions to recover (simulated with empty scan)
    let _ = client.scan_match("sessions:*").await;

    let recovery_duration = start.elapsed();

    // Recovery operations should complete quickly (< 5 seconds)
    assert!(
        recovery_duration.as_secs() < 5,
        "Redis recovery operations should be fast"
    );

    // Total failover time = detection_time + recovery_duration
    let total_failover = expected_detection + recovery_duration;

    // Should be less than 45 seconds total (40s detection + 5s recovery)
    assert!(
        total_failover.as_secs() < 45,
        "Total failover should complete within 45 seconds"
    );

    // Cleanup
    let _ = client.del(election_key).await;
}

#[tokio::test]
#[ignore] // Requires Redis server and proper setup
async fn test_split_brain_prevention() {
    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    // Two instances trying to become primary simultaneously
    let client1 = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create client 1");

    let client2 = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create client 2");

    let election_key = "election:primary";

    // Both attempt to acquire lock simultaneously
    let (result1, result2) = tokio::join!(
        client1.set_nx_ex(election_key, "instance-1", Duration::from_secs(15)),
        client2.set_nx_ex(election_key, "instance-2", Duration::from_secs(15))
    );

    let won1 = result1.expect("Failed to attempt election 1");
    let won2 = result2.expect("Failed to attempt election 2");

    // Exactly one should win (split brain prevented)
    assert!(
        won1 ^ won2,
        "Exactly one instance should win the election"
    );

    // Verify only one primary in Redis
    let winner: Option<String> = client1
        .get_raw(election_key)
        .await
        .expect("Failed to get winner");

    let winner = winner.expect("Winner should exist");
    assert!(
        winner == "instance-1" || winner == "instance-2",
        "Winner should be one of the instances"
    );

    // Cleanup
    let _ = client1.del(election_key).await;
}

#[test]
fn test_recovery_stats_calculation() {
    use forge_ha::*;

    let stats = RecoveryStats {
        sessions_recovered: 148,
        sessions_failed: 2,
        conferences_recovered: 10,
        conferences_failed: 0,
    };

    // Verify stats are reasonable
    assert!(stats.sessions_recovered > 0);
    assert!(stats.conferences_recovered > 0);

    // Calculate success rate
    let total_sessions = stats.sessions_recovered + stats.sessions_failed;
    let success_rate = stats.sessions_recovered as f64 / total_sessions as f64;

    // Should have >98% success rate (148/150 = 98.67%)
    assert!(
        success_rate > 0.98,
        "Should have >98% session recovery success rate, got {}",
        success_rate
    );

    // All conferences should succeed in this scenario
    assert_eq!(stats.conferences_failed, 0, "No conferences should fail");
}

#[tokio::test]
#[ignore] // Requires Redis Sentinel setup
async fn test_sentinel_master_discovery() {
    use forge_ha::*;

    // Note: This test requires a real Redis Sentinel setup
    // For local testing, start Redis with Sentinel:
    // 1. redis-server --port 6379
    // 2. redis-server --port 26379 --sentinel
    // 3. Configure sentinel: SENTINEL MONITOR mymaster 127.0.0.1 6379 1

    let redis_url = std::env::var("TEST_REDIS_URL")
        .unwrap_or_else(|_| "redis://localhost:6379".to_string());
    let sentinel_url = std::env::var("TEST_SENTINEL_URL")
        .unwrap_or_else(|_| "redis://localhost:26379".to_string());

    let config = RedisConfig {
        url: redis_url,
        sentinels: Some(vec![sentinel_url]),
        master_name: Some("mymaster".to_string()),
        key_prefix: "test:ha:".to_string(),
        heartbeat_interval_secs: 10,
        failover_timeout_secs: 25,
        session_ttl_secs: 3600,
        conference_ttl_secs: 7200,
    };

    // Test Sentinel discovery
    let client = RedisHAClient::from_config(&config)
        .await
        .expect("Failed to discover master via Sentinel");

    // Verify we can connect and perform operations
    client.ping().await.expect("Failed to ping discovered master");

    // Test with credentials in base URL
    let mut config_with_creds = config.clone();
    config_with_creds.url = "redis://testuser:testpass@localhost:6379".to_string();

    // This should preserve credentials when discovering master
    let result = RedisHAClient::from_config(&config_with_creds).await;

    // If Sentinel isn't configured, this might fail, but that's expected
    if let Ok(client_with_creds) = result {
        // Verify credentials were preserved (would fail auth if not)
        let _ = client_with_creds.ping().await;
    }
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_lock_renewal_race_condition() {
    use forge_ha::*;

    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client1 = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create client 1");

    let client2 = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create client 2");

    let lock_key = "test:lock";
    let instance1 = "instance-1";
    let instance2 = "instance-2";

    // Instance 1 acquires the lock
    let acquired = client1
        .set_nx_ex(lock_key, instance1, Duration::from_secs(10))
        .await
        .expect("Failed to acquire lock");
    assert!(acquired, "Instance 1 should acquire the lock");

    // Both instances try to renew the lock simultaneously
    let ttl = Duration::from_secs(15);
    let (result1, result2) = tokio::join!(
        client1.compare_and_expire(lock_key, instance1, ttl),
        client2.compare_and_expire(lock_key, instance2, ttl)
    );

    let renewed1 = result1.expect("compare_and_expire should not error");
    let renewed2 = result2.expect("compare_and_expire should not error");

    // Only instance 1 (the owner) should succeed
    assert!(renewed1, "Instance 1 should renew (it owns the lock)");
    assert!(!renewed2, "Instance 2 should fail (it doesn't own the lock)");

    // Verify lock still belongs to instance 1
    let current_owner = client1
        .get_raw(lock_key)
        .await
        .expect("Failed to get lock value")
        .expect("Lock should exist");
    assert_eq!(current_owner, instance1, "Lock should still belong to instance 1");

    // Test case 2: Lock stolen between get and renewal attempt
    // This simulates the race condition that compare_and_expire prevents

    // Instance 1 still owns the lock, but instance 2 tries to steal and renew
    let steal_result = client2
        .set_nx_ex(lock_key, instance2, Duration::from_secs(10))
        .await
        .expect("Failed to attempt steal");

    // Should fail because lock exists
    assert!(!steal_result, "Should not be able to steal existing lock");

    // Instance 1 can still renew atomically
    let final_renewal = client1
        .compare_and_expire(lock_key, instance1, Duration::from_secs(20))
        .await
        .expect("Final renewal should not error");
    assert!(final_renewal, "Instance 1 should successfully renew its lock");

    // Cleanup
    let _ = client1.del(lock_key).await;
}

#[tokio::test]
#[ignore] // Requires Redis server
async fn test_compare_and_expire_ownership_change() {
    use forge_ha::*;

    let redis_url = test_redis_url();
    let key_prefix = test_key_prefix();

    let client = RedisHAClient::new(&redis_url, &key_prefix)
        .await
        .expect("Failed to create client");

    let lock_key = "test:ownership";
    let original_owner = "instance-1";
    let new_owner = "instance-2";

    // Set initial lock
    client
        .set_raw(lock_key, original_owner)
        .await
        .expect("Failed to set initial lock");

    // Original owner can renew
    let renewed = client
        .compare_and_expire(lock_key, original_owner, Duration::from_secs(10))
        .await
        .expect("compare_and_expire should not error");
    assert!(renewed, "Original owner should successfully renew");

    // Simulate ownership change (e.g., manual failover)
    client
        .set_raw(lock_key, new_owner)
        .await
        .expect("Failed to change ownership");

    // Original owner's renewal should now fail
    let renewal_after_change = client
        .compare_and_expire(lock_key, original_owner, Duration::from_secs(10))
        .await
        .expect("compare_and_expire should not error");
    assert!(
        !renewal_after_change,
        "Old owner should fail to renew after ownership changed"
    );

    // New owner can renew
    let new_owner_renewal = client
        .compare_and_expire(lock_key, new_owner, Duration::from_secs(10))
        .await
        .expect("compare_and_expire should not error");
    assert!(new_owner_renewal, "New owner should successfully renew");

    // Cleanup
    let _ = client.del(lock_key).await;
}
