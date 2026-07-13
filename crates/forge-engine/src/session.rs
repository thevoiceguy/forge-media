//! Media session management for two-party calls

use chrono::{DateTime, Utc};
use forge_core::{CallId, EventBus, ForgeError, ForgeEvent, ParticipantId, Result};
use forge_rtp::srtp::SrtpContext;
use forge_rtp::{PortPair, PortPool, RtpSocketConfig, RtpSocketPair};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

#[cfg(all(target_os = "linux", feature = "xdp"))]
use forge_kernel::{ForwardKey, ForwardValue, XdpManager};

/// DTMF detection configuration
#[derive(Debug, Clone)]
pub struct DtmfConfig {
    /// Enable RFC 2833 (telephone-event) detection
    pub enable_rfc2833: bool,
    /// Enable inband (Goertzel) detection
    pub enable_inband: bool,
    /// Enable deduplication (recommended when multiple methods enabled)
    pub enable_dedup: bool,
    /// Opus payload type for inband detection (dynamic, typically 111)
    pub opus_payload_type: Option<u8>,
}

/// Voice-activity detection configuration. Defaults to enabled with
/// `forge_vad::VadConfig::default()` thresholds. Disable via
/// `enabled = false` on session-config-time deployments that don't
/// want speech events on the `EventBus`.
#[derive(Debug, Clone)]
pub struct VadConfig {
    pub enabled: bool,
    pub detector: forge_vad::VadConfig,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            detector: forge_vad::VadConfig::default(),
        }
    }
}

/// Transcoding configuration
#[derive(Debug, Clone)]
pub struct TranscodingConfig {
    /// Enable automatic transcoding when participants use different codecs
    pub enable_transcoding: bool,
    /// RTP payload type mapping for codec detection
    pub payload_type_map: forge_transcoder::rtp::PayloadTypeMap,
}

impl Default for DtmfConfig {
    fn default() -> Self {
        Self {
            enable_rfc2833: true,
            enable_inband: true,
            enable_dedup: true,
            opus_payload_type: Some(111), // Common dynamic payload type for Opus
        }
    }
}

impl Default for TranscodingConfig {
    fn default() -> Self {
        Self {
            enable_transcoding: true,
            payload_type_map: forge_transcoder::rtp::PayloadTypeMap::default(),
        }
    }
}

/// Configuration for a media session
#[derive(Debug, Clone)]
pub struct MediaSessionConfig {
    /// RTP socket configuration
    pub socket_config: RtpSocketConfig,
    /// Session timeout (idle duration before auto-termination)
    pub session_timeout: Duration,
    /// DTMF detection configuration
    pub dtmf_config: DtmfConfig,
    /// Voice-activity detection configuration
    pub vad_config: VadConfig,
    /// Transcoding configuration
    pub transcoding_config: TranscodingConfig,
    /// Cadence for publishing [`forge_core::ForgeEvent::MediaStatsSnapshot`]
    /// events with locally-measured receive-side stream statistics.
    /// `None` (the default) disables snapshot publication entirely; the
    /// per-packet counters are still maintained either way.
    pub media_stats_interval: Option<Duration>,
}

impl Default for MediaSessionConfig {
    fn default() -> Self {
        Self {
            socket_config: RtpSocketConfig::default(),
            session_timeout: Duration::from_secs(300), // 5 minutes
            dtmf_config: DtmfConfig::default(),
            vad_config: VadConfig::default(),
            transcoding_config: TranscodingConfig::default(),
            media_stats_interval: None,
        }
    }
}

/// Codec configuration for a participant
#[derive(Debug, Clone)]
pub struct ParticipantCodecConfig {
    /// RTP payload type
    pub payload_type: u8,
    /// Audio codec
    pub codec: forge_core::AudioCodec,
    /// Codec clock rate (Hz)
    pub clock_rate: u32,
}

impl Default for ParticipantCodecConfig {
    fn default() -> Self {
        Self {
            payload_type: 0, // PCMU
            codec: forge_core::AudioCodec::PCMU,
            clock_rate: 8000,
        }
    }
}

/// Participant leg within a two-party media session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantLabel {
    /// Participant A (typically caller)
    A,
    /// Participant B (typically callee)
    B,
}

impl ParticipantLabel {
    /// Human-readable leg label for logs and APIs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

impl std::str::FromStr for ParticipantLabel {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "a" | "lega" | "participant_a" | "participant-a" => Ok(Self::A),
            "b" | "legb" | "participant_b" | "participant-b" => Ok(Self::B),
            _ => Err(format!(
                "Invalid participant leg '{}'. Expected 'a' or 'b'",
                value
            )),
        }
    }
}

/// Partial runtime update for a participant's media configuration.
#[derive(Debug, Clone, Default)]
pub struct ParticipantMediaUpdate {
    /// `None` = leave unchanged, `Some(None)` = clear, `Some(Some(addr))` = set.
    pub remote_addr: Option<Option<SocketAddr>>,
    /// Replace the participant codec configuration.
    pub codec_config: Option<ParticipantCodecConfig>,
    /// Update the negotiated telephone-event payload type for this leg.
    pub telephone_event_payload_type: Option<u8>,
    /// `None` = leave unchanged, `Some(None)` = clear, `Some(Some(set))` = set.
    pub latch_allowed_ips: Option<Option<HashSet<IpAddr>>>,
}

/// Snapshot of a participant's runtime media configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantMediaState {
    /// Participant leg (`a` or `b`)
    pub leg: ParticipantLabel,
    /// Participant identifier
    pub participant_id: String,
    /// Remote RTP endpoint if explicitly configured or learned
    pub remote_rtp_addr: Option<SocketAddr>,
    /// RTP payload type for this leg
    pub payload_type: u8,
    /// Negotiated audio codec
    pub codec: forge_core::AudioCodec,
    /// Codec clock rate (Hz)
    pub clock_rate: u32,
    /// Telephone-event payload type for DTMF
    pub telephone_event_payload_type: u8,
    /// Allowed source IPs for symmetric RTP latching, if configured
    pub latch_allowed_ips: Option<Vec<IpAddr>>,
}

impl ParticipantMediaState {
    fn from_participant(
        leg: ParticipantLabel,
        participant: &Participant,
        telephone_event_payload_type: u8,
    ) -> Self {
        let mut latch_allowed_ips = participant
            .latch_allowed_ips
            .as_ref()
            .map(|allowed| allowed.iter().copied().collect::<Vec<_>>());
        if let Some(ref mut ips) = latch_allowed_ips {
            ips.sort_by_key(|ip| ip.to_string());
        }

        Self {
            leg,
            participant_id: participant.id.0.clone(),
            remote_rtp_addr: participant.remote_addr,
            payload_type: participant.codec_config.payload_type,
            codec: participant.codec_config.codec,
            clock_rate: participant.codec_config.clock_rate,
            telephone_event_payload_type,
            latch_allowed_ips,
        }
    }
}

/// Participant in a media session
#[derive(Debug, Clone)]
pub struct Participant {
    /// Participant ID
    pub id: ParticipantId,
    /// Remote RTP endpoint (learned via symmetric RTP)
    pub remote_addr: Option<SocketAddr>,
    /// Codec payload type (deprecated - use codec_config.payload_type)
    pub payload_type: u8,
    /// Codec configuration
    pub codec_config: ParticipantCodecConfig,
    /// Statistics
    pub stats: ParticipantStats,
    /// Optional allowlist of source IPs permitted to latch this participant's
    /// remote endpoint via symmetric-RTP learning.
    ///
    /// `None` preserves the legacy behavior (any source may latch). When
    /// populated — typically from the SDP `c=` line or an explicit operator
    /// policy — the forwarding engine drops packets whose source IP is not in
    /// the set, defeating off-path latching attacks (audit finding C3).
    pub latch_allowed_ips: Option<HashSet<IpAddr>>,
}

/// Statistics for a participant
#[derive(Debug, Clone, Default)]
pub struct ParticipantStats {
    /// Total packets received
    pub packets_received: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Total packets sent
    pub packets_sent: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Packets lost
    pub packets_lost: u64,
    /// Last packet received timestamp
    pub last_packet_at: Option<Instant>,
    /// Locally-measured receive-side stream statistics (sequence tracking
    /// + RFC 3550 interarrival jitter). Feeds the periodic
    /// [`forge_core::ForgeEvent::MediaStatsSnapshot`] event.
    pub rx_stream: RxStreamStats,
}

/// Local receive-side RTP stream statistics.
///
/// Tracks the stream this leg *receives*, measured at the forwarding
/// engine — as opposed to RTCP Receiver Reports, which describe how the
/// remote end receives the stream we send. Sequence numbers are extended
/// with a wrap-cycle count (RFC 3550 §A.1) so loss survives the 16-bit
/// rollover; duplicates are detected against a 64-packet window ending at
/// the highest sequence seen.
#[derive(Debug, Clone, Default)]
pub struct RxStreamStats {
    /// Unique RTP packets received (duplicates excluded).
    pub packets_received: u64,
    /// Late arrivals — sequence number older than the highest already seen.
    pub packets_out_of_order: u64,
    /// Re-receives of a sequence number inside the recent-packet window.
    pub packets_duplicate: u64,
    /// Extended sequence number of the first packet.
    base_seq_ext: Option<u64>,
    /// Highest extended sequence number seen.
    max_seq_ext: u64,
    /// Receive bitmask for the 64 sequence numbers ending at
    /// `max_seq_ext` (bit 0 = `max_seq_ext` itself), for dup detection.
    recent_window: u64,
    /// Interarrival jitter in RTP timestamp units (RFC 3550 §6.4.1).
    jitter_units: f64,
    /// Transit (arrival − RTP timestamp) of the previous packet, in
    /// timestamp units.
    last_transit: Option<f64>,
    /// Zero-point for converting arrival `Instant`s to timestamp units.
    epoch: Option<Instant>,
    /// RTP clock rate of the last recorded packet, for `jitter_ms()`.
    clock_rate: u32,
}

impl RxStreamStats {
    /// Record one received RTP packet.
    ///
    /// `clock_rate` is the negotiated RTP clock (48 000 for Opus, not the
    /// bridge rate). `count_jitter` should be `false` for packets whose
    /// RTP timestamp does not track wall-clock audio time (RFC 2833
    /// telephone-events hold their timestamp for the digit's duration) —
    /// the packet still counts toward the sequence statistics.
    pub fn record(
        &mut self,
        sequence: u16,
        rtp_timestamp: u32,
        arrival: Instant,
        clock_rate: u32,
        count_jitter: bool,
    ) {
        let Some(base) = self.base_seq_ext else {
            self.base_seq_ext = Some(sequence as u64);
            self.max_seq_ext = sequence as u64;
            self.recent_window = 1;
            self.packets_received = 1;
            self.update_jitter(rtp_timestamp, arrival, clock_rate, count_jitter);
            return;
        };

        // Extend the 16-bit sequence relative to the highest seen: a
        // forward delta under half the space advances (possibly crossing a
        // wrap); anything else is a late arrival some distance back.
        let max_lo = (self.max_seq_ext & 0xFFFF) as u16;
        let delta = sequence.wrapping_sub(max_lo);
        if delta != 0 && delta < 0x8000 {
            let advance = delta as u64;
            self.recent_window = if advance >= 64 {
                1
            } else {
                (self.recent_window << advance) | 1
            };
            self.max_seq_ext += advance;
            self.packets_received += 1;
        } else {
            let back = ((0x1_0000 - delta as u32) & 0xFFFF) as u64;
            let Some(ext) = self.max_seq_ext.checked_sub(back) else {
                return; // predates the extended-sequence origin; ignore
            };
            if back < 64 {
                let bit = 1u64 << back;
                if self.recent_window & bit != 0 {
                    self.packets_duplicate += 1;
                    return; // re-receive: no jitter update either
                }
                self.recent_window |= bit;
            }
            if ext < base {
                return; // stray pre-base packet (reordered call start)
            }
            self.packets_out_of_order += 1;
            self.packets_received += 1;
        }

        self.update_jitter(rtp_timestamp, arrival, clock_rate, count_jitter);
    }

    fn update_jitter(
        &mut self,
        rtp_timestamp: u32,
        arrival: Instant,
        clock_rate: u32,
        count_jitter: bool,
    ) {
        if !count_jitter || clock_rate == 0 {
            return;
        }
        self.clock_rate = clock_rate;
        let epoch = *self.epoch.get_or_insert(arrival);
        let arrival_units = arrival.duration_since(epoch).as_secs_f64() * clock_rate as f64;
        // RFC 3550 §6.4.1: J += (|D(i-1,i)| − J) / 16. A u32 RTP
        // timestamp wrap mid-call produces one outlier transit sample,
        // which the 1/16 filter absorbs — not worth unwrapping for.
        let transit = arrival_units - rtp_timestamp as f64;
        if let Some(last) = self.last_transit {
            let d = (transit - last).abs();
            self.jitter_units += (d - self.jitter_units) / 16.0;
        }
        self.last_transit = Some(transit);
    }

    /// Packets expected per the extended-sequence span (RFC 3550 §A.3).
    fn expected(&self) -> u64 {
        match self.base_seq_ext {
            Some(base) => self.max_seq_ext - base + 1,
            None => 0,
        }
    }

    /// Cumulative sequence-gap loss. Late arrivals repair this
    /// retroactively, so it can shrink between reads.
    pub fn packets_lost(&self) -> u64 {
        self.expected().saturating_sub(self.packets_received)
    }

    /// Interarrival jitter converted to milliseconds via the stream's RTP
    /// clock rate. `0.0` until two jitter-eligible packets have arrived.
    pub fn jitter_ms(&self) -> f32 {
        if self.clock_rate == 0 {
            return 0.0;
        }
        (self.jitter_units / self.clock_rate as f64 * 1000.0) as f32
    }
}

/// RTP state for generated audio injected into a participant leg.
#[derive(Debug)]
pub(crate) struct GeneratedRtpState {
    pub ssrc: u32,
    pub next_sequence: u16,
    pub next_timestamp: u32,
    /// RTP packets sent on this generated stream — the SR sender packet count.
    pub packets_sent: u32,
    /// RTP *payload* octets sent — the SR sender octet count (RFC 3550
    /// §6.4.1: payload only, excludes header/padding).
    pub octets_sent: u32,
    /// Matches SRs we originate against the LSR/DLSR echoed in incoming
    /// RRs to compute RTT (RFC 3550 §A.7). Without our own SRs the peer's
    /// RR carries `last_sr = 0` and no RTT is computable — which is why
    /// `rtcp_rtt_ms` was `null` before this stream originated SRs.
    pub rtt: forge_rtp::RttTracker,
    /// Wall-clock of the last SR we emitted for this stream, for the
    /// RTCP send cadence (RFC 3550 §6.2). `None` until the first SR.
    pub last_sr_at: Option<Instant>,
}

impl Default for GeneratedRtpState {
    fn default() -> Self {
        Self {
            ssrc: rand::random(),
            next_sequence: rand::random(),
            next_timestamp: rand::random(),
            packets_sent: 0,
            octets_sent: 0,
            // Window only bounds the tracker's sample retention; we emit
            // the per-RR sample directly, so a generous window is fine.
            rtt: forge_rtp::RttTracker::new(Duration::from_secs(30)),
            last_sr_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduledPlayoutSource {
    #[cfg_attr(not(feature = "ai"), allow(dead_code))]
    AI,
    MediaBridgeAudio,
    MediaBridgeDtmf,
}

impl ScheduledPlayoutSource {
    pub(crate) fn as_label(self) -> &'static str {
        match self {
            Self::AI => "ai",
            Self::MediaBridgeAudio => "media_bridge_audio",
            Self::MediaBridgeDtmf => "media_bridge_dtmf",
        }
    }
}

#[derive(Debug)]
pub(crate) enum ScheduledPlayoutKind {
    Audio {
        codec: forge_core::AudioCodec,
        payload_type: u8,
        samples: Vec<i16>,
    },
    Dtmf {
        payload_type: u8,
        payload: Vec<u8>,
    },
}

#[derive(Debug)]
pub(crate) struct ScheduledPlayoutItem {
    pub due_at: Instant,
    pub playback_id: Option<String>,
    pub marker: bool,
    pub timestamp: u32,
    pub stream_cursor_after: u32,
    pub kind: ScheduledPlayoutKind,
    pub source: ScheduledPlayoutSource,
}

/// Minimum wall-clock silence between scheduled audio for the resuming
/// frame to count as the start of a new talkspurt (and thus carry the RTP
/// marker bit). Comfortably above normal playout jitter so a producer that
/// merely falls a frame behind realtime does not spuriously re-mark, while
/// still catching any genuine speech gap. See `schedule_audio_playout_for_leg`.
const TALKSPURT_SILENCE_GAP: Duration = Duration::from_millis(60);

#[derive(Debug, Default)]
pub(crate) struct ScheduledPlayoutQueue {
    items: VecDeque<ScheduledPlayoutItem>,
    next_due_at: Option<Instant>,
    next_rtp_timestamp: Option<u32>,
    /// Wall-clock instant the last-scheduled audio frame is due to finish.
    /// Unlike `next_due_at`, this is **not** cleared when the queue drains
    /// during normal playout — only updated on audio append and recomputed
    /// on explicit clear. It lets us tell "audio resuming after silence"
    /// (new talkspurt → marker bit) apart from "next frame of a stream the
    /// pump happened to drain between deliveries" (no marker). DTMF
    /// scheduling does not touch it.
    audio_stream_end: Option<Instant>,
}

enum StatefulCodec {
    #[cfg(feature = "opus")]
    Opus(forge_codecs::opus::OpusCodec),
    #[cfg(feature = "g722")]
    G722(forge_codecs::g722::G722Codec),
}

impl StatefulCodec {
    fn new(codec: forge_core::AudioCodec) -> Result<Option<Self>> {
        match codec {
            #[cfg(feature = "opus")]
            forge_core::AudioCodec::Opus => {
                let opus_config = forge_codecs::opus::OpusConfig {
                    // Run the codec at 16 kHz mono (matches
                    // `codec_audio_sample_rate(Opus)`): the libopus decoder
                    // resamples the 48 kHz-clocked stream to 16 kHz and
                    // downmixes to mono on its own, and the encoder accepts
                    // 16 kHz mono PCM (RFC 7587 — the RTP clock stays 48 kHz
                    // regardless of the encoder's input rate). Keeps the
                    // bridge on the WS-contract 16 kHz path.
                    sample_rate: 16000,
                    channels: 1,
                    application: forge_codecs::opus::OpusApplication::Voip,
                    bitrate: 24000,
                    frame_duration_ms: 20,
                };
                let codec =
                    forge_codecs::opus::OpusCodec::with_config(opus_config).map_err(|e| {
                        ForgeError::Codec(format!("Failed to create Opus codec: {}", e))
                    })?;
                Ok(Some(Self::Opus(codec)))
            }
            #[cfg(not(feature = "opus"))]
            forge_core::AudioCodec::Opus => Err(ForgeError::Codec(
                "Opus support not enabled in forge-engine".to_string(),
            )),
            #[cfg(feature = "g722")]
            forge_core::AudioCodec::G722 => {
                Ok(Some(Self::G722(forge_codecs::g722::G722Codec::default())))
            }
            #[cfg(not(feature = "g722"))]
            forge_core::AudioCodec::G722 => Err(ForgeError::Codec(
                "G.722 support not enabled in forge-engine".to_string(),
            )),
            _ => Ok(None),
        }
    }

    fn decode(&mut self, payload: &[u8]) -> Result<Vec<i16>> {
        use forge_codecs::AudioCodec as _;

        match self {
            #[cfg(feature = "opus")]
            Self::Opus(codec) => codec
                .decode(payload)
                .map_err(|e| ForgeError::Codec(format!("Opus decode failed: {}", e))),
            #[cfg(feature = "g722")]
            Self::G722(codec) => codec
                .decode(payload)
                .map_err(|e| ForgeError::Codec(format!("G.722 decode failed: {}", e))),
        }
    }

    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        use forge_codecs::AudioCodec as _;

        match self {
            #[cfg(feature = "opus")]
            Self::Opus(codec) => codec
                .encode(samples)
                .map_err(|e| ForgeError::Codec(format!("Opus encode failed: {}", e))),
            #[cfg(feature = "g722")]
            Self::G722(codec) => codec
                .encode(samples)
                .map_err(|e| ForgeError::Codec(format!("G.722 encode failed: {}", e))),
        }
    }
}

struct ParticipantCodecRuntime {
    inbound_codec: forge_core::AudioCodec,
    outbound_codec: forge_core::AudioCodec,
    inbound: Option<StatefulCodec>,
    outbound: Option<StatefulCodec>,
}

impl ParticipantCodecRuntime {
    fn new(codec: forge_core::AudioCodec) -> Result<Self> {
        Ok(Self {
            inbound_codec: codec,
            outbound_codec: codec,
            inbound: StatefulCodec::new(codec)?,
            outbound: StatefulCodec::new(codec)?,
        })
    }

    fn reset(&mut self, codec: forge_core::AudioCodec) -> Result<()> {
        self.inbound_codec = codec;
        self.outbound_codec = codec;
        self.inbound = StatefulCodec::new(codec)?;
        self.outbound = StatefulCodec::new(codec)?;
        Ok(())
    }
}

/// State of a media session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Session is being initialized
    Initializing,
    /// Session is active and forwarding media
    Active,
    /// Session is on hold
    OnHold,
    /// Session is terminating
    Terminating,
    /// Session has terminated
    Terminated,
}

/// A two-party media session
pub struct MediaSession {
    /// Unique session/call ID
    call_id: CallId,
    /// Session state
    state: Arc<RwLock<SessionState>>,
    /// Participant A
    participant_a: Arc<RwLock<Participant>>,
    /// Participant B
    participant_b: Arc<RwLock<Participant>>,
    /// RTP/RTCP socket pair
    sockets: Arc<RtpSocketPair>,
    /// Port pair allocation
    ports: PortPair,
    /// Port pool reference for cleanup
    port_pool: Arc<PortPool>,
    /// Track if ports have been deallocated
    ports_deallocated: Arc<AtomicBool>,
    /// Session creation time
    created_at: Instant,
    /// Last activity time
    last_activity: Arc<RwLock<Instant>>,
    /// Configuration
    config: MediaSessionConfig,
    /// Event bus for publishing events
    event_bus: Option<Arc<EventBus>>,
    /// RFC 2833 (telephone-event) DTMF detector
    dtmf_detector: Arc<Mutex<forge_dtmf::Rfc2833Detector>>,
    /// Inband DTMF detector (Goertzel algorithm)
    inband_detector: Arc<Mutex<forge_dtmf::GoertzelDetector>>,
    /// DTMF event deduplicator
    dtmf_dedup: Arc<Mutex<forge_dtmf::DtmfDeduplicator>>,
    /// Voice-activity detector for this session's inbound audio.
    /// One state machine per call; the forwarding loop runs it on
    /// every decoded PCM frame and publishes
    /// `ForgeEvent::SpeechStarted` / `SpeechStopped` on state
    /// transitions. Initialised from `MediaSessionConfig.vad_config`.
    vad_detector: Arc<Mutex<forge_vad::VadDetector>>,
    /// Wallclock the most recent `SpeechStarted` was emitted at;
    /// used to compute `duration_ms` for the matching
    /// `SpeechStopped`. `None` before the first speech transition,
    /// or after each `SpeechStopped` clears it.
    speech_started_at: Arc<Mutex<Option<DateTime<Utc>>>>,
    /// Transcoder for A → B direction (optional, created when needed)
    transcoder_a_to_b: Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>>,
    /// Transcoder for B → A direction (optional, created when needed)
    transcoder_b_to_a: Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>>,
    /// SRTP context for participant A (inbound: unprotect A→us, outbound: protect us→A)
    srtp_a: Arc<Mutex<SrtpContext>>,
    /// SRTP context for participant B (inbound: unprotect B→us, outbound: protect us→B)
    srtp_b: Arc<Mutex<SrtpContext>>,
    /// DTLS-SRTP state for participant A. `None` until [`enable_dtls`]
    /// installs a leg; once set, the RTP recv loop demuxes DTLS packets
    /// from the A-side socket and drives the handshake.
    #[cfg(feature = "dtls")]
    dtls_a: Arc<Mutex<Option<crate::dtls_srtp::DtlsLeg>>>,
    /// DTLS-SRTP state for participant B (same shape as `dtls_a`).
    #[cfg(feature = "dtls")]
    dtls_b: Arc<Mutex<Option<crate::dtls_srtp::DtlsLeg>>>,
    /// Whether to relay RFC 2833 telephone-event packets to the other leg
    relay_rfc2833: AtomicBool,
    /// Telephone-event payload type negotiated with participant A (default 101)
    telephone_event_pt_a: AtomicU8,
    /// Telephone-event payload type negotiated with participant B (default 101)
    telephone_event_pt_b: AtomicU8,
    /// Forwarding task handles
    forwarding_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// Optional offer/answer SDP associated with the session
    sdp: Option<String>,
    /// SIP/SDP from-tag if provided
    from_tag: Option<String>,
    /// SIP/SDP to-tag if provided
    to_tag: Option<String>,
    /// XDP manager for kernel-level packet forwarding (Linux only)
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    xdp_manager: Option<Arc<XdpManager>>,
    /// Track if XDP fast path is active
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    xdp_active: Arc<AtomicBool>,
    /// AI session manager for AI integration (optional, uses interior mutability)
    #[cfg(feature = "ai")]
    ai_manager: Arc<RwLock<Option<Arc<crate::ai_integration::AISessionManager>>>>,
    /// Generic media bridge manager for bidirectional PCM streaming.
    media_bridge_manager: Arc<RwLock<Option<Arc<crate::media_bridge::MediaBridgeManager>>>>,
    /// Per-leg codec runtime state for inbound/outbound encoding and decoding.
    codec_runtime_a: Arc<Mutex<ParticipantCodecRuntime>>,
    /// Per-leg codec runtime state for inbound/outbound encoding and decoding.
    codec_runtime_b: Arc<Mutex<ParticipantCodecRuntime>>,
    /// Scheduled playout queue for participant A.
    playout_queue_a: Arc<Mutex<ScheduledPlayoutQueue>>,
    /// Scheduled playout queue for participant B.
    playout_queue_b: Arc<Mutex<ScheduledPlayoutQueue>>,
    /// RTP sequencing state for generated audio sent toward participant A.
    generated_rtp_state_a: Arc<Mutex<GeneratedRtpState>>,
    /// RTP sequencing state for generated audio sent toward participant B.
    generated_rtp_state_b: Arc<Mutex<GeneratedRtpState>>,
    /// Audio recorder for call recording (optional)
    pub(crate) recorder: Arc<RwLock<Option<forge_recorder::AudioRecorder>>>,
    /// Small mixer to combine both call legs before writing to the recorder
    pub(crate) recording_mixer: Arc<Mutex<RecordingMixer>>,
}

impl MediaSession {
    /// Create a new media session
    pub async fn new(
        call_id: CallId,
        participant_a_id: ParticipantId,
        participant_b_id: ParticipantId,
        port_pool: &Arc<PortPool>,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
    ) -> Result<Self> {
        // Allocate ports
        let ports = port_pool.allocate().await?;
        let mut port_guard = PortAllocationGuard::new(Arc::clone(port_pool), ports);
        tracing::info!(
            "Allocated ports for session {}: RTP={}, RTCP={}",
            call_id.0,
            ports.rtp_port,
            ports.rtcp_port
        );

        // Create socket pair
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;
        port_guard.disarm();

        let default_codec_config = ParticipantCodecConfig::default();

        let participant_a = Participant {
            id: participant_a_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU (legacy field)
            codec_config: default_codec_config.clone(),
            stats: ParticipantStats::default(),
            latch_allowed_ips: None,
        };

        let participant_b = Participant {
            id: participant_b_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU (legacy field)
            codec_config: default_codec_config,
            stats: ParticipantStats::default(),
            latch_allowed_ips: None,
        };
        let participant_a_codec = participant_a.codec_config.codec;
        let participant_b_codec = participant_b.codec_config.codec;

        let now = Instant::now();

        // VAD config is cloned out before `config` is moved into the
        // struct literal below.
        let vad_detector_config = config.vad_config.detector.clone();

        let session = Self {
            call_id: call_id.clone(),
            state: Arc::new(RwLock::new(SessionState::Initializing)),
            participant_a: Arc::new(RwLock::new(participant_a)),
            participant_b: Arc::new(RwLock::new(participant_b)),
            sockets: Arc::new(sockets),
            ports,
            port_pool: Arc::clone(port_pool),
            ports_deallocated: Arc::new(AtomicBool::new(false)),
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            dtmf_detector: Arc::new(Mutex::new(forge_dtmf::Rfc2833Detector::new(8000))),
            inband_detector: Arc::new(Mutex::new(forge_dtmf::GoertzelDetector::new(8000, 160))),
            dtmf_dedup: Arc::new(Mutex::new(forge_dtmf::DtmfDeduplicator::new())),
            vad_detector: Arc::new(Mutex::new(forge_vad::VadDetector::new(vad_detector_config))),
            speech_started_at: Arc::new(Mutex::new(None)),
            transcoder_a_to_b: Arc::new(Mutex::new(None)),
            transcoder_b_to_a: Arc::new(Mutex::new(None)),
            srtp_a: Arc::new(Mutex::new(SrtpContext::new())),
            srtp_b: Arc::new(Mutex::new(SrtpContext::new())),
            #[cfg(feature = "dtls")]
            dtls_a: Arc::new(Mutex::new(None)),
            #[cfg(feature = "dtls")]
            dtls_b: Arc::new(Mutex::new(None)),
            relay_rfc2833: AtomicBool::new(false),
            telephone_event_pt_a: AtomicU8::new(101),
            telephone_event_pt_b: AtomicU8::new(101),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
            sdp,
            from_tag,
            to_tag,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_manager: None,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_active: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "ai")]
            ai_manager: Arc::new(RwLock::new(None)),
            media_bridge_manager: Arc::new(RwLock::new(None)),
            codec_runtime_a: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_a_codec,
            )?)),
            codec_runtime_b: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_b_codec,
            )?)),
            playout_queue_a: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            playout_queue_b: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            generated_rtp_state_a: Arc::new(Mutex::new(GeneratedRtpState::default())),
            generated_rtp_state_b: Arc::new(Mutex::new(GeneratedRtpState::default())),
            recorder: Arc::new(RwLock::new(None)),
            recording_mixer: Arc::new(Mutex::new(RecordingMixer::default())),
        };

        // Publish session created event
        if let Some(bus) = &event_bus {
            let _ = bus.publish(ForgeEvent::SessionCreated {
                call_id,
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(session)
    }

    /// Create a new media session with specific codec configurations
    pub async fn new_with_codecs(
        call_id: CallId,
        participant_a_id: ParticipantId,
        participant_b_id: ParticipantId,
        codec_a: ParticipantCodecConfig,
        codec_b: ParticipantCodecConfig,
        port_pool: &Arc<PortPool>,
        mut config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
    ) -> Result<Self> {
        // Build custom payload type map from negotiated codecs
        let mut pt_map = forge_transcoder::rtp::PayloadTypeMap::default();

        // Update PT map with negotiated payload types
        match codec_a.codec {
            forge_core::AudioCodec::PCMU => pt_map.pcmu = codec_a.payload_type,
            forge_core::AudioCodec::PCMA => pt_map.pcma = codec_a.payload_type,
            #[cfg(feature = "opus")]
            forge_core::AudioCodec::Opus => pt_map.opus = codec_a.payload_type,
            _ => {} // Other codecs not yet supported in PT map
        }
        match codec_b.codec {
            forge_core::AudioCodec::PCMU => pt_map.pcmu = codec_b.payload_type,
            forge_core::AudioCodec::PCMA => pt_map.pcma = codec_b.payload_type,
            #[cfg(feature = "opus")]
            forge_core::AudioCodec::Opus => pt_map.opus = codec_b.payload_type,
            _ => {} // Other codecs not yet supported in PT map
        }

        // Update transcoding config with negotiated PT map
        config.transcoding_config.payload_type_map = pt_map;

        // Update DTMF config with negotiated Opus PT if applicable
        #[cfg(feature = "opus")]
        {
            if matches!(codec_a.codec, forge_core::AudioCodec::Opus) {
                config.dtmf_config.opus_payload_type = Some(codec_a.payload_type);
            } else if matches!(codec_b.codec, forge_core::AudioCodec::Opus) {
                config.dtmf_config.opus_payload_type = Some(codec_b.payload_type);
            }
        }

        tracing::debug!(
            "Built payload type map for session: PCMU={}, PCMA={}, Opus={}",
            pt_map.pcmu,
            pt_map.pcma,
            pt_map.opus
        );

        // Allocate ports
        let ports = port_pool.allocate().await?;
        let mut port_guard = PortAllocationGuard::new(Arc::clone(port_pool), ports);
        tracing::info!(
            "Allocated ports for session {}: RTP={}, RTCP={}",
            call_id.0,
            ports.rtp_port,
            ports.rtcp_port
        );

        // Create socket pair
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;
        port_guard.disarm();

        let participant_a = Participant {
            id: participant_a_id,
            remote_addr: None,
            payload_type: codec_a.payload_type,
            codec_config: codec_a,
            stats: ParticipantStats::default(),
            latch_allowed_ips: None,
        };

        let participant_b = Participant {
            id: participant_b_id,
            remote_addr: None,
            payload_type: codec_b.payload_type,
            codec_config: codec_b,
            stats: ParticipantStats::default(),
            latch_allowed_ips: None,
        };
        let participant_a_codec = participant_a.codec_config.codec;
        let participant_b_codec = participant_b.codec_config.codec;

        let now = Instant::now();

        // VAD config is cloned out before `config` is moved into the
        // struct literal below.
        let vad_detector_config = config.vad_config.detector.clone();

        let session = Self {
            call_id: call_id.clone(),
            state: Arc::new(RwLock::new(SessionState::Initializing)),
            participant_a: Arc::new(RwLock::new(participant_a)),
            participant_b: Arc::new(RwLock::new(participant_b)),
            sockets: Arc::new(sockets),
            ports,
            port_pool: Arc::clone(port_pool),
            ports_deallocated: Arc::new(AtomicBool::new(false)),
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            dtmf_detector: Arc::new(Mutex::new(forge_dtmf::Rfc2833Detector::new(8000))),
            inband_detector: Arc::new(Mutex::new(forge_dtmf::GoertzelDetector::new(8000, 160))),
            dtmf_dedup: Arc::new(Mutex::new(forge_dtmf::DtmfDeduplicator::new())),
            vad_detector: Arc::new(Mutex::new(forge_vad::VadDetector::new(vad_detector_config))),
            speech_started_at: Arc::new(Mutex::new(None)),
            transcoder_a_to_b: Arc::new(Mutex::new(None)),
            transcoder_b_to_a: Arc::new(Mutex::new(None)),
            srtp_a: Arc::new(Mutex::new(SrtpContext::new())),
            srtp_b: Arc::new(Mutex::new(SrtpContext::new())),
            #[cfg(feature = "dtls")]
            dtls_a: Arc::new(Mutex::new(None)),
            #[cfg(feature = "dtls")]
            dtls_b: Arc::new(Mutex::new(None)),
            relay_rfc2833: AtomicBool::new(false),
            telephone_event_pt_a: AtomicU8::new(101),
            telephone_event_pt_b: AtomicU8::new(101),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
            sdp,
            from_tag,
            to_tag,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_manager: None,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_active: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "ai")]
            ai_manager: Arc::new(RwLock::new(None)),
            media_bridge_manager: Arc::new(RwLock::new(None)),
            codec_runtime_a: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_a_codec,
            )?)),
            codec_runtime_b: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_b_codec,
            )?)),
            playout_queue_a: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            playout_queue_b: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            generated_rtp_state_a: Arc::new(Mutex::new(GeneratedRtpState::default())),
            generated_rtp_state_b: Arc::new(Mutex::new(GeneratedRtpState::default())),
            recorder: Arc::new(RwLock::new(None)),
            recording_mixer: Arc::new(Mutex::new(RecordingMixer::default())),
        };

        // Publish session created event
        if let Some(bus) = &event_bus {
            let _ = bus.publish(ForgeEvent::SessionCreated {
                call_id,
                timestamp: chrono::Utc::now(),
            });
        }

        tracing::info!(
            "Session {} created with codecs: A={:?}@{}Hz, B={:?}@{}Hz",
            session.call_id.0,
            session.participant_a.try_read().unwrap().codec_config.codec,
            session
                .participant_a
                .try_read()
                .unwrap()
                .codec_config
                .clock_rate,
            session.participant_b.try_read().unwrap().codec_config.codec,
            session
                .participant_b
                .try_read()
                .unwrap()
                .codec_config
                .clock_rate,
        );

        // Automatic transcoding initialization on codec mismatch
        let codec_a = session.participant_a.try_read().unwrap().codec_config.codec;
        let codec_b = session.participant_b.try_read().unwrap().codec_config.codec;

        if codec_a != codec_b {
            tracing::info!(
                "Codec mismatch detected in session {}: A={:?}, B={:?}. Initializing transcoders...",
                session.call_id.0,
                codec_a,
                codec_b
            );

            // Convert to transcoder codec types
            if let (Some(transcoder_codec_a), Some(transcoder_codec_b)) = (
                Self::to_transcoder_codec(codec_a),
                Self::to_transcoder_codec(codec_b),
            ) {
                // Initialize bidirectional transcoders
                if let Err(e) = session
                    .ensure_transcoder_a_to_b(transcoder_codec_a, transcoder_codec_b)
                    .await
                {
                    tracing::warn!(
                        "Failed to initialize transcoder A→B for session {}: {}",
                        session.call_id.0,
                        e
                    );
                }

                if let Err(e) = session
                    .ensure_transcoder_b_to_a(transcoder_codec_b, transcoder_codec_a)
                    .await
                {
                    tracing::warn!(
                        "Failed to initialize transcoder B→A for session {}: {}",
                        session.call_id.0,
                        e
                    );
                }

                tracing::info!(
                    "Transcoders initialized for session {}: {:?} ↔ {:?}",
                    session.call_id.0,
                    codec_a,
                    codec_b
                );
            } else {
                tracing::warn!(
                    "Codec mismatch in session {} but transcoding not available for {:?} ↔ {:?}",
                    session.call_id.0,
                    codec_a,
                    codec_b
                );
            }
        } else {
            tracing::debug!(
                "Session {} participants using same codec ({:?}), no transcoding needed",
                session.call_id.0,
                codec_a
            );
        }

        Ok(session)
    }

    /// Create a new media session with XDP support
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn new_with_xdp(
        call_id: CallId,
        participant_a_id: ParticipantId,
        participant_b_id: ParticipantId,
        port_pool: &Arc<PortPool>,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
        xdp_manager: Option<Arc<XdpManager>>,
        sdp: Option<String>,
        from_tag: Option<String>,
        to_tag: Option<String>,
    ) -> Result<Self> {
        // Allocate ports
        let ports = port_pool.allocate().await?;
        let mut port_guard = PortAllocationGuard::new(Arc::clone(port_pool), ports);
        tracing::info!(
            "Allocated ports for session {}: RTP={}, RTCP={}",
            call_id.0,
            ports.rtp_port,
            ports.rtcp_port
        );

        // Create socket pair
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;
        port_guard.disarm();

        let default_codec_config = ParticipantCodecConfig::default();

        let participant_a = Participant {
            id: participant_a_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU (legacy field)
            codec_config: default_codec_config.clone(),
            stats: ParticipantStats::default(),
            latch_allowed_ips: None,
        };

        let participant_b = Participant {
            id: participant_b_id,
            remote_addr: None,
            payload_type: 0, // Default to PCMU (legacy field)
            codec_config: default_codec_config,
            stats: ParticipantStats::default(),
            latch_allowed_ips: None,
        };
        let participant_a_codec = participant_a.codec_config.codec;
        let participant_b_codec = participant_b.codec_config.codec;

        let now = Instant::now();

        // VAD config is cloned out before `config` is moved into the
        // struct literal below.
        let vad_detector_config = config.vad_config.detector.clone();

        let session = Self {
            call_id: call_id.clone(),
            state: Arc::new(RwLock::new(SessionState::Initializing)),
            participant_a: Arc::new(RwLock::new(participant_a)),
            participant_b: Arc::new(RwLock::new(participant_b)),
            sockets: Arc::new(sockets),
            ports,
            port_pool: Arc::clone(port_pool),
            ports_deallocated: Arc::new(AtomicBool::new(false)),
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            dtmf_detector: Arc::new(Mutex::new(forge_dtmf::Rfc2833Detector::new(8000))),
            inband_detector: Arc::new(Mutex::new(forge_dtmf::GoertzelDetector::new(8000, 160))),
            dtmf_dedup: Arc::new(Mutex::new(forge_dtmf::DtmfDeduplicator::new())),
            vad_detector: Arc::new(Mutex::new(forge_vad::VadDetector::new(vad_detector_config))),
            speech_started_at: Arc::new(Mutex::new(None)),
            transcoder_a_to_b: Arc::new(Mutex::new(None)),
            transcoder_b_to_a: Arc::new(Mutex::new(None)),
            srtp_a: Arc::new(Mutex::new(SrtpContext::new())),
            srtp_b: Arc::new(Mutex::new(SrtpContext::new())),
            #[cfg(feature = "dtls")]
            dtls_a: Arc::new(Mutex::new(None)),
            #[cfg(feature = "dtls")]
            dtls_b: Arc::new(Mutex::new(None)),
            relay_rfc2833: AtomicBool::new(false),
            telephone_event_pt_a: AtomicU8::new(101),
            telephone_event_pt_b: AtomicU8::new(101),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
            sdp,
            from_tag,
            to_tag,
            xdp_manager,
            xdp_active: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "ai")]
            ai_manager: Arc::new(RwLock::new(None)),
            media_bridge_manager: Arc::new(RwLock::new(None)),
            codec_runtime_a: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_a_codec,
            )?)),
            codec_runtime_b: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_b_codec,
            )?)),
            playout_queue_a: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            playout_queue_b: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            generated_rtp_state_a: Arc::new(Mutex::new(GeneratedRtpState::default())),
            generated_rtp_state_b: Arc::new(Mutex::new(GeneratedRtpState::default())),
            recorder: Arc::new(RwLock::new(None)),
            recording_mixer: Arc::new(Mutex::new(RecordingMixer::default())),
        };

        // Publish session created event
        if let Some(bus) = &event_bus {
            let _ = bus.publish(ForgeEvent::SessionCreated {
                call_id,
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(session)
    }

    /// Get the call ID
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }

    /// Get the current session state
    pub async fn state(&self) -> SessionState {
        *self.state.read().await
    }

    /// Get the allocated port pair
    pub fn ports(&self) -> PortPair {
        self.ports
    }

    /// Get participant A statistics
    pub async fn participant_a_stats(&self) -> ParticipantStats {
        self.participant_a.read().await.stats.clone()
    }

    /// Get participant B statistics
    pub async fn participant_b_stats(&self) -> ParticipantStats {
        self.participant_b.read().await.stats.clone()
    }

    /// Get session uptime
    pub fn uptime(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Get time since last activity
    pub async fn idle_time(&self) -> Duration {
        self.last_activity.read().await.elapsed()
    }

    /// Check if session has timed out
    pub async fn is_timed_out(&self) -> bool {
        self.idle_time().await > self.config.session_timeout
    }

    /// Get the event bus
    pub fn event_bus(&self) -> Option<&Arc<EventBus>> {
        self.event_bus.as_ref()
    }

    /// Get the DTMF detector
    pub fn dtmf_detector(&self) -> &Arc<Mutex<forge_dtmf::Rfc2833Detector>> {
        &self.dtmf_detector
    }

    /// Get the inband DTMF detector
    pub fn inband_detector(&self) -> &Arc<Mutex<forge_dtmf::GoertzelDetector>> {
        &self.inband_detector
    }

    /// Get the DTMF deduplicator
    pub fn dtmf_dedup(&self) -> &Arc<Mutex<forge_dtmf::DtmfDeduplicator>> {
        &self.dtmf_dedup
    }

    /// Get the DTMF configuration
    pub fn dtmf_config(&self) -> &DtmfConfig {
        &self.config.dtmf_config
    }

    /// Get the voice-activity detector for this session.
    pub fn vad_detector(&self) -> &Arc<Mutex<forge_vad::VadDetector>> {
        &self.vad_detector
    }

    /// `speech_started_at` bookkeeping used by the forwarding loop
    /// to compute `duration_ms` on `SpeechStopped`.
    pub fn speech_started_at(&self) -> &Arc<Mutex<Option<DateTime<Utc>>>> {
        &self.speech_started_at
    }

    /// Get the VAD configuration.
    pub fn vad_config(&self) -> &VadConfig {
        &self.config.vad_config
    }

    /// Get the transcoding configuration
    pub fn transcoding_config(&self) -> &TranscodingConfig {
        &self.config.transcoding_config
    }

    /// Cadence for publishing `MediaStatsSnapshot` events (`None` = never).
    pub fn media_stats_interval(&self) -> Option<Duration> {
        self.config.media_stats_interval
    }

    /// Get transcoder for A → B direction
    pub fn transcoder_a_to_b(&self) -> &Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>> {
        &self.transcoder_a_to_b
    }

    /// Get transcoder for B → A direction
    pub fn transcoder_b_to_a(&self) -> &Arc<Mutex<Option<forge_transcoder::RtpTranscoder>>> {
        &self.transcoder_b_to_a
    }

    /// Convert forge_core::AudioCodec to forge_codecs::AudioCodecType
    fn to_transcoder_codec(codec: forge_core::AudioCodec) -> Option<forge_codecs::AudioCodecType> {
        match codec {
            forge_core::AudioCodec::PCMU => Some(forge_codecs::AudioCodecType::PCMU),
            forge_core::AudioCodec::PCMA => Some(forge_codecs::AudioCodecType::PCMA),
            forge_core::AudioCodec::Opus => Some(forge_codecs::AudioCodecType::Opus),
            forge_core::AudioCodec::PCM => Some(forge_codecs::AudioCodecType::PCM),
            // Codecs not supported by transcoder yet
            _ => None,
        }
    }

    /// Initialize transcoder for A → B if needed
    pub async fn ensure_transcoder_a_to_b(
        &self,
        src_codec: forge_codecs::AudioCodecType,
        dst_codec: forge_codecs::AudioCodecType,
    ) -> Result<()> {
        if !self.config.transcoding_config.enable_transcoding {
            return Ok(());
        }

        if src_codec == dst_codec {
            return Ok(()); // No transcoding needed
        }

        let mut transcoder = self.transcoder_a_to_b.lock().await;
        if transcoder.is_none() {
            tracing::info!(
                "Initializing transcoder for session {} A→B: {} → {}",
                self.call_id.0,
                codec_name(src_codec),
                codec_name(dst_codec)
            );

            let pt_map = self.config.transcoding_config.payload_type_map;
            let new_transcoder = forge_transcoder::RtpTranscoder::new(src_codec, dst_codec, pt_map)
                .map_err(|e| ForgeError::Internal(format!("Failed to create transcoder: {}", e)))?;

            *transcoder = Some(new_transcoder);
        }

        Ok(())
    }

    /// Initialize transcoder for B → A if needed
    pub async fn ensure_transcoder_b_to_a(
        &self,
        src_codec: forge_codecs::AudioCodecType,
        dst_codec: forge_codecs::AudioCodecType,
    ) -> Result<()> {
        if !self.config.transcoding_config.enable_transcoding {
            return Ok(());
        }

        if src_codec == dst_codec {
            return Ok(()); // No transcoding needed
        }

        let mut transcoder = self.transcoder_b_to_a.lock().await;
        if transcoder.is_none() {
            tracing::info!(
                "Initializing transcoder for session {} B→A: {} → {}",
                self.call_id.0,
                codec_name(src_codec),
                codec_name(dst_codec)
            );

            let pt_map = self.config.transcoding_config.payload_type_map;
            let new_transcoder = forge_transcoder::RtpTranscoder::new(src_codec, dst_codec, pt_map)
                .map_err(|e| ForgeError::Internal(format!("Failed to create transcoder: {}", e)))?;

            *transcoder = Some(new_transcoder);
        }

        Ok(())
    }

    /// Activate XDP fast path for this session
    /// Should be called after both participants' endpoints are learned
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn activate_xdp_fast_path(&self) -> Result<()> {
        // Check if XDP is available
        let xdp_manager = match &self.xdp_manager {
            Some(mgr) => mgr,
            None => {
                tracing::debug!("XDP not available for session {}", self.call_id.0);
                return Ok(());
            }
        };

        // Check if already active
        if self.xdp_active.load(Ordering::Relaxed) {
            tracing::debug!(
                "XDP fast path already active for session {}",
                self.call_id.0
            );
            return Ok(());
        }

        // Get participant addresses
        let (a_addr, b_addr) = {
            let a = self.participant_a.read().await;
            let b = self.participant_b.read().await;

            match (a.remote_addr, b.remote_addr) {
                (Some(a_addr), Some(b_addr)) => (a_addr, b_addr),
                _ => {
                    tracing::warn!(
                        "Cannot activate XDP fast path - not all endpoints learned for session {}",
                        self.call_id.0
                    );
                    return Ok(());
                }
            }
        };

        tracing::info!(
            "Activating XDP fast path for session {} (A: {} <-> B: {})",
            self.call_id.0,
            a_addr,
            b_addr
        );

        // Helper to convert SocketAddr to network byte order
        fn addr_to_network_bytes(addr: SocketAddr) -> (u32, u16) {
            let ip_bytes = match addr.ip() {
                std::net::IpAddr::V4(ipv4) => ipv4.octets(),
                std::net::IpAddr::V6(_) => {
                    // XDP currently only supports IPv4
                    return (0, 0);
                }
            };
            let ip_u32 = u32::from_ne_bytes(ip_bytes);
            let port_be = addr.port().to_be();
            (ip_u32, port_be)
        }

        let (a_ip, a_port) = addr_to_network_bytes(a_addr);
        let (b_ip, b_port) = addr_to_network_bytes(b_addr);
        let rtp_port_be = self.ports.rtp_port.to_be();

        // Insert bidirectional forwarding rules
        // Rule 1: A -> B (packets from A forwarded to B)
        let key_a_to_b = ForwardKey {
            src_ip: a_ip,
            src_port: a_port,
            dst_port: rtp_port_be,
            dst_ip: 0,    // Will be filled by XDP program (our local IP)
            protocol: 17, // UDP
            _padding: [0; 3],
        };

        let value_a_to_b = ForwardValue {
            dest_ip: b_ip,
            dest_port: b_port,
            src_ip: 0, // Our IP for reply
            src_port: rtp_port_be,
            last_seen: 0,
        };

        xdp_manager
            .insert_forward_rule(key_a_to_b, value_a_to_b)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP insert forward rule failed: {}", e)))?;

        // Rule 2: B -> A (packets from B forwarded to A)
        let key_b_to_a = ForwardKey {
            src_ip: b_ip,
            src_port: b_port,
            dst_port: rtp_port_be,
            dst_ip: 0,
            protocol: 17,
            _padding: [0; 3],
        };

        let value_b_to_a = ForwardValue {
            dest_ip: a_ip,
            dest_port: a_port,
            src_ip: 0,
            src_port: rtp_port_be,
            last_seen: 0,
        };

        xdp_manager
            .insert_forward_rule(key_b_to_a, value_b_to_a)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP insert forward rule failed: {}", e)))?;

        self.xdp_active.store(true, Ordering::Relaxed);

        tracing::info!("XDP fast path activated for session {}", self.call_id.0);

        Ok(())
    }

    /// Deactivate XDP fast path for this session
    #[cfg(all(target_os = "linux", feature = "xdp"))]
    pub async fn deactivate_xdp_fast_path(&self) -> Result<()> {
        // Check if XDP is available and active
        let xdp_manager = match &self.xdp_manager {
            Some(mgr) => mgr,
            None => return Ok(()),
        };

        if !self.xdp_active.load(Ordering::Relaxed) {
            return Ok(());
        }

        tracing::info!("Deactivating XDP fast path for session {}", self.call_id.0);

        // Get participant addresses
        let (a_addr, b_addr) = {
            let a = self.participant_a.read().await;
            let b = self.participant_b.read().await;

            match (a.remote_addr, b.remote_addr) {
                (Some(a_addr), Some(b_addr)) => (a_addr, b_addr),
                _ => {
                    self.xdp_active.store(false, Ordering::Relaxed);
                    return Ok(());
                }
            }
        };

        // Helper to convert SocketAddr to network byte order
        fn addr_to_network_bytes(addr: SocketAddr) -> (u32, u16) {
            let ip_bytes = match addr.ip() {
                std::net::IpAddr::V4(ipv4) => ipv4.octets(),
                std::net::IpAddr::V6(_) => return (0, 0),
            };
            let ip_u32 = u32::from_ne_bytes(ip_bytes);
            let port_be = addr.port().to_be();
            (ip_u32, port_be)
        }

        let (a_ip, a_port) = addr_to_network_bytes(a_addr);
        let (b_ip, b_port) = addr_to_network_bytes(b_addr);
        let rtp_port_be = self.ports.rtp_port.to_be();

        // Remove bidirectional forwarding rules
        let key_a_to_b = ForwardKey {
            src_ip: a_ip,
            src_port: a_port,
            dst_port: rtp_port_be,
            dst_ip: 0,
            protocol: 17,
            _padding: [0; 3],
        };

        let key_b_to_a = ForwardKey {
            src_ip: b_ip,
            src_port: b_port,
            dst_port: rtp_port_be,
            dst_ip: 0,
            protocol: 17,
            _padding: [0; 3],
        };

        xdp_manager
            .remove_forward_rule(&key_a_to_b)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP remove forward rule failed: {}", e)))?;
        xdp_manager
            .remove_forward_rule(&key_b_to_a)
            .await
            .map_err(|e| ForgeError::Internal(format!("XDP remove forward rule failed: {}", e)))?;

        self.xdp_active.store(false, Ordering::Relaxed);

        tracing::info!("XDP fast path deactivated for session {}", self.call_id.0);

        Ok(())
    }

    /// Start the RTP forwarding loop
    pub async fn start_forwarding(self: &Arc<Self>) -> Result<()> {
        let mut state = self.state.write().await;
        if *state != SessionState::Initializing {
            return Err(ForgeError::Internal(
                "Session must be in Initializing state to start forwarding".to_string(),
            ));
        }

        *state = SessionState::Active;
        drop(state);

        tracing::info!("Starting RTP forwarding for session {}", self.call_id.0);

        // Publish state change event
        if let Some(bus) = &self.event_bus {
            let _ = bus.publish(ForgeEvent::SessionActive {
                call_id: self.call_id.clone(),
                timestamp: chrono::Utc::now(),
            });
        }

        // Start forwarding task
        let forwarding_handle =
            crate::forwarding::ForwardingEngine::start_forwarding(Arc::clone(self)).await?;
        self.forwarding_tasks.lock().await.push(forwarding_handle);

        Ok(())
    }

    /// Stop the RTP forwarding loop
    pub async fn stop_forwarding(&self) -> Result<()> {
        let mut state = self.state.write().await;
        if *state == SessionState::Terminated {
            return Ok(());
        }

        *state = SessionState::Terminating;
        drop(state);

        tracing::info!("Stopping RTP forwarding for session {}", self.call_id.0);

        // Deactivate XDP fast path if active
        #[cfg(all(target_os = "linux", feature = "xdp"))]
        {
            if let Err(e) = self.deactivate_xdp_fast_path().await {
                tracing::error!("Failed to deactivate XDP fast path: {}", e);
            }
        }

        // Cancel all forwarding tasks
        let mut tasks = self.forwarding_tasks.lock().await;
        for task in tasks.drain(..) {
            task.abort();
        }

        *self.state.write().await = SessionState::Terminated;

        // Deallocate ports - guaranteed cleanup
        self.deallocate_ports().await;

        // Publish termination event
        if let Some(bus) = &self.event_bus {
            let _ = bus.publish(ForgeEvent::SessionTerminated {
                call_id: self.call_id.clone(),
                reason: "Stopped by request".to_string(),
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(())
    }

    /// Deallocate ports (idempotent)
    async fn deallocate_ports(&self) {
        // Use compare_exchange to ensure we only deallocate once
        if self
            .ports_deallocated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tracing::debug!(
                "Deallocating ports for session {}: RTP={}, RTCP={}",
                self.call_id.0,
                self.ports.rtp_port,
                self.ports.rtcp_port
            );
            self.port_pool.deallocate(self.ports).await;
        }
    }

    /// Update last activity timestamp
    /// This should be called whenever RTP packets are received/forwarded
    pub async fn update_activity(&self) {
        *self.last_activity.write().await = Instant::now();
    }

    /// Get the socket pair (for forwarding implementation)
    pub fn sockets(&self) -> &Arc<RtpSocketPair> {
        &self.sockets
    }

    /// Get mutable reference to participant A
    pub fn participant_a(&self) -> &Arc<RwLock<Participant>> {
        &self.participant_a
    }

    /// Get mutable reference to participant B
    pub fn participant_b(&self) -> &Arc<RwLock<Participant>> {
        &self.participant_b
    }

    fn participant_lock(&self, leg: ParticipantLabel) -> &Arc<RwLock<Participant>> {
        match leg {
            ParticipantLabel::A => &self.participant_a,
            ParticipantLabel::B => &self.participant_b,
        }
    }

    fn telephone_event_pt_for_leg(&self, leg: ParticipantLabel) -> u8 {
        match leg {
            ParticipantLabel::A => self.telephone_event_pt_a(),
            ParticipantLabel::B => self.telephone_event_pt_b(),
        }
    }

    fn codec_runtime_lock(&self, leg: ParticipantLabel) -> &Arc<Mutex<ParticipantCodecRuntime>> {
        match leg {
            ParticipantLabel::A => &self.codec_runtime_a,
            ParticipantLabel::B => &self.codec_runtime_b,
        }
    }

    fn playout_queue_lock(&self, leg: ParticipantLabel) -> &Arc<Mutex<ScheduledPlayoutQueue>> {
        match leg {
            ParticipantLabel::A => &self.playout_queue_a,
            ParticipantLabel::B => &self.playout_queue_b,
        }
    }

    pub(crate) fn codec_audio_sample_rate(
        codec: forge_core::AudioCodec,
        negotiated_clock_rate: u32,
    ) -> u32 {
        match codec {
            forge_core::AudioCodec::G722 => 16000,
            // Opus negotiates a 48 kHz RTP clock (`opus/48000/2`) but we run
            // the codec at 16 kHz: libopus decodes any encoded stream to the
            // decoder's configured rate (and downmixes to mono) internally,
            // so the bridge sees 16 kHz mono PCM. The 48 kHz RTP clock is
            // still used for timestamp stepping (see `clock_rate / 50`),
            // exactly the G.722 "wire clock != PCM rate" split.
            forge_core::AudioCodec::Opus => 16000,
            _ => negotiated_clock_rate,
        }
    }

    pub(crate) fn frame_samples_for_codec(
        codec: forge_core::AudioCodec,
        sample_rate: u32,
    ) -> Option<usize> {
        match codec {
            forge_core::AudioCodec::PCMU
            | forge_core::AudioCodec::PCMA
            | forge_core::AudioCodec::Opus
            | forge_core::AudioCodec::G722 => Some((sample_rate / 50) as usize),
            _ => None,
        }
    }

    pub(crate) async fn decode_with_codec_runtime(
        &self,
        leg: ParticipantLabel,
        codec: forge_core::AudioCodec,
        payload: &[u8],
    ) -> Result<Vec<i16>> {
        match codec {
            forge_core::AudioCodec::PCMU => Ok(payload
                .iter()
                .map(|&byte| forge_codecs::g711::decode_ulaw(byte))
                .collect()),
            forge_core::AudioCodec::PCMA => Ok(payload
                .iter()
                .map(|&byte| forge_codecs::g711::decode_alaw(byte))
                .collect()),
            forge_core::AudioCodec::Opus | forge_core::AudioCodec::G722 => {
                let mut runtime = self.codec_runtime_lock(leg).lock().await;
                if runtime.inbound_codec != codec {
                    runtime.reset(codec)?;
                }
                runtime
                    .inbound
                    .as_mut()
                    .ok_or_else(|| {
                        ForgeError::Codec(format!("No inbound codec runtime for {:?}", codec))
                    })?
                    .decode(payload)
            }
            other => Err(ForgeError::Codec(format!(
                "Inbound decode not supported for codec {:?}",
                other
            ))),
        }
    }

    pub(crate) async fn encode_with_codec_runtime(
        &self,
        leg: ParticipantLabel,
        codec: forge_core::AudioCodec,
        samples: &[i16],
    ) -> Result<Vec<u8>> {
        match codec {
            forge_core::AudioCodec::PCMU => Ok(samples
                .iter()
                .map(|&sample| forge_codecs::g711::encode_ulaw(sample))
                .collect()),
            forge_core::AudioCodec::PCMA => Ok(samples
                .iter()
                .map(|&sample| forge_codecs::g711::encode_alaw(sample))
                .collect()),
            forge_core::AudioCodec::Opus | forge_core::AudioCodec::G722 => {
                let mut runtime = self.codec_runtime_lock(leg).lock().await;
                if runtime.outbound_codec != codec {
                    runtime.reset(codec)?;
                }
                runtime
                    .outbound
                    .as_mut()
                    .ok_or_else(|| {
                        ForgeError::Codec(format!("No outbound codec runtime for {:?}", codec))
                    })?
                    .encode(samples)
            }
            other => Err(ForgeError::Codec(format!(
                "Outbound encode not supported for codec {:?}",
                other
            ))),
        }
    }

    pub(crate) async fn reset_codec_runtime(
        &self,
        leg: ParticipantLabel,
        codec: forge_core::AudioCodec,
    ) -> Result<()> {
        self.codec_runtime_lock(leg).lock().await.reset(codec)
    }

    pub(crate) async fn schedule_audio_playout(
        &self,
        target: crate::media_bridge::MediaTarget,
        sample_rate: u32,
        samples: &[i16],
        playback_id: Option<String>,
        mode: crate::media_bridge::PlayoutMode,
        source: ScheduledPlayoutSource,
    ) -> Result<()> {
        for leg in [ParticipantLabel::A, ParticipantLabel::B] {
            if !target.includes(leg) {
                continue;
            }
            self.schedule_audio_playout_for_leg(
                leg,
                sample_rate,
                samples,
                playback_id.clone(),
                mode,
                source,
            )
            .await?;
        }
        Ok(())
    }

    async fn schedule_audio_playout_for_leg(
        &self,
        leg: ParticipantLabel,
        sample_rate: u32,
        samples: &[i16],
        playback_id: Option<String>,
        mode: crate::media_bridge::PlayoutMode,
        source: ScheduledPlayoutSource,
    ) -> Result<()> {
        let codec_config = self.participant_lock(leg).read().await.codec_config.clone();
        let audio_sample_rate =
            Self::codec_audio_sample_rate(codec_config.codec, codec_config.clock_rate);
        let frame_samples = Self::frame_samples_for_codec(codec_config.codec, audio_sample_rate)
            .ok_or_else(|| {
                ForgeError::Codec(format!(
                    "Unsupported playout codec {:?} for leg {}",
                    codec_config.codec,
                    leg.as_str()
                ))
            })?;

        let resampled_samples = if sample_rate != audio_sample_rate {
            crate::forwarding::ForwardingEngine::resample_audio(
                samples,
                sample_rate,
                audio_sample_rate,
            )
        } else {
            samples.to_vec()
        };

        if mode == crate::media_bridge::PlayoutMode::Replace {
            self.clear_scheduled_playout_for_leg(leg, playback_id.as_deref())
                .await;
        }

        // Snapshot the RTP cursor before taking the queue lock so we don't nest locks.
        let rtp_cursor_fallback = self.generated_rtp_state(leg).lock().await.next_timestamp;

        let now = Instant::now();
        let mut queue = self.playout_queue_lock(leg).lock().await;

        // RFC 3551 §4.1: the RTP marker bit flags the first packet of a
        // talkspurt — audio resuming after a silence gap — not every packet.
        // Streaming callers hand us one 20 ms frame per call and the playout
        // pump drains the queue between frames, so "queue is empty" is *not*
        // a usable signal (it almost always is). Instead compare `now` with
        // when the previously scheduled audio was due to finish: if real time
        // has run past it by more than `TALKSPURT_SILENCE_GAP`, the outbound
        // stream underran (silence elapsed) and this frame opens a new
        // talkspurt. A `Replace` (barge-in) always opens one too.
        let starts_talkspurt = mode == crate::media_bridge::PlayoutMode::Replace
            || match queue.audio_stream_end {
                None => true,
                Some(end) => now >= end + TALKSPURT_SILENCE_GAP,
            };

        let mut due_at = queue.next_due_at.unwrap_or(now);
        if due_at < now {
            due_at = now;
        }
        let mut stream_cursor = queue.next_rtp_timestamp.unwrap_or(rtp_cursor_fallback);
        let timestamp_increment = codec_config.clock_rate / 50;

        for (index, chunk) in resampled_samples.chunks(frame_samples).enumerate() {
            let mut frame = chunk.to_vec();
            if frame.len() < frame_samples {
                frame.resize(frame_samples, 0);
            }

            let timestamp = stream_cursor;
            stream_cursor = stream_cursor.wrapping_add(timestamp_increment);

            queue.items.push_back(ScheduledPlayoutItem {
                due_at,
                playback_id: playback_id.clone(),
                marker: starts_talkspurt && index == 0,
                timestamp,
                stream_cursor_after: stream_cursor,
                kind: ScheduledPlayoutKind::Audio {
                    codec: codec_config.codec,
                    payload_type: codec_config.payload_type,
                    samples: frame,
                },
                source,
            });
            due_at += Duration::from_millis(20);
        }

        queue.next_due_at = Some(due_at);
        queue.next_rtp_timestamp = Some(stream_cursor);
        // Persist the stream end so the *next* append can tell continuation
        // from a post-silence resume even after the pump drains the queue.
        queue.audio_stream_end = Some(due_at);
        Ok(())
    }

    pub(crate) async fn schedule_dtmf_playout(
        &self,
        target: crate::media_bridge::MediaTarget,
        digit: forge_dtmf::DtmfDigit,
        duration_ms: u32,
        playback_id: Option<String>,
        mode: crate::media_bridge::PlayoutMode,
        source: ScheduledPlayoutSource,
    ) -> Result<()> {
        for leg in [ParticipantLabel::A, ParticipantLabel::B] {
            if !target.includes(leg) {
                continue;
            }
            self.schedule_dtmf_playout_for_leg(
                leg,
                digit,
                duration_ms,
                playback_id.clone(),
                mode,
                source,
            )
            .await?;
        }
        Ok(())
    }

    async fn schedule_dtmf_playout_for_leg(
        &self,
        leg: ParticipantLabel,
        digit: forge_dtmf::DtmfDigit,
        duration_ms: u32,
        playback_id: Option<String>,
        mode: crate::media_bridge::PlayoutMode,
        source: ScheduledPlayoutSource,
    ) -> Result<()> {
        let clock_rate = self
            .participant_lock(leg)
            .read()
            .await
            .codec_config
            .clock_rate;
        let payload_type = self.telephone_event_pt_for_leg(leg);
        let packet_interval_ms = 20u32;
        let duration_ms = duration_ms.max(packet_interval_ms);
        let mut generator = forge_dtmf::Rfc2833Generator::new(clock_rate, packet_interval_ms);

        let mut events = vec![generator.start_digit(digit)];
        while generator.current_duration_ms() + packet_interval_ms < duration_ms {
            if let Some(event) = generator.continue_digit() {
                events.push(event);
            }
        }
        if let Some(mut end_packets) = generator.end_digit() {
            events.append(&mut end_packets);
        }

        if mode == crate::media_bridge::PlayoutMode::Replace {
            self.clear_scheduled_playout_for_leg(leg, playback_id.as_deref())
                .await;
        }

        // Snapshot the RTP cursor before taking the queue lock so we don't nest locks.
        let rtp_cursor_fallback = self.generated_rtp_state(leg).lock().await.next_timestamp;

        let now = Instant::now();
        let mut queue = self.playout_queue_lock(leg).lock().await;
        let base_timestamp = queue.next_rtp_timestamp.unwrap_or(rtp_cursor_fallback);
        let mut due_at = queue.next_due_at.unwrap_or(now);
        if due_at < now {
            due_at = now;
        }

        let final_cursor = base_timestamp.wrapping_add(
            events
                .last()
                .map(|event| event.duration() as u32)
                .unwrap_or(0),
        );

        for (index, event) in events.into_iter().enumerate() {
            let stream_cursor_after = if event.is_end() {
                final_cursor
            } else {
                base_timestamp
            };
            queue.items.push_back(ScheduledPlayoutItem {
                due_at,
                playback_id: playback_id.clone(),
                marker: index == 0,
                timestamp: base_timestamp,
                stream_cursor_after,
                kind: ScheduledPlayoutKind::Dtmf {
                    payload_type,
                    payload: event.to_bytes(),
                },
                source,
            });
            due_at += Duration::from_millis(packet_interval_ms as u64);
        }

        queue.next_due_at = Some(due_at);
        queue.next_rtp_timestamp = Some(final_cursor);
        Ok(())
    }

    pub(crate) async fn clear_scheduled_playout(
        &self,
        target: Option<crate::media_bridge::MediaTarget>,
        playback_id: Option<&str>,
    ) -> usize {
        let mut removed = 0;

        for leg in [ParticipantLabel::A, ParticipantLabel::B] {
            if target.map(|t| t.includes(leg)).unwrap_or(true) {
                removed += self.clear_scheduled_playout_for_leg(leg, playback_id).await;
            }
        }

        removed
    }

    async fn clear_scheduled_playout_for_leg(
        &self,
        leg: ParticipantLabel,
        playback_id: Option<&str>,
    ) -> usize {
        let mut queue = self.playout_queue_lock(leg).lock().await;
        let original_len = queue.items.len();
        // playback_id = Some(id): drop only items tagged with that id (replace one playback,
        // keep concurrent ones). playback_id = None: drop everything queued for this leg
        // (codec/PT change, full barge-in).
        queue.items.retain(|item| {
            !playback_id
                .map(|id| item.playback_id.as_deref() == Some(id))
                .unwrap_or(true)
        });
        let removed = original_len.saturating_sub(queue.items.len());
        Self::recompute_playout_queue_state(&mut queue);
        removed
    }

    fn recompute_playout_queue_state(queue: &mut ScheduledPlayoutQueue) {
        if let Some(last) = queue.items.back() {
            queue.next_due_at = Some(last.due_at + Duration::from_millis(20));
            queue.next_rtp_timestamp = Some(last.stream_cursor_after);
            queue.audio_stream_end = Some(last.due_at + Duration::from_millis(20));
        } else {
            queue.next_due_at = None;
            queue.next_rtp_timestamp = None;
            // Queue was explicitly cleared (barge-in / codec change): the
            // committed audio is gone, so the next append starts a fresh
            // talkspurt. (Natural draining in `take_due_playout_items` does
            // not call this and deliberately leaves `audio_stream_end` set.)
            queue.audio_stream_end = None;
        }
    }

    pub(crate) async fn take_due_playout_items(
        &self,
        leg: ParticipantLabel,
        now: Instant,
    ) -> Vec<ScheduledPlayoutItem> {
        let mut queue = self.playout_queue_lock(leg).lock().await;
        let mut due = Vec::new();
        while queue
            .items
            .front()
            .map(|item| item.due_at <= now)
            .unwrap_or(false)
        {
            if let Some(item) = queue.items.pop_front() {
                due.push(item);
            }
        }
        if queue.items.is_empty() {
            queue.next_due_at = None;
            queue.next_rtp_timestamp = None;
        }
        due
    }

    /// Get the runtime media configuration for a participant leg.
    pub async fn participant_media_state(&self, leg: ParticipantLabel) -> ParticipantMediaState {
        let participant = self.participant_lock(leg).read().await;
        ParticipantMediaState::from_participant(
            leg,
            &participant,
            self.telephone_event_pt_for_leg(leg),
        )
    }

    /// Apply a runtime media update to a participant leg.
    ///
    /// This is primarily intended for signaling controllers that already know
    /// the negotiated remote RTP endpoint and codec mapping, such as a SIP
    /// B2BUA built on top of `siphon-rs`.
    pub async fn update_participant_media(
        &self,
        leg: ParticipantLabel,
        update: ParticipantMediaUpdate,
    ) -> Result<ParticipantMediaState> {
        let codec_update = update.codec_config.clone();
        let telephone_event_pt_update = update.telephone_event_payload_type;

        if let Some(Some(remote_addr)) = update.remote_addr {
            tracing::info!(
                "Setting remote RTP endpoint for session {} leg {}: {}",
                self.call_id.0,
                leg.as_str(),
                remote_addr
            );
        } else if matches!(update.remote_addr, Some(None)) {
            tracing::info!(
                "Clearing remote RTP endpoint for session {} leg {}",
                self.call_id.0,
                leg.as_str()
            );
        }

        {
            let mut participant = self.participant_lock(leg).write().await;

            if let Some(remote_addr) = update.remote_addr {
                participant.remote_addr = remote_addr;
            }

            if let Some(codec_config) = update.codec_config {
                participant.payload_type = codec_config.payload_type;
                participant.codec_config = codec_config;
            }

            if let Some(latch_allowed_ips) = update.latch_allowed_ips {
                participant.latch_allowed_ips = latch_allowed_ips;
            }
        }

        if let Some(telephone_event_pt) = telephone_event_pt_update {
            match leg {
                ParticipantLabel::A => self.set_telephone_event_pt_a(telephone_event_pt),
                ParticipantLabel::B => self.set_telephone_event_pt_b(telephone_event_pt),
            }
        }

        if let Some(codec_config) = codec_update {
            self.reset_codec_runtime(leg, codec_config.codec).await?;
            self.clear_scheduled_playout(
                Some(match leg {
                    ParticipantLabel::A => crate::media_bridge::MediaTarget::A,
                    ParticipantLabel::B => crate::media_bridge::MediaTarget::B,
                }),
                None,
            )
            .await;
        } else if telephone_event_pt_update.is_some() {
            self.clear_scheduled_playout(
                Some(match leg {
                    ParticipantLabel::A => crate::media_bridge::MediaTarget::A,
                    ParticipantLabel::B => crate::media_bridge::MediaTarget::B,
                }),
                None,
            )
            .await;
        }

        Ok(self.participant_media_state(leg).await)
    }

    /// Get associated SDP (if any)
    pub fn sdp(&self) -> Option<&str> {
        self.sdp.as_deref()
    }

    /// Get from-tag (if any)
    pub fn from_tag(&self) -> Option<&str> {
        self.from_tag.as_deref()
    }

    /// Get to-tag (if any)
    pub fn to_tag(&self) -> Option<&str> {
        self.to_tag.as_deref()
    }

    /// Get the SRTP context for participant A
    pub fn srtp_a(&self) -> &Arc<Mutex<SrtpContext>> {
        &self.srtp_a
    }

    /// Get the SRTP context for participant B
    pub fn srtp_b(&self) -> &Arc<Mutex<SrtpContext>> {
        &self.srtp_b
    }

    /// DTLS-SRTP leg for participant A. `None` until [`enable_dtls`]
    /// installs it. The RTP recv loop checks this on every packet and
    /// demuxes DTLS bytes here when a leg is present.
    #[cfg(feature = "dtls")]
    pub fn dtls_a(&self) -> &Arc<Mutex<Option<crate::dtls_srtp::DtlsLeg>>> {
        &self.dtls_a
    }

    /// DTLS-SRTP leg for participant B.
    #[cfg(feature = "dtls")]
    pub fn dtls_b(&self) -> &Arc<Mutex<Option<crate::dtls_srtp::DtlsLeg>>> {
        &self.dtls_b
    }

    /// Install a DTLS-SRTP leg on `side`, with the supplied long-lived
    /// certificate, DTLS role (`Server` for the SDP answerer /
    /// `a=setup:passive`, `Client` for offerer / `a=setup:active`),
    /// and the remote's SDP `a=fingerprint:` value (used to verify the
    /// presented cert post-handshake per RFC 5763 §5).
    ///
    /// Replaces any prior leg on the same side — callers must guarantee
    /// the previous leg is no longer in use (e.g., the session is being
    /// re-INVITEd with new SDP).
    #[cfg(feature = "dtls")]
    pub async fn enable_dtls(
        &self,
        side: ParticipantLabel,
        cert: Arc<forge_rtp::dtls::DtlsCertificate>,
        role: forge_rtp::dtls::DtlsRole,
        remote_fingerprint: String,
    ) -> Result<()> {
        let leg = crate::dtls_srtp::DtlsLeg::new(cert, role, remote_fingerprint)?;
        let slot = match side {
            ParticipantLabel::A => &self.dtls_a,
            ParticipantLabel::B => &self.dtls_b,
        };
        *slot.lock().await = Some(leg);
        Ok(())
    }

    /// Whether RFC 2833 relay is enabled for this session
    pub fn relay_rfc2833(&self) -> bool {
        self.relay_rfc2833.load(Ordering::Relaxed)
    }

    /// Enable or disable RFC 2833 relay
    pub fn set_relay_rfc2833(&self, relay: bool) {
        self.relay_rfc2833.store(relay, Ordering::Relaxed);
    }

    /// Telephone-event payload type negotiated with participant A
    pub fn telephone_event_pt_a(&self) -> u8 {
        self.telephone_event_pt_a.load(Ordering::Relaxed)
    }

    /// Set telephone-event payload type for participant A
    pub fn set_telephone_event_pt_a(&self, pt: u8) {
        self.telephone_event_pt_a.store(pt, Ordering::Relaxed);
    }

    /// Telephone-event payload type negotiated with participant B
    pub fn telephone_event_pt_b(&self) -> u8 {
        self.telephone_event_pt_b.load(Ordering::Relaxed)
    }

    /// Set telephone-event payload type for participant B
    pub fn set_telephone_event_pt_b(&self, pt: u8) {
        self.telephone_event_pt_b.store(pt, Ordering::Relaxed);
    }

    /// Get a copy of the AI session manager (if set)
    #[cfg(feature = "ai")]
    pub async fn ai_manager(&self) -> Option<Arc<crate::ai_integration::AISessionManager>> {
        self.ai_manager.read().await.clone()
    }

    /// Set the AI session manager
    #[cfg(feature = "ai")]
    pub async fn set_ai_manager(&self, manager: Arc<crate::ai_integration::AISessionManager>) {
        *self.ai_manager.write().await = Some(manager);
    }

    /// Get a copy of the generic media bridge manager (if set).
    pub async fn media_bridge_manager(
        &self,
    ) -> Option<Arc<crate::media_bridge::MediaBridgeManager>> {
        self.media_bridge_manager.read().await.clone()
    }

    /// Set the generic media bridge manager.
    pub async fn set_media_bridge_manager(
        &self,
        manager: Arc<crate::media_bridge::MediaBridgeManager>,
    ) {
        *self.media_bridge_manager.write().await = Some(manager);
    }

    /// RTP sequencing state for generated audio toward a participant leg.
    pub(crate) fn generated_rtp_state(
        &self,
        leg: ParticipantLabel,
    ) -> Arc<Mutex<GeneratedRtpState>> {
        match leg {
            ParticipantLabel::A => Arc::clone(&self.generated_rtp_state_a),
            ParticipantLabel::B => Arc::clone(&self.generated_rtp_state_b),
        }
    }

    /// Get the recorder mixer used for call recordings
    pub(crate) fn recording_mixer(&self) -> Arc<Mutex<RecordingMixer>> {
        Arc::clone(&self.recording_mixer)
    }

    // =====================================================================
    // Call Recording Methods
    // =====================================================================

    /// Enable call recording
    ///
    /// Creates an AudioRecorder and starts recording RTP audio from both
    /// participants to the specified file path. The recording format is
    /// determined by the file extension (.wav or .opus).
    ///
    /// # Arguments
    /// * `path` - Output file path for the recording
    /// * `format` - Audio format configuration (codec, sample rate, channels)
    ///
    /// # Example
    /// ```ignore
    /// use forge_core::AudioFormat;
    /// use std::path::Path;
    ///
    /// // Record in WAV format
    /// session.enable_recording(
    ///     Path::new("/tmp/call.wav"),
    ///     AudioFormat::pcm_mono(8000)
    /// ).await?;
    /// ```
    pub async fn enable_recording<P: AsRef<std::path::Path>>(
        &self,
        path: P,
        format: forge_core::AudioFormat,
    ) -> Result<()> {
        let mut recorder_guard = self.recorder.write().await;

        if recorder_guard.is_some() {
            return Err(ForgeError::Internal("Recording already enabled".into()));
        }

        // Reset mixer state so we don't carry buffered frames between recordings
        self.recording_mixer.lock().await.reset();

        // Create recorder
        let recorder = forge_recorder::AudioRecorder::new(path, format)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to create recorder: {}", e)))?;

        // Start recording
        recorder
            .start()
            .map_err(|e| ForgeError::Internal(format!("Failed to start recording: {}", e)))?;

        tracing::info!(
            call_id = %self.call_id.0,
            "Call recording started"
        );

        *recorder_guard = Some(recorder);
        Ok(())
    }

    /// Disable call recording
    ///
    /// Stops the active recording and finalizes the output file.
    /// This is automatically called when the session is dropped.
    pub async fn disable_recording(&self) -> Result<()> {
        let mut recorder_guard = self.recorder.write().await;

        if let Some(recorder) = recorder_guard.take() {
            // Flush any buffered mixed frame before finalizing
            self.recording_mixer.lock().await.flush(&recorder);

            recorder
                .stop()
                .map_err(|e| ForgeError::Internal(format!("Failed to stop recording: {}", e)))?;

            tracing::info!(
                call_id = %self.call_id.0,
                "Call recording stopped"
            );
        }

        Ok(())
    }

    /// Check if recording is currently enabled
    pub async fn is_recording(&self) -> bool {
        self.recorder.read().await.is_some()
    }

    // =====================================================================
    // High Availability (HA) Methods
    // =====================================================================

    /// Serialize session state for HA replication (requires 'ha' feature)
    #[cfg(feature = "ha")]
    pub async fn to_state(&self) -> forge_ha::SessionState {
        use chrono::Utc;

        let state = *self.state.read().await;
        let participant_a = self.participant_a.read().await.clone();
        let participant_b = self.participant_b.read().await.clone();
        let last_activity = *self.last_activity.read().await;

        // Convert state enum to string
        let state_str = match state {
            SessionState::Initializing => "Initializing",
            SessionState::Active => "Active",
            SessionState::OnHold => "OnHold",
            SessionState::Terminating => "Terminating",
            SessionState::Terminated => "Terminated",
        }
        .to_string();

        // Convert participants
        let participant_a_state = forge_ha::types::ParticipantState {
            id: participant_a.id.0.to_string(),
            remote_addr: participant_a.remote_addr,
            codec: forge_ha::types::CodecConfig {
                payload_type: participant_a.codec_config.payload_type,
                codec: format!("{:?}", participant_a.codec_config.codec),
                clock_rate: participant_a.codec_config.clock_rate,
            },
            stats: forge_ha::types::ParticipantStats {
                packets_received: participant_a.stats.packets_received,
                bytes_received: participant_a.stats.bytes_received,
                packets_sent: participant_a.stats.packets_sent,
                bytes_sent: participant_a.stats.bytes_sent,
                packets_lost: participant_a.stats.packets_lost,
            },
        };

        let participant_b_state = forge_ha::types::ParticipantState {
            id: participant_b.id.0.to_string(),
            remote_addr: participant_b.remote_addr,
            codec: forge_ha::types::CodecConfig {
                payload_type: participant_b.codec_config.payload_type,
                codec: format!("{:?}", participant_b.codec_config.codec),
                clock_rate: participant_b.codec_config.clock_rate,
            },
            stats: forge_ha::types::ParticipantStats {
                packets_received: participant_b.stats.packets_received,
                bytes_received: participant_b.stats.bytes_received,
                packets_sent: participant_b.stats.packets_sent,
                bytes_sent: participant_b.stats.bytes_sent,
                packets_lost: participant_b.stats.packets_lost,
            },
        };

        // Convert ports
        let ports = forge_ha::types::PortPair {
            rtp_port: self.ports.rtp_port,
            rtcp_port: self.ports.rtcp_port,
        };

        // Convert times (Instant can't be serialized, use Utc::now() as approximation)
        let created_at =
            Utc::now() - chrono::Duration::from_std(self.created_at.elapsed()).unwrap_or_default();
        let last_activity_time =
            Utc::now() - chrono::Duration::from_std(last_activity.elapsed()).unwrap_or_default();

        // Check if transcoders are active
        let transcoder_a_to_b_active = self.transcoder_a_to_b.lock().await.is_some();
        let transcoder_b_to_a_active = self.transcoder_b_to_a.lock().await.is_some();

        let transcoder_state = if transcoder_a_to_b_active || transcoder_b_to_a_active {
            Some(forge_ha::types::TranscoderState {
                a_to_b_active: transcoder_a_to_b_active,
                b_to_a_active: transcoder_b_to_a_active,
                source_codec: Some(format!("{:?}", participant_a.codec_config.codec)),
                dest_codec: Some(format!("{:?}", participant_b.codec_config.codec)),
            })
        } else {
            None
        };

        // Check if XDP is active
        #[cfg(all(target_os = "linux", feature = "xdp"))]
        let xdp_active = self.xdp_active.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(all(target_os = "linux", feature = "xdp")))]
        let xdp_active = false;

        // Get AI session ID if present
        #[cfg(feature = "ai")]
        let ai_session_id = if self.ai_manager.read().await.is_some() {
            Some(self.call_id.0.to_string())
        } else {
            None
        };
        #[cfg(not(feature = "ai"))]
        let ai_session_id: Option<String> = None;

        forge_ha::SessionState {
            call_id: self.call_id.0.to_string(),
            state: state_str,
            participant_a: participant_a_state,
            participant_b: participant_b_state,
            ports,
            created_at,
            last_activity: last_activity_time,
            sdp: self.sdp.clone(),
            from_tag: self.from_tag.clone(),
            to_tag: self.to_tag.clone(),
            transcoder_state,
            xdp_active,
            ai_session_id,
            version: 1,
            instance_id: "".to_string(), // Will be filled by caller
        }
    }

    /// Deserialize and recover session from HA state (requires 'ha' feature)
    #[cfg(feature = "ha")]
    pub async fn from_state(
        state: forge_ha::SessionState,
        port_pool: &Arc<PortPool>,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
    ) -> Result<Self> {
        tracing::info!(
            "Recovering session {} from HA state (ports: RTP={}, RTCP={})",
            state.call_id,
            state.ports.rtp_port,
            state.ports.rtcp_port
        );

        // Reconstruct port pair
        let ports = forge_rtp::PortPair {
            rtp_port: state.ports.rtp_port,
            rtcp_port: state.ports.rtcp_port,
        };

        // Create socket pair with recovered ports
        let sockets = RtpSocketPair::new(ports, config.socket_config.clone()).await?;

        // Parse participant IDs (stored as UUID strings in state)
        let participant_a_id = ParticipantId(state.participant_a.id.clone());
        let participant_b_id = ParticipantId(state.participant_b.id.clone());

        // Parse codec
        let parse_codec = |codec_str: &str| -> forge_core::AudioCodec {
            match codec_str {
                "PCMU" => forge_core::AudioCodec::PCMU,
                "PCMA" => forge_core::AudioCodec::PCMA,
                "Opus" => forge_core::AudioCodec::Opus,
                "G729" => forge_core::AudioCodec::G729,
                _ => forge_core::AudioCodec::PCMU, // Default fallback
            }
        };

        let participant_a = Participant {
            id: participant_a_id,
            remote_addr: state.participant_a.remote_addr,
            payload_type: state.participant_a.codec.payload_type,
            codec_config: ParticipantCodecConfig {
                payload_type: state.participant_a.codec.payload_type,
                codec: parse_codec(&state.participant_a.codec.codec),
                clock_rate: state.participant_a.codec.clock_rate,
            },
            stats: ParticipantStats {
                packets_received: state.participant_a.stats.packets_received,
                bytes_received: state.participant_a.stats.bytes_received,
                packets_sent: state.participant_a.stats.packets_sent,
                bytes_sent: state.participant_a.stats.bytes_sent,
                packets_lost: state.participant_a.stats.packets_lost,
                last_packet_at: None, // Will be updated when packets arrive
                // Receive-side measurement state is node-local (it anchors
                // on monotonic Instants) — starts fresh after failover.
                rx_stream: RxStreamStats::default(),
            },
            latch_allowed_ips: None,
        };

        let participant_b = Participant {
            id: participant_b_id,
            remote_addr: state.participant_b.remote_addr,
            payload_type: state.participant_b.codec.payload_type,
            codec_config: ParticipantCodecConfig {
                payload_type: state.participant_b.codec.payload_type,
                codec: parse_codec(&state.participant_b.codec.codec),
                clock_rate: state.participant_b.codec.clock_rate,
            },
            stats: ParticipantStats {
                packets_received: state.participant_b.stats.packets_received,
                bytes_received: state.participant_b.stats.bytes_received,
                packets_sent: state.participant_b.stats.packets_sent,
                bytes_sent: state.participant_b.stats.bytes_sent,
                packets_lost: state.participant_b.stats.packets_lost,
                last_packet_at: None,
                rx_stream: RxStreamStats::default(),
            },
            latch_allowed_ips: None,
        };
        let participant_a_codec = participant_a.codec_config.codec;
        let participant_b_codec = participant_b.codec_config.codec;

        // Parse session state
        let session_state = match state.state.as_str() {
            "Initializing" => SessionState::Initializing,
            "Active" => SessionState::Active,
            "OnHold" => SessionState::OnHold,
            "Terminating" => SessionState::Terminating,
            "Terminated" => SessionState::Terminated,
            _ => SessionState::Active, // Default to active for recovery
        };

        let call_id = CallId(state.call_id.clone());

        let now = Instant::now();

        // VAD config is cloned out before `config` is moved into the
        // struct literal below.
        let vad_detector_config = config.vad_config.detector.clone();

        let session = Self {
            call_id,
            state: Arc::new(RwLock::new(session_state)),
            participant_a: Arc::new(RwLock::new(participant_a)),
            participant_b: Arc::new(RwLock::new(participant_b)),
            sockets: Arc::new(sockets),
            ports,
            port_pool: Arc::clone(port_pool),
            ports_deallocated: Arc::new(AtomicBool::new(false)),
            created_at: now, // Use current time as approximation
            last_activity: Arc::new(RwLock::new(now)),
            config,
            event_bus: event_bus.clone(),
            dtmf_detector: Arc::new(Mutex::new(forge_dtmf::Rfc2833Detector::new(8000))),
            inband_detector: Arc::new(Mutex::new(forge_dtmf::GoertzelDetector::new(8000, 160))),
            dtmf_dedup: Arc::new(Mutex::new(forge_dtmf::DtmfDeduplicator::new())),
            vad_detector: Arc::new(Mutex::new(forge_vad::VadDetector::new(vad_detector_config))),
            speech_started_at: Arc::new(Mutex::new(None)),
            transcoder_a_to_b: Arc::new(Mutex::new(None)),
            transcoder_b_to_a: Arc::new(Mutex::new(None)),
            srtp_a: Arc::new(Mutex::new(SrtpContext::new())),
            srtp_b: Arc::new(Mutex::new(SrtpContext::new())),
            #[cfg(feature = "dtls")]
            dtls_a: Arc::new(Mutex::new(None)),
            #[cfg(feature = "dtls")]
            dtls_b: Arc::new(Mutex::new(None)),
            forwarding_tasks: Arc::new(Mutex::new(Vec::new())),
            sdp: state.sdp,
            from_tag: state.from_tag,
            to_tag: state.to_tag,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_manager: None,
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            xdp_active: Arc::new(AtomicBool::new(state.xdp_active)),
            #[cfg(feature = "ai")]
            ai_manager: Arc::new(RwLock::new(None)),
            media_bridge_manager: Arc::new(RwLock::new(None)),
            codec_runtime_a: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_a_codec,
            )?)),
            codec_runtime_b: Arc::new(Mutex::new(ParticipantCodecRuntime::new(
                participant_b_codec,
            )?)),
            playout_queue_a: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            playout_queue_b: Arc::new(Mutex::new(ScheduledPlayoutQueue::default())),
            generated_rtp_state_a: Arc::new(Mutex::new(GeneratedRtpState::default())),
            generated_rtp_state_b: Arc::new(Mutex::new(GeneratedRtpState::default())),
            recorder: Arc::new(RwLock::new(None)),
            recording_mixer: Arc::new(Mutex::new(RecordingMixer::default())),
            relay_rfc2833: AtomicBool::new(false),
            telephone_event_pt_a: AtomicU8::new(101),
            telephone_event_pt_b: AtomicU8::new(101),
        };

        tracing::info!(
            "Session {} recovered successfully from HA state",
            session.call_id.0
        );

        Ok(session)
    }

    /// Synchronize session state to Redis for HA (requires 'ha' feature)
    #[cfg(feature = "ha")]
    pub async fn sync_to_redis(
        &self,
        redis: &forge_ha::RedisHAClient,
        instance_id: &str,
        ttl: std::time::Duration,
    ) -> Result<()> {
        let mut state = self.to_state().await;
        state.instance_id = instance_id.to_string();

        forge_ha::SessionStateSync::sync(redis, &self.call_id.0.to_string(), &state, ttl)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to sync session to Redis: {}", e)))?;

        Ok(())
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        tracing::debug!("MediaSession {} dropped", self.call_id.0);

        // Ensure ports are deallocated even if stop_forwarding was never called
        // Check if ports have already been deallocated
        if !self.ports_deallocated.load(Ordering::SeqCst) {
            tracing::warn!(
                "Session {} dropped without cleanup - spawning port deallocation task",
                self.call_id.0
            );

            // Spawn a detached task to deallocate ports asynchronously
            let port_pool = Arc::clone(&self.port_pool);
            let ports = self.ports;
            let ports_deallocated = Arc::clone(&self.ports_deallocated);
            let call_id = self.call_id.0.clone();

            tokio::spawn(async move {
                // Double-check to avoid race condition
                if ports_deallocated
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    tracing::debug!(
                        "Drop cleanup: Deallocating ports for session {}: RTP={}, RTCP={}",
                        call_id,
                        ports.rtp_port,
                        ports.rtcp_port
                    );
                    port_pool.deallocate(ports).await;
                }
            });
        }
    }
}

/// Direction for recording mixer bookkeeping.
///
/// Identifies which participant (A or B) in a two-party call sent a given audio frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingSide {
    /// Participant A (typically the caller)
    A,
    /// Participant B (typically the callee)
    B,
}

/// Minimal mixer that pairs recent frames from each leg before writing to recordings.
///
/// # Purpose
///
/// In two-party calls, RTP packets arrive independently from each participant. To create
/// a proper stereo recording where both sides can be heard together, we need to mix the
/// audio streams. This mixer buffers one frame at a time and combines it with the next
/// frame from the opposite side.
///
/// # Buffering Strategy
///
/// - When a frame arrives, if there's no buffered frame, store it
/// - When a frame arrives and a frame from the **opposite** side is buffered, mix them
/// - When a frame arrives and a frame from the **same** side is buffered, flush the old
///   frame and buffer the new one
/// - Frames older than 100ms are automatically flushed to prevent unbounded buffering
///
/// # Mixing Algorithm
///
/// Mixing uses amplitude-based detection to distinguish active speech from silence:
/// - Samples with amplitude > 10 are considered "active"
/// - When both sides are active, average the samples to prevent clipping
/// - When only one side is active, pass through unchanged
///
/// This approach preserves silence while properly mixing overlapping speech.
#[derive(Default)]
pub(crate) struct RecordingMixer {
    /// Buffered frame: (side, samples, timestamp)
    pending: Option<(RecordingSide, Vec<i16>, Instant)>,
}

/// Maximum age for buffered frame before auto-flush (100ms)
const STALE_FRAME_THRESHOLD: Duration = Duration::from_millis(100);

/// Amplitude threshold for considering a sample "active" (helps distinguish silence from speech)
const AMPLITUDE_THRESHOLD: i16 = 10;

impl RecordingMixer {
    /// Clear any buffered frame.
    ///
    /// Called when starting a new recording to ensure clean state.
    pub fn reset(&mut self) {
        self.pending = None;
    }

    /// Flush any buffered frame to the recorder.
    ///
    /// Called when stopping a recording to ensure the last frame is written.
    /// Write errors are silently ignored since the recording is ending anyway.
    pub fn flush(&mut self, recorder: &forge_recorder::AudioRecorder) {
        if let Some((_, samples, _)) = self.pending.take() {
            let _ = recorder.write_samples(&samples);
        }
    }

    /// Process incoming audio samples, mixing with buffered frames when appropriate.
    ///
    /// # Behavior
    ///
    /// 1. **No buffered frame**: Store the incoming frame
    /// 2. **Buffered frame from opposite side**: Mix them together and write
    /// 3. **Buffered frame from same side**: Flush buffered frame, store new one
    /// 4. **Stale buffered frame (>100ms)**: Flush it, then store new frame
    ///
    /// # Parameters
    ///
    /// - `call_id`: For logging context
    /// - `side`: Which participant sent this frame
    /// - `samples`: PCM audio samples (16-bit signed)
    /// - `recorder`: The active audio recorder
    ///
    /// # Notes
    ///
    /// Write errors are logged at `warn` level since recording failures should be visible
    /// but shouldn't interrupt call processing.
    pub fn push(
        &mut self,
        call_id: &CallId,
        side: RecordingSide,
        samples: &[i16],
        recorder: &forge_recorder::AudioRecorder,
    ) {
        if samples.is_empty() {
            return;
        }

        let now = Instant::now();

        if let Some((pending_side, pending_samples, timestamp)) = self.pending.take() {
            // Check if buffered frame is stale and auto-flush
            let age = now.duration_since(timestamp);
            if age > STALE_FRAME_THRESHOLD {
                if let Err(e) = recorder.write_samples(&pending_samples) {
                    tracing::warn!(
                        call_id = %call_id.0,
                        age_ms = age.as_millis(),
                        "Failed to write stale frame to recorder: {}",
                        e
                    );
                }
                // Buffer the new frame since the old one was stale
                self.pending = Some((side, samples.to_vec(), now));
                return;
            }

            if pending_side != side {
                let mixed = Self::mix_frames(&pending_samples, samples);
                if let Err(e) = recorder.write_samples(&mixed) {
                    tracing::warn!(
                        call_id = %call_id.0,
                        "Failed to write mixed samples to recorder: {}",
                        e
                    );
                }
            } else {
                if let Err(e) = recorder.write_samples(&pending_samples) {
                    tracing::warn!(
                        call_id = %call_id.0,
                        "Failed to write samples to recorder: {}",
                        e
                    );
                }
                self.pending = Some((side, samples.to_vec(), now));
            }
        } else {
            self.pending = Some((side, samples.to_vec(), now));
        }
    }

    /// Mix two audio frames together using amplitude-aware averaging.
    ///
    /// # Algorithm
    ///
    /// For each sample position:
    /// - If both sides have active audio (amplitude > 10): average them
    /// - If only one side is active: pass through unchanged
    /// - If neither side is active: sum them (both near zero anyway)
    ///
    /// This prevents:
    /// - Clipping when both parties speak simultaneously
    /// - Attenuating valid silence or low-level audio
    /// - Double loudness when only one party is speaking
    ///
    /// # Parameters
    ///
    /// - `a`: Samples from one side
    /// - `b`: Samples from the other side
    ///
    /// # Returns
    ///
    /// Mixed samples with length equal to the longer input
    fn mix_frames(a: &[i16], b: &[i16]) -> Vec<i16> {
        let len = a.len().max(b.len());
        let mut output = Vec::with_capacity(len);

        for i in 0..len {
            let sa = *a.get(i).unwrap_or(&0);
            let sb = *b.get(i).unwrap_or(&0);
            let sum = sa as i32 + sb as i32;

            // Use amplitude threshold to distinguish active audio from silence
            // This prevents treating true silence as "not contributing"
            let a_active = sa.abs() > AMPLITUDE_THRESHOLD;
            let b_active = sb.abs() > AMPLITUDE_THRESHOLD;
            let contributors = a_active as i32 + b_active as i32;

            // Average only when both sides contribute active audio
            let mixed = if contributors > 1 { sum / 2 } else { sum };

            output.push(mixed.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        }

        output
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

/// Guard that returns allocated ports if construction fails before the session owns them
struct PortAllocationGuard {
    port_pool: Arc<PortPool>,
    ports: PortPair,
    active: bool,
}

impl PortAllocationGuard {
    fn new(port_pool: Arc<PortPool>, ports: PortPair) -> Self {
        Self {
            port_pool,
            ports,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for PortAllocationGuard {
    fn drop(&mut self) {
        if self.active {
            let pool = Arc::clone(&self.port_pool);
            let ports = self.ports;
            tokio::spawn(async move {
                pool.deallocate(ports).await;
                tracing::debug!(
                    "PortAllocationGuard cleaned up ports after construction failure: RTP={}, RTCP={}",
                    ports.rtp_port,
                    ports.rtcp_port
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_rtp::PortPoolConfig;
    use std::net::{SocketAddr, UdpSocket};

    #[test]
    fn opus_bridge_rate_is_16k_with_48k_rtp_clock() {
        // Opus negotiates a 48 kHz RTP clock but runs on the bridge at
        // 16 kHz (libopus does the 48<->16 conversion + stereo->mono
        // internally), exactly the G.722 wire-clock-vs-PCM-rate split.
        assert_eq!(
            MediaSession::codec_audio_sample_rate(forge_core::AudioCodec::Opus, 48000),
            16000,
            "Opus bridge PCM rate must be 16 kHz, not the 48 kHz RTP clock"
        );
        // 16 kHz / 50 = 320 samples per 20 ms bridge frame.
        assert_eq!(
            MediaSession::frame_samples_for_codec(forge_core::AudioCodec::Opus, 16000),
            Some(320)
        );
        // RTP timestamps still step at the 48 kHz clock (960 per 20 ms).
        assert_eq!(48000u32 / 50, 960);
    }

    #[cfg(feature = "opus")]
    #[test]
    fn opus_codec_is_built_at_16k_mono() {
        // The engine builds the Opus codec at 16 kHz mono (so libopus does
        // the 48<->16 conversion + stereo->mono); 20 ms => 320 samples.
        let StatefulCodec::Opus(codec) = StatefulCodec::new(forge_core::AudioCodec::Opus)
            .expect("opus codec builds")
            .expect("opus is Some")
        else {
            panic!("expected an Opus codec");
        };
        assert_eq!(codec.config().sample_rate, 16000);
        assert_eq!(codec.config().channels, 1);
        assert_eq!(codec.config().frame_size(), 320);
    }

    #[tokio::test]
    async fn test_session_creation() {
        let config = PortPoolConfig::new(30000, 31000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session = MediaSession::new(
            call_id.clone(),
            participant_a,
            participant_b,
            &port_pool,
            MediaSessionConfig::default(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(session.call_id(), &call_id);
        assert_eq!(session.state().await, SessionState::Initializing);
        assert!(session.uptime() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_session_lifecycle() {
        let config = PortPoolConfig::new(31000, 32000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session = Arc::new(
            MediaSession::new(
                call_id,
                participant_a,
                participant_b,
                &port_pool,
                MediaSessionConfig::default(),
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
        assert_eq!(session.state().await, SessionState::Active);

        // Stop forwarding
        session.stop_forwarding().await.unwrap();
        assert_eq!(session.state().await, SessionState::Terminated);
    }

    #[tokio::test]
    async fn test_session_timeout() {
        let config = PortPoolConfig::new(32000, 33000).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let call_id = CallId::generate();
        let participant_a = ParticipantId::generate();
        let participant_b = ParticipantId::generate();

        let session_config = MediaSessionConfig {
            session_timeout: Duration::from_millis(50),
            ..Default::default()
        };

        let session = MediaSession::new(
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
        .unwrap();

        // Should not be timed out initially
        assert!(!session.is_timed_out().await);

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should be timed out now
        assert!(session.is_timed_out().await);
    }

    #[tokio::test]
    // The PortPool shuffles its 100-port range and allocates randomly, so
    // pre-binding a single port only collides with the session's socket by
    // chance. This test exercises a real error path (port release on socket
    // failure), but reproducing it reliably would require either shrinking
    // the pool below its 100-port minimum or prebinding every port in the
    // range, both of which fight the production invariants. Marked ignored
    // until the pool exposes a deterministic allocation hook for tests.
    #[ignore = "flaky: relies on random pool allocation hitting a specific port"]
    async fn test_ports_released_on_socket_failure() {
        let config = PortPoolConfig::new(20000, 20200).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let prebind_addr: SocketAddr = "127.0.0.1:20000".parse().unwrap();
        let _sock = UdpSocket::bind(prebind_addr).expect("failed to prebind test socket");

        let mut session_config = MediaSessionConfig::default();
        session_config.socket_config.bind_addr =
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        session_config.socket_config.reuse_address = false;

        let result = MediaSession::new(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            &port_pool,
            session_config,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(
            result.is_err(),
            "socket creation should fail when port is already bound"
        );

        // Give the guard's spawned deallocation task time to complete
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(
            port_pool.allocated_count().await,
            0,
            "ports should be returned to pool"
        );
    }

    #[tokio::test]
    async fn test_opus_dtmf_configuration() {
        let config = PortPoolConfig::new(20000, 20200).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        // Test 1: Default config has Opus DTMF enabled (Some(111))
        let session_default = MediaSession::new(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            &port_pool,
            MediaSessionConfig::default(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            session_default.dtmf_config().opus_payload_type,
            Some(111),
            "Default config should enable Opus DTMF with PT 111"
        );

        // Test 2: Can disable Opus DTMF by setting to None
        let mut config_disabled = MediaSessionConfig::default();
        config_disabled.dtmf_config.opus_payload_type = None;

        let session_disabled = MediaSession::new(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            &port_pool,
            config_disabled,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            session_disabled.dtmf_config().opus_payload_type,
            None,
            "Opus DTMF should be disabled when set to None"
        );

        // Test 3: Can use custom Opus payload type
        let mut config_custom = MediaSessionConfig::default();
        config_custom.dtmf_config.opus_payload_type = Some(96);

        let session_custom = MediaSession::new(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            &port_pool,
            config_custom,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            session_custom.dtmf_config().opus_payload_type,
            Some(96),
            "Should support custom Opus payload types"
        );
    }

    #[tokio::test]
    async fn test_update_participant_media() {
        let config = PortPoolConfig::new(20200, 20400).unwrap();
        let port_pool = Arc::new(PortPool::new(config));

        let session = MediaSession::new(
            CallId::generate(),
            ParticipantId::new("alice"),
            ParticipantId::new("bob"),
            &port_pool,
            MediaSessionConfig::default(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let mut latch_allowed_ips = HashSet::new();
        latch_allowed_ips.insert("203.0.113.10".parse::<IpAddr>().unwrap());

        let updated = session
            .update_participant_media(
                ParticipantLabel::A,
                ParticipantMediaUpdate {
                    remote_addr: Some(Some("203.0.113.10:4000".parse().unwrap())),
                    codec_config: Some(ParticipantCodecConfig {
                        payload_type: 9,
                        codec: forge_core::AudioCodec::G722,
                        clock_rate: 8000,
                    }),
                    telephone_event_payload_type: Some(110),
                    latch_allowed_ips: Some(Some(latch_allowed_ips)),
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.leg, ParticipantLabel::A);
        assert_eq!(updated.participant_id, "alice");
        assert_eq!(
            updated.remote_rtp_addr,
            Some("203.0.113.10:4000".parse().unwrap())
        );
        assert_eq!(updated.payload_type, 9);
        assert_eq!(updated.codec, forge_core::AudioCodec::G722);
        assert_eq!(updated.clock_rate, 8000);
        assert_eq!(updated.telephone_event_payload_type, 110);
        assert_eq!(
            updated.latch_allowed_ips,
            Some(vec!["203.0.113.10".parse::<IpAddr>().unwrap()])
        );

        let cleared = session
            .update_participant_media(
                ParticipantLabel::A,
                ParticipantMediaUpdate {
                    remote_addr: Some(None),
                    codec_config: None,
                    telephone_event_payload_type: None,
                    latch_allowed_ips: Some(None),
                },
            )
            .await
            .unwrap();

        assert_eq!(cleared.remote_rtp_addr, None);
        assert_eq!(cleared.latch_allowed_ips, None);
    }

    async fn make_test_session(start_port: u16) -> Arc<MediaSession> {
        let config = PortPoolConfig::new(start_port, start_port + 200).unwrap();
        let port_pool = Arc::new(PortPool::new(config));
        Arc::new(
            MediaSession::new(
                CallId::generate(),
                ParticipantId::new("alice"),
                ParticipantId::new("bob"),
                &port_pool,
                MediaSessionConfig::default(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap(),
        )
    }

    fn count_audio_frames(items: &[ScheduledPlayoutItem]) -> usize {
        items
            .iter()
            .filter(|i| matches!(i.kind, ScheduledPlayoutKind::Audio { .. }))
            .count()
    }

    #[tokio::test]
    async fn test_schedule_audio_playout_creates_paced_frames() {
        let session = make_test_session(40000).await;
        // 320 samples @ 8kHz PCMU → 2 frames (160 samples each), 20ms apart.
        let samples = vec![0i16; 320];
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &samples,
                Some("p1".to_string()),
                crate::media_bridge::PlayoutMode::Append,
                ScheduledPlayoutSource::AI,
            )
            .await
            .unwrap();

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        assert_eq!(queue.items.len(), 2);
        // Marker only on the first frame in the burst.
        assert!(queue.items[0].marker);
        assert!(!queue.items[1].marker);
        // 8kHz / 50fps = 160-tick increment per frame.
        let t0 = queue.items[0].timestamp;
        assert_eq!(queue.items[1].timestamp, t0.wrapping_add(160));
        // due_at separated by 20ms.
        assert_eq!(
            queue.items[1].due_at - queue.items[0].due_at,
            Duration::from_millis(20)
        );
        // Nothing went to leg B.
        let queue_b = session.playout_queue_lock(ParticipantLabel::B).lock().await;
        assert!(queue_b.items.is_empty());
    }

    /// Streaming callers hand us one 20 ms frame per call. Back-to-back
    /// appends with no intervening silence are a single talkspurt, so only
    /// the very first packet may carry the marker bit. Regression test for
    /// the marker-on-every-packet bug observed against Twilio (RFC 3551 §4.1).
    #[tokio::test]
    async fn test_streamed_frames_mark_only_first_packet() {
        let session = make_test_session(40500).await;
        let frame = vec![0i16; 160]; // exactly one 8kHz/20ms frame
        for _ in 0..4 {
            session
                .schedule_audio_playout(
                    crate::media_bridge::MediaTarget::A,
                    8000,
                    &frame,
                    None,
                    crate::media_bridge::PlayoutMode::Append,
                    ScheduledPlayoutSource::MediaBridgeAudio,
                )
                .await
                .unwrap();
        }

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        assert_eq!(queue.items.len(), 4);
        assert!(
            queue.items[0].marker,
            "first frame of a talkspurt is marked"
        );
        for (i, item) in queue.items.iter().enumerate().skip(1) {
            assert!(
                !item.marker,
                "frame {i} continues the talkspurt and must not be marked"
            );
        }
    }

    /// After a real silence gap (the producer stops feeding for longer than
    /// `TALKSPURT_SILENCE_GAP`), the resuming frame opens a new talkspurt and
    /// must carry the marker bit again — even though the pump has long since
    /// drained the queue.
    #[tokio::test]
    async fn test_talkspurt_resumes_after_silence_gap() {
        let session = make_test_session(40700).await;
        let frame = vec![0i16; 160];
        let sched = |s: &Arc<MediaSession>, f: &[i16]| {
            let s = Arc::clone(s);
            let f = f.to_vec();
            async move {
                s.schedule_audio_playout(
                    crate::media_bridge::MediaTarget::A,
                    8000,
                    &f,
                    None,
                    crate::media_bridge::PlayoutMode::Append,
                    ScheduledPlayoutSource::MediaBridgeAudio,
                )
                .await
                .unwrap();
            }
        };

        sched(&session, &frame).await;
        // Simulate the playout draining and a silence gap longer than the
        // talkspurt threshold before the next frame arrives.
        session
            .take_due_playout_items(ParticipantLabel::A, Instant::now() + Duration::from_secs(1))
            .await;
        tokio::time::sleep(TALKSPURT_SILENCE_GAP + Duration::from_millis(40)).await;
        sched(&session, &frame).await;

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        assert_eq!(queue.items.len(), 1, "first frame already drained");
        assert!(
            queue.items[0].marker,
            "frame resuming after a silence gap starts a new talkspurt"
        );
    }

    /// A `Replace` (barge-in) always opens a new talkspurt, even when audio
    /// is still queued and no wall-clock gap has elapsed.
    #[tokio::test]
    async fn test_replace_marks_new_talkspurt() {
        let session = make_test_session(40900).await;
        let frame = vec![0i16; 160];
        // Existing playback under a different id — stays queued so no drain
        // and no gap; `audio_stream_end` remains in the future.
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &frame,
                Some("greeting".to_string()),
                crate::media_bridge::PlayoutMode::Append,
                ScheduledPlayoutSource::AI,
            )
            .await
            .unwrap();
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &frame,
                Some("barge-in".to_string()),
                crate::media_bridge::PlayoutMode::Replace,
                ScheduledPlayoutSource::AI,
            )
            .await
            .unwrap();

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        let barge = queue
            .items
            .iter()
            .find(|i| i.playback_id.as_deref() == Some("barge-in"))
            .expect("barge-in frame queued");
        assert!(barge.marker, "Replace opens a new talkspurt → marker set");
    }

    #[tokio::test]
    async fn test_schedule_audio_playout_append_continues_rtp_cursor() {
        let session = make_test_session(40300).await;
        let samples = vec![0i16; 320]; // 2 frames
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &samples,
                None,
                crate::media_bridge::PlayoutMode::Append,
                ScheduledPlayoutSource::MediaBridgeAudio,
            )
            .await
            .unwrap();
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &samples,
                None,
                crate::media_bridge::PlayoutMode::Append,
                ScheduledPlayoutSource::MediaBridgeAudio,
            )
            .await
            .unwrap();

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        assert_eq!(queue.items.len(), 4);
        // Frame 3 must continue from frame 2's stream cursor, not reset.
        assert_eq!(queue.items[2].timestamp, queue.items[1].stream_cursor_after);
        assert_eq!(
            queue.items[3].timestamp,
            queue.items[2].timestamp.wrapping_add(160)
        );
    }

    #[tokio::test]
    async fn test_replace_mode_with_id_keeps_other_playbacks() {
        let session = make_test_session(40600).await;
        let samples = vec![0i16; 320];
        for id in ["a", "b"] {
            session
                .schedule_audio_playout(
                    crate::media_bridge::MediaTarget::A,
                    8000,
                    &samples,
                    Some(id.to_string()),
                    crate::media_bridge::PlayoutMode::Append,
                    ScheduledPlayoutSource::MediaBridgeAudio,
                )
                .await
                .unwrap();
        }
        // Replace "a" — should drop only items tagged "a".
        let one_frame = vec![0i16; 160];
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &one_frame,
                Some("a".to_string()),
                crate::media_bridge::PlayoutMode::Replace,
                ScheduledPlayoutSource::MediaBridgeAudio,
            )
            .await
            .unwrap();

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        // Two "b" frames remain plus one "a" replacement.
        assert_eq!(queue.items.len(), 3);
        let b_count = queue
            .items
            .iter()
            .filter(|i| i.playback_id.as_deref() == Some("b"))
            .count();
        let a_count = queue
            .items
            .iter()
            .filter(|i| i.playback_id.as_deref() == Some("a"))
            .count();
        assert_eq!(b_count, 2);
        assert_eq!(a_count, 1);
    }

    #[tokio::test]
    async fn test_replace_mode_without_id_clears_leg() {
        let session = make_test_session(40900).await;
        let samples = vec![0i16; 320];
        for id in ["a", "b"] {
            session
                .schedule_audio_playout(
                    crate::media_bridge::MediaTarget::A,
                    8000,
                    &samples,
                    Some(id.to_string()),
                    crate::media_bridge::PlayoutMode::Append,
                    ScheduledPlayoutSource::MediaBridgeAudio,
                )
                .await
                .unwrap();
        }
        let one_frame = vec![0i16; 160];
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &one_frame,
                None,
                crate::media_bridge::PlayoutMode::Replace,
                ScheduledPlayoutSource::MediaBridgeAudio,
            )
            .await
            .unwrap();

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        // Replace with id=None drops everything before scheduling, leaving only the new item.
        assert_eq!(queue.items.len(), 1);
        assert_eq!(queue.items[0].playback_id, None);
    }

    #[tokio::test]
    async fn test_take_due_playout_items_returns_in_order_and_updates_cursor() {
        let session = make_test_session(41200).await;
        let samples = vec![0i16; 480]; // 3 frames
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &samples,
                None,
                crate::media_bridge::PlayoutMode::Append,
                ScheduledPlayoutSource::AI,
            )
            .await
            .unwrap();

        // Wait past the second frame's due_at to claim 2 items.
        tokio::time::sleep(Duration::from_millis(25)).await;
        let now = Instant::now();
        let due = session
            .take_due_playout_items(ParticipantLabel::A, now)
            .await;
        assert_eq!(due.len(), 2);
        assert_eq!(count_audio_frames(&due), 2);
        // One item left and queue cursor still points at the trailing item.
        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        assert_eq!(queue.items.len(), 1);
        assert!(queue.next_rtp_timestamp.is_some());
    }

    #[tokio::test]
    async fn test_codec_change_clears_scheduled_playout() {
        let session = make_test_session(41500).await;
        let samples = vec![0i16; 320];
        session
            .schedule_audio_playout(
                crate::media_bridge::MediaTarget::A,
                8000,
                &samples,
                Some("p".to_string()),
                crate::media_bridge::PlayoutMode::Append,
                ScheduledPlayoutSource::AI,
            )
            .await
            .unwrap();
        assert_eq!(
            session
                .playout_queue_lock(ParticipantLabel::A)
                .lock()
                .await
                .items
                .len(),
            2
        );

        // Switching codec must drop any frames already encoded under the old codec/PT.
        session
            .update_participant_media(
                ParticipantLabel::A,
                ParticipantMediaUpdate {
                    remote_addr: None,
                    codec_config: Some(ParticipantCodecConfig {
                        payload_type: 8,
                        codec: forge_core::AudioCodec::PCMA,
                        clock_rate: 8000,
                    }),
                    telephone_event_payload_type: None,
                    latch_allowed_ips: None,
                },
            )
            .await
            .unwrap();

        let queue = session.playout_queue_lock(ParticipantLabel::A).lock().await;
        assert!(queue.items.is_empty());
        assert!(queue.next_due_at.is_none());
        assert!(queue.next_rtp_timestamp.is_none());
    }

    /// Feed `stats` a run of packets described as `(seq, rtp_ts, arrival_ms)`
    /// at an 8 kHz clock with jitter counting on.
    fn feed(stats: &mut RxStreamStats, packets: &[(u16, u32, u64)]) {
        let t0 = Instant::now();
        for &(seq, ts, at_ms) in packets {
            stats.record(seq, ts, t0 + Duration::from_millis(at_ms), 8000, true);
        }
    }

    #[test]
    fn rx_stream_in_order_no_loss() {
        let mut s = RxStreamStats::default();
        feed(
            &mut s,
            &[(1, 160, 0), (2, 320, 20), (3, 480, 40), (4, 640, 60)],
        );
        assert_eq!(s.packets_received, 4);
        assert_eq!(s.packets_lost(), 0);
        assert_eq!(s.packets_out_of_order, 0);
        assert_eq!(s.packets_duplicate, 0);
        // Perfect 20 ms pacing at matching timestamps → zero transit
        // variation → zero jitter.
        assert!(s.jitter_ms().abs() < 1e-6, "jitter_ms = {}", s.jitter_ms());
    }

    #[test]
    fn rx_stream_gap_counts_lost_and_late_arrival_repairs() {
        let mut s = RxStreamStats::default();
        feed(&mut s, &[(1, 160, 0), (2, 320, 20), (5, 800, 80)]);
        assert_eq!(s.packets_lost(), 2); // 3 and 4 missing

        // Packet 3 arrives late: repairs one loss, counts out-of-order.
        feed(&mut s, &[(3, 480, 100)]);
        assert_eq!(s.packets_received, 4);
        assert_eq!(s.packets_lost(), 1);
        assert_eq!(s.packets_out_of_order, 1);
    }

    #[test]
    fn rx_stream_duplicates_detected_and_not_double_counted() {
        let mut s = RxStreamStats::default();
        feed(
            &mut s,
            &[(1, 160, 0), (2, 320, 20), (2, 320, 21), (1, 160, 25)],
        );
        assert_eq!(s.packets_received, 2);
        assert_eq!(s.packets_duplicate, 2);
        assert_eq!(s.packets_lost(), 0);
        assert_eq!(s.packets_out_of_order, 0);
    }

    #[test]
    fn rx_stream_sequence_wrap_extends() {
        let mut s = RxStreamStats::default();
        feed(
            &mut s,
            &[
                (65534, 160, 0),
                (65535, 320, 20),
                (0, 480, 40),
                (1, 640, 60),
            ],
        );
        assert_eq!(s.packets_received, 4);
        assert_eq!(s.packets_lost(), 0);
        assert_eq!(s.packets_out_of_order, 0);
    }

    #[test]
    fn rx_stream_pre_base_packet_ignored() {
        let mut s = RxStreamStats::default();
        feed(&mut s, &[(5, 800, 0), (3, 480, 5)]);
        assert_eq!(s.packets_received, 1);
        assert_eq!(s.packets_out_of_order, 0);
        assert_eq!(s.packets_lost(), 0);
    }

    #[test]
    fn rx_stream_jitter_tracks_arrival_variation() {
        let mut s = RxStreamStats::default();
        // Timestamps step a clean 20 ms but arrivals alternate ±5 ms —
        // classic network jitter. RFC 3550 filter must land above zero
        // and below the raw 5 ms swing.
        feed(
            &mut s,
            &[
                (1, 160, 0),
                (2, 320, 25),
                (3, 480, 40),
                (4, 640, 65),
                (5, 800, 80),
                (6, 960, 105),
            ],
        );
        let j = s.jitter_ms();
        assert!(j > 0.0 && j < 5.0, "jitter_ms = {j}");
    }

    #[test]
    fn rx_stream_telephone_event_counts_sequence_not_jitter() {
        let mut s = RxStreamStats::default();
        let t0 = Instant::now();
        s.record(1, 160, t0, 8000, true);
        s.record(2, 320, t0 + Duration::from_millis(20), 8000, true);
        // RFC 2833 burst: timestamp frozen at the digit start — would fake
        // a huge transit swing if it entered the jitter filter.
        s.record(3, 320, t0 + Duration::from_millis(40), 8000, false);
        s.record(4, 320, t0 + Duration::from_millis(60), 8000, false);
        // Audio resumes on the original cadence.
        s.record(5, 800, t0 + Duration::from_millis(80), 8000, true);

        assert_eq!(s.packets_received, 5);
        assert_eq!(s.packets_lost(), 0);
        assert!(s.jitter_ms().abs() < 1e-6, "jitter_ms = {}", s.jitter_ms());
    }
}
