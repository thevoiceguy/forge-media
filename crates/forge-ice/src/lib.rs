//! ICE (Interactive Connectivity Establishment) implementation
//!
//! Implements RFC 8445 - Interactive Connectivity Establishment (ICE)
//! Provides NAT traversal for UDP-based media sessions.

pub mod candidate;
pub mod gather;
pub mod stun;
pub mod agent;
pub mod checks;

pub use candidate::{IceCandidate, CandidateType, Protocol};
pub use agent::IceAgent;
