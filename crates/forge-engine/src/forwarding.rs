//! RTP packet forwarding engine

use crate::session::{MediaSession, Participant, RecordingSide, SessionState};
use forge_core::{ForgeError, Result};
use forge_rtp::rtcp::RtcpPacket;
use metrics::{counter, histogram};
use std::sync::Arc;
use tokio::sync::RwLock;

/// RTP packet forwarding engine
pub struct ForwardingEngine;

impl ForwardingEngine {
    /// Start the RTP forwarding loop for a session
    ///
    /// This spawns a task that continuously receives RTP packets from the socket
    /// and forwards them to the appropriate participant.
    pub async fn start_forwarding(
        session: Arc<MediaSession>,
    ) -> Result<tokio::task::JoinHandle<()>> {
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
                tracing::info!(
                    "Session {} no longer active, stopping forwarding",
                    call_id.0
                );
                break;
            }

            // Wait for either RTP or RTCP packet
            tokio::select! {
                // Handle RTP packets (raw receive for SRTP support)
                result = sockets.recv_rtp_raw() => {
                    match result {
                        Ok((raw_data, source_addr)) => {
                            // Determine which leg sent this packet for SRTP unprotect
                            let srtp_ctx = {
                                let a = participant_a.read().await;
                                let b = participant_b.read().await;
                                if a.remote_addr == Some(source_addr) {
                                    session.srtp_a().clone()
                                } else if b.remote_addr == Some(source_addr) {
                                    session.srtp_b().clone()
                                } else {
                                    // Unknown source — use srtp_a by default for first-packet learning
                                    session.srtp_a().clone()
                                }
                            };

                            // SRTP unprotect (passthrough if no keys set)
                            let plain_data = {
                                let mut ctx = srtp_ctx.lock().await;
                                match ctx.unprotect_rtp(&raw_data) {
                                    Ok(data) => data,
                                    Err(e) => {
                                        tracing::warn!("SRTP unprotect failed for session {}: {}", call_id.0, e);
                                        counter!("forge_srtp_unprotect_errors_total", 1);
                                        continue;
                                    }
                                }
                            };

                            // Parse plain RTP
                            match forge_rtp::RtpPacket::parse(bytes::Bytes::from(plain_data)) {
                                Ok(packet) => {
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
                                    tracing::error!("RTP parse after SRTP unprotect failed for session {}: {}", call_id.0, e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error receiving RTP packet for session {}: {}", call_id.0, e);
                        }
                    }
                }
                // Handle RTCP packets (with SRTP unprotect)
                result = sockets.recv_rtcp() => {
                    match result {
                        Ok((raw_data, source_addr)) => {
                            // Determine which leg sent this RTCP
                            let rtcp_srtp_ctx = {
                                let a = participant_a.read().await;
                                if let Some(rtp_addr) = a.remote_addr {
                                    if rtp_addr.ip() == source_addr.ip()
                                        && (rtp_addr.port() + 1 == source_addr.port()
                                            || rtp_addr.port() == source_addr.port())
                                    {
                                        session.srtp_a().clone()
                                    } else {
                                        session.srtp_b().clone()
                                    }
                                } else {
                                    session.srtp_a().clone()
                                }
                            };

                            // SRTCP unprotect (passthrough if no keys)
                            let plain_data = {
                                let mut ctx = rtcp_srtp_ctx.lock().await;
                                match ctx.unprotect_rtcp(&raw_data) {
                                    Ok(data) => bytes::Bytes::from(data),
                                    Err(e) => {
                                        tracing::warn!("SRTCP unprotect failed for session {}: {}", call_id.0, e);
                                        counter!("forge_srtcp_unprotect_errors_total", 1);
                                        continue;
                                    }
                                }
                            };

                            Self::handle_rtcp_packet(
                                &session,
                                &sockets,
                                &participant_a,
                                &participant_b,
                                &plain_data,
                                source_addr,
                            )
                            .await;
                        }
                        Err(e) => {
                            tracing::error!("Error receiving RTCP packet for session {}: {}", call_id.0, e);
                        }
                    }
                }
                // Timeout check and AI audio response handling (run periodically)
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    // Check for timeout
                    if session.is_timed_out().await {
                        tracing::info!("Session {} timed out, stopping forwarding", call_id.0);
                        let _ = session.stop_forwarding().await;
                        break;
                    }

                    // Check for AI audio responses
                    if let Some(ai_manager) = session.ai_manager().await {
                        if let Some(audio_response) = ai_manager.try_recv_audio_response(&call_id).await {
                            Self::handle_ai_audio_response(
                                &session,
                                &sockets,
                                &participant_a,
                                &participant_b,
                                audio_response,
                            )
                            .await;
                        }
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

        // Check for RFC 2833 telephone-event packets (dynamic payload type per leg)
        let te_pt_a = session.telephone_event_pt_a();
        let te_pt_b = session.telephone_event_pt_b();
        let pkt_pt = packet.header.payload_type();
        if (pkt_pt == te_pt_a || pkt_pt == te_pt_b) && session.dtmf_config().enable_rfc2833 {
            tracing::debug!(
                "Received RFC 2833 telephone-event packet for session {} from {}",
                call_id.0,
                source_addr
            );

            // Process with DTMF detector
            let mut detector = session.dtmf_detector().lock().await;
            match detector.process_with_timestamp(&packet.payload, packet.header.timestamp) {
                Ok(events) => {
                    // Check deduplication before publishing
                    let mut dedup = session.dtmf_dedup().lock().await;
                    for event in events {
                        if dedup.should_publish(&event) {
                            tracing::info!(
                                "RFC 2833 DTMF detected for session {}: {} ({:?})",
                                call_id.0,
                                event.digit,
                                event.event_type
                            );

                            // Record metrics
                            counter!("forge_dtmf_events_total", 1, "method" => "rfc2833", "digit" => format!("{}", event.digit));
                            counter!("forge_dtmf_rfc2833_events_total", 1, "digit" => format!("{}", event.digit), "event_type" => format!("{:?}", event.event_type));

                            // Publish event to EventBus
                            if let Some(bus) = session.event_bus() {
                                let _ = bus.publish(event.to_forge_event(call_id.clone()));
                            }
                        } else {
                            tracing::debug!(
                                "RFC 2833 DTMF suppressed (duplicate) for session {}: {}",
                                call_id.0,
                                event.digit
                            );
                            counter!("forge_dtmf_duplicates_suppressed_total", 1, "method" => "rfc2833", "digit" => format!("{}", event.digit));
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

            session.update_activity().await;
            counter!("forge_dtmf_rfc2833_packets_total", 1);

            if !session.relay_rfc2833() {
                // Detect-only mode: consume the packet, don't forward
                return;
            }
            // Relay mode: fall through to normal forwarding path.
            // The packet's PT won't match audio codecs, so pcm_samples will be
            // empty — recording and inband detection are safely skipped.
            counter!("forge_dtmf_rfc2833_relayed_total", 1);
        }

        // Decode audio for recording and optional DTMF detection
        let payload_type = packet.header.payload_type();
        let opus_pt = session.dtmf_config().opus_payload_type;
        let is_opus_payload = opus_pt.map(|pt| payload_type == pt).unwrap_or(false);

        // Decode G.711 (payload types 0=PCMU, 8=PCMA) or Opus to PCM
        // This is needed for both recording AND inband DTMF detection
        let pcm_samples: Vec<i16> = if payload_type == 0 || payload_type == 8 || is_opus_payload {
            if payload_type == 0 {
                // PCMU (µ-law)
                packet
                    .payload
                    .iter()
                    .map(|&byte| forge_codecs::g711::decode_ulaw(byte))
                    .collect()
            } else if payload_type == 8 {
                // PCMA (A-law)
                packet
                    .payload
                    .iter()
                    .map(|&byte| forge_codecs::g711::decode_alaw(byte))
                    .collect()
            } else if is_opus_payload {
                // Opus - decode using the Opus decoder
                #[cfg(feature = "opus")]
                {
                    let mut decoder = session.opus_decoder().lock().await;
                    match decoder.decode(&packet.payload) {
                        Ok(samples) => samples,
                        Err(e) => {
                            tracing::trace!(
                                "Opus decoding failed for session {}: {}",
                                call_id.0,
                                e
                            );
                            Vec::new()
                        }
                    }
                }
                #[cfg(not(feature = "opus"))]
                {
                    tracing::trace!("Opus decoding disabled: opus feature not enabled");
                    Vec::new()
                }
            } else {
                // Shouldn't reach here, but handle gracefully
                Vec::new()
            }
        } else {
            // Not an audio codec we can decode
            Vec::new()
        };

        // Determine which participant sent this packet
        // Note: We match by IP only (not IP:port) to handle non-symmetric RTP where
        // phones send from different ports than they receive on (common with NAT)
        let (sender, receiver) = {
            let a = participant_a.read().await;
            let b = participant_b.read().await;

            // Check if source IP matches participant A's remote IP
            let a_ip_match = a.remote_addr.map(|addr| addr.ip() == source_addr.ip()).unwrap_or(false);
            // Check if source IP matches participant B's remote IP
            let b_ip_match = b.remote_addr.map(|addr| addr.ip() == source_addr.ip()).unwrap_or(false);

            if a_ip_match && !b_ip_match {
                // Packet from A (IP matches A only), forward to B
                (Side::A, Side::B)
            } else if b_ip_match && !a_ip_match {
                // Packet from B (IP matches B only), forward to A
                (Side::B, Side::A)
            } else if a_ip_match && b_ip_match {
                // Both IPs match (same IP for both legs, e.g., hairpin call)
                // Fall back to exact port match, then first-packet learning
                if a.remote_addr == Some(source_addr) {
                    (Side::A, Side::B)
                } else if b.remote_addr == Some(source_addr) {
                    (Side::B, Side::A)
                } else {
                    // Same IP, different port - use port proximity heuristic
                    let a_port = a.remote_addr.map(|addr| addr.port()).unwrap_or(0);
                    let b_port = b.remote_addr.map(|addr| addr.port()).unwrap_or(0);
                    let src_port = source_addr.port();
                    let a_diff = (a_port as i32 - src_port as i32).abs();
                    let b_diff = (b_port as i32 - src_port as i32).abs();
                    if a_diff <= b_diff {
                        (Side::A, Side::B)
                    } else {
                        (Side::B, Side::A)
                    }
                }
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

        // Process decoded samples (if any)
        if !pcm_samples.is_empty() {
            // Audio tap for call recording (ALWAYS record if recorder is active)
            if let Some(recorder) = session.recorder.read().await.as_ref() {
                let recording_side = match sender {
                    Side::A => RecordingSide::A,
                    Side::B => RecordingSide::B,
                };
                let mixer = session.recording_mixer();
                let mut mixer_guard = mixer.lock().await;
                mixer_guard.push(call_id, recording_side, &pcm_samples, recorder);
            }

            // Audio tap for AI integration
            if let Some(ai_manager) = session.ai_manager().await {
                if ai_manager.has_ai(call_id) {
                    // Send audio samples to AI
                    if let Err(e) = ai_manager.send_audio(call_id, &pcm_samples).await {
                        tracing::debug!(
                            "Failed to send audio to AI for session {}: {}",
                            call_id.0,
                            e
                        );
                    }
                }
            }

            // Inband DTMF detection (only if enabled)
            if session.dtmf_config().enable_inband {
                counter!("forge_dtmf_inband_packets_processed_total", 1);

                let mut detector = session.inband_detector().lock().await;
                match detector.process_samples(&pcm_samples) {
                    Ok(events) => {
                        // Check deduplication before publishing
                        let mut dedup = session.dtmf_dedup().lock().await;
                        for event in events {
                            if dedup.should_publish(&event) {
                                tracing::info!(
                                    "Inband DTMF detected for session {}: {} ({:?})",
                                    call_id.0,
                                    event.digit,
                                    event.event_type
                                );

                                // Record metrics
                                counter!("forge_dtmf_events_total", 1, "method" => "inband", "digit" => format!("{}", event.digit));
                                counter!("forge_dtmf_inband_events_total", 1, "digit" => format!("{}", event.digit), "event_type" => format!("{:?}", event.event_type));

                                // Publish event to EventBus
                                if let Some(bus) = session.event_bus() {
                                    let _ = bus.publish(event.to_forge_event(call_id.clone()));
                                }
                            } else {
                                tracing::debug!(
                                    "Inband DTMF suppressed (duplicate) for session {}: {}",
                                    call_id.0,
                                    event.digit
                                );
                                counter!("forge_dtmf_duplicates_suppressed_total", 1, "method" => "inband", "digit" => format!("{}", event.digit));
                            }
                        }
                    }
                    Err(e) => {
                        tracing::trace!("Inband DTMF processing for session {}: {}", call_id.0, e);
                    }
                }
            }
            // Note: Continue with normal forwarding (recording and DTMF are passive)
        }

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

        // Transcode if needed (different codecs between participants)
        let packet = if session.transcoding_config().enable_transcoding {
            Self::handle_transcoding(
                session,
                &sender,
                &receiver,
                participant_a,
                participant_b,
                packet,
            )
            .await
        } else {
            packet
        };

        // Forward packet to receiver
        let receiver_addr = {
            let (a, b) = (participant_a.read().await, participant_b.read().await);
            match receiver {
                Side::A => a.remote_addr,
                Side::B => b.remote_addr,
            }
        };

        if let Some(addr) = receiver_addr {
            // Serialize packet
            let data = packet.to_bytes();

            // SRTP protect before sending (passthrough if no keys set)
            let srtp_ctx = match receiver {
                Side::A => session.srtp_a().clone(),
                Side::B => session.srtp_b().clone(),
            };
            let send_data = {
                let mut ctx = srtp_ctx.lock().await;
                match ctx.protect_rtp(&data) {
                    Ok(protected) => protected,
                    Err(e) => {
                        tracing::error!("SRTP protect failed for session {}: {}", call_id.0, e);
                        counter!("forge_srtp_protect_errors_total", 1);
                        return;
                    }
                }
            };

            if let Err(e) = sockets.send_rtp_to(&send_data, addr).await {
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

    /// Handle transcoding if participants use different codecs
    async fn handle_transcoding(
        session: &Arc<MediaSession>,
        sender: &Side,
        _receiver: &Side,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        mut packet: forge_rtp::RtpPacket,
    ) -> forge_rtp::RtpPacket {
        // Get payload types for both participants
        let (src_pt, dst_pt) = {
            let a = participant_a.read().await;
            let b = participant_b.read().await;
            match sender {
                Side::A => (a.payload_type, b.payload_type),
                Side::B => (b.payload_type, a.payload_type),
            }
        };

        // Check if transcoding is needed
        if src_pt == dst_pt {
            return packet; // Same codec, no transcoding needed
        }

        // Convert payload types to codec types
        let pt_map = session.transcoding_config().payload_type_map;
        let src_codec = match pt_map.to_codec(src_pt) {
            Some(codec) => codec,
            None => {
                tracing::trace!(
                    "Unknown source codec for PT {}, skipping transcoding",
                    src_pt
                );
                return packet;
            }
        };

        let dst_codec = match pt_map.to_codec(dst_pt) {
            Some(codec) => codec,
            None => {
                tracing::trace!(
                    "Unknown destination codec for PT {}, skipping transcoding",
                    dst_pt
                );
                return packet;
            }
        };

        // Initialize transcoder for this direction if needed
        let transcoder_result = match sender {
            Side::A => session.ensure_transcoder_a_to_b(src_codec, dst_codec).await,
            Side::B => session.ensure_transcoder_b_to_a(src_codec, dst_codec).await,
        };

        if let Err(e) = transcoder_result {
            tracing::error!("Failed to initialize transcoder: {}", e);
            return packet;
        }

        // Get the appropriate transcoder
        let transcoder = match sender {
            Side::A => session.transcoder_a_to_b(),
            Side::B => session.transcoder_b_to_a(),
        };

        // Transcode the payload
        let mut transcoder_guard = transcoder.lock().await;
        if let Some(ref mut tc) = *transcoder_guard {
            // Start timing transcoding operation
            let transcode_start = std::time::Instant::now();

            let input_len = packet.payload.len();
            match tc.transcode_rtp_payload(&packet.payload) {
                Ok(transcoded_payloads) => {
                    if !transcoded_payloads.is_empty() {
                        // Concatenate all transcoded frames into single payload.
                        // This handles cases where resampling produces multiple output frames
                        // (e.g., PCMU at 8kHz → G.722 at 16kHz, where 20ms PCMU becomes
                        // 320 samples that get split into two 160-sample G.722 frames).
                        let num_frames = transcoded_payloads.len();
                        let frame_sizes: Vec<usize> = transcoded_payloads.iter().map(|f| f.len()).collect();
                        let combined_payload: Vec<u8> = if num_frames == 1 {
                            transcoded_payloads.into_iter().next().unwrap()
                        } else {
                            let total_len: usize = frame_sizes.iter().sum();
                            let mut combined = Vec::with_capacity(total_len);
                            for frame in &transcoded_payloads {
                                combined.extend_from_slice(frame);
                            }
                            tracing::info!(
                                "Transcoding produced {} frames: {:?} (input {} bytes, output {} bytes)",
                                num_frames,
                                frame_sizes,
                                input_len,
                                total_len
                            );
                            combined
                        };

                        let output_len = combined_payload.len();

                        // Record successful transcoding duration
                        let transcode_duration = transcode_start.elapsed();
                        histogram!("forge_transcoding_duration_seconds", transcode_duration.as_secs_f64(),
                            "from_codec" => codec_name(src_codec),
                            "to_codec" => codec_name(dst_codec));

                        // Update packet with transcoded payload
                        packet.payload = combined_payload.into();

                        // Update payload type in header (preserve marker bit)
                        packet.header.marker_payload_type =
                            (packet.header.marker_payload_type & 0x80) | dst_pt;

                        // Record transcoding metrics with codec labels
                        counter!("forge_transcoding_packets_total", 1,
                            "from_codec" => codec_name(src_codec),
                            "to_codec" => codec_name(dst_codec));
                        counter!(
                            "forge_transcoding_bytes_total",
                            output_len as u64,
                            "from_codec" => codec_name(src_codec),
                            "to_codec" => codec_name(dst_codec)
                        );

                        tracing::trace!(
                            "Transcoded packet: {} → {} ({} bytes → {} bytes) in {:?}",
                            codec_name(src_codec),
                            codec_name(dst_codec),
                            packet.payload.len(),
                            output_len,
                            transcode_duration
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("Transcoding failed: {}, forwarding original packet", e);
                    counter!("forge_transcoding_errors_total", 1,
                        "from_codec" => codec_name(src_codec),
                        "to_codec" => codec_name(dst_codec));
                }
            }
        }

        packet
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
                        counter!(
                            "forge_rtcp_sender_packets_total",
                            sr.sender_packet_count as u64
                        );
                        counter!(
                            "forge_rtcp_sender_bytes_total",
                            sr.sender_octet_count as u64
                        );

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
                    b.remote_addr
                        .map(|addr| std::net::SocketAddr::new(addr.ip(), addr.port() + 1))
                } else if let Some(rtp_addr_b) = b.remote_addr {
                    if rtp_addr_b.ip() == source_addr.ip()
                        && (rtp_addr_b.port() + 1 == source_addr.port()
                            || rtp_addr_b.port() == source_addr.port())
                    {
                        // From B, forward to A
                        Some(std::net::SocketAddr::new(
                            rtp_addr.ip(),
                            rtp_addr.port() + 1,
                        ))
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

        // SRTP protect RTCP before forwarding
        if let Some(addr) = receiver_addr {
            // Determine receiver side for SRTP context
            let rtcp_srtp_ctx = {
                let a = participant_a.read().await;
                if let Some(rtp_addr) = a.remote_addr {
                    if rtp_addr.ip() == source_addr.ip()
                        && (rtp_addr.port() + 1 == source_addr.port()
                            || rtp_addr.port() == source_addr.port())
                    {
                        // From A → forward to B
                        session.srtp_b().clone()
                    } else {
                        session.srtp_a().clone()
                    }
                } else {
                    session.srtp_b().clone()
                }
            };

            let send_data = {
                let mut ctx = rtcp_srtp_ctx.lock().await;
                match ctx.protect_rtcp(data) {
                    Ok(protected) => protected,
                    Err(e) => {
                        tracing::error!("SRTCP protect failed for session {}: {}", call_id.0, e);
                        counter!("forge_srtcp_protect_errors_total", 1);
                        return;
                    }
                }
            };

            if let Err(e) = sockets.send_rtcp_to(&send_data, addr).await {
                tracing::error!(
                    "Failed to forward RTCP packet for session {}: {}",
                    call_id.0,
                    e
                );
            } else {
                tracing::trace!(
                    "Forwarded RTCP packet for session {} to {}",
                    call_id.0,
                    addr
                );

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

    /// Handle AI audio response and send to participants
    async fn handle_ai_audio_response(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        audio_response: crate::ai_integration::AIAudioResponse,
    ) {
        let call_id = session.call_id();

        tracing::debug!(
            "Processing AI audio response for session {}: {} samples @ {}Hz",
            call_id.0,
            audio_response.samples.len(),
            audio_response.sample_rate
        );

        // AI SSRC (fixed value for AI-generated audio)
        const AI_SSRC: u32 = 0xA1A1A1A1;

        // Generate a random starting sequence number (would be better to track per session)
        let base_seq_num: u16 = rand::random();

        // Send AI audio to both participants
        for (side, participant) in [(Side::A, participant_a), (Side::B, participant_b)] {
            let p = participant.read().await;

            // Skip if participant doesn't have a remote address yet
            let Some(remote_addr) = p.remote_addr else {
                tracing::debug!(
                    "Skipping AI audio for participant {:?} - no remote address",
                    side
                );
                continue;
            };

            let codec = p.codec_config.codec;
            let payload_type = p.codec_config.payload_type;
            let clock_rate = p.codec_config.clock_rate;

            tracing::trace!(
                "Encoding AI audio for participant {:?}: codec={:?}, pt={}, clock_rate={}",
                side,
                codec,
                payload_type,
                clock_rate
            );

            // Resample if needed
            let resampled_samples = if audio_response.sample_rate != clock_rate {
                Self::resample_audio(
                    &audio_response.samples,
                    audio_response.sample_rate,
                    clock_rate,
                )
            } else {
                audio_response.samples.clone()
            };

            // Encode audio to codec format
            let encoded_payload = match codec {
                forge_core::AudioCodec::PCMU => {
                    // G.711 µ-law encoding
                    use bytes::Bytes;
                    let encoded: Vec<u8> = resampled_samples
                        .iter()
                        .map(|&sample| forge_codecs::g711::encode_ulaw(sample))
                        .collect();
                    Bytes::from(encoded)
                }
                forge_core::AudioCodec::PCMA => {
                    // G.711 A-law encoding
                    use bytes::Bytes;
                    let encoded: Vec<u8> = resampled_samples
                        .iter()
                        .map(|&sample| forge_codecs::g711::encode_alaw(sample))
                        .collect();
                    Bytes::from(encoded)
                }
                forge_core::AudioCodec::Opus => {
                    #[cfg(feature = "opus")]
                    {
                        // Opus encoding
                        let mut encoder = session.opus_encoder().lock().await;
                        match encoder.encode(&resampled_samples) {
                            Ok(encoded) => bytes::Bytes::from(encoded),
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to encode AI audio to Opus for session {}: {}",
                                    call_id.0,
                                    e
                                );
                                continue;
                            }
                        }
                    }
                    #[cfg(not(feature = "opus"))]
                    {
                        tracing::warn!("Cannot encode AI audio to Opus - opus feature not enabled");
                        continue;
                    }
                }
                _ => {
                    tracing::warn!("Unsupported codec for AI audio: {:?}", codec);
                    continue;
                }
            };

            // Calculate RTP timestamp (samples since start)
            // For simplicity, we use a fixed base timestamp
            let timestamp: u32 = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u32)
                .wrapping_mul(clock_rate / 1000);

            // Create RTP packet
            let packet = forge_rtp::RtpPacket::build(
                payload_type,
                base_seq_num,
                timestamp,
                AI_SSRC,
                encoded_payload.clone(),
                false, // marker bit
            );

            // Serialize and send
            let packet_bytes = packet.to_bytes();
            let packet_len = packet_bytes.len();

            if let Err(e) = sockets.send_rtp_to(&packet_bytes, remote_addr).await {
                tracing::error!(
                    "Failed to send AI audio RTP packet for session {}: {}",
                    call_id.0,
                    e
                );
            } else {
                tracing::trace!(
                    "Sent AI audio RTP packet for session {} to {:?}: {} bytes",
                    call_id.0,
                    side,
                    packet_len
                );

                // Update statistics
                drop(p); // Release read lock before acquiring write lock
                let mut p = participant.write().await;
                p.stats.packets_sent += 1;
                p.stats.bytes_sent += packet_len as u64;

                // Record metrics
                counter!("forge_ai_audio_packets_sent_total", 1);
                counter!("forge_ai_audio_bytes_sent_total", packet_len as u64);
            }
        }
    }

    /// Simple audio resampling using linear interpolation
    fn resample_audio(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
        if from_rate == to_rate {
            return samples.to_vec();
        }

        let ratio = from_rate as f64 / to_rate as f64;
        let output_len = (samples.len() as f64 / ratio).ceil() as usize;
        let mut output = Vec::with_capacity(output_len);

        for i in 0..output_len {
            let src_index = (i as f64) * ratio;
            let src_index_floor = src_index.floor() as usize;
            let src_index_ceil = (src_index_floor + 1).min(samples.len() - 1);
            let frac = src_index - src_index_floor as f64;

            if src_index_floor >= samples.len() {
                break;
            }

            // Linear interpolation
            let sample_a = samples[src_index_floor] as f64;
            let sample_b = samples[src_index_ceil] as f64;
            let interpolated = sample_a + (sample_b - sample_a) * frac;

            output.push(interpolated.round() as i16);
        }

        output
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
        gauge!(
            "forge_rtcp_packets_lost_total",
            block.cumulative_lost as f64
        );

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

/// Helper function to get codec name for logging
fn codec_name(codec: forge_codecs::AudioCodecType) -> &'static str {
    match codec {
        forge_codecs::AudioCodecType::PCMU => "G.711 µ-law",
        forge_codecs::AudioCodecType::PCMA => "G.711 A-law",
        forge_codecs::AudioCodecType::G722 => "G.722",
        forge_codecs::AudioCodecType::G729 => "G.729",
        forge_codecs::AudioCodecType::Opus => "Opus",
        forge_codecs::AudioCodecType::PCM => "PCM",
    }
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

    #[test]
    fn test_resample_audio_same_rate() {
        let samples = vec![100i16, 200, 300, 400, 500];
        let from_rate = 16000;
        let to_rate = 16000;

        let resampled = ForwardingEngine::resample_audio(&samples, from_rate, to_rate);

        // Should be identical when rates are the same
        assert_eq!(resampled.len(), samples.len());
        assert_eq!(resampled, samples);
    }

    #[test]
    fn test_resample_audio_downsample() {
        let samples = vec![100i16, 200, 300, 400, 500, 600, 700, 800];
        let from_rate = 16000;
        let to_rate = 8000;

        let resampled = ForwardingEngine::resample_audio(&samples, from_rate, to_rate);

        // Downsampling from 16kHz to 8kHz should halve the length
        assert_eq!(resampled.len(), 4);
        assert!(resampled.len() < samples.len());
    }

    #[test]
    fn test_resample_audio_upsample() {
        let samples = vec![100i16, 200, 300, 400];
        let from_rate = 8000;
        let to_rate = 16000;

        let resampled = ForwardingEngine::resample_audio(&samples, from_rate, to_rate);

        // Upsampling from 8kHz to 16kHz should double the length
        assert_eq!(resampled.len(), 8);
        assert!(resampled.len() > samples.len());
    }

    #[test]
    fn test_resample_audio_empty() {
        let samples: Vec<i16> = vec![];
        let from_rate = 16000;
        let to_rate = 8000;

        let resampled = ForwardingEngine::resample_audio(&samples, from_rate, to_rate);

        assert_eq!(resampled.len(), 0);
    }

    #[test]
    fn test_resample_audio_single_sample() {
        let samples = vec![100i16];
        let from_rate = 16000;
        let to_rate = 8000;

        let resampled = ForwardingEngine::resample_audio(&samples, from_rate, to_rate);

        assert_eq!(resampled.len(), 1);
        // Single sample should be preserved
        assert_eq!(resampled[0], 100);
    }

    #[test]
    fn test_resample_audio_interpolation_quality() {
        // Test that interpolation produces reasonable values
        let samples = vec![0i16, 1000];
        let from_rate = 8000;
        let to_rate = 16000;

        let resampled = ForwardingEngine::resample_audio(&samples, from_rate, to_rate);

        // Should have 4 samples (doubling)
        assert_eq!(resampled.len(), 4);

        // First sample should be close to 0
        assert_eq!(resampled[0], 0);

        // Intermediate samples should be between 0 and 1000
        assert!(resampled[1] >= 0 && resampled[1] <= 1000);
        assert!(resampled[2] >= 0 && resampled[2] <= 1000);

        // Last sample should be close to 1000
        assert_eq!(resampled[3], 1000);
    }

    #[test]
    fn test_resample_audio_common_rates() {
        // Test common telephony sample rate conversions
        let samples = vec![100i16; 160]; // 20ms at 8kHz

        // 8kHz → 16kHz (common for G.711 → AI)
        let resampled_16k = ForwardingEngine::resample_audio(&samples, 8000, 16000);
        assert_eq!(resampled_16k.len(), 320); // 20ms at 16kHz

        // 16kHz → 8kHz (common for AI → G.711)
        let resampled_8k = ForwardingEngine::resample_audio(&samples, 16000, 8000);
        assert_eq!(resampled_8k.len(), 80); // 20ms at 8kHz if input was 16kHz

        // 24kHz → 8kHz (OpenAI → G.711)
        let samples_24k = vec![100i16; 480]; // 20ms at 24kHz
        let resampled = ForwardingEngine::resample_audio(&samples_24k, 24000, 8000);
        assert_eq!(resampled.len(), 160); // 20ms at 8kHz
    }

    #[test]
    fn test_codec_name_helper() {
        use forge_codecs::AudioCodecType;

        assert_eq!(codec_name(AudioCodecType::PCMU), "G.711 µ-law");
        assert_eq!(codec_name(AudioCodecType::PCMA), "G.711 A-law");
        assert_eq!(codec_name(AudioCodecType::Opus), "Opus");
    }
}
