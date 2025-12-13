//! forge-webrtc - WebRTC support for Forge Media Engine
//!
//! This crate provides WebRTC capabilities including:
//! - ICE (Interactive Connectivity Establishment)
//! - DTLS handshake for key exchange
//! - SRTP encryption/decryption
//! - SDP offer/answer generation
//! - Peer connection management
//!
//! # Example
//! ```no_run
//! use forge_webrtc::PeerConnection;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a new peer connection
//! let mut peer = PeerConnection::new(vec!["stun:stun.l.google.com:19302".to_string()]).await?;
//!
//! // Create an SDP offer
//! let offer = peer.create_offer().await?;
//!
//! // Set remote answer (received from remote peer)
//! # let answer_sdp = ""; // Example: received from remote peer
//! peer.set_remote_answer(answer_sdp).await?;
//! # Ok(())
//! # }
//! ```

pub mod peer;

pub use peer::{ConnectionState, PeerConnection};

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

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for WebRTC operations
pub type Result<T> = std::result::Result<T, WebRtcError>;
