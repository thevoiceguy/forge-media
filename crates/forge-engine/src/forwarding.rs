//! RTP packet forwarding engine

use crate::session::{MediaSession, Participant, SessionState};
use forge_core::{ForgeError, Result};
use forge_rtp::rtcp::RtcpPacket;
use metrics::counter;
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
    #[tracing::instrument(skip(session), fields(call_id = %session.call_id().0))]
    async fn forwarding_loop(session: Arc<MediaSession>) {
        let call_id = session.call_id();
        let sockets = session.sockets().clone();
        let participant_a = session.participant_a().clone();
        let participant_b = session.participant_b().clone();

        tracing::debug!("Entering forwarding loop");

        loop {
            // Check if session is still active
            let state = session.state().await;
            if state != SessionState::Active {
                tracing::info!("Session {} no longer active, stopping forwarding", call_id.0);
                break;
            }

            // Wait for either RTP or RTCP packet
            tokio::select! {
                // Handle RTP packets
                result = sockets.recv_rtp() => {
                    match result {
                        Ok((packet, source_addr)) => {
                            Self::handle_rtp_packet(
                                &session,
                                &sockets,
                                &participant_a,
                                &participant_b,
                                packet,
                                source_addr,
                            )
                            .await;
                        }
                        Err(e) => {
                            tracing::error!("Error receiving RTP packet for session {}: {}", call_id.0, e);
                        }
                    }
                }
                // Handle RTCP packets
                result = sockets.recv_rtcp() => {
                    match result {
                        Ok((data, source_addr)) => {
                            Self::handle_rtcp_packet(
                                &session,
                                &sockets,
                                &participant_a,
                                &participant_b,
                                &data,
                                source_addr,
                            )
                            .await;
                        }
                        Err(e) => {
                            tracing::error!("Error receiving RTCP packet for session {}: {}", call_id.0, e);
                        }
                    }
                }
                // Timeout check (run periodically)
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    if session.is_timed_out().await {
                        tracing::info!("Session {} timed out, stopping forwarding", call_id.0);
                        let _ = session.stop_forwarding().await;
                        break;
                    }
                }
            }
        }

        tracing::info!("Forwarding loop terminated for session {}", call_id.0);
    }

    /// Handle an RTP packet
    async fn handle_rtp_packet(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        packet: forge_rtp::RtpPacket,
        source_addr: std::net::SocketAddr,
    ) {
        let call_id = session.call_id();

        // Check for RFC 2833 telephone-event packets (payload type 101)
        if packet.header.payload_type() == 101 {
            tracing::debug!(
                "Received RFC 2833 telephone-event packet for session {} from {}",
                call_id.0,
                source_addr
            );

            // Process with DTMF detector
            let mut detector = session.dtmf_detector().lock().await;
            match detector.process_with_timestamp(&packet.payload, packet.header.timestamp) {
                Ok(events) => {
                    for event in events {
                        tracing::info!(
                            "DTMF detected for session {}: {} ({:?})",
                            call_id.0,
                            event.digit,
                            event.event_type
                        );

                        // Publish event to EventBus
                        if let Some(bus) = session.event_bus() {
                            bus.publish(event.to_forge_event(call_id.clone()));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to process RFC 2833 packet for session {}: {}",
                        call_id.0,
                        e
                    );
                }
            }

            // Update activity but don't forward RFC 2833 packets
            session.update_activity().await;
            counter!("forge_dtmf_rfc2833_packets_total", 1);
            return;
        }

        // Check for inband DTMF in G.711 audio (payload types 0=PCMU, 8=PCMA)
        let payload_type = packet.header.payload_type();
        if payload_type == 0 || payload_type == 8 {
            // Decode G.711 to PCM samples for DTMF detection
            let pcm_samples: Vec<i16> = if payload_type == 0 {
                // PCMU (µ-law)
                packet.payload.iter().map(|&byte| {
                    forge_codecs::g711::decode_ulaw(byte)
                }).collect()
            } else {
                // PCMA (A-law)
                packet.payload.iter().map(|&byte| {
                    forge_codecs::g711::decode_alaw(byte)
                }).collect()
            };

            // Process with inband DTMF detector
            let mut detector = session.inband_detector().lock().await;
            match detector.process_samples(&pcm_samples) {
                Ok(events) => {
                    for event in events {
                        tracing::info!(
                            "Inband DTMF detected for session {}: {} ({:?})",
                            call_id.0,
                            event.digit,
                            event.event_type
                        );

                        // Publish event to EventBus
                        if let Some(bus) = session.event_bus() {
                            bus.publish(event.to_forge_event(call_id.clone()));
                        }
                    }
                }
                Err(e) => {
                    tracing::trace!(
                        "Inband DTMF processing for session {}: {}",
                        call_id.0,
                        e
                    );
                }
            }
            // Note: Continue with normal forwarding (inband detection is passive)
        }

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
                    "Learning remote RTP endpoint for session {}: {}",
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
                        "Received RTP packet from unknown source {} for session {}",
                        source_addr,
                        call_id.0
                    );
                    return;
                }
            }
        };

        // Check if both endpoints are now learned and activate XDP fast path
        #[cfg(all(target_os = "linux", feature = "xdp"))]
        {
            let (a_addr, b_addr) = {
                let a = participant_a.read().await;
                let b = participant_b.read().await;
                (a.remote_addr, b.remote_addr)
            };

            if a_addr.is_some() && b_addr.is_some() {
                // Both endpoints learned - activate XDP fast path
                if let Err(e) = session.activate_xdp_fast_path().await {
                    tracing::error!(
                        "Failed to activate XDP fast path for session {}: {}",
                        call_id.0,
                        e
                    );
                }
            }
        }

        // Update sender statistics and session activity
        let packet_len = packet.payload.len() as u64;
        Self::update_stats(&sender, participant_a, participant_b, packet_len, true).await;

        // Record metrics
        counter!("forge_rtp_packets_received_total", 1);
        counter!("forge_rtp_bytes_received_total", packet_len);

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
                Self::update_stats(&receiver, participant_a, participant_b, packet_len, false)
                    .await;

                // Record sent metrics
                counter!("forge_rtp_packets_sent_total", 1);
                counter!("forge_rtp_bytes_sent_total", packet_len);
            }
        } else {
            tracing::debug!(
                "Cannot forward RTP packet - receiver endpoint not yet learned for session {}",
                call_id.0
            );
        }
    }

    /// Handle an RTCP packet
    async fn handle_rtcp_packet(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        data: &bytes::Bytes,
        source_addr: std::net::SocketAddr,
    ) {
        let call_id = session.call_id();

        // Record RTCP metrics
        counter!("forge_rtcp_packets_received_total", 1);
        counter!("forge_rtcp_bytes_received_total", data.len() as u64);

        // Try to parse the RTCP packet for logging/debugging and metrics
        match RtcpPacket::parse(data) {
            Ok(rtcp_packet) => {
                tracing::debug!(
                    "Received RTCP packet for session {} from {}: {:?}",
                    call_id.0,
                    source_addr,
                    rtcp_packet
                );

                // Extract and record RTCP statistics
                match &rtcp_packet {
                    forge_rtp::rtcp::RtcpPacket::SenderReport(sr) => {
                        // Record sender statistics
                        counter!("forge_rtcp_sender_packets_total", sr.sender_packet_count as u64);
                        counter!("forge_rtcp_sender_bytes_total", sr.sender_octet_count as u64);

                        // Process reception report blocks
                        for block in &sr.report_blocks {
                            Self::record_report_block_metrics(block);
                        }
                    }
                    forge_rtp::rtcp::RtcpPacket::ReceiverReport(rr) => {
                        // Process reception report blocks
                        for block in &rr.report_blocks {
                            Self::record_report_block_metrics(block);
                        }
                    }
                    _ => {
                        // Other RTCP packet types (SDES, BYE, etc.)
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to parse RTCP packet for session {} from {}: {}",
                    call_id.0,
                    source_addr,
                    e
                );
                // Continue forwarding even if we can't parse it
            }
        }

        // Determine which participant sent this packet and forward to the other
        let receiver_addr = {
            let a = participant_a.read().await;
            let b = participant_b.read().await;

            // Check if this is from participant A (matching RTP port + 1)
            if let Some(rtp_addr) = a.remote_addr {
                if rtp_addr.ip() == source_addr.ip()
                    && (rtp_addr.port() + 1 == source_addr.port()
                        || rtp_addr.port() == source_addr.port())
                {
                    // From A, forward to B
                    b.remote_addr.map(|addr| {
                        std::net::SocketAddr::new(addr.ip(), addr.port() + 1)
                    })
                } else if let Some(rtp_addr_b) = b.remote_addr {
                    if rtp_addr_b.ip() == source_addr.ip()
                        && (rtp_addr_b.port() + 1 == source_addr.port()
                            || rtp_addr_b.port() == source_addr.port())
                    {
                        // From B, forward to A
                        Some(std::net::SocketAddr::new(rtp_addr.ip(), rtp_addr.port() + 1))
                    } else {
                        tracing::debug!(
                            "Received RTCP from unknown source {} for session {}",
                            source_addr,
                            call_id.0
                        );
                        None
                    }
                } else {
                    None
                }
            } else if let Some(rtp_addr_b) = b.remote_addr {
                if rtp_addr_b.ip() == source_addr.ip()
                    && (rtp_addr_b.port() + 1 == source_addr.port()
                        || rtp_addr_b.port() == source_addr.port())
                {
                    // From B, but A doesn't have an endpoint yet
                    None
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Update session activity
        session.update_activity().await;

        // Forward RTCP packet
        if let Some(addr) = receiver_addr {
            if let Err(e) = sockets.send_rtcp_to(data, addr).await {
                tracing::error!(
                    "Failed to forward RTCP packet for session {}: {}",
                    call_id.0,
                    e
                );
            } else {
                tracing::trace!("Forwarded RTCP packet for session {} to {}", call_id.0, addr);

                // Record sent metrics
                counter!("forge_rtcp_packets_sent_total", 1);
                counter!("forge_rtcp_bytes_sent_total", data.len() as u64);
            }
        } else {
            tracing::trace!(
                "Cannot forward RTCP packet - receiver endpoint not yet learned for session {}",
                call_id.0
            );
        }
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

    /// Record metrics from RTCP reception report block
    fn record_report_block_metrics(block: &forge_rtp::rtcp::ReceptionReportBlock) {
        use metrics::gauge;

        // Record packet loss metrics
        // fraction_lost is expressed as a fixed-point number with 8-bit fraction (0-255 = 0-100%)
        let packet_loss_fraction = (block.fraction_lost as f64) / 256.0;
        gauge!("forge_rtcp_packet_loss_fraction", packet_loss_fraction);
        gauge!("forge_rtcp_packets_lost_total", block.cumulative_lost as f64);

        // Record jitter (in timestamp units)
        gauge!("forge_rtcp_jitter", block.jitter as f64);

        // Record highest sequence number
        gauge!("forge_rtcp_highest_seq", block.extended_highest_seq as f64);
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
                None,
                None,
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
