//! Core types for High Availability

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::SocketAddr;
use uuid::Uuid;

/// Unique identifier for an HA instance
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(Uuid);

impl InstanceId {
    /// Generate a new random instance ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create from string
    pub fn from_string(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(s)?))
    }

    /// Get the inner UUID
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for InstanceId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

/// Role of an HA instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HARole {
    /// Primary instance - actively serving traffic
    Primary,
    /// Standby instance - ready for failover
    Standby,
    /// Unknown/initializing
    Unknown,
}

impl fmt::Display for HARole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HARole::Primary => write!(f, "primary"),
            HARole::Standby => write!(f, "standby"),
            HARole::Unknown => write!(f, "unknown"),
        }
    }
}

/// Health state of an instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    /// All systems operational
    Healthy,
    /// Degraded but functional
    Degraded,
    /// Failed, not operational
    Failed,
}

impl fmt::Display for HealthState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HealthState::Healthy => write!(f, "healthy"),
            HealthState::Degraded => write!(f, "degraded"),
            HealthState::Failed => write!(f, "failed"),
        }
    }
}

/// Deployment mode for HA
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeploymentMode {
    /// Cloud deployment (GCP, AWS, Azure, Linode)
    Cloud,
    /// On-premises deployment (VRRP/Keepalived)
    OnPrem,
}

/// Cloud provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    /// Google Cloud Platform
    Gcp,
    /// Amazon Web Services
    Aws,
    /// Microsoft Azure
    Azure,
    /// Linode
    Linode,
}

/// Instance health information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceHealth {
    /// Instance identifier
    pub instance_id: InstanceId,
    /// Current role
    pub role: HARole,
    /// Health state
    pub state: HealthState,
    /// IP address
    pub ip_address: String,
    /// Advertised address (for external access)
    pub advertised_address: Option<String>,
    /// Port range for this instance
    pub port_range: PortRange,
    /// Last heartbeat timestamp
    pub last_heartbeat: DateTime<Utc>,
    /// Number of active sessions
    pub session_count: usize,
    /// Number of active conferences
    pub conference_count: usize,
    /// Uptime in seconds
    pub uptime_seconds: u64,
    /// Server version
    pub version: String,
}

/// Port range for an instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRange {
    pub min: u16,
    pub max: u16,
}

impl PortRange {
    pub fn new(min: u16, max: u16) -> Self {
        Self { min, max }
    }

    pub fn contains(&self, port: u16) -> bool {
        port >= self.min && port <= self.max
    }

    pub fn size(&self) -> usize {
        (self.max - self.min + 1) as usize
    }
}

/// Participant codec configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecConfig {
    pub payload_type: u8,
    pub codec: String,
    pub clock_rate: u32,
}

/// Participant statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantStats {
    pub packets_received: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub packets_lost: u64,
}

/// Serializable participant state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantState {
    pub id: String,
    pub remote_addr: Option<SocketAddr>,
    pub codec: CodecConfig,
    pub stats: ParticipantStats,
}

/// Port pair (RTP + RTCP)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PortPair {
    pub rtp_port: u16,
    pub rtcp_port: u16,
}

/// Transcoder state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscoderState {
    pub a_to_b_active: bool,
    pub b_to_a_active: bool,
    pub source_codec: Option<String>,
    pub dest_codec: Option<String>,
}

/// Session state for Redis persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub call_id: String,
    pub state: String,  // "Initializing", "Active", "OnHold", "Terminating"
    pub participant_a: ParticipantState,
    pub participant_b: ParticipantState,
    pub ports: PortPair,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub sdp: Option<String>,
    pub from_tag: Option<String>,
    pub to_tag: Option<String>,
    pub transcoder_state: Option<TranscoderState>,
    pub xdp_active: bool,
    pub ai_session_id: Option<String>,
    pub version: u32,
    pub instance_id: String,
}

/// Conference participant state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceParticipantState {
    pub id: String,
    pub call_id: String,
    pub role: String,  // "Host" or "Guest"
    pub state: String,  // "Active", "Muted", "OnHold", "Waiting"
    pub gain: f32,
    pub join_time: DateTime<Utc>,
    pub is_recording: bool,
    pub packets_received: u64,
}

/// Conference audio format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

/// Conference room security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceSecurityConfig {
    pub guest_pin: Option<String>,
    pub host_pin: Option<String>,
    pub require_guest_pin: bool,
    pub max_pin_attempts: u32,
    pub default_locked: bool,
}

/// Conference room configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceRoomConfig {
    pub security: ConferenceSecurityConfig,
    pub max_channels: usize,
    pub wait_for_moderator: bool,
}

/// Conference room state for Redis persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceState {
    pub room_id: String,
    pub format: AudioFormat,
    pub frame_size: usize,
    pub participants: Vec<ConferenceParticipantState>,
    pub is_locked: bool,
    pub recording_active: bool,
    pub recording_path: Option<String>,
    pub room_config: ConferenceRoomConfig,
    pub ai_active: bool,
    pub version: u32,
    pub instance_id: String,
}

/// HA cluster status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HAStatus {
    /// This instance's information
    pub instance: InstanceHealth,
    /// Primary instance information (if known)
    pub primary: Option<InstanceHealth>,
    /// All known instances
    pub instances: Vec<InstanceHealth>,
    /// Redis connection status
    pub redis_connected: bool,
    /// Last failover timestamp (if any)
    pub last_failover: Option<DateTime<Utc>>,
    /// Total failover count
    pub failover_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_id_generation() {
        let id1 = InstanceId::new();
        let id2 = InstanceId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_instance_id_from_string() {
        let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
        let id = InstanceId::from_string(uuid_str).unwrap();
        assert_eq!(id.to_string(), uuid_str);
    }

    #[test]
    fn test_port_range() {
        let range = PortRange::new(30000, 35000);
        assert!(range.contains(30000));
        assert!(range.contains(32500));
        assert!(range.contains(35000));
        assert!(!range.contains(29999));
        assert!(!range.contains(35001));
        assert_eq!(range.size(), 5001);
    }

    #[test]
    fn test_ha_role_serialization() {
        let role = HARole::Primary;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, r#""primary""#);

        let deserialized: HARole = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, role);
    }
}
