//! RTP packet forwarding engine

use crate::session::{MediaSession, Participant, SessionState};
use forge_core::{ForgeError, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

/// RTP packet forwarding engine
pub struct ForwardingEngine;

impl ForwardingEngine {
    /// Start the RTP forwarding loop for a session
    ///
    /// This spawns a task that continuously receives RTP packets from the socket
    /// and forwards them to the appropriate participant.
    pub async fn start_forwarding(session: Arc<MediaSession>) -> Result<tokio::task::JoinHandle<()>> {
        // Verify session is in correct state
        let state = session.state().await;
        if state != SessionState::Active {
            return Err(ForgeError::Internal(
                "Session must be active to start forwarding".to_string(),
            ));
        }

        let call_id = session.call_id().clone();
        tracing::info!("Starting RTP forwarding loop for session {}", call_id.0);

        // Spawn forwarding task
        let handle = tokio::spawn(async move {
            Self::forwarding_loop(session).await;
        });

        Ok(handle)
    }

    /// Main forwarding loop
    async fn forwarding_loop(session: Arc<MediaSession>) {
        let call_id = session.call_id();
        let sockets = session.sockets().clone();
        let participant_a = session.participant_a().clone();
        let participant_b = session.participant_b().clone();

        loop {
            // Check if session is still active
            let state = session.state().await;
            if state != SessionState::Active {
                tracing::info!("Session {} no longer active, stopping forwarding", call_id.0);
                break;
            }

            // Receive RTP packet
            let result = tokio::time::timeout(
                tokio::time::Duration::from_millis(100),
                sockets.recv_rtp(),
            )
            .await;

            match result {
                Ok(Ok((packet, source_addr))) => {
                    // Determine which participant sent this packet
                    let (sender, receiver) = {
                        let a = participant_a.read().await;
                        let b = participant_b.read().await;

                        if a.remote_addr == Some(source_addr) {
                            // Packet from A, forward to B
                            (Side::A, Side::B)
                        } else if b.remote_addr == Some(source_addr) {
                            // Packet from B, forward to A
                            (Side::B, Side::A)
                        } else {
                            // Unknown sender - learn the endpoint
                            tracing::debug!(
                                "Learning remote endpoint for session {}: {}",
                                call_id.0,
                                source_addr
                            );

                            // If participant A doesn't have an endpoint, assign it
                            if a.remote_addr.is_none() {
                                drop(a);
                                drop(b);
                                participant_a.write().await.remote_addr = Some(source_addr);
                                (Side::A, Side::B)
                            } else if b.remote_addr.is_none() {
                                drop(a);
                                drop(b);
                                participant_b.write().await.remote_addr = Some(source_addr);
                                (Side::B, Side::A)
                            } else {
                                // Both endpoints known but packet from unknown source
                                tracing::warn!(
                                    "Received packet from unknown source {} for session {}",
                                    source_addr,
                                    call_id.0
                                );
                                continue;
                            }
                        }
                    };

                    // Update sender statistics and session activity
                    let packet_len = packet.payload.len() as u64;
                    Self::update_stats(&sender, &participant_a, &participant_b, packet_len, true)
                        .await;

                    // Update session activity timestamp
                    session.update_activity().await;

                    // Forward packet to receiver
                    let receiver_addr = {
                        let (a, b) = (participant_a.read().await, participant_b.read().await);
                        match receiver {
                            Side::A => a.remote_addr,
                            Side::B => b.remote_addr,
                        }
                    };

                    if let Some(addr) = receiver_addr {
                        // Serialize and send packet
                        let data = packet.to_bytes();
                        if let Err(e) = sockets.send_rtp_to(&data, addr).await {
                            tracing::error!(
                                "Failed to forward RTP packet for session {}: {}",
                                call_id.0,
                                e
                            );
                        } else {
                            // Update receiver statistics
                            Self::update_stats(
                                &receiver,
                                &participant_a,
                                &participant_b,
                                packet_len,
                                false,
                            )
                            .await;
                        }
                    } else {
                        tracing::warn!(
                            "Cannot forward packet - receiver endpoint not yet learned for session {}",
                            call_id.0
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("Error receiving RTP packet for session {}: {}", call_id.0, e);
                    // Continue processing - transient errors shouldn't stop forwarding
                }
                Err(_) => {
                    // Timeout - this is normal, just continue
                }
            }

            // Check for session timeout
            if session.is_timed_out().await {
                tracing::info!("Session {} timed out, stopping forwarding", call_id.0);
                let _ = session.stop_forwarding().await;
                break;
            }
        }

        tracing::info!("Forwarding loop terminated for session {}", call_id.0);
    }

    /// Update participant statistics
    async fn update_stats(
        side: &Side,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        packet_len: u64,
        is_received: bool,
    ) {
        let participant = match side {
            Side::A => participant_a,
            Side::B => participant_b,
        };

        let mut p = participant.write().await;
        if is_received {
            p.stats.packets_received += 1;
            p.stats.bytes_received += packet_len;
            p.stats.last_packet_at = Some(std::time::Instant::now());
        } else {
            p.stats.packets_sent += 1;
            p.stats.bytes_sent += packet_len;
        }
    }
}

/// Which side of the session (A or B)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    A,
    B,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::MediaSessionConfig;
    use forge_core::{CallId, ParticipantId};
    use forge_rtp::{PortPool, PortPoolConfig};
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn test_forwarding_basic() {
        // Create port pool
        let config = PortPoolConfig::new(40000, 41000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        // Create session
        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session_config = MediaSessionConfig {
            socket_config: forge_rtp::RtpSocketConfig {
                bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                ..Default::default()
            },
            ..Default::default()
        };

        let session = Arc::new(
            MediaSession::new(
                call_id,
                participant_a,
                participant_b,
                &port_pool,
                session_config,
                None,
            )
            .await
            .unwrap(),
        );

        // Start forwarding
        session.start_forwarding().await.unwrap();

        // In a real test, we would:
        // 1. Create test clients that send RTP packets
        // 2. Verify packets are forwarded correctly
        // 3. Check statistics are updated
        // For now, just verify the session is active
        assert_eq!(session.state().await, SessionState::Active);

        // Stop forwarding
        session.stop_forwarding().await.unwrap();
        assert_eq!(session.state().await, SessionState::Terminated);
    }
}
