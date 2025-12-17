//! Configuration for High Availability

use crate::types::{CloudProvider, DeploymentMode, HARole, PortRange};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// High Availability configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HAConfig {
    /// Enable HA mode
    #[serde(default)]
    pub enabled: bool,

    /// Instance ID (auto-generate if None)
    pub instance_id: Option<String>,

    /// Role selection mode
    #[serde(default)]
    pub role: RoleConfig,

    /// Deployment mode
    #[serde(default)]
    pub deployment_mode: DeploymentMode,

    /// Port range for this instance
    pub port_range: PortRange,

    /// Redis configuration
    pub redis: RedisConfig,

    /// Cloud-specific configuration
    #[serde(default)]
    pub cloud: Option<CloudConfig>,

    /// On-premises configuration
    #[serde(default)]
    pub onprem: Option<OnPremConfig>,
}

/// Role configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleConfig {
    /// Auto-elect based on Redis state
    Auto,
    /// Force primary role
    Primary,
    /// Force standby role
    Standby,
}

impl Default for RoleConfig {
    fn default() -> Self {
        RoleConfig::Auto
    }
}

/// Redis configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    /// Redis connection URL
    pub url: String,

    /// Optional: Redis Sentinel URLs
    pub sentinels: Option<Vec<String>>,

    /// Optional: Sentinel master name
    pub master_name: Option<String>,

    /// Redis key prefix for namespacing
    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,

    /// Heartbeat interval in seconds
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,

    /// Failover timeout in seconds
    #[serde(default = "default_failover_timeout_secs")]
    pub failover_timeout_secs: u64,

    /// Session state TTL in seconds
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,

    /// Conference state TTL in seconds
    #[serde(default = "default_conference_ttl_secs")]
    pub conference_ttl_secs: u64,
}

impl RedisConfig {
    /// Get heartbeat interval as Duration
    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(self.heartbeat_interval_secs)
    }

    /// Get failover timeout as Duration
    pub fn failover_timeout(&self) -> Duration {
        Duration::from_secs(self.failover_timeout_secs)
    }

    /// Get session TTL as Duration
    pub fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.session_ttl_secs)
    }

    /// Get conference TTL as Duration
    pub fn conference_ttl(&self) -> Duration {
        Duration::from_secs(self.conference_ttl_secs)
    }
}

/// Cloud deployment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudConfig {
    /// Cloud provider
    pub provider: CloudProvider,

    /// Health check path
    #[serde(default = "default_health_check_path")]
    pub health_check_path: String,

    /// Whether standby should return 503
    #[serde(default = "default_standby_returns_503")]
    pub standby_returns_503: bool,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            provider: CloudProvider::Gcp,
            health_check_path: default_health_check_path(),
            standby_returns_503: default_standby_returns_503(),
        }
    }
}

/// On-premises deployment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnPremConfig {
    /// Virtual IP address
    pub vip: String,

    /// Network interface for VRRP
    pub interface: String,

    /// VRRP virtual router ID
    pub virtual_router_id: u8,

    /// VRRP priority (higher = preferred primary)
    pub priority: u8,

    /// VRRP authentication password
    pub auth_password: String,
}

impl Default for DeploymentMode {
    fn default() -> Self {
        DeploymentMode::Cloud
    }
}

// Default value functions for serde
fn default_key_prefix() -> String {
    "forge:ha:".to_string()
}

fn default_heartbeat_interval_secs() -> u64 {
    10
}

fn default_failover_timeout_secs() -> u64 {
    25
}

fn default_session_ttl_secs() -> u64 {
    3600
}

fn default_conference_ttl_secs() -> u64 {
    7200
}

fn default_health_check_path() -> String {
    "/health".to_string()
}

fn default_standby_returns_503() -> bool {
    true
}

impl HAConfig {
    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        // Validate port range
        if self.port_range.min >= self.port_range.max {
            return Err(format!(
                "Invalid port range: {} >= {}",
                self.port_range.min, self.port_range.max
            ));
        }

        // Ensure port range is even (for RTP/RTCP pairs)
        if self.port_range.min % 2 != 0 {
            return Err(format!(
                "Port range min must be even for RTP/RTCP pairs: {}",
                self.port_range.min
            ));
        }

        // Validate Redis URL
        if self.redis.url.is_empty() {
            return Err("Redis URL cannot be empty".to_string());
        }

        // Validate deployment-specific config
        match self.deployment_mode {
            DeploymentMode::Cloud => {
                if self.cloud.is_none() {
                    return Err("Cloud config required for cloud deployment".to_string());
                }
            }
            DeploymentMode::OnPrem => {
                if self.onprem.is_none() {
                    return Err("OnPrem config required for on-premises deployment".to_string());
                }
                let onprem = self.onprem.as_ref().unwrap();
                if onprem.vip.is_empty() {
                    return Err("VIP address required for on-premises deployment".to_string());
                }
                if onprem.interface.is_empty() {
                    return Err("Network interface required for on-premises deployment".to_string());
                }
            }
        }

        Ok(())
    }

    /// Get the effective role (resolves Auto to Primary or Standby)
    pub fn effective_role(&self) -> HARole {
        match self.role {
            RoleConfig::Auto => HARole::Unknown, // Will be determined at runtime
            RoleConfig::Primary => HARole::Primary,
            RoleConfig::Standby => HARole::Standby,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redis_config_durations() {
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

        assert_eq!(config.heartbeat_interval(), Duration::from_secs(10));
        assert_eq!(config.failover_timeout(), Duration::from_secs(25));
        assert_eq!(config.session_ttl(), Duration::from_secs(3600));
        assert_eq!(config.conference_ttl(), Duration::from_secs(7200));
    }

    #[test]
    fn test_ha_config_validation_valid() {
        let config = HAConfig {
            enabled: true,
            instance_id: Some("test-instance".to_string()),
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            port_range: PortRange::new(30000, 35000),
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: None,
                master_name: None,
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            cloud: Some(CloudConfig {
                provider: CloudProvider::Gcp,
                health_check_path: "/health".to_string(),
                standby_returns_503: true,
            }),
            onprem: None,
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_ha_config_validation_invalid_port_range() {
        let config = HAConfig {
            enabled: true,
            instance_id: Some("test-instance".to_string()),
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            port_range: PortRange::new(35000, 30000), // Invalid: min > max
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: None,
                master_name: None,
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            cloud: Some(CloudConfig::default()),
            onprem: None,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_ha_config_validation_odd_port() {
        let config = HAConfig {
            enabled: true,
            instance_id: Some("test-instance".to_string()),
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            port_range: PortRange::new(30001, 35000), // Invalid: odd start port
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: None,
                master_name: None,
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            cloud: Some(CloudConfig::default()),
            onprem: None,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_effective_role() {
        let mut config = HAConfig {
            enabled: true,
            instance_id: Some("test-instance".to_string()),
            role: RoleConfig::Auto,
            deployment_mode: DeploymentMode::Cloud,
            port_range: PortRange::new(30000, 35000),
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                sentinels: None,
                master_name: None,
                key_prefix: "forge:ha:".to_string(),
                heartbeat_interval_secs: 10,
                failover_timeout_secs: 25,
                session_ttl_secs: 3600,
                conference_ttl_secs: 7200,
            },
            cloud: Some(CloudConfig::default()),
            onprem: None,
        };

        assert_eq!(config.effective_role(), HARole::Unknown);

        config.role = RoleConfig::Primary;
        assert_eq!(config.effective_role(), HARole::Primary);

        config.role = RoleConfig::Standby;
        assert_eq!(config.effective_role(), HARole::Standby);
    }
}
