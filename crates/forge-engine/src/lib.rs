//! forge-engine - Core media session management
//!
//! This crate provides the main media session management and RTP forwarding engine.

pub mod session;
pub mod forwarding;
pub mod manager;

pub use session::{
    MediaSession, MediaSessionConfig, Participant, ParticipantStats, SessionState,
};
pub use forwarding::ForwardingEngine;
pub use manager::{SessionManager, SessionManagerConfig};

#[cfg(feature = "xdp")]
pub use forge_kernel::xdp::{XdpManager, XdpMode};
