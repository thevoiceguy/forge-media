//! ICE (Interactive Connectivity Establishment) implementation
//!
//! Implements RFC 8445 - Interactive Connectivity Establishment (ICE)
//! Provides NAT traversal for UDP-based media sessions.
//!
//! # Features
//!
//! - Full ICE candidate gathering (host and server-reflexive)
//! - STUN client with MESSAGE-INTEGRITY authentication (RFC 8489)
//! - ICE connectivity checks with role management
//! - Cryptographic credential generation using OS entropy
//! - Support for both controlling and controlled roles
//!
//! # Basic Usage
//!
//! ```no_run
//! use forge_ice::IceAgent;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create ICE agent for RTP (component 1)
//! let mut agent = IceAgent::new(
//!     1,                                              // Component ID (1 = RTP, 2 = RTCP)
//!     50000,                                          // Local port
//!     vec!["stun:stun.l.google.com:19302".into()],  // STUN servers
//! );
//!
//! // Get local credentials for signaling
//! let (local_ufrag, local_pwd) = agent.get_local_credentials();
//! println!("Local credentials: {}:{}", local_ufrag, local_pwd);
//!
//! // Set remote credentials received via signaling
//! agent.set_remote_credentials(
//!     "remote_ufrag_from_peer".into(),
//!     "remote_password_from_peer".into(),
//! )?;
//!
//! // Configure ICE role (controlling initiates checks)
//! agent.set_controlling(true);
//!
//! // Gather local candidates (host + server-reflexive)
//! agent.gather_candidates().await?;
//!
//! // Get candidates for signaling
//! for candidate in agent.get_local_candidates() {
//!     println!("Local candidate: {}", candidate.to_sdp_attribute());
//! }
//!
//! // Add remote candidates received via signaling
//! // agent.add_remote_candidate(remote_candidate);
//!
//! // Perform connectivity checks (includes MESSAGE-INTEGRITY authentication)
//! let ice_success = agent.perform_connectivity_checks().await?;
//! if ice_success {
//!     println!("ICE completed successfully!");
//!
//!     // Get the negotiated socket for media
//!     if let Some(socket) = agent.get_socket() {
//!         // Use socket for RTP/RTCP communication
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # ICE with Authentication
//!
//! ```no_run
//! use forge_ice::{IceAgent, checks::IceAuthContext};
//!
//! # async fn authenticated_ice() -> Result<(), Box<dyn std::error::Error>> {
//! // Create agent with STUN servers
//! let mut agent = IceAgent::new(
//!     1,
//!     50000,
//!     vec!["stun:stun.l.google.com:19302".into()],
//! );
//!
//! // Set remote credentials (triggers authenticated STUN)
//! agent.set_remote_credentials(
//!     "peer_ufrag".into(),
//!     "peer_password_at_least_22_chars".into(),
//! )?;
//!
//! // Connectivity checks automatically use MESSAGE-INTEGRITY
//! agent.gather_candidates().await?;
//! agent.perform_connectivity_checks().await?;
//! # Ok(())
//! # }
//! ```

pub mod agent;
pub mod candidate;
pub mod checks;
pub mod gather;
pub mod stun;
pub mod turn;

pub use agent::IceAgent;
pub use candidate::{CandidateType, IceCandidate, Protocol};
pub use turn::{TurnClient, TurnInbound, TurnServer};
