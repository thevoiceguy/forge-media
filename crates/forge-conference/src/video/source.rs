//! A participant's video ingress (design §5.1, §13).
//!
//! RTP → frame assembler → decoder (on the codec pool) → frame slot. The
//! slot is a single-frame mailbox: the compositor reads whatever is
//! newest at its own clock, so a 30 fps source in a 15 fps room costs
//! decode work and nothing else.
//!
//! Loss handling: a gap the assembler notices is NACKed at once (each
//! sequence number at most once per `nack_retry`); a frame given up on
//! marks the picture invalid until a keyframe, and a PLI goes upstream
//! through a per-source gate. Frames that reach us while the picture is
//! invalid are dropped rather than decoded against missing references.
//!
//! Limits (§13): coded frame size, decoded resolution, source frame rate,
//! decode queue depth, and consecutive decoder errors. Beyond the last
//! the source is failed and its tile becomes an avatar; the room and the
//! other participants are untouched.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::VideoCodec;
use forge_rtp::rtcp::{PsFeedback, RtcpPacket, RtpFeedback};
use forge_rtp::{AssemblerEvent, CodedFrame, FrameAssembler, KeyframeRequestGate, RtpPacket};
use forge_video::codec::VideoDecoder;
use forge_video::frame::{Resolution, VideoFrame};
use metrics::counter;
use parking_lot::Mutex;
use tracing::{debug, warn};

use super::pool::CodecPool;

/// What a source may cost us before we stop believing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLimits {
    /// Largest coded frame the assembler will hold (bytes).
    pub max_coded_frame_bytes: usize,
    /// Decoded frames larger than this are dropped.
    pub max_resolution: Resolution,
    /// Frames per second beyond which frames are dropped.
    pub max_fps: u32,
    /// Coded frames waiting for a pool thread before we drop and re-key.
    pub max_decode_queue: usize,
    /// Consecutive decoder errors that fail the source.
    pub max_decoder_errors: u32,
    /// Minimum spacing between PLIs to this source.
    pub pli_min_interval: Duration,
    /// Minimum spacing between NACKs for the same sequence number.
    pub nack_retry: Duration,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_coded_frame_bytes: 4 * 1024 * 1024,
            max_resolution: Resolution::new(1920, 1080),
            max_fps: 60,
            max_decode_queue: 3,
            max_decoder_errors: 10,
            pli_min_interval: Duration::from_millis(500),
            nack_retry: Duration::from_millis(100),
        }
    }
}

/// Counters the room reports per participant.
#[derive(Debug, Default)]
pub struct SourceStats {
    pub packets_received: AtomicU64,
    pub bytes_received: AtomicU64,
    pub frames_received: AtomicU64,
    pub frames_decoded: AtomicU64,
    /// Frames the assembler gave up on.
    pub frames_lost: AtomicU64,
    /// Frames we chose not to decode (invalid picture, queue, rate, size).
    pub frames_dropped: AtomicU64,
    pub decode_errors: AtomicU64,
    pub nacks_sent: AtomicU64,
    pub plis_sent: AtomicU64,
    /// Decoded width and height of the last frame.
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Decoded frames per second over the last full second.
    pub fps: AtomicU32,
    /// Ingress bit rate over the last full second (kb/s).
    pub bitrate_kbps: AtomicU32,
}

/// The newest decoded frame and when it landed.
#[derive(Default)]
pub struct Slot {
    frame: Option<Arc<VideoFrame>>,
    at: Option<Instant>,
}

impl Slot {
    /// The frame if it is younger than `max_age`.
    pub fn fresh(&self, now: Instant, max_age: Duration) -> Option<Arc<VideoFrame>> {
        let at = self.at?;
        if now.saturating_duration_since(at) <= max_age {
            self.frame.clone()
        } else {
            None
        }
    }

    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    pub fn age(&self, now: Instant) -> Option<Duration> {
        self.at.map(|a| now.saturating_duration_since(a))
    }
}

/// Per-second windows for the measured rates.
#[derive(Debug)]
struct RateWindow {
    since: Instant,
    bytes: u64,
}

struct DecoderState {
    decoder: Option<Box<dyn VideoDecoder>>,
    consecutive_errors: u32,
}

pub struct VideoSource {
    id: String,
    room_id: String,
    codec: VideoCodec,
    limits: SourceLimits,
    assembler: Mutex<FrameAssembler>,
    /// `true` while the assembler's picture is decodable (a keyframe has
    /// arrived since the last loss).
    valid: AtomicBool,
    decoder: Arc<Mutex<DecoderState>>,
    slot: Arc<Mutex<Slot>>,
    queued: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    /// A decoder error on the pool asks for a keyframe on the next packet.
    pli_pending: Arc<AtomicBool>,
    pli_gate: Mutex<KeyframeRequestGate>,
    nacked: Mutex<HashMap<u16, Instant>>,
    rate: Mutex<RateWindow>,
    /// Frames accepted in the current second, for the fps limit.
    accepted_this_second: Mutex<(Instant, u32)>,
    remote_ssrc: AtomicU32,
    local_ssrc: u32,
    last_packet: Mutex<Option<Instant>>,
    pub stats: Arc<SourceStats>,
}

impl VideoSource {
    pub fn new(
        id: &str,
        room_id: &str,
        codec: VideoCodec,
        decoder: Box<dyn VideoDecoder>,
        limits: SourceLimits,
        local_ssrc: u32,
    ) -> Self {
        let now = Instant::now();
        Self {
            id: id.to_string(),
            room_id: room_id.to_string(),
            codec,
            assembler: Mutex::new(FrameAssembler::with_limits(
                codec,
                16,
                limits.max_coded_frame_bytes,
            )),
            valid: AtomicBool::new(false),
            decoder: Arc::new(Mutex::new(DecoderState {
                decoder: Some(decoder),
                consecutive_errors: 0,
            })),
            slot: Arc::new(Mutex::new(Slot::default())),
            queued: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            pli_pending: Arc::new(AtomicBool::new(true)),
            pli_gate: Mutex::new(KeyframeRequestGate::new(limits.pli_min_interval)),
            nacked: Mutex::new(HashMap::new()),
            rate: Mutex::new(RateWindow {
                since: now,
                bytes: 0,
            }),
            accepted_this_second: Mutex::new((now, 0)),
            remote_ssrc: AtomicU32::new(0),
            local_ssrc,
            last_packet: Mutex::new(None),
            limits,
            stats: Arc::new(SourceStats::default()),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// The decoder gave up (§13): the tile shows an avatar from now on.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn remote_ssrc(&self) -> u32 {
        self.remote_ssrc.load(Ordering::Relaxed)
    }

    pub fn last_packet_age(&self, now: Instant) -> Option<Duration> {
        self.last_packet
            .lock()
            .map(|t| now.saturating_duration_since(t))
    }

    /// The newest decoded frame if it is younger than `max_age`.
    pub fn frame(&self, now: Instant, max_age: Duration) -> Option<Arc<VideoFrame>> {
        self.slot.lock().fresh(now, max_age)
    }

    pub fn has_frame(&self) -> bool {
        self.slot.lock().has_frame()
    }

    pub fn frame_age(&self, now: Instant) -> Option<Duration> {
        self.slot.lock().age(now)
    }

    /// Feed one RTP packet (already SRTP-unprotected and parsed). Returns
    /// the RTCP feedback to send back to the sender, if any.
    pub fn push(&self, packet: RtpPacket, pool: &CodecPool, now: Instant) -> Vec<RtcpPacket> {
        self.stats.packets_received.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_received
            .fetch_add(packet.payload.len() as u64, Ordering::Relaxed);
        *self.last_packet.lock() = Some(now);
        self.remote_ssrc
            .store(packet.header.ssrc, Ordering::Relaxed);
        self.roll_rate(now, packet.payload.len() as u64);

        if self.failed() {
            return Vec::new();
        }

        let mut want_keyframe = false;
        let mut to_decode: Vec<CodedFrame> = Vec::new();
        let missing;
        {
            let mut asm = self.assembler.lock();
            let mut valid = self.valid.load(Ordering::Relaxed);
            for ev in asm.push(packet) {
                match ev {
                    AssemblerEvent::Frame(f) => {
                        self.stats.frames_received.fetch_add(1, Ordering::Relaxed);
                        if f.keyframe {
                            valid = true;
                        }
                        if valid {
                            to_decode.push(f);
                        } else {
                            self.drop_frame("waiting for a keyframe");
                            want_keyframe = true;
                        }
                    }
                    AssemblerEvent::Lost { from_seq, to_seq } => {
                        debug!(participant = %self.id, from_seq, to_seq, "video frame lost");
                        self.stats.frames_lost.fetch_add(1, Ordering::Relaxed);
                        counter!("forge_conference_video_frames_lost_total", "room_id" => self.room_id.clone())
                            .increment(1);
                        valid = false;
                        want_keyframe = true;
                    }
                    AssemblerEvent::Invalid { timestamp, error } => {
                        debug!(participant = %self.id, timestamp, %error, "invalid video frame");
                        self.stats.frames_lost.fetch_add(1, Ordering::Relaxed);
                        counter!("forge_conference_video_frames_lost_total", "room_id" => self.room_id.clone())
                            .increment(1);
                        valid = false;
                        want_keyframe = true;
                    }
                }
            }
            self.valid.store(valid, Ordering::Relaxed);
            missing = asm.missing();
        }

        for f in to_decode {
            if !self.admit(now) {
                self.drop_frame("over the frame-rate limit");
                continue;
            }
            if self.queued.load(Ordering::Relaxed) >= self.limits.max_decode_queue {
                // The pool is behind: a stale frame is worthless, and the
                // frames after it need this one, so re-key instead.
                self.drop_frame("decode queue full");
                self.valid.store(false, Ordering::Relaxed);
                want_keyframe = true;
                continue;
            }
            self.spawn_decode(f, pool);
        }

        let mut out = Vec::new();
        let lost = self.nack_list(&missing, now);
        if !lost.is_empty() {
            self.stats.nacks_sent.fetch_add(1, Ordering::Relaxed);
            counter!("forge_conference_video_nacks_sent_total", "room_id" => self.room_id.clone())
                .increment(1);
            out.push(RtcpPacket::TransportFeedback(RtpFeedback::nack(
                self.local_ssrc,
                self.remote_ssrc(),
                &lost,
            )));
        }
        if want_keyframe || self.pli_pending.swap(false, Ordering::AcqRel) {
            if self.pli_gate.lock().allow_at(now) {
                self.stats.plis_sent.fetch_add(1, Ordering::Relaxed);
                counter!("forge_conference_video_plis_sent_total", "room_id" => self.room_id.clone())
                    .increment(1);
                out.push(RtcpPacket::PayloadFeedback(PsFeedback::pli(
                    self.local_ssrc,
                    self.remote_ssrc(),
                )));
            } else {
                // Gated: keep the request so a later packet can carry it.
                self.pli_pending.store(true, Ordering::Release);
            }
        }
        out
    }

    /// Ask the sender for a keyframe at the next opportunity (a new
    /// subscriber cannot use P-frames; a host re-enabled the video).
    pub fn request_keyframe(&self) {
        self.pli_pending.store(true, Ordering::Release);
    }

    /// The frame-rate limit: at most `max_fps` accepted frames per second.
    fn admit(&self, now: Instant) -> bool {
        let mut w = self.accepted_this_second.lock();
        if now.saturating_duration_since(w.0) >= Duration::from_secs(1) {
            *w = (now, 0);
        }
        if w.1 >= self.limits.max_fps {
            return false;
        }
        w.1 += 1;
        true
    }

    fn drop_frame(&self, why: &str) {
        debug!(participant = %self.id, "video frame dropped: {why}");
        self.stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
        counter!("forge_conference_video_frames_dropped_total", "room_id" => self.room_id.clone())
            .increment(1);
    }

    /// Sequence numbers to NACK now: the assembler's gaps, minus those
    /// asked for within `nack_retry`. Forgets numbers that have been
    /// filled or given up on.
    fn nack_list(&self, missing: &[u16], now: Instant) -> Vec<u16> {
        let mut nacked = self.nacked.lock();
        nacked.retain(|seq, _| missing.contains(seq));
        let mut out = Vec::new();
        for &seq in missing {
            let due = nacked
                .get(&seq)
                .map(|t| now.saturating_duration_since(*t) >= self.limits.nack_retry)
                .unwrap_or(true);
            if due {
                nacked.insert(seq, now);
                out.push(seq);
            }
        }
        out.sort_unstable();
        out
    }

    fn spawn_decode(&self, frame: CodedFrame, pool: &CodecPool) {
        self.queued.fetch_add(1, Ordering::AcqRel);
        let queued = Arc::clone(&self.queued);
        let decoder = Arc::clone(&self.decoder);
        let slot = Arc::clone(&self.slot);
        let failed = Arc::clone(&self.failed);
        let pli_pending = Arc::clone(&self.pli_pending);
        let stats = Arc::clone(&self.stats);
        let limits = self.limits.clone();
        let id = self.id.clone();
        let room_id = self.room_id.clone();
        let submitted = pool.submit(move || {
            // Decrement whatever happens, including a panic unwinding
            // through the codec: `catch_unwind` in the pool ends the job,
            // and the guard runs on the way out.
            struct Guard(Arc<AtomicUsize>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _guard = Guard(queued);

            let mut state = decoder.lock();
            let state = &mut *state;
            let Some(dec) = state.decoder.as_mut() else {
                return;
            };
            let outcome = dec.decode(&frame);
            match outcome {
                Ok(Some(decoded)) => {
                    state.consecutive_errors = 0;
                    let res = decoded.resolution();
                    if res.width > limits.max_resolution.width
                        || res.height > limits.max_resolution.height
                    {
                        warn!(participant = %id, %res, cap = %limits.max_resolution,
                              "decoded video frame over the resolution cap; dropped");
                        stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
                        counter!("forge_conference_video_frames_dropped_total", "room_id" => room_id.clone())
                            .increment(1);
                        return;
                    }
                    stats.width.store(res.width, Ordering::Relaxed);
                    stats.height.store(res.height, Ordering::Relaxed);
                    stats.frames_decoded.fetch_add(1, Ordering::Relaxed);
                    counter!("forge_conference_video_frames_decoded_total", "room_id" => room_id.clone())
                        .increment(1);
                    let mut s = slot.lock();
                    s.frame = Some(Arc::new(decoded));
                    s.at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(e) => {
                    state.consecutive_errors += 1;
                    stats.decode_errors.fetch_add(1, Ordering::Relaxed);
                    counter!("forge_conference_video_decode_errors_total", "room_id" => room_id.clone())
                        .increment(1);
                    pli_pending.store(true, Ordering::Release);
                    dec.reset();
                    let errors = state.consecutive_errors;
                    if errors >= limits.max_decoder_errors {
                        warn!(participant = %id, error = %e, errors,
                              "video decoder failed repeatedly; disabling this participant's video");
                        state.decoder = None;
                        failed.store(true, Ordering::Release);
                    } else {
                        debug!(participant = %id, error = %e, "video decode error");
                    }
                }
            }
        });
        if !submitted {
            self.queued.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Roll the per-second ingress window.
    fn roll_rate(&self, now: Instant, bytes: u64) {
        let mut r = self.rate.lock();
        r.bytes += bytes;
        let elapsed = now.saturating_duration_since(r.since);
        if elapsed >= Duration::from_secs(1) {
            let secs = elapsed.as_secs_f64();
            self.stats.bitrate_kbps.store(
                (r.bytes as f64 * 8.0 / 1000.0 / secs) as u32,
                Ordering::Relaxed,
            );
            r.since = now;
            r.bytes = 0;
        }
    }

    /// Called by the room once a second with the decoded-frame count it
    /// observed, so `stats.fps` reflects what actually reached the slot.
    pub fn set_measured_fps(&self, fps: u32) {
        self.stats.fps.store(fps, Ordering::Relaxed);
    }

    /// Frames decoded so far (the room differences this once a second).
    pub fn frames_decoded(&self) -> u64 {
        self.stats.frames_decoded.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for VideoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoSource")
            .field("id", &self.id)
            .field("codec", &self.codec)
            .field("failed", &self.failed())
            .finish()
    }
}
