//! forge-engine - Core media session management
//!
//! This crate provides the main media session management and RTP forwarding engine.

#[cfg(feature = "ai")]
pub mod ai_integration;
#[cfg(feature = "dtls")]
pub mod dtls_srtp;
pub mod forwarding;
pub mod injection;
pub mod manager;
pub mod media_bridge;
pub mod metrics;
#[cfg(feature = "ai")]
pub mod persistence;
pub mod session;
pub mod srtp_install;

#[cfg(feature = "ai")]
pub use ai_integration::{AISession, AISessionConfig, AISessionManager, AISessionState};
#[cfg(feature = "dtls")]
pub use dtls_srtp::{
    install_keys as install_dtls_srtp_keys, is_dtls_packet, is_rtp_packet,
    is_unsupported_first_byte, DtlsLeg, HandshakeOutcome,
};
pub use forge_dtmf::DtmfDigit;
pub use forwarding::ForwardingEngine;
pub use injection::{
    AudioTarget, MixMode, PlaybackHandle, PlaybackId, PlaybackManager, PlaybackStatus,
};
pub use manager::{SessionManager, SessionManagerConfig};
pub use media_bridge::{
    InboundMediaFrame, MediaBridgeHandle, MediaBridgeManager, MediaTarget, OutboundDtmfRequest,
    OutboundMediaFrame, OutboundMediaRequest, PlayoutMode,
};
#[cfg(feature = "ai")]
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
