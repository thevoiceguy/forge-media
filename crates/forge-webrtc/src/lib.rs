//! forge-webrtc - WebRTC support for Forge Media Engine
//!
//! An endpoint-shaped WebRTC peer connection:
//! - ICE (RFC 8445) with trickle (RFC 8838): connectivity checks, nomination
//!   and keepalives all on the one media socket; both roles
//! - DTLS-SRTP (RFC 5764) key exchange with the certificate fingerprint bound
//!   to the signalled SDP; both `a=setup` roles
//! - SRTP/SRTCP (RFC 3711, RFC 7714) with keys installed straight from the
//!   DTLS export — no engine session required
//! - SDP offer **and** answer (RFC 3264, RFC 8829), one audio section —
//!   Opus and G.711 (PCMU/PCMA), preference-ordered via
//!   [`PeerConfig::codecs`] — BUNDLE + rtcp-mux; renegotiation on the same
//!   transport (re-offer and rollback); ICE restart deliberately unsupported
//!
//! # Example
//! ```no_run
//! use forge_webrtc::{PeerConnection, PeerEvent};
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Offerer
//! let mut caller = PeerConnection::new(vec!["stun:stun.l.google.com:19302".to_string()]).await?;
//! let offer = caller.create_offer().await?;
//! let mut caller_events = caller.take_events().unwrap();
//!
//! // Answerer (normally on the other side of a signalling channel)
//! let mut callee = PeerConnection::new(vec![]).await?;
//! callee.set_remote_offer(&offer).await?;
//! let answer = callee.create_answer().await?;
//! caller.set_remote_answer(&answer).await?;
//!
//! // Trickle candidates as they appear
//! while let Some(ev) = caller_events.recv().await {
//!     match ev {
//!         PeerEvent::LocalCandidate(c) => callee.add_ice_candidate(c).await?,
//!         PeerEvent::Connected => break,
//!         _ => {}
//!     }
//! }
//! caller.wait_connected(Duration::from_secs(10)).await?;
//! let audio = caller.sender()?;
//! audio.send_audio(bytes::Bytes::from_static(&[0xf8, 0xff, 0xfe]), 960).await?;
//! # Ok(())
//! # }
//! ```

pub mod peer;
pub mod sdp;
pub mod transport;

pub use forge_core::AudioCodec;
pub use forge_ice::{IceCandidate, TurnServer};
pub use peer::{
    AudioSender, ConnectionState, PeerConfig, PeerConnection, PeerEvent, SignalingState,
};
pub use sdp::Direction;
pub use transport::{IceRole, TransportConfig, TransportEvent};

use thiserror::Error;

/// WebRTC-specific error types
#[derive(Error, Debug)]
pub enum WebRtcError {
    /// ICE error
    #[error("ICE error: {0}")]
    IceError(String),

    /// DTLS error
    #[error("DTLS error: {0}")]
    DtlsError(String),

    /// SDP error
    #[error("SDP error: {0}")]
    SdpError(#[from] forge_sdp::SdpError),

    /// Invalid state
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// Connection failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// The remote description changed the ICE credentials (an ICE restart),
    /// which this implementation does not support.
    #[error("ICE restart is not supported")]
    IceRestartUnsupported,

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for WebRTC operations
pub type Result<T> = std::result::Result<T, WebRtcError>;
