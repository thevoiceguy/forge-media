//! Configuration types for Forge

use crate::types::IpVersionConfig;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::Duration;

/// Main Forge configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeConfig {
    /// Engine configuration
    #[serde(default)]
    pub engine: EngineConfig,

    /// API configuration
    #[serde(default)]
    pub api: ApiConfig,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            engine: EngineConfig::default(),
            api: ApiConfig::default(),
        }
    }
}

/// Media engine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Port range for RTP/RTCP allocation
    #[serde(default = "default_port_range")]
    pub port_range: PortRange,

    /// Network interfaces
    #[serde(default)]
    pub interfaces: Vec<InterfaceConfig>,

    /// TOS/DSCP value for QoS
    #[serde(default = "default_tos")]
    pub tos: u8,

    /// Session timeout in seconds
    #[serde(default = "default_session_timeout_secs")]
    pub session_timeout_secs: u64,

    /// IP version configuration
    #[serde(default)]
    pub ip_version: IpVersionConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            port_range: default_port_range(),
            interfaces: vec![],
            tos: default_tos(),
            session_timeout_secs: default_session_timeout_secs(),
            ip_version: IpVersionConfig::DualStack,
        }
    }
}

fn default_port_range() -> PortRange {
    PortRange {
        start: 30000,
        end: 40000,
    }
}

fn default_tos() -> u8 {
    0xB8 // EF (Expedited Forwarding) for voice
}

fn default_session_timeout_secs() -> u64 {
    300
}

/// Port range configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl PortRange {
    pub fn as_range(&self) -> RangeInclusive<u16> {
        self.start..=self.end
    }

    pub fn count(&self) -> usize {
        (self.end - self.start + 1) as usize
    }
}

/// Network interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceConfig {
    /// Interface name (e.g., "eth0")
    pub name: String,

    /// Local IP address
    pub address: IpAddr,

    /// Advertised address for NAT scenarios
    pub advertised_address: Option<IpAddr>,
}

/// API server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// HTTP bind address
    #[serde(default = "default_http_bind")]
    pub http_bind: String,

    /// Enable HTTPS
    #[serde(default)]
    pub enable_https: bool,

    /// HTTPS bind address
    pub https_bind: Option<String>,

    /// TLS certificate path
    pub tls_cert: Option<PathBuf>,

    /// TLS key path
    pub tls_key: Option<PathBuf>,

    /// WebSocket bind address
    pub ws_bind: Option<String>,

    /// Enable CORS
    #[serde(default = "default_true")]
    pub enable_cors: bool,

    /// Allowed origins for CORS
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            http_bind: default_http_bind(),
            enable_https: false,
            https_bind: None,
            tls_cert: None,
            tls_key: None,
            ws_bind: None,
            enable_cors: true,
            cors_origins: vec!["*".to_string()],
        }
    }
}

fn default_http_bind() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_true() -> bool {
    true
}
