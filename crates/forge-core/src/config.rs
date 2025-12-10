//! Configuration types for Forge

use crate::types::IpVersionConfig;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use crate::types::AudioFormat;

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

    /// XDP (eBPF) acceleration configuration
    #[serde(default)]
    pub xdp: XdpConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            port_range: default_port_range(),
            interfaces: vec![],
            tos: default_tos(),
            session_timeout_secs: default_session_timeout_secs(),
            ip_version: IpVersionConfig::DualStack,
            xdp: XdpConfig::default(),
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

/// XDP (eBPF) acceleration configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdpConfig {
    /// Enable XDP acceleration
    #[serde(default)]
    pub enabled: bool,

    /// Network interface to attach XDP program
    #[serde(default = "default_xdp_interface")]
    pub interface: String,

    /// XDP mode (native or generic)
    #[serde(default)]
    pub mode: XdpMode,

    /// Fallback to userspace if XDP fails to load
    #[serde(default = "default_true")]
    pub fallback: bool,
}

impl Default for XdpConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for compatibility
            interface: default_xdp_interface(),
            mode: XdpMode::Generic,
            fallback: true,
        }
    }
}

fn default_xdp_interface() -> String {
    "lo".to_string()
}

/// XDP operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XdpMode {
    /// Native XDP mode (XDP_DRV) - fastest, requires driver support
    Native,
    /// Generic XDP mode (XDP_SKB) - software fallback
    Generic,
}

impl Default for XdpMode {
    fn default() -> Self {
        Self::Generic
    }
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
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,

    /// Static bearer tokens for API authentication
    /// If empty, authentication is disabled (not recommended for production)
    #[serde(default)]
    pub auth_tokens: Vec<String>,

    /// Maximum requests allowed per window for rate limiting
    #[serde(default = "default_rate_limit_requests")]
    pub rate_limit_requests_per_window: usize,

    /// Rate limit window size in seconds
    #[serde(default = "default_rate_limit_window_secs")]
    pub rate_limit_window_secs: u64,

    /// Base directory for recordings (all recording paths are constrained within this directory)
    #[serde(default = "default_recording_base_dir")]
    pub recording_base_dir: std::path::PathBuf,

    /// Base directory for playback prompts/announcements
    #[serde(default = "default_prompts_base_dir")]
    pub prompts_base_dir: std::path::PathBuf,

    /// SIPREC configuration
    #[serde(default)]
    pub siprec: SiprecConfig,
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
            cors_origins: default_cors_origins(),
            auth_tokens: Vec::new(),
            rate_limit_requests_per_window: default_rate_limit_requests(),
            rate_limit_window_secs: default_rate_limit_window_secs(),
            recording_base_dir: default_recording_base_dir(),
            prompts_base_dir: default_prompts_base_dir(),
            siprec: SiprecConfig::default(),
        }
    }
}

fn default_http_bind() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_true() -> bool {
    true
}

fn default_cors_origins() -> Vec<String> {
    vec!["http://localhost:3000".to_string()]
}

fn default_rate_limit_requests() -> usize {
    120
}

fn default_rate_limit_window_secs() -> u64 {
    60
}

fn default_recording_base_dir() -> std::path::PathBuf {
    "/var/lib/forge/recordings".into()
}

fn default_prompts_base_dir() -> std::path::PathBuf {
    "/var/lib/forge/prompts".into()
}

/// SIPREC (compliance recording) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiprecConfig {
    /// Enable SIPREC captures
    #[serde(default)]
    pub enabled: bool,

    /// Output directory for SIPREC recordings
    #[serde(default = "default_siprec_output_dir")]
    pub output_dir: PathBuf,

    /// Audio format to use for captures
    #[serde(default = "default_siprec_audio_format")]
    pub format: AudioFormat,
}

impl Default for SiprecConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output_dir: default_siprec_output_dir(),
            format: default_siprec_audio_format(),
        }
    }
}

fn default_siprec_output_dir() -> PathBuf {
    "/var/lib/forge/siprec".into()
}

fn default_siprec_audio_format() -> AudioFormat {
    AudioFormat::pcm_mono()
}
