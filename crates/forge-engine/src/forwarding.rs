//! RTP packet forwarding engine

use crate::media_bridge::{InboundMediaFrame, OutboundMediaRequest};
use crate::session::{MediaSession, Participant, ParticipantLabel, RecordingSide, SessionState};
use forge_core::{ForgeError, ForgeEvent, Result};
use forge_rtp::rtcp::RtcpPacket;
use metrics::{counter, histogram};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Minimum interval between RTCP Sender Reports we originate for a
/// generated stream (RFC 3550 §6.2 RECOMMENDED minimum). Reports gate the
/// RTT computation in [`forge_rtp::RttTracker`]; 5 s keeps RTCP a small
/// fraction of session bandwidth while still sampling RTT several times a
/// minute. See [`ForwardingEngine::maybe_emit_sender_reports`].
const SR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// RTP header fields threaded into [`ForwardingEngine::update_stats`] so a
/// received packet also updates the leg's receive-side stream statistics.
struct RxPacketMeta {
    sequence: u16,
    rtp_timestamp: u32,
    /// Negotiated RTP clock rate for jitter→ms conversion.
    clock_rate: u32,
    /// `false` for packets whose RTP timestamp does not track audio time
    /// (RFC 2833 telephone-events) — excluded from the jitter filter.
    count_jitter: bool,
}

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

        // 20 ms playout / housekeeping tick. Pinned across iterations
        // and reset on fire so inbound-RTP traffic doesn't cancel it.
        //
        // Before this pin, the tick lived as a `sleep(20ms)` arm
        // inside `tokio::select!`; whenever the RTP recv arm won the
        // race, the sleep was dropped and re-created from zero on the
        // next loop turn. At a steady 50 pps inbound that meant the
        // playout drain branch effectively never fired — packets piled
        // up in the scheduled-playout queue and got flushed in bursts
        // (10–20 packets within microseconds) whenever a brief gap in
        // inbound jitter let the sleep arm finally complete. The
        // receiver's jitter buffer dropped most of the burst and the
        // peer heard nothing intelligible.
        let playout_tick = tokio::time::sleep(tokio::time::Duration::from_millis(20));
        tokio::pin!(playout_tick);
        // Loop-local cadence gate for MediaStatsSnapshot publication; no
        // session-level state needed since only this loop emits.
        let mut last_media_stats_at: Option<std::time::Instant> = None;

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
                            // RFC 5764 §5.1.2 demux: peek the first byte
                            // to route DTLS handshake bytes away from the
                            // SRTP/RTP path. DTLS packets would otherwise
                            // crash `unprotect_rtp` with a parse error.
                            #[cfg(feature = "dtls")]
                            if let Some(first) = raw_data.first().copied() {
                                if crate::dtls_srtp::is_dtls_packet(first) {
                                    Self::handle_dtls_packet(
                                        &session,
                                        &sockets,
                                        &participant_a,
                                        &participant_b,
                                        &raw_data,
                                        source_addr,
                                    )
                                    .await;
                                    continue;
                                }
                                if crate::dtls_srtp::is_unsupported_first_byte(first) {
                                    counter!("forge_rtp_unsupported_first_byte_total", 1);
                                    continue;
                                }
                            }

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

                            // HEP3 capture (chunk 0x05 = RTCP). Hook
                            // after SRTCP unprotect so Homer sees the
                            // plaintext payload. No-op (one atomic
                            // load + null-check) when no emitter is
                            // installed. Local-addr lookup goes
                            // through the cached SocketPair handle so
                            // we don't pay a syscall per RTCP packet
                            // beyond what `local_rtcp_addr()` already
                            // does internally on the cached socket.
                            if let Some(emitter) = forge_hep::forge_hep() {
                                if let Ok(local) = sockets.local_rtcp_addr() {
                                    emitter.emit_rtcp(
                                        forge_hep::Direction::Inbound,
                                        forge_hep::IpProto::Udp,
                                        source_addr,
                                        local,
                                        &plain_data,
                                        Some(&call_id.0),
                                    );
                                }
                            }

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
                // Timeout check and generated audio playout (run periodically).
                // Tick is pinned outside the loop; reset on fire so it
                // continues at a steady 20 ms cadence regardless of
                // inbound RTP traffic.
                _ = &mut playout_tick => {
                    playout_tick
                        .as_mut()
                        .reset(tokio::time::Instant::now() + tokio::time::Duration::from_millis(20));
                    // Check for timeout
                    if session.is_timed_out().await {
                        tracing::info!("Session {} timed out, stopping forwarding", call_id.0);
                        let _ = session.stop_forwarding().await;
                        break;
                    }

                    #[cfg(feature = "ai")]
                    Self::drain_ai_audio_responses(
                        &session,
                        &sockets,
                        &participant_a,
                        &participant_b,
                    )
                    .await;
                    Self::drain_media_bridge_outbound(
                        &session,
                        &sockets,
                        &participant_a,
                        &participant_b,
                    )
                    .await;
                    Self::drain_scheduled_playout(
                        &session,
                        &sockets,
                        &participant_a,
                        &participant_b,
                    )
                    .await;
                    Self::maybe_emit_sender_reports(
                        &session,
                        &sockets,
                        &participant_a,
                        &participant_b,
                    )
                    .await;
                    Self::maybe_emit_media_stats(
                        &session,
                        &participant_a,
                        &participant_b,
                        &mut last_media_stats_at,
                    )
                    .await;
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

        let Some((sender, receiver)) =
            Self::determine_packet_sides(session, participant_a, participant_b, source_addr).await
        else {
            return;
        };

        let sender_codec = match sender {
            Side::A => participant_a.read().await.codec_config.clone(),
            Side::B => participant_b.read().await.codec_config.clone(),
        };

        let pcm_samples =
            Self::decode_audio_payload(session, call_id, &packet, &sender_codec, sender.label())
                .await;

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
            #[cfg(feature = "ai")]
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

            if let Some(media_bridge) = session.media_bridge_manager().await {
                let frame = InboundMediaFrame {
                    leg: sender.label(),
                    codec: sender_codec.codec,
                    payload_type: sender_codec.payload_type,
                    sample_rate: MediaSession::codec_audio_sample_rate(
                        sender_codec.codec,
                        sender_codec.clock_rate,
                    ),
                    timestamp: packet.header.timestamp,
                    sequence_number: packet.header.sequence_number,
                    samples: pcm_samples.clone(),
                };

                if let Err(e) = media_bridge.try_send_inbound_frame(call_id, frame) {
                    tracing::debug!(
                        "Failed to send inbound media frame to bridge for session {}: {}",
                        call_id.0,
                        e
                    );
                }
            }

            // Voice-activity detection. Drives `SpeechStarted` /
            // `SpeechStopped` events on the EventBus when the per-
            // session detector flips state under hysteresis. Runs on
            // every audio packet when `vad_config().enabled` is true;
            // skipped entirely when disabled so consumers paying the
            // call-quality cost (a cheap RMS + ZCR per frame) opt in.
            if session.vad_config().enabled {
                use forge_vad::VadState;
                let mut detector = session.vad_detector().lock().await;
                let prev = detector.state();
                let _ = detector.process(&pcm_samples);
                let new = detector.state();
                drop(detector);
                if new != prev {
                    let now = chrono::Utc::now();
                    let mut started_guard = session.speech_started_at().lock().await;
                    match new {
                        VadState::Speech => {
                            *started_guard = Some(now);
                            drop(started_guard);
                            if let Some(bus) = session.event_bus() {
                                let _ = bus.publish(ForgeEvent::SpeechStarted {
                                    call_id: call_id.clone(),
                                    timestamp: now,
                                });
                            }
                        }
                        VadState::Silence => {
                            let started_at = started_guard.take();
                            drop(started_guard);
                            let duration_ms = started_at
                                .map(|t| {
                                    (now - t)
                                        .to_std()
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0)
                                })
                                .unwrap_or(0);
                            if let Some(bus) = session.event_bus() {
                                let _ = bus.publish(ForgeEvent::SpeechStopped {
                                    call_id: call_id.clone(),
                                    timestamp: now,
                                    duration_ms,
                                });
                            }
                        }
                        VadState::Unknown => {
                            // Hysteresis says we don't fire yet.
                        }
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
        Self::update_stats(
            &sender,
            participant_a,
            participant_b,
            packet_len,
            true,
            Some(RxPacketMeta {
                sequence: packet.header.sequence_number,
                rtp_timestamp: packet.header.timestamp,
                // The negotiated RTP clock (48 kHz for Opus), not the
                // bridge/audio rate — jitter math runs in RTP timestamp
                // units.
                clock_rate: sender_codec.clock_rate,
                // Relayed RFC 2833 telephone-events reach this point but
                // hold their RTP timestamp for the digit's duration, which
                // would fake a transit swing; keep them out of the jitter
                // filter (they still count toward sequence stats).
                count_jitter: pkt_pt != te_pt_a && pkt_pt != te_pt_b,
            }),
        )
        .await;

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
                Self::update_stats(
                    &receiver,
                    participant_a,
                    participant_b,
                    packet_len,
                    false,
                    None,
                )
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

    #[cfg(feature = "ai")]
    async fn drain_ai_audio_responses(
        session: &Arc<MediaSession>,
        _sockets: &Arc<forge_rtp::RtpSocketPair>,
        _participant_a: &Arc<RwLock<Participant>>,
        _participant_b: &Arc<RwLock<Participant>>,
    ) {
        let call_id = session.call_id();

        if let Some(ai_manager) = session.ai_manager().await {
            while let Some(audio_response) = ai_manager.try_recv_audio_response(call_id).await {
                if let Err(e) = session
                    .schedule_audio_playout(
                        crate::media_bridge::MediaTarget::Both,
                        audio_response.sample_rate,
                        &audio_response.samples,
                        None,
                        crate::media_bridge::PlayoutMode::Append,
                        crate::session::ScheduledPlayoutSource::AI,
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to schedule AI audio response for session {}: {}",
                        call_id.0,
                        e
                    );
                }
            }
        }
    }

    async fn drain_media_bridge_outbound(
        session: &Arc<MediaSession>,
        _sockets: &Arc<forge_rtp::RtpSocketPair>,
        _participant_a: &Arc<RwLock<Participant>>,
        _participant_b: &Arc<RwLock<Participant>>,
    ) {
        let call_id = session.call_id();

        if let Some(media_bridge) = session.media_bridge_manager().await {
            while let Some(request) = media_bridge.try_recv_outbound_request(call_id).await {
                let result = match request {
                    OutboundMediaRequest::Audio(frame) => {
                        session
                            .schedule_audio_playout(
                                frame.target,
                                frame.sample_rate,
                                &frame.samples,
                                frame.playback_id,
                                frame.mode,
                                crate::session::ScheduledPlayoutSource::MediaBridgeAudio,
                            )
                            .await
                    }
                    OutboundMediaRequest::Dtmf(request) => {
                        session
                            .schedule_dtmf_playout(
                                request.target,
                                request.digit,
                                request.duration_ms,
                                request.playback_id,
                                request.mode,
                                crate::session::ScheduledPlayoutSource::MediaBridgeDtmf,
                            )
                            .await
                    }
                    OutboundMediaRequest::Flush {
                        target,
                        playback_id,
                    }
                    | OutboundMediaRequest::Stop {
                        target,
                        playback_id,
                    } => {
                        session
                            .clear_scheduled_playout(target, playback_id.as_deref())
                            .await;
                        Ok(())
                    }
                };

                if let Err(e) = result {
                    tracing::warn!(
                        "Failed to process outbound media request for session {}: {}",
                        call_id.0,
                        e
                    );
                }
            }
        }
    }

    async fn drain_scheduled_playout(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
    ) {
        let now = std::time::Instant::now();

        for leg in [ParticipantLabel::A, ParticipantLabel::B] {
            for item in session.take_due_playout_items(leg, now).await {
                if let Err(e) = Self::send_scheduled_playout_item(
                    session,
                    sockets,
                    participant_a,
                    participant_b,
                    leg,
                    item,
                )
                .await
                {
                    tracing::warn!(
                        "Failed to send scheduled playout for session {} leg {}: {}",
                        session.call_id().0,
                        leg.as_str(),
                        e
                    );
                }
            }
        }
    }

    /// Publish one [`ForgeEvent::MediaStatsSnapshot`] per leg that has
    /// received RTP, when the session's `media_stats_interval` cadence is
    /// configured and due. Called from the 20 ms housekeeping tick;
    /// `last_emit` is the loop-local gate shared by both legs so the two
    /// snapshots of a tick carry the same cadence.
    ///
    /// Counters are cumulative since call start (see the event docs), so a
    /// consumer that misses a snapshot loses resolution, not data — which
    /// is why publishing rides the lossy broadcast bus like every other
    /// `ForgeEvent`.
    async fn maybe_emit_media_stats(
        session: &Arc<MediaSession>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        last_emit: &mut Option<std::time::Instant>,
    ) {
        let Some(interval) = session.media_stats_interval() else {
            return;
        };
        let Some(bus) = session.event_bus() else {
            return;
        };
        let now = std::time::Instant::now();
        let due = last_emit
            .map(|t| now.duration_since(t) >= interval)
            .unwrap_or(true);
        if !due {
            return;
        }
        *last_emit = Some(now);

        for (leg, participant) in [
            (forge_core::MediaLeg::A, participant_a),
            (forge_core::MediaLeg::B, participant_b),
        ] {
            let rx = {
                let p = participant.read().await;
                if p.stats.rx_stream.packets_received == 0 {
                    continue; // leg hasn't received RTP (e.g. WS-only side)
                }
                p.stats.rx_stream.clone()
            };
            let _ = bus.publish(ForgeEvent::MediaStatsSnapshot {
                call_id: session.call_id().clone(),
                leg,
                rx_packets_received: rx.packets_received,
                rx_packets_lost: rx.packets_lost(),
                rx_packets_out_of_order: rx.packets_out_of_order,
                rx_packets_duplicate: rx.packets_duplicate,
                rx_jitter_ms: rx.jitter_ms(),
                timestamp: chrono::Utc::now(),
            });
        }
    }

    /// Originate RTCP Sender Reports for the generated audio streams.
    ///
    /// siphon-ai's audio toward the carrier is a *generated* RTP stream
    /// (forge's own SSRC), so the peer's Receiver Reports can only carry a
    /// usable `last_sr` / `delay_since_last_sr` — and thus let us compute
    /// RTT (RFC 3550 §A.7) — if we first send SRs it can echo back. Called
    /// from the 20 ms housekeeping tick but rate-limited to one SR per
    /// [`SR_INTERVAL`] per leg: §6.2 keeps RTCP a small fraction of session
    /// bandwidth, and 5 s is the RECOMMENDED minimum interval. We only emit
    /// once a leg has actually generated RTP and has a known remote address.
    async fn maybe_emit_sender_reports(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
    ) {
        let now = std::time::Instant::now();

        for leg in [ParticipantLabel::A, ParticipantLabel::B] {
            let remote_rtp = match leg {
                ParticipantLabel::A => participant_a.read().await.remote_addr,
                ParticipantLabel::B => participant_b.read().await.remote_addr,
            };
            let Some(remote_rtp) = remote_rtp else {
                continue;
            };

            // Build the SR under the state lock: gate on cadence + having
            // sent audio, snapshot the sender stats, and record the SR with
            // the RttTracker so a later RR can resolve to an RTT sample.
            let sr = {
                let state_arc = session.generated_rtp_state(leg);
                let mut state = state_arc.lock().await;
                if state.packets_sent == 0 {
                    continue; // nothing generated yet → nothing to report on
                }
                let due = state
                    .last_sr_at
                    .map(|t| now.duration_since(t) >= SR_INTERVAL)
                    .unwrap_or(true);
                if !due {
                    continue;
                }

                let ntp = forge_rtp::rtcp::ntp::now();
                let mut sr = forge_rtp::rtcp::SenderReport::new(state.ssrc);
                sr.ntp_timestamp_msw = (ntp >> 32) as u32;
                sr.ntp_timestamp_lsw = (ntp & 0xFFFF_FFFF) as u32;
                sr.rtp_timestamp = state.next_timestamp;
                sr.sender_packet_count = state.packets_sent;
                sr.sender_octet_count = state.octets_sent;

                state.rtt.record_outgoing_sr(ntp);
                state.last_sr_at = Some(now);
                sr
            };

            // Frame with the 4-byte RTCP common header (PT=SR, RC=0 — no
            // reception blocks; we're a terminator reporting on our own
            // generated stream).
            let payload = sr.to_bytes();
            let header = forge_rtp::rtcp::RtcpHeader {
                version: 2,
                padding: false,
                count: 0,
                packet_type: forge_rtp::rtcp::RtcpPacketType::SR,
                length: (((4 + payload.len()) / 4) - 1) as u16,
            };
            let mut packet = header.to_bytes();
            packet.extend_from_slice(&payload);

            // SRTCP-protect (passthrough when no keys are installed) and
            // send to the leg's RTCP address (RTP port + 1).
            let srtp_ctx = match leg {
                ParticipantLabel::A => session.srtp_a().clone(),
                ParticipantLabel::B => session.srtp_b().clone(),
            };
            let send_data = {
                let mut ctx = srtp_ctx.lock().await;
                match ctx.protect_rtcp(&packet) {
                    Ok(protected) => protected,
                    Err(e) => {
                        tracing::error!(
                            "SRTCP protect of generated SR failed for session {}: {}",
                            session.call_id().0,
                            e
                        );
                        counter!("forge_srtcp_protect_errors_total", 1);
                        continue;
                    }
                }
            };

            let rtcp_addr =
                std::net::SocketAddr::new(remote_rtp.ip(), remote_rtp.port().wrapping_add(1));
            if let Err(e) = sockets.send_rtcp_to(&send_data, rtcp_addr).await {
                tracing::warn!(
                    "Failed to send generated SR for session {} leg {}: {}",
                    session.call_id().0,
                    leg.as_str(),
                    e
                );
            } else {
                counter!("forge_rtcp_sender_reports_sent_total", 1);
                counter!("forge_rtcp_packets_sent_total", 1);
            }
        }
    }

    async fn determine_packet_sides(
        session: &Arc<MediaSession>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        source_addr: std::net::SocketAddr,
    ) -> Option<(Side, Side)> {
        let call_id = session.call_id();
        let a = participant_a.read().await;
        let b = participant_b.read().await;

        let a_ip_match = a
            .remote_addr
            .map(|addr| addr.ip() == source_addr.ip())
            .unwrap_or(false);
        let b_ip_match = b
            .remote_addr
            .map(|addr| addr.ip() == source_addr.ip())
            .unwrap_or(false);

        if a_ip_match && !b_ip_match {
            return Some((Side::A, Side::B));
        }
        if b_ip_match && !a_ip_match {
            return Some((Side::B, Side::A));
        }
        if a_ip_match && b_ip_match {
            if a.remote_addr == Some(source_addr) {
                return Some((Side::A, Side::B));
            }
            if b.remote_addr == Some(source_addr) {
                return Some((Side::B, Side::A));
            }

            let a_port = a.remote_addr.map(|addr| addr.port()).unwrap_or(0);
            let b_port = b.remote_addr.map(|addr| addr.port()).unwrap_or(0);
            let src_port = source_addr.port();
            let a_diff = (a_port as i32 - src_port as i32).abs();
            let b_diff = (b_port as i32 - src_port as i32).abs();
            return Some(if a_diff <= b_diff {
                (Side::A, Side::B)
            } else {
                (Side::B, Side::A)
            });
        }

        let source_ip = source_addr.ip();
        let a_can_latch = a.remote_addr.is_none()
            && a.latch_allowed_ips
                .as_ref()
                .is_none_or(|allowed| allowed.contains(&source_ip));
        let b_can_latch = b.remote_addr.is_none()
            && b.latch_allowed_ips
                .as_ref()
                .is_none_or(|allowed| allowed.contains(&source_ip));

        if a_can_latch {
            tracing::info!(
                "Learning remote RTP endpoint for session {} leg A: {}",
                call_id.0,
                source_addr
            );
            counter!("forge_rtp_latch_learned_total", 1);
            drop(a);
            drop(b);
            participant_a.write().await.remote_addr = Some(source_addr);
            return Some((Side::A, Side::B));
        }
        if b_can_latch {
            tracing::info!(
                "Learning remote RTP endpoint for session {} leg B: {}",
                call_id.0,
                source_addr
            );
            counter!("forge_rtp_latch_learned_total", 1);
            drop(a);
            drop(b);
            participant_b.write().await.remote_addr = Some(source_addr);
            return Some((Side::B, Side::A));
        }

        tracing::warn!(
            "Rejected RTP packet from {} for session {} (unknown source or disallowed by latch policy)",
            source_addr,
            call_id.0
        );
        counter!("forge_rtp_latch_rejected_total", 1);
        None
    }

    async fn decode_audio_payload(
        session: &Arc<MediaSession>,
        call_id: &forge_core::CallId,
        packet: &forge_rtp::RtpPacket,
        codec_config: &crate::session::ParticipantCodecConfig,
        leg: ParticipantLabel,
    ) -> Vec<i16> {
        if packet.header.payload_type() != codec_config.payload_type {
            return Vec::new();
        }

        match session
            .decode_with_codec_runtime(leg, codec_config.codec, &packet.payload)
            .await
        {
            Ok(samples) => samples,
            Err(e) => {
                tracing::trace!(
                    "Audio decode failed for session {} leg {}: {}",
                    call_id.0,
                    leg.as_str(),
                    e
                );
                Vec::new()
            }
        }
    }

    async fn send_scheduled_playout_item(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        leg: ParticipantLabel,
        item: crate::session::ScheduledPlayoutItem,
    ) -> Result<()> {
        let remote_addr = {
            let participant = match leg {
                ParticipantLabel::A => participant_a,
                ParticipantLabel::B => participant_b,
            };
            participant.read().await.remote_addr
        };

        let Some(remote_addr) = remote_addr else {
            tracing::debug!(
                "Skipping scheduled playout for session {} leg {} - no remote address",
                session.call_id().0,
                leg.as_str()
            );
            return Ok(());
        };

        let packet_len = match item.kind {
            crate::session::ScheduledPlayoutKind::Audio {
                codec,
                payload_type,
                samples,
                ..
            } => {
                let encoded = session
                    .encode_with_codec_runtime(leg, codec, &samples)
                    .await?;
                let packet_len = Self::send_generated_rtp_packet(
                    session,
                    sockets,
                    participant_a,
                    participant_b,
                    leg,
                    remote_addr,
                    payload_type,
                    item.timestamp,
                    item.stream_cursor_after,
                    bytes::Bytes::from(encoded),
                    item.marker,
                )
                .await?;

                counter!(
                    "forge_generated_audio_packets_sent_total",
                    1,
                    "source" => item.source.as_label()
                );
                counter!(
                    "forge_generated_audio_bytes_sent_total",
                    packet_len,
                    "source" => item.source.as_label()
                );
                if matches!(item.source, crate::session::ScheduledPlayoutSource::AI) {
                    counter!("forge_ai_audio_packets_sent_total", 1);
                    counter!("forge_ai_audio_bytes_sent_total", packet_len);
                }

                packet_len
            }
            crate::session::ScheduledPlayoutKind::Dtmf {
                payload_type,
                payload,
            } => {
                let packet_len = Self::send_generated_rtp_packet(
                    session,
                    sockets,
                    participant_a,
                    participant_b,
                    leg,
                    remote_addr,
                    payload_type,
                    item.timestamp,
                    item.stream_cursor_after,
                    bytes::Bytes::from(payload),
                    item.marker,
                )
                .await?;
                counter!("forge_dtmf_rfc2833_injected_packets_total", 1);
                counter!("forge_dtmf_rfc2833_injected_bytes_total", packet_len);
                packet_len
            }
        };

        counter!(
            "forge_generated_media_packets_sent_total",
            1,
            "source" => item.source.as_label()
        );
        counter!(
            "forge_generated_media_bytes_sent_total",
            packet_len,
            "source" => item.source.as_label()
        );

        Ok(())
    }

    async fn send_generated_rtp_packet(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        leg: ParticipantLabel,
        remote_addr: std::net::SocketAddr,
        payload_type: u8,
        timestamp: u32,
        stream_cursor_after: u32,
        payload: bytes::Bytes,
        marker: bool,
    ) -> Result<u64> {
        let (sequence_number, ssrc) = {
            let state = session.generated_rtp_state(leg);
            let mut state = state.lock().await;
            let sequence_number = state.next_sequence;
            let ssrc = state.ssrc;
            state.next_sequence = state.next_sequence.wrapping_add(1);
            state.next_timestamp = stream_cursor_after;
            // Sender stats for the periodic SR (RFC 3550 §6.4.1). Octet
            // count is payload only, snapshotted before `payload` is moved
            // into the RTP packet below.
            state.packets_sent = state.packets_sent.wrapping_add(1);
            state.octets_sent = state.octets_sent.wrapping_add(payload.len() as u32);
            (sequence_number, ssrc)
        };

        let packet = forge_rtp::RtpPacket::build(
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            payload,
            marker,
        );

        let packet_bytes = packet.to_bytes();
        let srtp_ctx = match leg {
            ParticipantLabel::A => session.srtp_a().clone(),
            ParticipantLabel::B => session.srtp_b().clone(),
        };
        let send_data = {
            let mut ctx = srtp_ctx.lock().await;
            ctx.protect_rtp(&packet_bytes)
                .map_err(|e| ForgeError::Srtp(e.to_string()))?
        };

        sockets
            .send_rtp_to(&send_data, remote_addr)
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;

        let packet_len = send_data.len() as u64;
        Self::update_stats(
            &Side::from_label(leg),
            participant_a,
            participant_b,
            packet_len,
            false,
            None,
        )
        .await;

        Ok(packet_len)
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
                        let frame_sizes: Vec<usize> =
                            transcoded_payloads.iter().map(|f| f.len()).collect();
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

                        Self::process_report_blocks(
                            session,
                            sockets,
                            participant_a,
                            source_addr,
                            &sr.report_blocks,
                        )
                        .await;
                    }
                    forge_rtp::rtcp::RtcpPacket::ReceiverReport(rr) => {
                        Self::process_report_blocks(
                            session,
                            sockets,
                            participant_a,
                            source_addr,
                            &rr.report_blocks,
                        )
                        .await;
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

                // HEP3 capture (chunk 0x05 = RTCP). Emit the plaintext
                // bytes (`data`) so Homer shows the SR/RR contents
                // rather than the SRTCP-protected blob.
                if let Some(emitter) = forge_hep::forge_hep() {
                    if let Ok(local) = sockets.local_rtcp_addr() {
                        emitter.emit_rtcp(
                            forge_hep::Direction::Outbound,
                            forge_hep::IpProto::Udp,
                            local,
                            addr,
                            data,
                            Some(&call_id.0),
                        );
                    }
                }
            }
        } else {
            tracing::trace!(
                "Cannot forward RTCP packet - receiver endpoint not yet learned for session {}",
                call_id.0
            );
        }
    }

    /// Simple audio resampling using linear interpolation
    pub(crate) fn resample_audio(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
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
    ///
    /// `rx` carries the RTP header fields needed to update the leg's
    /// [`RxStreamStats`] (sequence tracking + interarrival jitter) under
    /// the same write lock; pass `None` on the send side.
    async fn update_stats(
        side: &Side,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        packet_len: u64,
        is_received: bool,
        rx: Option<RxPacketMeta>,
    ) {
        let participant = match side {
            Side::A => participant_a,
            Side::B => participant_b,
        };

        let mut p = participant.write().await;
        if is_received {
            let now = std::time::Instant::now();
            p.stats.packets_received += 1;
            p.stats.bytes_received += packet_len;
            p.stats.last_packet_at = Some(now);
            if let Some(rx) = rx {
                p.stats.rx_stream.record(
                    rx.sequence,
                    rx.rtp_timestamp,
                    now,
                    rx.clock_rate,
                    rx.count_jitter,
                );
            }
        } else {
            p.stats.packets_sent += 1;
            p.stats.bytes_sent += packet_len;
        }
    }

    /// Drive every per-RR-block observability sink in one place:
    /// Prometheus metrics, HEP QoS, and the [`ForgeEvent::RtcpReportReceived`]
    /// bus event that siphon-ai's `RtpStatsTracker` consumes for the
    /// `rtp_stats` WS event.
    ///
    /// The jitter→ms conversion needs an RTP clock rate; we read it
    /// from `participant_a`. siphon-ai's bridge mode configures both
    /// legs with the same codec, so this is the right value there.
    /// Drive a DTLS-SRTP handshake step on the RTP socket. Called from
    /// the recv-loop demux when the first byte falls in the RFC 5764
    /// §5.1.2 DTLS range (20-63). Picks the leg by `source_addr`,
    /// feeds the packet into the per-leg `DtlsLeg`, ships any
    /// outgoing DTLS bytes back to the peer, and on handshake
    /// completion installs the derived SRTP keys into the existing
    /// `srtp_a`/`srtp_b` context so the next inbound SRTP packet
    /// decodes cleanly.
    ///
    /// Best-effort: if no DTLS leg has been provisioned on either
    /// side (`enable_dtls` not called), the packet is silently
    /// dropped with a metric — we don't want a stray DTLS packet on
    /// a plaintext call to break anything.
    #[cfg(feature = "dtls")]
    async fn handle_dtls_packet(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        participant_b: &Arc<RwLock<Participant>>,
        raw_data: &[u8],
        source_addr: std::net::SocketAddr,
    ) {
        use crate::dtls_srtp::HandshakeOutcome;

        counter!("forge_dtls_packets_received_total", 1);

        // Pick the side by source_addr. Same shape as the SRTP-context
        // selection above. Default to A if neither participant has
        // learned a remote_addr yet (very early in the call).
        let (dtls_slot, srtp_ctx) = {
            let a = participant_a.read().await;
            let b = participant_b.read().await;
            if a.remote_addr == Some(source_addr) {
                (session.dtls_a().clone(), session.srtp_a().clone())
            } else if b.remote_addr == Some(source_addr) {
                (session.dtls_b().clone(), session.srtp_b().clone())
            } else {
                (session.dtls_a().clone(), session.srtp_a().clone())
            }
        };

        let outcome = {
            let mut guard = dtls_slot.lock().await;
            let Some(leg) = guard.as_mut() else {
                // No DTLS configured on this side. Could be a stale
                // packet from a previous call, or misrouted traffic.
                // Drop with a metric.
                counter!("forge_dtls_packets_dropped_no_leg_total", 1);
                return;
            };
            leg.feed(Some(raw_data))
        };

        // Ship any outgoing DTLS bytes back to the peer first — both
        // InProgress and Complete may carry them.
        let outgoing = match &outcome {
            HandshakeOutcome::InProgress { outgoing }
            | HandshakeOutcome::Complete { outgoing, .. } => outgoing.clone(),
            HandshakeOutcome::Failed(_) => Vec::new(),
        };
        if !outgoing.is_empty() {
            if let Err(e) = sockets.send_rtp_to(&outgoing, source_addr).await {
                tracing::warn!(
                    call_id = %session.call_id().0,
                    error = %e,
                    "failed to send outgoing DTLS bytes",
                );
                counter!("forge_dtls_send_errors_total", 1);
            }
        }

        match outcome {
            HandshakeOutcome::InProgress { .. } => {}
            HandshakeOutcome::Complete {
                local_srtp_key,
                remote_srtp_key,
                ..
            } => {
                counter!("forge_dtls_handshakes_completed_total", 1);
                crate::dtls_srtp::install_keys(&srtp_ctx, local_srtp_key, remote_srtp_key).await;
                tracing::info!(
                    call_id = %session.call_id().0,
                    "DTLS-SRTP handshake complete; SRTP keys installed",
                );
            }
            HandshakeOutcome::Failed(e) => {
                counter!("forge_dtls_handshakes_failed_total", 1);
                tracing::warn!(
                    call_id = %session.call_id().0,
                    error = %e,
                    "DTLS-SRTP handshake failed; clearing leg",
                );
                // Clear the leg so we don't keep retrying with a
                // poisoned context.
                *dtls_slot.lock().await = None;
            }
        }
    }

    /// A future refinement disambiguates by `source_addr` for true
    /// B2BUA setups with mixed clock rates between legs.
    async fn process_report_blocks(
        session: &Arc<MediaSession>,
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        participant_a: &Arc<RwLock<Participant>>,
        source_addr: std::net::SocketAddr,
        blocks: &[forge_rtp::rtcp::ReceptionReportBlock],
    ) {
        let call_id = session.call_id();
        let clock_rate = participant_a.read().await.codec_config.clock_rate;
        let bus = session.event_bus().cloned();
        // The carrier's RR reports on the stream we *generate* toward it,
        // which lives on leg A in siphon-ai's terminator model (matching
        // this function's existing leg-A assumption).
        let generated = session.generated_rtp_state(ParticipantLabel::A);
        for block in blocks {
            Self::record_report_block_metrics(block);
            Self::emit_qos_report(sockets, source_addr, &call_id.0, block);
            // RTT (RFC 3550 §A.7): only when the block reports on our
            // generated SSRC and carries the echo of an SR we sent. A
            // non-matching SSRC or `last_sr == 0` yields `None`.
            let rtt_ms = {
                let mut state = generated.lock().await;
                if block.ssrc == state.ssrc {
                    state.rtt.observe_incoming_rr(
                        block.last_sr,
                        block.delay_since_last_sr,
                        forge_rtp::rtcp::ntp::now(),
                    )
                } else {
                    None
                }
            };
            if let Some(bus) = &bus {
                let _ = bus.publish(Self::rtcp_report_event(
                    call_id.clone(),
                    block,
                    clock_rate,
                    rtt_ms,
                ));
            }
        }
    }

    /// Translate one RTCP reception-report block into a
    /// [`ForgeEvent::RtcpReportReceived`] payload, converting the RR's
    /// timestamp-unit jitter into milliseconds using the supplied RTP
    /// clock rate. `rtt_ms` is the RTT sample resolved from this block's
    /// `last_sr` / `delay_since_last_sr` against an SR we originated
    /// (RFC 3550 §A.7), or `None` when no matching SR is on record.
    fn rtcp_report_event(
        call_id: forge_core::CallId,
        block: &forge_rtp::rtcp::ReceptionReportBlock,
        clock_rate: u32,
        rtt_ms: Option<f32>,
    ) -> ForgeEvent {
        // `block.jitter` is in RTP timestamp units (1 / clock_rate
        // seconds each). Convert to ms; treat a degenerate
        // `clock_rate == 0` as the SIP default 8 kHz to avoid a
        // panic/Inf — better an approximate value than a dropped event.
        let cr = if clock_rate == 0 { 8000 } else { clock_rate };
        let jitter_ms = (block.jitter as f32) / (cr as f32) * 1000.0;
        let packet_loss_ratio = (block.fraction_lost as f32) / 256.0;
        ForgeEvent::RtcpReportReceived {
            call_id,
            jitter_ms,
            packet_loss_ratio,
            rtt_ms,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Build and emit one [`forge_hep::RtpQosReport`] from an RR/SR
    /// reception block. No-op when no emitter is installed. Best-
    /// effort — failures (e.g., can't read local_rtcp_addr) are
    /// silently dropped: this is observability, not call control.
    fn emit_qos_report(
        sockets: &Arc<forge_rtp::RtpSocketPair>,
        source_addr: std::net::SocketAddr,
        correlation_id: &str,
        block: &forge_rtp::rtcp::ReceptionReportBlock,
    ) {
        let Some(emitter) = forge_hep::forge_hep() else {
            return;
        };
        let Ok(local) = sockets.local_rtcp_addr() else {
            return;
        };
        let report = forge_hep::RtpQosReport {
            ssrc: Some(block.ssrc),
            fraction_lost: Some((block.fraction_lost as f64) / 256.0),
            packets_lost: Some(block.cumulative_lost),
            jitter: Some(block.jitter),
            ..Default::default()
        };
        emitter.emit_rtp_qos(
            forge_hep::IpProto::Udp,
            source_addr,
            local,
            &report,
            Some(correlation_id),
        );
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

impl Side {
    fn label(self) -> ParticipantLabel {
        match self {
            Self::A => ParticipantLabel::A,
            Self::B => ParticipantLabel::B,
        }
    }

    fn from_label(label: ParticipantLabel) -> Self {
        match label {
            ParticipantLabel::A => Self::A,
            ParticipantLabel::B => Self::B,
        }
    }
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

    fn rr_block(jitter: u32, fraction_lost: u8) -> forge_rtp::rtcp::ReceptionReportBlock {
        let mut block = forge_rtp::rtcp::ReceptionReportBlock::new(0xDEAD_BEEF);
        block.jitter = jitter;
        block.fraction_lost = fraction_lost;
        block
    }

    #[test]
    fn rtcp_report_event_converts_jitter_at_8khz() {
        let call_id = CallId::generate();
        // 80 ticks @ 8 kHz = 10 ms.
        let block = rr_block(80, 0);
        let event = ForwardingEngine::rtcp_report_event(call_id.clone(), &block, 8000, None);
        match event {
            ForgeEvent::RtcpReportReceived {
                jitter_ms,
                packet_loss_ratio,
                rtt_ms,
                call_id: cid,
                ..
            } => {
                assert!((jitter_ms - 10.0).abs() < 1e-3);
                assert_eq!(packet_loss_ratio, 0.0);
                assert!(rtt_ms.is_none());
                assert_eq!(cid, call_id);
            }
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    #[test]
    fn rtcp_report_event_propagates_rtt() {
        let block = rr_block(0, 0);
        let event =
            ForwardingEngine::rtcp_report_event(CallId::generate(), &block, 8000, Some(42.5));
        match event {
            ForgeEvent::RtcpReportReceived { rtt_ms, .. } => {
                assert_eq!(rtt_ms, Some(42.5));
            }
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    #[test]
    fn rtcp_report_event_converts_jitter_at_48khz_opus() {
        // 480 ticks @ 48 kHz = 10 ms.
        let block = rr_block(480, 0);
        let event = ForwardingEngine::rtcp_report_event(CallId::generate(), &block, 48000, None);
        match event {
            ForgeEvent::RtcpReportReceived { jitter_ms, .. } => {
                assert!((jitter_ms - 10.0).abs() < 1e-3, "jitter_ms = {jitter_ms}");
            }
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    #[test]
    fn rtcp_report_event_fraction_lost_to_ratio() {
        // fraction_lost is 8-bit fixed-point: 64 / 256 = 0.25 (25% loss).
        let block = rr_block(0, 64);
        let event = ForwardingEngine::rtcp_report_event(CallId::generate(), &block, 8000, None);
        match event {
            ForgeEvent::RtcpReportReceived {
                packet_loss_ratio, ..
            } => {
                assert!((packet_loss_ratio - 0.25).abs() < 1e-6);
            }
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    #[test]
    fn rtcp_report_event_handles_zero_clock_rate() {
        // Degenerate input — must not panic or produce Inf; fall back to
        // the SIP default 8 kHz so the value is at least interpretable.
        let block = rr_block(80, 0);
        let event = ForwardingEngine::rtcp_report_event(CallId::generate(), &block, 0, None);
        match event {
            ForgeEvent::RtcpReportReceived { jitter_ms, .. } => {
                assert!(jitter_ms.is_finite());
                // 80 / 8000 * 1000 = 10 ms
                assert!((jitter_ms - 10.0).abs() < 1e-3);
            }
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    async fn make_rtcp_test_session(
        start_port: u16,
        bus: Option<Arc<forge_core::EventBus>>,
    ) -> Arc<MediaSession> {
        make_test_session_with_config(start_port, bus, MediaSessionConfig::default()).await
    }

    async fn make_test_session_with_config(
        start_port: u16,
        bus: Option<Arc<forge_core::EventBus>>,
        mut session_config: MediaSessionConfig,
    ) -> Arc<MediaSession> {
        let config = PortPoolConfig::new(start_port, start_port + 100).unwrap();
        let port_pool = Arc::new(PortPool::new(config));
        session_config.socket_config = forge_rtp::RtpSocketConfig {
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ..Default::default()
        };
        Arc::new(
            MediaSession::new(
                CallId::generate(),
                ParticipantId::generate(),
                ParticipantId::generate(),
                &port_pool,
                session_config,
                bus,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        )
    }

    /// An incoming RR that echoes (via `last_sr`) an SR we originated for
    /// our generated stream resolves to an RTT on the published event.
    #[tokio::test]
    async fn process_report_blocks_resolves_rtt_from_our_sr() {
        let bus = Arc::new(forge_core::EventBus::new());
        let session = make_rtcp_test_session(41200, Some(bus.clone())).await;

        // Pretend we generated audio and emitted an SR carrying `ntp`.
        let ntp = forge_rtp::rtcp::ntp::now();
        let our_ssrc = {
            let st = session.generated_rtp_state(ParticipantLabel::A);
            let mut st = st.lock().await;
            st.rtt.record_outgoing_sr(ntp);
            st.ssrc
        };

        // Carrier RR echoing our SR: last_sr = middle-32 of `ntp`, no delay.
        let mut block = forge_rtp::rtcp::ReceptionReportBlock::new(our_ssrc);
        block.last_sr = forge_rtp::ntp_middle32(ntp);
        block.delay_since_last_sr = 0;

        let mut rx = bus.subscribe();
        let sockets = session.sockets().clone();
        let participant_a = session.participant_a().clone();
        ForwardingEngine::process_report_blocks(
            &session,
            &sockets,
            &participant_a,
            "127.0.0.1:10000".parse().unwrap(),
            &[block],
        )
        .await;

        match rx
            .try_recv()
            .expect("an RtcpReportReceived event was published")
        {
            ForgeEvent::RtcpReportReceived { rtt_ms, .. } => {
                let rtt = rtt_ms.expect("RTT resolved from our recorded SR");
                // Real elapsed since record is tiny but non-negative.
                assert!((0.0..5000.0).contains(&rtt), "rtt_ms = {rtt}");
            }
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    /// An RR reporting on a foreign SSRC (not our generated stream) never
    /// yields an RTT, even with a non-zero `last_sr`.
    #[tokio::test]
    async fn process_report_blocks_no_rtt_for_foreign_ssrc() {
        let bus = Arc::new(forge_core::EventBus::new());
        let session = make_rtcp_test_session(41400, Some(bus.clone())).await;

        let our_ssrc = {
            let st = session.generated_rtp_state(ParticipantLabel::A);
            let st = st.lock().await;
            st.ssrc
        };
        let mut block = forge_rtp::rtcp::ReceptionReportBlock::new(our_ssrc.wrapping_add(1));
        block.last_sr = 0x1234_5678;
        block.delay_since_last_sr = 0;

        let mut rx = bus.subscribe();
        let sockets = session.sockets().clone();
        let participant_a = session.participant_a().clone();
        ForwardingEngine::process_report_blocks(
            &session,
            &sockets,
            &participant_a,
            "127.0.0.1:10000".parse().unwrap(),
            &[block],
        )
        .await;

        match rx.try_recv().expect("event published") {
            ForgeEvent::RtcpReportReceived { rtt_ms, .. } => assert!(rtt_ms.is_none()),
            other => panic!("expected RtcpReportReceived, got {other:?}"),
        }
    }

    /// The SR emitter only fires once a leg has generated audio AND has a
    /// known remote address; when it does, it stamps the cadence clock.
    #[tokio::test]
    async fn sender_report_gated_on_audio_and_remote_addr() {
        let session = make_rtcp_test_session(41600, None).await;
        let sockets = session.sockets().clone();
        let pa = session.participant_a().clone();
        let pb = session.participant_b().clone();

        // No remote addr, no audio → nothing emitted.
        ForwardingEngine::maybe_emit_sender_reports(&session, &sockets, &pa, &pb).await;
        assert!(
            session
                .generated_rtp_state(ParticipantLabel::A)
                .lock()
                .await
                .last_sr_at
                .is_none(),
            "no SR before any audio / remote addr"
        );

        // Remote addr learned + audio generated → SR emitted.
        pa.write().await.remote_addr = Some("127.0.0.1:41700".parse().unwrap());
        {
            let st = session.generated_rtp_state(ParticipantLabel::A);
            let mut st = st.lock().await;
            st.packets_sent = 50;
            st.octets_sent = 8_000;
        }
        ForwardingEngine::maybe_emit_sender_reports(&session, &sockets, &pa, &pb).await;
        assert!(
            session
                .generated_rtp_state(ParticipantLabel::A)
                .lock()
                .await
                .last_sr_at
                .is_some(),
            "SR emitted once audio + remote addr are present"
        );
    }

    /// Seed leg A's receive-side stats with a short run containing one
    /// sequence gap: 3 packets received, 1 lost.
    async fn seed_rx_stats(session: &Arc<MediaSession>) {
        let pa = session.participant_a().clone();
        let mut p = pa.write().await;
        let t0 = std::time::Instant::now();
        p.stats.rx_stream.record(1, 160, t0, 8000, true);
        p.stats.rx_stream.record(
            2,
            320,
            t0 + std::time::Duration::from_millis(20),
            8000,
            true,
        );
        p.stats.rx_stream.record(
            4,
            640,
            t0 + std::time::Duration::from_millis(60),
            8000,
            true,
        );
    }

    /// With a cadence configured, one snapshot is published for the leg
    /// that has received RTP — and none for the silent leg.
    #[tokio::test]
    async fn media_stats_snapshot_emitted_for_receiving_leg_only() {
        let bus = Arc::new(forge_core::EventBus::new());
        let session = make_test_session_with_config(
            41600,
            Some(bus.clone()),
            MediaSessionConfig {
                media_stats_interval: Some(std::time::Duration::ZERO),
                ..Default::default()
            },
        )
        .await;
        seed_rx_stats(&session).await;

        let mut rx = bus.subscribe();
        let pa = session.participant_a().clone();
        let pb = session.participant_b().clone();
        let mut last_emit = None;
        ForwardingEngine::maybe_emit_media_stats(&session, &pa, &pb, &mut last_emit).await;

        match rx.try_recv().expect("a MediaStatsSnapshot was published") {
            ForgeEvent::MediaStatsSnapshot {
                call_id,
                leg,
                rx_packets_received,
                rx_packets_lost,
                rx_packets_out_of_order,
                rx_packets_duplicate,
                rx_jitter_ms,
                ..
            } => {
                assert_eq!(&call_id, session.call_id());
                assert_eq!(leg, forge_core::MediaLeg::A);
                assert_eq!(rx_packets_received, 3);
                assert_eq!(rx_packets_lost, 1);
                assert_eq!(rx_packets_out_of_order, 0);
                assert_eq!(rx_packets_duplicate, 0);
                assert!(rx_jitter_ms.is_finite());
            }
            other => panic!("expected MediaStatsSnapshot, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "leg B received no RTP — no snapshot for it"
        );
        assert!(last_emit.is_some(), "cadence gate advanced");
    }

    /// No `media_stats_interval` (the default) → no snapshots, even with
    /// receive-side stats present.
    #[tokio::test]
    async fn media_stats_snapshot_disabled_by_default() {
        let bus = Arc::new(forge_core::EventBus::new());
        let session = make_rtcp_test_session(41800, Some(bus.clone())).await;
        seed_rx_stats(&session).await;

        let mut rx = bus.subscribe();
        let pa = session.participant_a().clone();
        let pb = session.participant_b().clone();
        let mut last_emit = None;
        ForwardingEngine::maybe_emit_media_stats(&session, &pa, &pb, &mut last_emit).await;

        assert!(rx.try_recv().is_err(), "disabled by default");
        assert!(last_emit.is_none());
    }

    /// The cadence gate holds between ticks: a second housekeeping pass
    /// inside the interval publishes nothing.
    #[tokio::test]
    async fn media_stats_snapshot_respects_cadence() {
        let bus = Arc::new(forge_core::EventBus::new());
        let session = make_test_session_with_config(
            42000,
            Some(bus.clone()),
            MediaSessionConfig {
                media_stats_interval: Some(std::time::Duration::from_secs(60)),
                ..Default::default()
            },
        )
        .await;
        seed_rx_stats(&session).await;

        let mut rx = bus.subscribe();
        let pa = session.participant_a().clone();
        let pb = session.participant_b().clone();
        let mut last_emit = None;
        ForwardingEngine::maybe_emit_media_stats(&session, &pa, &pb, &mut last_emit).await;
        assert!(rx.try_recv().is_ok(), "first pass emits");

        ForwardingEngine::maybe_emit_media_stats(&session, &pa, &pb, &mut last_emit).await;
        assert!(rx.try_recv().is_err(), "second pass inside interval holds");
    }
}
