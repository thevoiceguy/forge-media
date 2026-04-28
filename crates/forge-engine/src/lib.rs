//! forge-engine - Core media session management
//!
//! This crate provides the main media session management and RTP forwarding engine.

pub mod ai_integration;
pub mod forwarding;
pub mod injection;
pub mod manager;
pub mod media_bridge;
pub mod persistence;
pub mod session;

pub use ai_integration::{AISession, AISessionConfig, AISessionManager, AISessionState};
pub use forwarding::ForwardingEngine;
pub use injection::{
    AudioTarget, MixMode, PlaybackHandle, PlaybackId, PlaybackManager, PlaybackStatus,
};
pub use manager::{SessionManager, SessionManagerConfig};
pub use media_bridge::{
    InboundMediaFrame, MediaBridgeHandle, MediaBridgeManager, MediaTarget, OutboundMediaFrame,
};
pub use persistence::{
    ConnectionState, PersistedAISession, PersistenceBackend, PersistenceBackendType,
    PersistenceConfig,
};
pub use session::{
    MediaSession, MediaSessionConfig, Participant, ParticipantCodecConfig, ParticipantLabel,
    ParticipantMediaState, ParticipantMediaUpdate, ParticipantStats, SessionState,
};

#[cfg(feature = "xdp")]
pub use forge_kernel::xdp::{XdpManager, XdpMode};
