//! Forge RTP - RTP/RTCP/SRTP implementation
//!
//! This crate provides comprehensive RTP packet handling, SRTP encryption/decryption,
//! jitter buffering, and RTCP processing.

pub mod jitter;
pub mod metrics;
pub mod port_pool;
pub mod rtcp;
pub mod rtp;
pub mod rtt;
pub mod socket;
pub mod srtp;
pub mod video;

#[cfg(feature = "dtls")]
pub mod dtls;

pub use jitter::*;
pub use port_pool::*;
pub use rtcp::*;
pub use rtp::*;
pub use rtt::{ntp_middle32, RttTracker};
pub use socket::*;
pub use srtp::*;
pub use video::{
    AssemblerEvent, CodedFrame, FrameAssembler, KeyframeRequestGate, PayloadError, PayloadInfo,
    RtxCache, StreamRewriter,
};
