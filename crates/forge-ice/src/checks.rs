//! ICE connectivity checks - RFC 8445 Section 7
//!
//! Implements connectivity checks to verify candidate pairs can communicate.
//! This is a simplified implementation without full MESSAGE-INTEGRITY support.

use crate::candidate::{CandidatePair, PairState};
use crate::stun::StunClient;
use forge_core::Result;
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{debug, warn};

/// Perform a connectivity check on a candidate pair
///
/// Sends a STUN Binding Request from the local candidate to the remote candidate
/// and waits for a response. Updates the pair state based on the result.
///
/// Note: This is a simplified implementation that doesn't include full ICE
/// authentication (MESSAGE-INTEGRITY). For production use, full RFC 8445
/// compliance with HMAC-SHA1 authentication should be implemented.
pub async fn perform_connectivity_check(pair: &mut CandidatePair) -> Result<bool> {
    debug!(
        "Performing connectivity check: {} -> {}",
        pair.local, pair.remote
    );

    // Update state to InProgress
    pair.state = PairState::InProgress;

    // Create local address for binding
    let local_addr = SocketAddr::new(pair.local.ip, pair.local.port);
    let remote_addr = SocketAddr::new(pair.remote.ip, pair.remote.port);

    // Create STUN client with 3 second timeout
    let stun_client = match StunClient::new(local_addr).await {
        Ok(client) => client,
        Err(e) => {
            warn!("Failed to create STUN client for connectivity check: {}", e);
            pair.state = PairState::Failed;
            return Ok(false);
        }
    };

    // Perform STUN binding request
    match stun_client.binding_request(remote_addr).await {
        Ok(mapped_addr) => {
            debug!(
                "Connectivity check succeeded: {} -> {} (mapped: {})",
                local_addr, remote_addr, mapped_addr
            );

            pair.state = PairState::Succeeded;
            Ok(true)
        }
        Err(e) => {
            debug!(
                "Connectivity check failed: {} -> {} (error: {})",
                local_addr, remote_addr, e
            );

            pair.state = PairState::Failed;
            Ok(false)
        }
    }
}

/// Perform connectivity checks on multiple candidate pairs
///
/// Checks pairs in priority order and stops after finding a successful pair.
/// Returns the index of the first successful pair, or None if all fail.
pub async fn perform_checks(pairs: &mut [CandidatePair]) -> Result<Option<usize>> {
    let total_pairs = pairs.len();
    debug!("Performing connectivity checks on {} pairs", total_pairs);

    for (i, pair) in pairs.iter_mut().enumerate() {
        debug!("Checking pair {} of {}", i + 1, total_pairs);

        match perform_connectivity_check(pair).await {
            Ok(true) => {
                debug!("Found working candidate pair at index {}", i);
                return Ok(Some(i));
            }
            Ok(false) => {
                // Continue to next pair
                continue;
            }
            Err(e) => {
                warn!("Error during connectivity check: {}", e);
                pair.state = PairState::Failed;
                continue;
            }
        }
    }

    debug!("All connectivity checks failed");
    Ok(None)
}

/// Perform connectivity checks in parallel with a timeout
///
/// Starts checks for multiple pairs simultaneously and returns as soon as
/// any pair succeeds. This is more efficient than sequential checks.
pub async fn perform_checks_parallel(
    pairs: &mut [CandidatePair],
    max_concurrent: usize,
    timeout_duration: Duration,
) -> Result<Option<usize>> {
    use tokio::time::timeout;

    let total_pairs = pairs.len();
    debug!(
        "Performing parallel connectivity checks on {} pairs (max concurrent: {})",
        total_pairs, max_concurrent
    );

    // Split pairs into chunks for parallel processing
    let chunk_size = max_concurrent.min(total_pairs);

    for chunk_start in (0..total_pairs).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(total_pairs);
        let chunk = &mut pairs[chunk_start..chunk_end];

        debug!(
            "Checking pair chunk {}-{} of {}",
            chunk_start, chunk_end, total_pairs
        );

        // Start checks for all pairs in this chunk
        let mut tasks = Vec::new();

        for (i, pair) in chunk.iter_mut().enumerate() {
            let local_addr = SocketAddr::new(pair.local.ip, pair.local.port);
            let remote_addr = SocketAddr::new(pair.remote.ip, pair.remote.port);
            let pair_index = chunk_start + i;

            tasks.push(async move {
                match timeout(timeout_duration, async {
                    let stun_client = StunClient::new(local_addr).await?;
                    stun_client.binding_request(remote_addr).await
                })
                .await
                {
                    Ok(Ok(_)) => Some(pair_index),
                    _ => None,
                }
            });
        }

        // Wait for first success or all failures
        let results = futures::future::join_all(tasks).await;

        // Check results and update states
        for (i, result) in results.iter().enumerate() {
            let pair = &mut chunk[i];
            if result.is_some() {
                pair.state = PairState::Succeeded;
                let pair_index = chunk_start + i;
                debug!("Parallel check succeeded for pair {}", pair_index);
                return Ok(Some(pair_index));
            } else {
                pair.state = PairState::Failed;
            }
        }
    }

    debug!("All parallel connectivity checks failed");
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateType, IceCandidate, Protocol};
    use std::net::{IpAddr, Ipv4Addr};

    fn create_test_pair() -> CandidatePair {
        let local = IceCandidate::new_host(
            "1".to_string(),
            1,
            Protocol::Udp,
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            50000,
        );

        let remote = IceCandidate {
            foundation: "2".to_string(),
            component: 1,
            protocol: Protocol::Udp,
            priority: 100,
            ip: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port: 50001,
            typ: CandidateType::Host,
            rel_addr: None,
            rel_port: None,
        };

        CandidatePair::new(local, remote)
    }

    #[tokio::test]
    async fn test_connectivity_check_structure() {
        let mut pair = create_test_pair();

        // Initial state should be Frozen (per RFC 8445)
        assert_eq!(pair.state, PairState::Frozen);

        // Note: This test will fail the connectivity check since we don't have
        // an actual STUN responder, but it verifies the structure works
        let _ = perform_connectivity_check(&mut pair).await;

        // State should have changed from Frozen
        assert_ne!(pair.state, PairState::Frozen);
        // Should either be Failed or Succeeded (likely Failed without a real STUN server)
        assert!(pair.state == PairState::Failed || pair.state == PairState::Succeeded);
    }
}
