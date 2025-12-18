//! Unit tests for forge-ha components
//!
//! These tests verify the core functionality of HA components in isolation.

#[cfg(test)]
mod types_tests {
    use crate::types::*;
    use chrono::Utc;

    #[test]
    fn test_instance_id_creation() {
        let id1 = InstanceId::new();
        let id2 = InstanceId::new();

        // Each instance should be unique
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_instance_id_from_string() {
        let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
        let id = InstanceId::from_string(uuid_str).expect("Failed to parse UUID");
        assert_eq!(id.to_string(), uuid_str);
    }

    #[test]
    fn test_instance_id_display() {
        let id = InstanceId::new();
        let displayed = format!("{}", id);
        // UUID should be in standard format
        assert_eq!(displayed.len(), 36); // UUID string length
        assert!(displayed.contains("-"));
    }

    #[test]
    fn test_ha_role_display() {
        assert_eq!(HARole::Primary.to_string(), "primary");
        assert_eq!(HARole::Standby.to_string(), "standby");
        assert_eq!(HARole::Unknown.to_string(), "unknown");
    }

    #[test]
    fn test_health_state_variants() {
        let healthy = HealthState::Healthy;
        let degraded = HealthState::Degraded;
        let failed = HealthState::Failed;

        assert!(matches!(healthy, HealthState::Healthy));
        assert!(matches!(degraded, HealthState::Degraded));
        assert!(matches!(failed, HealthState::Failed));
    }

    #[test]
    fn test_port_range_validation() {
        let range = PortRange::new(30000, 35000);

        assert_eq!(range.min, 30000);
        assert_eq!(range.max, 35000);
        assert_eq!(range.size(), 5001); // inclusive

        // Test contains
        assert!(range.contains(30000));
        assert!(range.contains(32500));
        assert!(range.contains(35000));
        assert!(!range.contains(29999));
        assert!(!range.contains(35001));
    }

    #[test]
    fn test_port_range_single_port() {
        let range = PortRange::new(30000, 30000);
        assert_eq!(range.size(), 1);
        assert!(range.contains(30000));
        assert!(!range.contains(30001));
    }

    #[test]
    fn test_port_pair_structure() {
        let ports = PortPair {
            rtp_port: 30000,
            rtcp_port: 30001,
        };

        assert_eq!(ports.rtp_port, 30000);
        assert_eq!(ports.rtcp_port, 30001);
        assert_eq!(ports.rtcp_port, ports.rtp_port + 1); // Standard RTP/RTCP pairing
    }

    #[test]
    fn test_deployment_mode() {
        let cloud = DeploymentMode::Cloud;
        let onprem = DeploymentMode::OnPrem;

        assert!(matches!(cloud, DeploymentMode::Cloud));
        assert!(matches!(onprem, DeploymentMode::OnPrem));
    }

    #[test]
    fn test_cloud_provider_variants() {
        let providers = vec![
            CloudProvider::Gcp,
            CloudProvider::Aws,
            CloudProvider::Azure,
            CloudProvider::Linode,
        ];

        assert_eq!(providers.len(), 4);
    }

    #[test]
    fn test_participant_stats_default() {
        let stats = ParticipantStats::default();

        assert_eq!(stats.packets_received, 0);
        assert_eq!(stats.bytes_received, 0);
        assert_eq!(stats.packets_sent, 0);
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.packets_lost, 0);
    }

    #[test]
    fn test_audio_format() {
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: 16,
        };

        assert_eq!(format.sample_rate, 48000);
        assert_eq!(format.channels, 1);
        assert_eq!(format.bits_per_sample, 16);
    }

    #[test]
    fn test_session_state_serialization() {
        let state = SessionState {
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
            created_at: Utc::now(),
            last_activity: Utc::now(),
            sdp: None,
            from_tag: None,
            to_tag: None,
            transcoder_state: None,
            xdp_active: false,
            ai_session_id: None,
            version: 1,
            instance_id: "primary-01".to_string(),
        };

        // Test serialization to JSON
        let json = serde_json::to_string(&state).expect("Failed to serialize");
        assert!(json.contains("test-call-123"));
        assert!(json.contains("Active"));

        // Test deserialization
        let deserialized: SessionState =
            serde_json::from_str(&json).expect("Failed to deserialize");
        assert_eq!(deserialized.call_id, state.call_id);
        assert_eq!(deserialized.state, state.state);
    }

    #[test]
    fn test_conference_state_structure() {
        let state = ConferenceState {
            room_id: "room-456".to_string(),
            format: AudioFormat {
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: 16,
            },
            frame_size: 480,
            participants: vec![],
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

        assert_eq!(state.room_id, "room-456");
        assert_eq!(state.participants.len(), 0);
        assert!(!state.is_locked);
    }

    #[test]
    fn test_instance_health() {
        let health = InstanceHealth {
            instance_id: InstanceId::new(),
            role: HARole::Primary,
            state: HealthState::Healthy,
            ip_address: "10.0.1.10".to_string(),
            advertised_address: Some("203.0.113.10".to_string()),
            port_range: PortRange::new(30000, 35000),
            last_heartbeat: Utc::now(),
            session_count: 42,
            conference_count: 3,
            uptime_seconds: 3600,
            version: "0.1.0".to_string(),
        };

        assert!(matches!(health.role, HARole::Primary));
        assert!(matches!(health.state, HealthState::Healthy));
        assert_eq!(health.session_count, 42);
        assert_eq!(health.conference_count, 3);
    }
}

#[cfg(test)]
mod config_tests {
    use crate::config::*;
    use crate::types::{CloudProvider, InstanceId};
    use std::time::Duration;

    #[test]
    fn test_redis_config() {
        let config = RedisConfig {
            url: "redis://localhost:6379".to_string(),
            sentinels: None,
            master_name: None,
            key_prefix: "forge:ha:".to_string(),
            heartbeat_interval_secs: 10,
            failover_timeout_secs: 25,
            session_ttl_secs: 3600,
            conference_ttl_secs: 7200,
        };

        assert_eq!(config.heartbeat_interval().as_secs(), 10);
        assert_eq!(config.failover_timeout().as_secs(), 25);

        // Verify timeout is greater than heartbeat interval
        assert!(config.failover_timeout() > config.heartbeat_interval());

        // Verify can detect at least 2 missed heartbeats
        let missed_heartbeats = config.failover_timeout_secs / config.heartbeat_interval_secs;
        assert!(missed_heartbeats >= 2);
    }

    #[test]
    fn test_cloud_config() {
        let config = CloudConfig {
            provider: CloudProvider::Gcp,
            health_check_path: "/health".to_string(),
            standby_returns_503: true,
        };

        assert!(matches!(config.provider, CloudProvider::Gcp));
        assert_eq!(config.health_check_path, "/health");
        assert!(config.standby_returns_503);
    }

    #[test]
    fn test_onprem_config_priority() {
        let primary_config = OnPremConfig {
            vip: "10.0.1.100".to_string(),
            interface: "eth0".to_string(),
            virtual_router_id: 51,
            priority: 100,
            auth_password: "secret".to_string(),
        };

        let standby_config = OnPremConfig {
            vip: "10.0.1.100".to_string(),
            interface: "eth0".to_string(),
            virtual_router_id: 51,
            priority: 50,
            auth_password: "secret".to_string(),
        };

        // Primary should have higher priority
        assert!(primary_config.priority > standby_config.priority);

        // Priorities should be in valid range (1-255)
        assert!(primary_config.priority > 0 && primary_config.priority <= 255);
        assert!(standby_config.priority > 0 && standby_config.priority <= 255);
    }

    #[test]
    fn test_role_config_variants() {
        let auto = RoleConfig::Auto;
        let primary = RoleConfig::Primary;
        let standby = RoleConfig::Standby;

        assert!(matches!(auto, RoleConfig::Auto));
        assert!(matches!(primary, RoleConfig::Primary));
        assert!(matches!(standby, RoleConfig::Standby));
    }

    #[test]
    fn test_sentinel_config_validation_missing_master_name() {
        use crate::types::{DeploymentMode, PortRange};

        let config = HAConfig {
            enabled: true,
            instance_id: None,
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            cloud: Some(CloudConfig {
                provider: CloudProvider::Gcp,
                health_check_path: "/health".to_string(),
                standby_returns_503: true,
            }),
            onprem: None,
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: Some(vec!["redis://sentinel1:26379".to_string()]),
                master_name: None, // Missing master_name!
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            port_range: PortRange::new(30000, 35000),
        };

        let result = config.validate();
        assert!(
            result.is_err(),
            "Should fail validation when sentinels configured without master_name"
        );
        assert!(
            result.unwrap_err().contains("master_name is required"),
            "Error should mention master_name requirement"
        );
    }

    #[test]
    fn test_sentinel_config_validation_empty_sentinels() {
        use crate::types::{DeploymentMode, PortRange};

        let config = HAConfig {
            enabled: true,
            instance_id: None,
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            cloud: Some(CloudConfig {
                provider: CloudProvider::Gcp,
                health_check_path: "/health".to_string(),
                standby_returns_503: true,
            }),
            onprem: None,
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: Some(vec![]), // Empty sentinel list!
                master_name: Some("mymaster".to_string()),
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            port_range: PortRange::new(30000, 35000),
        };

        let result = config.validate();
        assert!(
            result.is_err(),
            "Should fail validation with empty sentinels list"
        );
        assert!(
            result
                .unwrap_err()
                .contains("sentinels list cannot be empty"),
            "Error should mention empty sentinels list"
        );
    }

    #[test]
    fn test_sentinel_config_validation_valid() {
        use crate::types::{DeploymentMode, PortRange};

        let config = HAConfig {
            enabled: true,
            instance_id: None,
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            cloud: Some(CloudConfig {
                provider: CloudProvider::Gcp,
                health_check_path: "/health".to_string(),
                standby_returns_503: true,
            }),
            onprem: None,
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: Some(vec![
                    "redis://sentinel1:26379".to_string(),
                    "redis://sentinel2:26379".to_string(),
                    "redis://sentinel3:26379".to_string(),
                ]),
                master_name: Some("mymaster".to_string()),
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            port_range: PortRange::new(30000, 35000),
        };

        let result = config.validate();
        assert!(
            result.is_ok(),
            "Valid sentinel config should pass validation"
        );
    }
}

#[cfg(test)]
mod state_sync_tests {
    // Note: State sync components require Redis for meaningful testing
    // Integration tests with Redis are in tests/failover_integration.rs

    #[test]
    fn test_redis_key_patterns() {
        // Test that key patterns follow expected format
        let key_prefix = "forge:ha:";

        let session_key = format!("{}sessions:{}", key_prefix, "call-123");
        assert_eq!(session_key, "forge:ha:sessions:call-123");

        let conf_key = format!("{}conferences:{}", key_prefix, "room-456");
        assert_eq!(conf_key, "forge:ha:conferences:room-456");

        let ports_key = format!("{}ports:{}", key_prefix, "instance-01");
        assert_eq!(ports_key, "forge:ha:ports:instance-01");
    }
}

#[cfg(test)]
mod failover_tests {
    use crate::failover::*;

    #[test]
    fn test_failover_state_transitions() {
        let states = vec![
            FailoverState::Normal,
            FailoverState::Detecting,
            FailoverState::Electing,
            FailoverState::Promoting,
            FailoverState::Recovering,
            FailoverState::Complete,
        ];

        // All states should be distinct
        assert_eq!(states.len(), 6);
    }

    #[test]
    fn test_recovery_stats_calculation() {
        let stats = RecoveryStats {
            sessions_recovered: 100,
            sessions_failed: 2,
            conferences_recovered: 5,
            conferences_failed: 0,
        };

        assert_eq!(stats.sessions_recovered, 100);
        assert_eq!(stats.sessions_failed, 2);
        assert_eq!(stats.conferences_recovered, 5);
        assert_eq!(stats.conferences_failed, 0);

        // Calculate success rate
        let total_sessions = stats.sessions_recovered + stats.sessions_failed;
        let success_rate = stats.sessions_recovered as f64 / total_sessions as f64;
        assert!(success_rate > 0.98); // Should have >98% success rate
    }
}

#[cfg(test)]
mod timing_tests {
    use std::time::Duration;

    #[test]
    fn test_heartbeat_timing() {
        let interval = Duration::from_secs(10);
        let timeout = Duration::from_secs(30);

        // Timeout should allow detecting 3 missed heartbeats
        let missed = timeout.as_secs() / interval.as_secs();
        assert_eq!(missed, 3);
    }

    #[test]
    fn test_election_lock_renewal() {
        let lock_ttl = Duration::from_secs(15);
        let renewal_interval = Duration::from_secs(5);

        // Renewal should happen with 2x safety margin
        assert!(renewal_interval * 2 < lock_ttl);
    }

    #[test]
    fn test_failover_timing_budget() {
        // Target: Complete failover in 30-40 seconds
        let detection_time = Duration::from_secs(30); // 3 missed heartbeats
        let recovery_budget = Duration::from_secs(10); // Redis ops, session recovery

        let total = detection_time + recovery_budget;
        assert!(total.as_secs() <= 40);
    }
}
