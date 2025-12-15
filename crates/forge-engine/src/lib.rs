//! forge-engine - Core media session management
//!
//! This crate provides the main media session management and RTP forwarding engine.

pub mod ai_integration;
pub mod forwarding;
pub mod manager;
pub mod session;

pub use ai_integration::{AISession, AISessionConfig, AISessionManager, AISessionState};
pub use forwarding::ForwardingEngine;
pub use manager::{SessionManager, SessionManagerConfig};
pub use session::{
    MediaSession, MediaSessionConfig, Participant, ParticipantCodecConfig, ParticipantStats,
    SessionState,
};

#[cfg(feature = "xdp")]
pub use forge_kernel::xdp::{XdpManager, XdpMode};
