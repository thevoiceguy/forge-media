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
//! Simulcast: a sender may send the same camera at up to three sizes
//! (RFC 8853), each its own SSRC named by its rid. Each layer is
//! assembled, NACKed and re-keyed on its own, and only one is decoded:
//! the smallest that fills the largest tile the room draws the source at
//! ([`VideoSource::set_wanted_height`]). A switch asks the new layer for a
//! keyframe and goes on decoding the old one until it arrives, so the
//! tile never goes blank.
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
use forge_rtp::video::dimensions::keyframe_dimensions;
use forge_rtp::{AssemblerEvent, CodedFrame, FrameAssembler, KeyframeRequestGate, RtpPacket};
use forge_video::codec::VideoDecoder;
use forge_video::device::DeviceBackend;
use forge_video::frame::{MediaDevice, Resolution, VideoFrame};
use forge_video::scale::{fit_within, resize_with, ScaleMode, Scaler};
use metrics::counter;
use parking_lot::Mutex;
use tracing::{debug, warn};

use super::content::StreamKind;
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

/// A simulcast sender's layers, as its SDP negotiated them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Simulcast {
    /// The rids, in the order of the sender's `a=simulcast` list.
    pub rids: Vec<String>,
    /// The id of the RFC 8852 rid header extension, which names the
    /// layer of a packet whose SSRC is not yet known.
    pub rid_extension: Option<u8>,
}

/// One layer as the room reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    /// `None` for a sender without simulcast.
    pub rid: Option<String>,
    /// 0 until a packet has named it.
    pub ssrc: u32,
    /// From its last keyframe, 0×0 until one is read.
    pub resolution: Resolution,
    pub bitrate_kbps: u32,
    /// Packets arrived within the last second.
    pub live: bool,
    /// The layer being decoded.
    pub decoding: bool,
}

/// A layer is live while its packets keep coming: a simulcast sender
/// stops its largest layer when its uplink cannot carry it.
const LAYER_LIVE: Duration = Duration::from_secs(1);

/// How long a smaller layer must be enough before the room moves down to
/// it; a move up is at once.
const LAYER_DOWN_AFTER: Duration = Duration::from_secs(2);

/// How long a switch waits for the new layer's keyframe before asking
/// again.
const LAYER_REKEY_AFTER: Duration = Duration::from_secs(1);

/// One of a source's RTP streams: its own SSRC, assembler, loss and
/// keyframe state. A sender without simulcast has one.
struct Layer {
    rid: Option<String>,
    ssrc: AtomicU32,
    assembler: Mutex<FrameAssembler>,
    /// `true` while this layer's picture is decodable (a keyframe has
    /// arrived since the last loss).
    valid: AtomicBool,
    pli_pending: AtomicBool,
    pli_gate: Mutex<KeyframeRequestGate>,
    nacked: Mutex<HashMap<u16, Instant>>,
    last_packet: Mutex<Option<Instant>>,
    rate: Mutex<RateWindow>,
    bitrate_kbps: AtomicU32,
    /// From the last keyframe whose header could be read.
    width: AtomicU32,
    height: AtomicU32,
}

impl Layer {
    fn new(rid: Option<String>, codec: VideoCodec, limits: &SourceLimits, now: Instant) -> Self {
        Self {
            rid,
            ssrc: AtomicU32::new(0),
            assembler: Mutex::new(FrameAssembler::with_limits(
                codec,
                16,
                limits.max_coded_frame_bytes,
            )),
            valid: AtomicBool::new(false),
            pli_pending: AtomicBool::new(false),
            pli_gate: Mutex::new(KeyframeRequestGate::new(limits.pli_min_interval)),
            nacked: Mutex::new(HashMap::new()),
            last_packet: Mutex::new(None),
            rate: Mutex::new(RateWindow {
                since: now,
                bytes: 0,
            }),
            bitrate_kbps: AtomicU32::new(0),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
        }
    }

    fn live(&self, now: Instant) -> bool {
        self.last_packet
            .lock()
            .is_some_and(|t| now.saturating_duration_since(t) <= LAYER_LIVE)
    }

    fn height(&self) -> Option<u32> {
        Some(self.height.load(Ordering::Relaxed)).filter(|h| *h > 0)
    }

    fn roll_rate(&self, now: Instant, bytes: u64) {
        let mut r = self.rate.lock();
        r.bytes += bytes;
        let elapsed = now.saturating_duration_since(r.since);
        if elapsed >= Duration::from_secs(1) {
            self.bitrate_kbps.store(
                (r.bytes as f64 * 8.0 / 1000.0 / elapsed.as_secs_f64()) as u32,
                Ordering::Relaxed,
            );
            r.since = now;
            r.bytes = 0;
        }
    }
}

/// Which layer is decoded, and the switch in progress.
#[derive(Debug, Default)]
struct Choice {
    /// The layer whose frames are decoded.
    decoding: usize,
    /// The layer being switched to, waiting for its keyframe, and since
    /// when it was asked for one.
    target: Option<(usize, Instant)>,
    /// Since when a smaller layer has been enough.
    down_since: Option<Instant>,
    /// Whether a layer has been chosen yet: the first choice is made at
    /// once, since nothing has been decoded to keep showing.
    chosen: bool,
}

struct DecoderState {
    decoder: Option<Box<dyn VideoDecoder>>,
    consecutive_errors: u32,
}

pub struct VideoSource {
    id: String,
    room_id: String,
    /// A camera, or a shared screen. A screen larger than the cap is
    /// shrunk to it rather than dropped, since text is what it carries.
    kind: StreamKind,
    codec: VideoCodec,
    /// The sender's negotiated `a=fmtp`, for forwarding decisions.
    profile: String,
    limits: SourceLimits,
    /// One per RTP stream: one, or a simulcast sender's two or three.
    layers: Vec<Layer>,
    rid_extension: Option<u8>,
    choice: Mutex<Choice>,
    /// The tallest the room draws this source this tick; 0 when it is in
    /// no composite.
    wanted_height: AtomicU32,
    decoder: Arc<Mutex<DecoderState>>,
    /// Where the decoder runs; the room's device when it has this
    /// codec, the host otherwise (design §15.7, block 8b).
    device: MediaDevice,
    /// The room's device: what a decoded frame is moved to when the
    /// decoder ran elsewhere, and whose scaler shrinks an oversize
    /// screen on it.
    hw: Arc<dyn DeviceBackend>,
    scaler: Arc<Mutex<Option<Box<dyn Scaler>>>>,
    slot: Arc<Mutex<Slot>>,
    queued: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    /// A decoder error on the pool asks the decoded layer for a keyframe
    /// on its next packet.
    decode_error: Arc<AtomicBool>,
    rate: Mutex<RateWindow>,
    /// Frames accepted in the current second, for the fps limit.
    accepted_this_second: Mutex<(Instant, u32)>,
    local_ssrc: u32,
    last_packet: Mutex<Option<Instant>>,
    pub stats: Arc<SourceStats>,
}

impl VideoSource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        room_id: &str,
        kind: StreamKind,
        codec: VideoCodec,
        profile: &str,
        decoder: Box<dyn VideoDecoder>,
        hw: Arc<dyn DeviceBackend>,
        limits: SourceLimits,
        local_ssrc: u32,
    ) -> Self {
        Self::layered(
            id, room_id, kind, codec, profile, decoder, hw, limits, local_ssrc, None,
        )
    }

    /// A source with a layer per rid of `simulcast` (one without it, or
    /// with fewer than two rids).
    #[allow(clippy::too_many_arguments)]
    pub fn layered(
        id: &str,
        room_id: &str,
        kind: StreamKind,
        codec: VideoCodec,
        profile: &str,
        decoder: Box<dyn VideoDecoder>,
        hw: Arc<dyn DeviceBackend>,
        limits: SourceLimits,
        local_ssrc: u32,
        simulcast: Option<&Simulcast>,
    ) -> Self {
        let now = Instant::now();
        let device = decoder.device();
        let (layers, rid_extension) = match simulcast.filter(|s| s.rids.len() >= 2) {
            Some(sc) => (
                sc.rids
                    .iter()
                    .map(|rid| Layer::new(Some(rid.clone()), codec, &limits, now))
                    .collect(),
                sc.rid_extension,
            ),
            None => (vec![Layer::new(None, codec, &limits, now)], None),
        };
        Self {
            layers,
            rid_extension,
            choice: Mutex::new(Choice::default()),
            wanted_height: AtomicU32::new(0),
            device,
            hw,
            scaler: Arc::new(Mutex::new(None)),
            id: id.to_string(),
            room_id: room_id.to_string(),
            kind,
            codec,
            profile: profile.to_string(),
            decoder: Arc::new(Mutex::new(DecoderState {
                decoder: Some(decoder),
                consecutive_errors: 0,
            })),
            slot: Arc::new(Mutex::new(Slot::default())),
            queued: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            // The first packet of the decoded layer asks for a keyframe.
            decode_error: Arc::new(AtomicBool::new(true)),
            rate: Mutex::new(RateWindow {
                since: now,
                bytes: 0,
            }),
            accepted_this_second: Mutex::new((now, 0)),
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

    /// Where this stream is decoded.
    pub fn device(&self) -> MediaDevice {
        self.device.clone()
    }

    pub fn kind(&self) -> StreamKind {
        self.kind
    }

    /// The `a=fmtp` this sender negotiated, `""` when its codec has
    /// none. Forwarding its stream on to someone else needs it (§5.6).
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The size of the last frame decoded, or 0×0 before the first.
    pub fn measured_resolution(&self) -> Resolution {
        Resolution::new(
            self.stats.width.load(Ordering::Relaxed),
            self.stats.height.load(Ordering::Relaxed),
        )
    }

    /// The decoder gave up (§13): the tile shows an avatar from now on.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// The SSRC of the layer being decoded: what a NACK or PLI names, and
    /// what the passthrough fast path forwards (§5.6).
    pub fn remote_ssrc(&self) -> u32 {
        let decoding = self.choice.lock().decoding;
        self.layers
            .get(decoding)
            .map(|l| l.ssrc.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Whether the sender sends simulcast.
    pub fn is_simulcast(&self) -> bool {
        self.layers.len() > 1
    }

    /// Each layer as it stands.
    pub fn layers(&self, now: Instant) -> Vec<LayerInfo> {
        let decoding = self.choice.lock().decoding;
        self.layers
            .iter()
            .enumerate()
            .map(|(i, l)| LayerInfo {
                rid: l.rid.clone(),
                ssrc: l.ssrc.load(Ordering::Relaxed),
                resolution: Resolution::new(
                    l.width.load(Ordering::Relaxed),
                    l.height.load(Ordering::Relaxed),
                ),
                bitrate_kbps: l.bitrate_kbps.load(Ordering::Relaxed),
                live: l.live(now),
                decoding: i == decoding,
            })
            .collect()
    }

    /// Which layer a packet belongs to: by its SSRC once known, else by
    /// the rid it names, which binds the SSRC. A sender without
    /// simulcast has one layer, which takes any SSRC (a sender may change
    /// it).
    fn layer_of(&self, packet: &RtpPacket) -> Option<usize> {
        let ssrc = packet.header.ssrc;
        if self.layers.len() == 1 {
            self.layers[0].ssrc.store(ssrc, Ordering::Relaxed);
            return Some(0);
        }
        if let Some(i) = self
            .layers
            .iter()
            .position(|l| l.ssrc.load(Ordering::Relaxed) == ssrc)
        {
            return Some(i);
        }
        let rid = packet.stream_id(self.rid_extension?)?;
        let i = self
            .layers
            .iter()
            .position(|l| l.rid.as_deref() == Some(rid))?;
        let was = self.layers[i].ssrc.swap(ssrc, Ordering::Relaxed);
        debug!(participant = %self.id, rid, ssrc, was, "simulcast layer bound to its SSRC");
        Some(i)
    }

    /// The tallest the room draws this source now: every tick, from the
    /// tiles of the outputs it composes. Picks the layer to decode.
    pub fn set_wanted_height(&self, height: u32, now: Instant) {
        self.wanted_height.store(height, Ordering::Relaxed);
        if self.layers.len() > 1 {
            self.choose_layer(now);
        }
    }

    /// The layer to decode: of the live layers, the smallest at least as
    /// tall as wanted, else the tallest. Heights come from keyframes;
    /// while any live layer's is unknown (or its codec's header is not
    /// read) the layer with the highest bitrate stands in for the
    /// tallest. A move up starts at once, a move down once the smaller
    /// layer has been enough for [`LAYER_DOWN_AFTER`].
    fn choose_layer(&self, now: Instant) {
        let live: Vec<usize> = (0..self.layers.len())
            .filter(|i| self.layers[*i].live(now))
            .collect();
        if live.is_empty() {
            return;
        }
        let wanted = self.wanted_height.load(Ordering::Relaxed);
        let rank = |i: usize| -> (u32, u32) {
            let l = &self.layers[i];
            (
                l.height().unwrap_or(0),
                l.bitrate_kbps.load(Ordering::Relaxed),
            )
        };
        let all_known = live.iter().all(|i| self.layers[*i].height().is_some());
        let tallest = *live.iter().max_by_key(|i| rank(**i)).expect("not empty");
        let best = if all_known {
            live.iter()
                .copied()
                .filter(|i| rank(*i).0 >= wanted)
                .min_by_key(|i| rank(*i))
                .unwrap_or(tallest)
        } else {
            tallest
        };

        let mut c = self.choice.lock();
        let first = !std::mem::replace(&mut c.chosen, true);
        let current_live = live.contains(&c.decoding);
        // A layer ranked the same as the one decoded (both unmeasured, at
        // the start) is no reason to pay a keyframe for a switch.
        if best == c.decoding || (current_live && rank(best) == rank(c.decoding)) {
            c.target = None;
            c.down_since = None;
            return;
        }
        let up = first || !current_live || rank(best) > rank(c.decoding);
        if !up {
            let since = *c.down_since.get_or_insert(now);
            if now.saturating_duration_since(since) < LAYER_DOWN_AFTER {
                return;
            }
        }
        c.down_since = None;
        match c.target {
            Some((t, asked)) if t == best => {
                if now.saturating_duration_since(asked) >= LAYER_REKEY_AFTER {
                    self.layers[t].pli_pending.store(true, Ordering::Release);
                    c.target = Some((t, now));
                }
            }
            _ => {
                debug!(participant = %self.id, from = c.decoding, to = best, wanted,
                       "switching the decoded simulcast layer");
                self.layers[best].pli_pending.store(true, Ordering::Release);
                c.target = Some((best, now));
            }
        }
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
        let Some(index) = self.layer_of(&packet) else {
            // A simulcast packet naming no layer we know: until its rid
            // binds its SSRC there is nowhere to put it.
            counter!("forge_conference_video_packets_unmatched_total", "room_id" => self.room_id.clone())
                .increment(1);
            return Vec::new();
        };
        let layer = &self.layers[index];
        let bytes = packet.payload.len() as u64;
        self.stats.packets_received.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_received
            .fetch_add(bytes, Ordering::Relaxed);
        *self.last_packet.lock() = Some(now);
        *layer.last_packet.lock() = Some(now);
        self.roll_rate(now, bytes);
        layer.roll_rate(now, bytes);

        if self.failed() {
            return Vec::new();
        }

        // Whether this layer's frames are decoded, or it is the one a
        // switch is waiting on: only those are worth NACKs and PLIs.
        let (decoding, target) = {
            let c = self.choice.lock();
            (c.decoding == index, c.target.map(|t| t.0) == Some(index))
        };
        let mut want_keyframe = false;
        let mut to_decode: Vec<CodedFrame> = Vec::new();
        let mut switched = false;
        let missing;
        {
            let mut asm = layer.assembler.lock();
            let mut valid = layer.valid.load(Ordering::Relaxed);
            for ev in asm.push(packet) {
                match ev {
                    AssemblerEvent::Frame(f) => {
                        if f.keyframe {
                            valid = true;
                            if let Some((w, h)) = keyframe_dimensions(self.codec, &f.data) {
                                layer.width.store(w, Ordering::Relaxed);
                                layer.height.store(h, Ordering::Relaxed);
                            }
                        }
                        let take = if decoding {
                            true
                        } else if target && f.keyframe {
                            // The keyframe a switch waited for: decode
                            // this layer from here on.
                            let mut c = self.choice.lock();
                            if c.target.map(|t| t.0) == Some(index) {
                                debug!(participant = %self.id, from = c.decoding, to = index,
                                       "decoding the new simulcast layer");
                                c.decoding = index;
                                c.target = None;
                                switched = true;
                            }
                            switched
                        } else {
                            switched
                        };
                        if !take {
                            continue;
                        }
                        self.stats.frames_received.fetch_add(1, Ordering::Relaxed);
                        if valid {
                            to_decode.push(f);
                        } else {
                            self.drop_frame("waiting for a keyframe");
                            want_keyframe = true;
                        }
                    }
                    AssemblerEvent::Lost { from_seq, to_seq } => {
                        valid = false;
                        if decoding || target {
                            debug!(participant = %self.id, from_seq, to_seq, "video frame lost");
                            self.stats.frames_lost.fetch_add(1, Ordering::Relaxed);
                            counter!("forge_conference_video_frames_lost_total", "room_id" => self.room_id.clone())
                                .increment(1);
                            want_keyframe = true;
                        }
                    }
                    AssemblerEvent::Invalid { timestamp, error } => {
                        valid = false;
                        if decoding || target {
                            debug!(participant = %self.id, timestamp, %error, "invalid video frame");
                            self.stats.frames_lost.fetch_add(1, Ordering::Relaxed);
                            counter!("forge_conference_video_frames_lost_total", "room_id" => self.room_id.clone())
                                .increment(1);
                            want_keyframe = true;
                        }
                    }
                }
            }
            layer.valid.store(valid, Ordering::Relaxed);
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
                layer.valid.store(false, Ordering::Relaxed);
                want_keyframe = true;
                continue;
            }
            self.spawn_decode(f, pool);
        }

        let ssrc = layer.ssrc.load(Ordering::Relaxed);
        let mut out = Vec::new();
        if decoding || target || switched {
            let lost = self.nack_list(layer, &missing, now);
            if !lost.is_empty() {
                self.stats.nacks_sent.fetch_add(1, Ordering::Relaxed);
                counter!("forge_conference_video_nacks_sent_total", "room_id" => self.room_id.clone())
                    .increment(1);
                out.push(RtcpPacket::TransportFeedback(RtpFeedback::nack(
                    self.local_ssrc,
                    ssrc,
                    &lost,
                )));
            }
        }
        let decode_error =
            (decoding || switched) && self.decode_error.swap(false, Ordering::AcqRel);
        if want_keyframe || decode_error || layer.pli_pending.swap(false, Ordering::AcqRel) {
            if layer.pli_gate.lock().allow_at(now) {
                self.stats.plis_sent.fetch_add(1, Ordering::Relaxed);
                counter!("forge_conference_video_plis_sent_total", "room_id" => self.room_id.clone())
                    .increment(1);
                out.push(RtcpPacket::PayloadFeedback(PsFeedback::pli(
                    self.local_ssrc,
                    ssrc,
                )));
            } else {
                // Gated: keep the request so a later packet can carry it.
                layer.pli_pending.store(true, Ordering::Release);
            }
        }
        out
    }

    /// Ask the sender for a keyframe at the next opportunity (a new
    /// subscriber cannot use P-frames; a host re-enabled the video): of
    /// the layer being decoded, and of one a switch is waiting on.
    pub fn request_keyframe(&self) {
        let c = self.choice.lock();
        if let Some(l) = self.layers.get(c.decoding) {
            l.pli_pending.store(true, Ordering::Release);
        }
        if let Some((t, _)) = c.target {
            self.layers[t].pli_pending.store(true, Ordering::Release);
        }
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
    fn nack_list(&self, layer: &Layer, missing: &[u16], now: Instant) -> Vec<u16> {
        let mut nacked = layer.nacked.lock();
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
        let decode_error = Arc::clone(&self.decode_error);
        let stats = Arc::clone(&self.stats);
        let limits = self.limits.clone();
        let kind = self.kind;
        let id = self.id.clone();
        let room_id = self.room_id.clone();
        let hw = Arc::clone(&self.hw);
        let scaler = Arc::clone(&self.scaler);
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
                Ok(Some(mut decoded)) => {
                    state.consecutive_errors = 0;
                    let mut res = decoded.resolution();
                    if res.width > limits.max_resolution.width
                        || res.height > limits.max_resolution.height
                    {
                        // A camera over the cap is a camera misbehaving.
                        // A screen over it is a big monitor: shrink it,
                        // with the filter that keeps its text readable.
                        let shrunk = match (kind, &decoded) {
                            (StreamKind::Content, VideoFrame::Host(host)) => {
                                let to = fit_within(res, limits.max_resolution);
                                Some(VideoFrame::Host(resize_with(
                                    host,
                                    to.width,
                                    to.height,
                                    ScaleMode::Box,
                                )))
                            }
                            // Decoded on the device: shrunk there, on
                            // the room's scaler, made on first need.
                            (StreamKind::Content, VideoFrame::Device(_)) => {
                                let to = fit_within(res, limits.max_resolution);
                                let mut sc = scaler.lock();
                                if sc.is_none() {
                                    match hw.scaler() {
                                        Ok(s) => *sc = Some(s),
                                        Err(e) => {
                                            warn!(participant = %id, error = %e,
                                                  "no scaler on the room's device");
                                        }
                                    }
                                }
                                sc.as_mut().and_then(|s| match s.scale(&decoded, to) {
                                    Ok(f) => Some(f),
                                    Err(e) => {
                                        warn!(participant = %id, error = %e,
                                              "could not shrink a shared screen on the device");
                                        None
                                    }
                                })
                            }
                            _ => None,
                        };
                        match shrunk {
                            Some(frame) => {
                                debug!(participant = %id, from = %res, to = %frame.resolution(),
                                       "shared screen over the resolution cap; shrunk");
                                decoded = frame;
                                res = decoded.resolution();
                            }
                            None => {
                                warn!(participant = %id, %res, cap = %limits.max_resolution,
                                      "decoded video frame over the resolution cap; dropped");
                                stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
                                counter!("forge_conference_video_frames_dropped_total", "room_id" => room_id.clone())
                                    .increment(1);
                                return;
                            }
                        }
                    }
                    // A frame decoded off the room's device (a codec
                    // the device lacks) is moved onto it once, here,
                    // rather than by every output that draws it.
                    if decoded.device() != hw.device() {
                        match hw.resident(&decoded) {
                            Ok(f) => decoded = f,
                            Err(e) => {
                                warn!(participant = %id, error = %e,
                                      "could not move a decoded frame onto the room's device; dropped");
                                stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
                                counter!("forge_conference_video_frames_dropped_total", "room_id" => room_id.clone())
                                    .increment(1);
                                return;
                            }
                        }
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
                    decode_error.store(true, Ordering::Release);
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

#[cfg(test)]
mod tests {
    use super::*;
    use forge_video::device::HostBackend;
    use forge_video::raw::raw_registry;

    fn source(rids: &[&str]) -> VideoSource {
        let decoder = raw_registry()
            .decoder(VideoCodec::VP8, &MediaDevice::Host)
            .unwrap();
        VideoSource::layered(
            "p1",
            "room",
            StreamKind::Camera,
            VideoCodec::VP8,
            "",
            decoder,
            Arc::new(HostBackend),
            SourceLimits::default(),
            1,
            Some(&Simulcast {
                rids: rids.iter().map(|r| r.to_string()).collect(),
                rid_extension: Some(4),
            }),
        )
    }

    /// Mark a layer as arriving now, `height` tall and at `kbps`.
    fn arriving(s: &VideoSource, i: usize, height: u32, kbps: u32, now: Instant) {
        let l = &s.layers[i];
        *l.last_packet.lock() = Some(now);
        l.height.store(height, Ordering::Relaxed);
        l.width.store(height * 16 / 9, Ordering::Relaxed);
        l.bitrate_kbps.store(kbps, Ordering::Relaxed);
    }

    /// Finish whatever switch is pending, as its keyframe would.
    fn keyframe_arrives(s: &VideoSource) {
        let mut c = s.choice.lock();
        if let Some((t, _)) = c.target.take() {
            c.decoding = t;
        }
    }

    fn target(s: &VideoSource) -> Option<usize> {
        s.choice.lock().target.map(|t| t.0)
    }

    #[test]
    fn the_smallest_layer_that_fills_the_tile_is_decoded() {
        let s = source(&["h", "m", "l"]);
        let now = Instant::now();
        arriving(&s, 0, 720, 1200, now);
        arriving(&s, 1, 360, 500, now);
        arriving(&s, 2, 180, 150, now);

        // A 300-row tile: 360 fills it, 180 does not.
        s.set_wanted_height(300, now);
        assert_eq!(target(&s), Some(1));
        assert!(
            s.layers[1].pli_pending.load(Ordering::Relaxed),
            "asked for a keyframe"
        );
        keyframe_arrives(&s);
        assert_eq!(s.choice.lock().decoding, 1);

        // Taller than any layer: the tallest.
        s.set_wanted_height(1080, now);
        assert_eq!(target(&s), Some(0), "a move up is at once");
        keyframe_arrives(&s);

        // In no composite: the smallest, once that has held for a while.
        s.set_wanted_height(0, now);
        assert_eq!(target(&s), None, "a move down waits");
        let later = now + LAYER_DOWN_AFTER;
        for (i, (h, kbps)) in [(720, 1200), (360, 500), (180, 150)]
            .into_iter()
            .enumerate()
        {
            arriving(&s, i, h, kbps, later);
        }
        s.set_wanted_height(0, later);
        assert_eq!(target(&s), Some(2));
    }

    #[test]
    fn a_move_down_is_called_off_when_the_tile_grows_back() {
        let s = source(&["h", "l"]);
        let now = Instant::now();
        arriving(&s, 0, 720, 1200, now);
        arriving(&s, 1, 180, 150, now);
        s.set_wanted_height(600, now);
        assert_eq!(s.choice.lock().decoding, 0);
        s.set_wanted_height(100, now);
        s.set_wanted_height(600, now + Duration::from_millis(500));
        s.set_wanted_height(100, now + Duration::from_millis(600));
        assert_eq!(target(&s), None, "the wait started again");
        let later = now + Duration::from_millis(600) + LAYER_DOWN_AFTER;
        arriving(&s, 0, 720, 1200, later);
        arriving(&s, 1, 180, 150, later);
        s.set_wanted_height(100, later);
        assert_eq!(target(&s), Some(1));
    }

    #[test]
    fn a_layer_the_sender_stopped_is_left_at_once() {
        let s = source(&["h", "l"]);
        let start = Instant::now();
        arriving(&s, 0, 720, 1200, start);
        arriving(&s, 1, 180, 150, start);
        s.set_wanted_height(720, start);
        assert_eq!(s.choice.lock().decoding, 0);

        // The sender's uplink sheds the large layer: only the small one
        // keeps arriving.
        let later = start + LAYER_LIVE + Duration::from_millis(100);
        *s.layers[1].last_packet.lock() = Some(later);
        s.set_wanted_height(720, later);
        assert_eq!(target(&s), Some(1), "no waiting to leave a stopped layer");
    }

    #[test]
    fn without_sizes_the_highest_bitrate_stands_for_the_tallest() {
        let s = source(&["a", "b"]);
        let now = Instant::now();
        arriving(&s, 0, 0, 150, now);
        arriving(&s, 1, 0, 900, now);
        s.set_wanted_height(90, now);
        assert_eq!(target(&s), Some(1));
    }

    #[test]
    fn a_target_that_sends_no_keyframe_is_asked_again() {
        let s = source(&["h", "l"]);
        let now = Instant::now();
        arriving(&s, 0, 180, 150, now);
        arriving(&s, 1, 720, 1200, now);
        s.set_wanted_height(720, now);
        assert_eq!(target(&s), Some(1));
        s.layers[1].pli_pending.store(false, Ordering::Relaxed);
        s.set_wanted_height(720, now + Duration::from_millis(200));
        assert!(!s.layers[1].pli_pending.load(Ordering::Relaxed));
        let later = now + LAYER_REKEY_AFTER;
        arriving(&s, 0, 180, 150, later);
        arriving(&s, 1, 720, 1200, later);
        s.set_wanted_height(720, later);
        assert!(
            s.layers[1].pli_pending.load(Ordering::Relaxed),
            "asked again"
        );
    }

    #[test]
    fn one_rid_is_not_simulcast() {
        let s = source(&["only"]);
        assert!(!s.is_simulcast());
        assert_eq!(s.layers(Instant::now())[0].rid, None);
    }
}
