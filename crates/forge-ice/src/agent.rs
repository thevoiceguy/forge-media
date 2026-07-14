//! ICE Agent - coordinates ICE session management

use crate::candidate::{CandidatePair, IceCandidate, PairState};
use crate::checks::{perform_checks, IceAuthContext};
use crate::gather::{gather_host_candidates, gather_server_reflexive_candidates};
use forge_core::Result;
use rand::rngs::SysRng;
use rand::TryRng;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tracing::{debug, info};

/// ICE Agent for managing ICE session
pub struct IceAgent {
    /// Local ICE username fragment
    local_ufrag: String,
    /// Local ICE password
    local_pwd: String,
    /// Remote ICE username fragment
    remote_ufrag: Option<String>,
    /// Remote ICE password
    remote_pwd: Option<String>,
    /// Local candidates
    local_candidates: Vec<IceCandidate>,
    /// Remote candidates
    remote_candidates: Vec<IceCandidate>,
    /// Candidate pairs for connectivity checks
    candidate_pairs: Vec<CandidatePair>,
    /// STUN server addresses
    stun_servers: Vec<String>,
    /// ICE role (true = controlling)
    controlling: bool,
    /// ICE tie-breaker value
    tie_breaker: u64,
    /// Component ID (1 = RTP, 2 = RTCP)
    component: u16,
    /// Local port for gathering (0 = auto-assign)
    local_port: u16,
    /// Bound UDP socket (created during gathering)
    socket: Option<std::sync::Arc<UdpSocket>>,
}

impl IceAgent {
    /// Create a new ICE Agent with random credentials
    ///
    /// # Arguments
    /// * `component` - Component ID (1 = RTP, 2 = RTCP)
    /// * `local_port` - Local port to bind for gathering
    /// * `stun_servers` - List of STUN server addresses (e.g., "stun.l.google.com:19302")
    pub fn new(component: u16, local_port: u16, stun_servers: Vec<String>) -> Self {
        let (ufrag, pwd) = Self::generate_credentials();
        let mut tie_breaker_bytes = [0u8; 8];
        SysRng
            .try_fill_bytes(&mut tie_breaker_bytes)
            .expect("OS RNG must not fail");
        let tie_breaker = u64::from_ne_bytes(tie_breaker_bytes);

        info!(
            "Created ICE Agent for component {} with ufrag={}",
            component, ufrag
        );

        Self {
            local_ufrag: ufrag,
            local_pwd: pwd,
            remote_ufrag: None,
            remote_pwd: None,
            local_candidates: Vec::new(),
            remote_candidates: Vec::new(),
            candidate_pairs: Vec::new(),
            stun_servers,
            controlling: true, // Default to controlling role (configurable via set_controlling)
            tie_breaker,
            component,
            local_port,
            socket: None,
        }
    }

    /// Generate random ICE credentials (ufrag and password)
    ///
    /// Generates random ICE ufrag and password using OS entropy, hex-encoded for transport
    fn generate_credentials() -> (String, String) {
        let mut ufrag_bytes = [0u8; 16];
        let mut pwd_bytes = [0u8; 32];

        SysRng
            .try_fill_bytes(&mut ufrag_bytes)
            .expect("OS RNG must not fail");
        SysRng
            .try_fill_bytes(&mut pwd_bytes)
            .expect("OS RNG must not fail");

        // Hex-encode to printable strings with full-byte entropy
        let ufrag = hex::encode(ufrag_bytes);
        let pwd = hex::encode(pwd_bytes);

        (ufrag, pwd)
    }

    /// Get local ICE credentials
    pub fn get_local_credentials(&self) -> (&str, &str) {
        (&self.local_ufrag, &self.local_pwd)
    }

    /// Set remote ICE credentials
    ///
    /// # Arguments
    /// * `ufrag` - Remote username fragment (minimum 4 characters per RFC 8445)
    /// * `pwd` - Remote password (minimum 22 characters per RFC 8445)
    ///
    /// # Errors
    /// Returns error if credentials are empty or too short
    pub fn set_remote_credentials(&mut self, ufrag: String, pwd: String) -> Result<()> {
        if ufrag.is_empty() || pwd.is_empty() {
            return Err(forge_core::ForgeError::Ice(
                "ICE credentials cannot be empty".to_string(),
            ));
        }
        if ufrag.len() < 4 {
            return Err(forge_core::ForgeError::Ice(format!(
                "ICE ufrag too short: {} chars (minimum 4 per RFC 8445)",
                ufrag.len()
            )));
        }
        if pwd.len() < 22 {
            return Err(forge_core::ForgeError::Ice(format!(
                "ICE password too short: {} chars (minimum 22 per RFC 8445)",
                pwd.len()
            )));
        }

        debug!("Setting remote ICE credentials: ufrag={}", ufrag);
        self.remote_ufrag = Some(ufrag);
        self.remote_pwd = Some(pwd);
        Ok(())
    }

    /// Gather local candidates (host and server-reflexive)
    ///
    /// This performs the full candidate gathering process:
    /// 1. Bind a UDP socket (if not already bound)
    /// 2. Gather host candidates from local network interfaces
    /// 3. Query STUN servers to discover server-reflexive candidates
    pub async fn gather_candidates(&mut self) -> Result<()> {
        // Bind a UDP socket if we don't have one yet
        if self.socket.is_none() {
            let bind_addr = if self.local_port == 0 {
                // Let OS assign a port
                "0.0.0.0:0"
            } else {
                // Use specified port
                &format!("0.0.0.0:{}", self.local_port)
            };

            let socket = UdpSocket::bind(bind_addr).await.map_err(|e| {
                forge_core::ForgeError::Ice(format!("Failed to bind UDP socket: {}", e))
            })?;

            let local_addr = socket.local_addr().map_err(|e| {
                forge_core::ForgeError::Ice(format!("Failed to get local address: {}", e))
            })?;

            // Update local_port with the actual bound port
            self.local_port = local_addr.port();

            info!("Bound UDP socket to {}", local_addr);
            self.socket = Some(std::sync::Arc::new(socket));
        }

        info!(
            "Gathering ICE candidates for component {} on port {}",
            self.component, self.local_port
        );

        // Gather host candidates
        let host_candidates = gather_host_candidates(self.component, self.local_port).await?;

        info!("Gathered {} host candidates", host_candidates.len());

        for candidate in &host_candidates {
            debug!("  Host: {}", candidate);
        }

        // Gather server-reflexive candidates using STUN
        let srflx_candidates = gather_server_reflexive_candidates(
            &host_candidates,
            &self.stun_servers,
            self.component,
        )
        .await?;

        info!(
            "Gathered {} server-reflexive candidates",
            srflx_candidates.len()
        );

        for candidate in &srflx_candidates {
            debug!("  ServerReflexive: {}", candidate);
        }

        // Store all candidates
        self.local_candidates.clear();
        self.local_candidates.extend(host_candidates);
        self.local_candidates.extend(srflx_candidates);

        info!("Total local candidates: {}", self.local_candidates.len());

        Ok(())
    }

    /// Add a remote candidate
    pub fn add_remote_candidate(&mut self, candidate: IceCandidate) {
        debug!("Adding remote candidate: {}", candidate);

        // Check for duplicates
        if self.remote_candidates.iter().any(|c| {
            c.ip == candidate.ip
                && c.port == candidate.port
                && c.component == candidate.component
                && c.protocol == candidate.protocol
        }) {
            debug!("Ignoring duplicate remote candidate");
            return;
        }

        self.remote_candidates.push(candidate);
    }

    /// Form candidate pairs from local and remote candidates
    ///
    /// Creates pairs of (local, remote) candidates and computes their priorities
    /// according to RFC 8445 Section 6.1.2
    pub fn form_candidate_pairs(&mut self) {
        info!("Forming candidate pairs");

        self.candidate_pairs.clear();

        for local in &self.local_candidates {
            for remote in &self.remote_candidates {
                // Only pair candidates for the same component
                if local.component != remote.component {
                    continue;
                }

                // Only pair candidates for the same protocol (UDP/TCP)
                if local.protocol != remote.protocol {
                    continue;
                }

                let pair = CandidatePair::new(local.clone(), remote.clone());

                debug!(
                    "Formed pair (priority={}): {} <-> {}",
                    pair.priority, pair.local, pair.remote
                );

                self.candidate_pairs.push(pair);
            }
        }

        // Sort pairs by priority (descending)
        self.candidate_pairs
            .sort_by_key(|pair| std::cmp::Reverse(pair.priority));

        info!("Formed {} candidate pairs", self.candidate_pairs.len());
    }

    /// Perform connectivity checks on all candidate pairs
    ///
    /// Runs connectivity checks sequentially in priority order (highest first)
    /// and stops after finding a working pair. Updates pair states.
    ///
    /// Returns true if at least one pair succeeded, false if all failed.
    pub async fn perform_connectivity_checks(&mut self) -> Result<bool> {
        if self.candidate_pairs.is_empty() {
            info!("No candidate pairs to check");
            return Ok(false);
        }

        let (remote_ufrag, remote_pwd) = match (&self.remote_ufrag, &self.remote_pwd) {
            (Some(ufrag), Some(pwd)) => (ufrag.as_str(), pwd.as_str()),
            _ => {
                return Err(forge_core::ForgeError::Ice(
                    "Remote ICE credentials not set".to_string(),
                ))
            }
        };

        let auth = IceAuthContext {
            local_ufrag: &self.local_ufrag,
            local_pwd: &self.local_pwd,
            remote_ufrag,
            remote_pwd,
            controlling: self.controlling,
            tie_breaker: self.tie_breaker,
        };

        info!(
            "Performing connectivity checks on {} pairs",
            self.candidate_pairs.len()
        );

        // Perform checks (they're already sorted by priority)
        match perform_checks(&mut self.candidate_pairs, Some(&auth)).await? {
            Some(index) => {
                info!(
                    "Connectivity checks complete: selected pair {} with priority {}",
                    index, self.candidate_pairs[index].priority
                );
                Ok(true)
            }
            None => {
                info!("All connectivity checks failed");
                Ok(false)
            }
        }
    }

    /// Select the best working candidate pair
    ///
    /// Finds the highest priority successful pair. This is called after
    /// connectivity checks have been performed.
    ///
    /// Returns the index of the selected pair, or None if no pairs succeeded.
    pub fn select_best_pair(&self) -> Option<usize> {
        // Find the highest priority successful pair
        // (pairs are already sorted by priority descending)
        self.candidate_pairs
            .iter()
            .enumerate()
            .find(|(_, pair)| pair.state == PairState::Succeeded)
            .map(|(index, _)| index)
    }

    /// Nominate the selected candidate pair
    ///
    /// Marks the best successful pair as the nominated pair. In full ICE,
    /// this would involve sending a USE-CANDIDATE attribute.
    pub fn nominate_pair(&mut self) -> Option<usize> {
        if let Some(index) = self.select_best_pair() {
            info!(
                "Nominated pair {}: {} <-> {}",
                index, self.candidate_pairs[index].local, self.candidate_pairs[index].remote
            );
            Some(index)
        } else {
            None
        }
    }

    /// Get all local candidates
    pub fn get_local_candidates(&self) -> &[IceCandidate] {
        &self.local_candidates
    }

    /// Get all remote candidates
    pub fn get_remote_candidates(&self) -> &[IceCandidate] {
        &self.remote_candidates
    }

    /// Get all candidate pairs
    pub fn get_candidate_pairs(&self) -> &[CandidatePair] {
        &self.candidate_pairs
    }

    /// Get the selected candidate pair (if ICE has completed)
    pub fn get_selected_pair(&self) -> Option<&CandidatePair> {
        self.candidate_pairs
            .iter()
            .find(|pair| pair.state == PairState::Succeeded)
    }

    /// Get the local address from the selected pair
    pub fn get_local_address(&self) -> Option<SocketAddr> {
        self.get_selected_pair()
            .map(|pair| SocketAddr::new(pair.local.ip, pair.local.port))
    }

    /// Get the remote address from the selected pair
    pub fn get_remote_address(&self) -> Option<SocketAddr> {
        self.get_selected_pair()
            .map(|pair| SocketAddr::new(pair.remote.ip, pair.remote.port))
    }

    /// Check if ICE has completed (at least one pair succeeded)
    pub fn is_completed(&self) -> bool {
        self.candidate_pairs
            .iter()
            .any(|pair| pair.state == PairState::Succeeded)
    }

    /// Get component ID
    pub fn component(&self) -> u16 {
        self.component
    }

    /// Set ICE role (controlling vs controlled)
    pub fn set_controlling(&mut self, controlling: bool) {
        self.controlling = controlling;
    }

    /// Get the bound UDP socket (if available)
    ///
    /// Returns the socket created during candidate gathering.
    /// Returns None if candidates haven't been gathered yet.
    pub fn get_socket(&self) -> Option<std::sync::Arc<UdpSocket>> {
        self.socket.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_credentials() {
        let (ufrag, pwd) = IceAgent::generate_credentials();

        // Check lengths (hex-encoded)
        assert_eq!(ufrag.len(), 32);
        assert_eq!(pwd.len(), 64);

        // Check that they're alphanumeric
        assert!(ufrag.chars().all(|c| c.is_alphanumeric()));
        assert!(pwd.chars().all(|c| c.is_alphanumeric()));

        // Check that multiple generations produce different values
        let (ufrag2, pwd2) = IceAgent::generate_credentials();
        assert_ne!(ufrag, ufrag2);
        assert_ne!(pwd, pwd2);
    }

    #[test]
    fn test_ice_agent_creation() {
        let agent = IceAgent::new(1, 50000, vec!["stun.l.google.com:19302".to_string()]);

        assert_eq!(agent.component(), 1);
        assert_eq!(agent.local_port, 50000);
        assert_eq!(agent.stun_servers.len(), 1);
        assert!(agent.local_candidates.is_empty());
        assert!(agent.remote_candidates.is_empty());
        assert!(!agent.is_completed());
    }

    #[test]
    fn test_set_remote_credentials() {
        // Test 1: Valid credentials (meets RFC 8445 minimums)
        let mut agent = IceAgent::new(1, 50000, vec![]);
        let result = agent.set_remote_credentials(
            "remote123".to_string(),
            "remotepwd4567890123456".to_string(), // 22 chars minimum
        );
        assert!(result.is_ok());
        assert_eq!(agent.remote_ufrag, Some("remote123".to_string()));
        assert_eq!(agent.remote_pwd, Some("remotepwd4567890123456".to_string()));

        // Test 2: Empty ufrag
        let mut agent = IceAgent::new(1, 50000, vec![]);
        let result =
            agent.set_remote_credentials("".to_string(), "validpassword12345678901".to_string());
        assert!(result.is_err());

        // Test 3: Empty password
        let mut agent = IceAgent::new(1, 50000, vec![]);
        let result = agent.set_remote_credentials("valid".to_string(), "".to_string());
        assert!(result.is_err());

        // Test 4: Ufrag too short (< 4 chars)
        let mut agent = IceAgent::new(1, 50000, vec![]);
        let result =
            agent.set_remote_credentials("abc".to_string(), "validpassword12345678901".to_string());
        assert!(result.is_err());

        // Test 5: Password too short (< 22 chars)
        let mut agent = IceAgent::new(1, 50000, vec![]);
        let result = agent.set_remote_credentials("valid".to_string(), "short".to_string());
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_gather_candidates() {
        let mut agent = IceAgent::new(1, 0, vec![]); // Port 0 = OS assigns

        let result = agent.gather_candidates().await;
        assert!(result.is_ok());

        // Should have gathered at least some host candidates
        // (exact number depends on network interfaces available)
        let local_candidates = agent.get_local_candidates();
        println!("Gathered {} local candidates", local_candidates.len());

        for candidate in local_candidates {
            println!("  - {}", candidate);
        }
    }
}
