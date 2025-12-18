//! ICE (Interactive Connectivity Establishment) implementation
//!
//! Implements RFC 8445 - Interactive Connectivity Establishment (ICE)
//! Provides NAT traversal for UDP-based media sessions.

pub mod agent;
pub mod candidate;
pub mod checks;
pub mod gather;
pub mod stun;

pub use agent::IceAgent;
pub use candidate::{CandidateType, IceCandidate, Protocol};
