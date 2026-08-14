//! Error types for Forge

use thiserror::Error;

/// Main error type for Forge operations
#[derive(Debug, Error)]
pub enum ForgeError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Room not found: {0}")]
    RoomNotFound(String),

    #[error("Participant not found: {0}")]
    ParticipantNotFound(String),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Codec error: {0}")]
    Codec(String),

    #[error("Network error: {0}")]
    Network(String),

    /// A bind failed because something already holds the address.
    ///
    /// Split out of [`Self::Network`] because it is the one bind failure
    /// worth **retrying on a different port**: `PortPool` tracks only its
    /// own bookkeeping, and the configured RTP range normally sits inside
    /// the OS ephemeral range (`net.ipv4.ip_local_port_range`), so a port
    /// the pool believes free can be held by an unrelated socket — a DNS
    /// lookup by any process on the host is enough. Carries the address
    /// because knowing *which* port was taken is what makes the problem
    /// diagnosable.
    #[error("Address already in use: {0}")]
    AddrInUse(std::net::SocketAddr),

    #[error("Port allocation failed")]
    PortAllocationFailed,

    #[error("RTP error: {0}")]
    Rtp(String),

    #[error("SRTP error: {0}")]
    Srtp(String),

    #[error("RTCP error: {0}")]
    Rtcp(String),

    #[error("SDP error: {0}")]
    Sdp(String),

    #[error("ICE error: {0}")]
    Ice(String),

    #[error("Transcoding error: {0}")]
    Transcoding(String),

    #[error("Recording error: {0}")]
    Recording(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Operation timeout")]
    Timeout,

    #[error("Resource limit exceeded: {0}")]
    ResourceLimit(String),
}

pub type Result<T> = std::result::Result<T, ForgeError>;
