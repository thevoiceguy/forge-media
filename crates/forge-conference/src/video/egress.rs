//! A room's video egress (design §5.3, §5.4, §7).
//!
//! Subscribers ask for a *flavor* of a *layout output*; those with the
//! same needs share one encoder. Each subscriber has its own SSRC,
//! sequence space and timestamp offset, so it sees one continuous stream
//! whatever the encoder behind it does, and its own retransmission cache
//! (sequence numbers are per subscriber, so the cache must be too).
//!
//! The room does not own sockets: packets leave through a bounded
//! channel the conference server drains, SRTP-protects and sends. A full
//! channel drops the packet (video is real time), and the subscriber's
//! receiver will NACK or PLI its way back.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use forge_core::VideoCodec;
use forge_rtp::video::payload::packetize;
use forge_rtp::{KeyframeRequestGate, RtpPacket, RtxCache};
use forge_video::codec::{EncoderSettings, VideoEncoder};
use forge_video::flavor::Flavor;
use forge_video::frame::{Resolution, VideoFrame};
use metrics::counter;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Which composite a subscriber watches: the shared one, or a private
/// one that leaves the subscriber's own tile out (`exclude_self`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputKey {
    pub exclude: Option<String>,
    pub resolution: Resolution,
}

/// The bitrate ladder's default cap per resolution (§7).
pub fn default_kbps(res: Resolution) -> u32 {
    match res.height {
        h if h >= 1080 => 2500,
        h if h >= 720 => 1200,
        h if h >= 360 => 500,
        _ => 200,
    }
}

/// Counters the room reports per subscriber.
#[derive(Debug, Default)]
pub struct SubscriberStats {
    pub packets_sent: AtomicU64,
    pub bytes_sent: AtomicU64,
    /// Packets the server's channel could not take.
    pub packets_dropped: AtomicU64,
    pub frames_sent: AtomicU64,
    pub keyframes_sent: AtomicU64,
    pub nacks_received: AtomicU64,
    pub packets_retransmitted: AtomicU64,
    pub plis_received: AtomicU64,
    /// Latest REMB from the receiver, kb/s (0 = none yet).
    pub remb_kbps: AtomicU32,
}

struct Sequence {
    seq: u16,
    cache: RtxCache,
}

/// One receiver of a flavor.
pub struct Subscriber {
    pub id: String,
    pub flavor: Flavor,
    pub output: OutputKey,
    pub payload_type: u8,
    pub ssrc: u32,
    ts_offset: u32,
    seq: Mutex<Sequence>,
    tx: mpsc::Sender<Bytes>,
    /// Shared with the encoder serving this flavor.
    pub wants_keyframe: Arc<AtomicBool>,
    pub stats: Arc<SubscriberStats>,
    max_payload: usize,
}

impl Subscriber {
    pub fn new(
        id: &str,
        flavor: Flavor,
        output: OutputKey,
        payload_type: u8,
        wants_keyframe: Arc<AtomicBool>,
        cache_packets: usize,
        channel_packets: usize,
        max_payload: usize,
    ) -> (Arc<Self>, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(channel_packets.max(8));
        let sub = Arc::new(Self {
            id: id.to_string(),
            flavor,
            output,
            payload_type,
            ssrc: rand::random(),
            ts_offset: rand::random(),
            seq: Mutex::new(Sequence {
                seq: rand::random(),
                cache: RtxCache::new(cache_packets),
            }),
            tx,
            wants_keyframe,
            stats: Arc::new(SubscriberStats::default()),
            max_payload: max_payload.max(64),
        });
        (sub, rx)
    }

    /// Packetize one coded frame into this subscriber's stream and send.
    pub fn send_frame(&self, codec: VideoCodec, frame: &forge_rtp::CodedFrame, pts: u32) {
        let payloads = match packetize(codec, frame, self.max_payload) {
            Ok(p) => p,
            Err(e) => {
                warn!(subscriber = %self.id, %codec, error = %e, "could not packetize video frame");
                return;
            }
        };
        let n = payloads.len();
        let ts = pts.wrapping_add(self.ts_offset);
        let mut s = self.seq.lock();
        for (i, p) in payloads.into_iter().enumerate() {
            let seq = s.seq;
            let packet = RtpPacket::build(self.payload_type, seq, ts, self.ssrc, p, i + 1 == n);
            let bytes = packet.to_bytes().freeze();
            s.cache.push(seq, bytes.clone());
            s.seq = seq.wrapping_add(1);
            self.deliver(bytes);
        }
        self.stats.frames_sent.fetch_add(1, Ordering::Relaxed);
        if frame.keyframe {
            self.stats.keyframes_sent.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Answer a NACK from the cache; numbers beyond it are ignored (§13).
    pub fn retransmit(&self, seqs: &[u16]) -> usize {
        self.stats.nacks_received.fetch_add(1, Ordering::Relaxed);
        let s = self.seq.lock();
        let found: Vec<Bytes> = s
            .cache
            .lookup(seqs)
            .into_iter()
            .map(|(_, b)| b.clone())
            .collect();
        drop(s);
        let n = found.len();
        for b in found {
            self.deliver(b);
        }
        self.stats
            .packets_retransmitted
            .fetch_add(n as u64, Ordering::Relaxed);
        n
    }

    /// A PLI or FIR from this receiver: the shared encoder re-keys.
    pub fn request_keyframe(&self) {
        self.stats.plis_received.fetch_add(1, Ordering::Relaxed);
        self.wants_keyframe.store(true, Ordering::Release);
    }

    pub fn set_remb(&self, bitrate_bps: u64) {
        let kbps = (bitrate_bps / 1000).min(u32::MAX as u64) as u32;
        self.stats.remb_kbps.store(kbps, Ordering::Relaxed);
    }

    pub fn remb_kbps(&self) -> u32 {
        self.stats.remb_kbps.load(Ordering::Relaxed)
    }

    fn deliver(&self, bytes: Bytes) {
        let len = bytes.len() as u64;
        match self.tx.try_send(bytes) {
            Ok(()) => {
                self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                self.stats.bytes_sent.fetch_add(len, Ordering::Relaxed);
            }
            Err(_) => {
                self.stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// What a subscriber gets back: its SSRC and the packets to send.
pub struct VideoSubscription {
    pub ssrc: u32,
    pub payload_type: u8,
    pub flavor: Flavor,
    /// RTP packets, unencrypted, in order. Protect and send them.
    pub packets: mpsc::Receiver<Bytes>,
}

impl std::fmt::Debug for VideoSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoSubscription")
            .field("ssrc", &self.ssrc)
            .field("payload_type", &self.payload_type)
            .field("flavor", &self.flavor)
            .finish()
    }
}

/// One encoder serving every subscriber of a flavor (§5.3).
pub struct FlavorEncoder {
    pub flavor: Flavor,
    encoder: Box<dyn VideoEncoder>,
    pub wants_keyframe: Arc<AtomicBool>,
    gate: KeyframeRequestGate,
    frames_since_keyframe: u32,
    keyframe_interval_frames: u32,
    /// Frame-rate decimation accumulator against the room clock.
    due: u32,
    target_kbps: u32,
    pub encode_errors: u64,
    pub frames_encoded: u64,
    pub keyframes: u64,
}

impl FlavorEncoder {
    pub fn new(
        flavor: Flavor,
        encoder: Box<dyn VideoEncoder>,
        keyframe_interval: Duration,
        keyframe_min_interval: Duration,
    ) -> Self {
        let frames = (keyframe_interval.as_secs_f64() * flavor.fps as f64).round() as u32;
        let target_kbps = encoder.settings().bitrate_kbps;
        Self {
            wants_keyframe: Arc::new(AtomicBool::new(true)),
            gate: KeyframeRequestGate::new(keyframe_min_interval),
            frames_since_keyframe: 0,
            keyframe_interval_frames: frames.max(1),
            due: 0,
            target_kbps,
            encode_errors: 0,
            frames_encoded: 0,
            keyframes: 0,
            flavor,
            encoder,
        }
    }

    pub fn settings(&self) -> &EncoderSettings {
        self.encoder.settings()
    }

    pub fn target_kbps(&self) -> u32 {
        self.target_kbps
    }

    /// Whether this flavor takes the room's tick: a 15 fps flavor in a
    /// 30 fps room encodes every other canvas.
    pub fn due(&mut self, room_fps: u32) -> bool {
        let room_fps = room_fps.max(1);
        self.due += self.flavor.fps.min(room_fps);
        if self.due >= room_fps {
            self.due -= room_fps;
            true
        } else {
            false
        }
    }

    /// Point the encoder at the lowest rate its subscribers can take,
    /// never above the flavor's cap (§7).
    pub fn set_target_kbps(&mut self, kbps: u32) {
        let kbps = kbps.clamp(1, self.flavor.max_kbps.max(1));
        if kbps != self.target_kbps {
            match self.encoder.set_bitrate(kbps) {
                Ok(()) => {
                    debug!(flavor = %self.flavor, kbps, "video encoder bitrate changed");
                    self.target_kbps = kbps;
                }
                Err(e) => {
                    warn!(flavor = %self.flavor, error = %e, "could not change video bitrate")
                }
            }
        }
    }

    /// Encode the canvas; decides the keyframe (requested and past the
    /// gate, or the fixed interval). Returns the coded frames.
    pub fn encode(
        &mut self,
        canvas: &VideoFrame,
        now: Instant,
        room_id: &str,
    ) -> Vec<forge_rtp::CodedFrame> {
        let requested = self.wants_keyframe.load(Ordering::Acquire);
        let mut keyframe = false;
        if requested && self.gate.allow_at(now) {
            keyframe = true;
        }
        if self.frames_since_keyframe >= self.keyframe_interval_frames {
            keyframe = true;
        }
        match self.encoder.encode(canvas, keyframe) {
            Ok(frames) => {
                self.frames_encoded += 1;
                let keyed = frames.iter().any(|f| f.keyframe);
                if keyed {
                    self.wants_keyframe.store(false, Ordering::Release);
                    self.frames_since_keyframe = 0;
                    self.keyframes += 1;
                    counter!("forge_conference_video_keyframes_sent_total", "room_id" => room_id.to_string())
                        .increment(1);
                } else {
                    self.frames_since_keyframe += 1;
                }
                frames
            }
            Err(e) => {
                self.encode_errors += 1;
                counter!("forge_conference_video_encode_errors_total", "room_id" => room_id.to_string())
                    .increment(1);
                warn!(flavor = %self.flavor, error = %e, "video encode failed");
                Vec::new()
            }
        }
    }
}

impl std::fmt::Debug for FlavorEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlavorEncoder")
            .field("flavor", &self.flavor)
            .field("target_kbps", &self.target_kbps)
            .finish()
    }
}
