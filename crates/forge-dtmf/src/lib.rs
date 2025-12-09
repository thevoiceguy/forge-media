//! DTMF (Dual-Tone Multi-Frequency) detection and generation
//!
//! This crate provides comprehensive DTMF support for VoIP systems:
//! - RFC 2833 (telephone-event) - RTP-based DTMF
//! - Inband DTMF detection using Goertzel algorithm
//! - Event notification system
//!
//! # Examples
//!
//! ## RFC 2833 Parsing
//! ```rust,no_run
//! use forge_dtmf::{Rfc2833Event, DtmfDigit};
//!
//! let rtp_payload = vec![0x05, 0x0A, 0x00, 0xA0]; // Digit '5'
//! let event = Rfc2833Event::from_bytes(&rtp_payload).unwrap();
//! assert_eq!(event.digit(), Some(DtmfDigit::Five));
//! ```

pub mod rfc2833;
pub mod inband;
pub mod detector;
pub mod dedup;

pub use rfc2833::{Rfc2833Event, Rfc2833Generator, Rfc2833Detector};
pub use inband::{InbandDetector, GoertzelDetector};
pub use detector::{DtmfDetector, DtmfEvent, DtmfDigit, DtmfEventType, DtmfMethod};
pub use dedup::DtmfDeduplicator;

use thiserror::Error;

/// DTMF error types
#[derive(Error, Debug)]
pub enum DtmfError {
    #[error("Invalid DTMF digit: {0}")]
    InvalidDigit(String),

    #[error("Invalid RFC 2833 payload: {0}")]
    InvalidRfc2833(String),

    #[error("Invalid audio format: {0}")]
    InvalidAudioFormat(String),

    #[error("Detection error: {0}")]
    DetectionError(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for DTMF operations
pub type Result<T> = std::result::Result<T, DtmfError>;
