//! forge-siprec - SIPREC (Session Recording Protocol) Implementation
//!
//! This crate implements RFC 7865 (Session Recording Metadata) and RFC 7866
//! (Session Recording Protocol) for compliance recording.
//!
//! # Features
//!
//! - **SRC (Session Recording Client)**: Send recording streams to SRS
//! - **SRS (Session Recording Server)**: Receive and store recording streams
//! - **Metadata XML**: RFC 7865 compliant metadata generation
//! - **Media Forking**: Duplicate RTP streams to recording server
//! - **SRTP Support**: Forward SRTP keys for encrypted recordings
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐          ┌─────────────┐
//! │   Caller    │◄────────►│   Callee    │
//! └─────────────┘          └─────────────┘
//!        │                        │
//!        │    Media (RTP)         │
//!        └────────┬───────────────┘
//!                 │
//!                 │ Fork
//!                 ▼
//!         ┌───────────────┐
//!         │  SRC (Client) │
//!         └───────────────┘
//!                 │
//!                 │ SIPREC
//!                 │ (SIP + RTP + Metadata XML)
//!                 ▼
//!         ┌───────────────┐
//!         │  SRS (Server) │
//!         └───────────────┘
//!                 │
//!                 ▼
//!           [Recording Storage]
//! ```
//!
//! # Example Usage
//!
//! ## As Session Recording Client (SRC)
//!
//! ```rust,ignore
//! use forge_siprec::{SrcConfig, SessionRecordingClient};
//!
//! let config = SrcConfig {
//!     srs_uri: "sip:recorder@srs.example.com".to_string(),
//!     local_address: "192.168.1.10".parse().unwrap(),
//!     ..Default::default()
//! };
//!
//! let src = SessionRecordingClient::new(config).await?;
//!
//! // Start recording a call
//! let session_id = src.start_recording(
//!     "call-123",
//!     "sip:alice@example.com",
//!     "sip:bob@example.com"
//! ).await?;
//!
//! // Forward RTP packets to SRS
//! src.forward_rtp(session_id, rtp_packet).await?;
//!
//! // Stop recording
//! src.stop_recording(session_id).await?;
//! ```
//!
//! ## As Session Recording Server (SRS)
//!
//! ```rust,ignore
//! use forge_siprec::{SrsConfig, SessionRecordingServer};
//!
//! let config = SrsConfig {
//!     bind_address: "0.0.0.0:5060".parse().unwrap(),
//!     storage_path: "/var/lib/forge/recordings".into(),
//!     ..Default::default()
//! };
//!
//! let srs = SessionRecordingServer::new(config).await?;
//! srs.start().await?;
//! ```

pub mod metadata;
pub mod src;
pub mod srs;

// Re-export legacy manager for backward compatibility
mod manager;
pub use manager::{SiprecConfig, SiprecError, SiprecManager, SiprecSession, Result};

// Re-export new API
pub use metadata::{
    ExtensionData, MediaStream, MediaType, Participant, ParticipantRole,
    RecordingSession, RtpSession,
};
pub use src::{SessionRecordingClient, SrcConfig, SrcError};
pub use srs::{SessionRecordingServer, SrsConfig, SrsError};
