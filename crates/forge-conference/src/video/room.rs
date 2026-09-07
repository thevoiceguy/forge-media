//! The video room (design §4, §5.2–§5.4, §8, §9.2).
//!
//! Lives beside the audio [`ConferenceRoom`](crate::ConferenceRoom) and
//! borrows its participant list and per-participant energy. A tokio task
//! owns the [`VideoClock`]; on every tick it queues one compose job on
//! the codec pool and waits for it, so the clock's overrun back-off
//! measures the real work. The job reads the audio levels, updates the
//! active speaker, orders the tiles per layout, renders one canvas per
//! layout output, encodes it once per flavor, and packetizes into each
//! subscriber's stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use forge_core::VideoCodec;
use forge_rtp::rtcp::{PayloadFeedback, RtcpPacket, TransportFeedback};
use forge_rtp::RtpPacket;
use forge_video::codec::{CodecRegistry, EncoderSettings};
use forge_video::compose::{Compositor, HostCompositor, TileSource};
use forge_video::flavor::Flavor;
use forge_video::frame::{MediaDevice, Resolution, VideoFrame};
use forge_video::layout::Layout;
use forge_video::{ClockEvent, VideoClock};
use metrics::{counter, gauge, histogram};
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::egress::{default_kbps, FlavorEncoder, OutputKey, Subscriber, VideoSubscription};
use super::pool::CodecPool;
use super::source::{SourceLimits, VideoSource};
use super::speaker::{ActiveSpeaker, Level};
use crate::{ConferenceError, ConferenceRoom, Result};

/// The codec registry and thread pool every video room on a node shares.
#[derive(Clone)]
pub struct VideoBackend {
    pub registry: Arc<CodecRegistry>,
    pub pool: Arc<CodecPool>,
    /// Where this node's rooms run (phase 7 adds GPUs).
    pub device: MediaDevice,
}

impl VideoBackend {
    pub fn new(registry: CodecRegistry, pool: Arc<CodecPool>) -> Self {
        Self {
            registry: Arc::new(registry),
            pool,
            device: MediaDevice::Host,
        }
    }

    /// The raw test codec on a small pool: what the tests use.
    pub fn raw() -> Self {
        Self::new(forge_video::raw::raw_registry(), CodecPool::new(2))
    }
}

impl std::fmt::Debug for VideoBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoBackend")
            .field("device", &self.device)
            .field("pool", &self.pool.size())
            .finish()
    }
}

/// A room's video settings (the meeting's `video_*` fields plus tunables).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRoomSettings {
    pub layout: Layout,
    /// Tiles in the composite (1–16).
    pub max_tiles: usize,
    /// Canvas size; also the cap for what subscribers may ask for.
    pub resolution: Resolution,
    pub fps: u32,
    /// Give each participant a private composite without their own tile.
    pub exclude_self: bool,
    /// Codec preference for subscribers.
    pub codecs: Vec<VideoCodec>,
    /// Fixed keyframe interval per encoder (§5.4).
    pub keyframe_interval: Duration,
    /// Keyframe requests to one encoder are coalesced to this spacing.
    pub keyframe_min_interval: Duration,
    /// A tile keeps its last frame this long before showing an avatar.
    pub freeze_timeout: Duration,
    /// Packets kept per subscriber for retransmission.
    pub rtx_cache_packets: usize,
    /// Packets buffered per subscriber before the server drains them.
    pub egress_queue_packets: usize,
    /// Largest RTP payload we emit.
    pub max_payload: usize,
    pub limits: SourceLimits,
}

impl Default for VideoRoomSettings {
    fn default() -> Self {
        Self {
            layout: Layout::Grid,
            max_tiles: 16,
            resolution: Resolution::new(1280, 720),
            fps: 15,
            exclude_self: false,
            codecs: vec![VideoCodec::H264, VideoCodec::VP8],
            keyframe_interval: Duration::from_secs(10),
            keyframe_min_interval: Duration::from_secs(1),
            freeze_timeout: Duration::from_secs(2),
            rtx_cache_packets: 256,
            egress_queue_packets: 256,
            max_payload: 1200,
            limits: SourceLimits::default(),
        }
    }
}

/// A participant's video, as the room and the API see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoState {
    /// No video negotiated: an avatar tile.
    Off,
    /// Frames are arriving.
    On,
    /// Video was negotiated but nothing decodable has arrived lately.
    Lost,
    /// A host turned this participant's video off.
    Disabled,
    /// The decoder gave up on this source (§13).
    Failed,
}

impl VideoState {
    pub fn name(&self) -> &'static str {
        match self {
            VideoState::Off => "off",
            VideoState::On => "on",
            VideoState::Lost => "lost",
            VideoState::Disabled => "disabled",
            VideoState::Failed => "failed",
        }
    }
}

/// What the room tells the server about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoRoomEvent {
    /// The clock changed rate; `overload` when it was halved.
    FpsChanged { from: u32, to: u32, overload: bool },
    /// A participant's video state changed.
    ParticipantState {
        participant_id: String,
        state: VideoState,
    },
    /// The active speaker changed (`None` when the speaker left).
    ActiveSpeaker { participant_id: Option<String> },
    /// Layout, pin or spotlight changed.
    LayoutChanged {
        layout: Layout,
        pinned: Option<String>,
        spotlight: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct Participant {
    name: String,
    /// Host control (§8): `false` hides the participant's video.
    enabled: bool,
    state: VideoState,
    joined: u64,
}

#[derive(Debug, Clone, Default)]
struct Control {
    layout: Option<Layout>,
    pinned: Option<String>,
    spotlight: Option<String>,
}

struct Output {
    compositor: HostCompositor,
    encoders: HashMap<Flavor, FlavorEncoder>,
}

/// What a subscriber asks for; the room clamps it to its settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeRequest {
    pub codec: VideoCodec,
    /// The negotiated `a=fmtp` (H.264 profile, …), `""` when none.
    pub profile: String,
    pub payload_type: u8,
    /// Wanted resolution; the room's is the cap and the default.
    pub resolution: Option<Resolution>,
    /// Wanted frame rate; the room's is the cap and the default.
    pub fps: Option<u32>,
    /// Bitrate cap; the ladder's default for the resolution otherwise.
    pub max_kbps: Option<u32>,
}

/// One participant's video, for the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoParticipantInfo {
    pub participant_id: String,
    pub state: VideoState,
    pub display_name: String,
    pub speaking: bool,
    /// The ingress side, when the participant sends video.
    pub source: Option<VideoSourceInfo>,
    /// The egress side, when the participant receives video.
    pub subscription: Option<VideoSubscriberInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSourceInfo {
    pub codec: VideoCodec,
    pub resolution: Resolution,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub packets_received: u64,
    pub frames_decoded: u64,
    pub frames_lost: u64,
    pub frames_dropped: u64,
    pub decode_errors: u64,
    pub nacks_sent: u64,
    pub plis_sent: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSubscriberInfo {
    pub flavor: Flavor,
    pub ssrc: u32,
    pub packets_sent: u64,
    pub packets_dropped: u64,
    pub frames_sent: u64,
    pub keyframes_sent: u64,
    pub nacks_received: u64,
    pub packets_retransmitted: u64,
    pub plis_received: u64,
    pub remb_kbps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFlavorInfo {
    pub flavor: Flavor,
    pub subscribers: usize,
    pub target_kbps: u32,
    pub keyframes: u64,
    pub encode_errors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoOutputInfo {
    pub exclude: Option<String>,
    pub resolution: Resolution,
    pub flavors: Vec<VideoFlavorInfo>,
}

/// The room's video, for the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRoomStatus {
    pub layout: Layout,
    pub fps: u32,
    pub target_fps: u32,
    pub resolution: Resolution,
    pub pinned: Option<String>,
    pub spotlight: Option<String>,
    pub active_speaker: Option<String>,
    pub sources: usize,
    pub encoders: usize,
    pub ticks: u64,
    pub overruns: u64,
    pub outputs: Vec<VideoOutputInfo>,
}

pub struct VideoRoom {
    id: String,
    settings: RwLock<VideoRoomSettings>,
    backend: VideoBackend,
    audio: Weak<ConferenceRoom>,
    participants: DashMap<String, Participant>,
    join_seq: AtomicU64,
    sources: DashMap<String, Arc<VideoSource>>,
    subscribers: DashMap<String, Arc<Subscriber>>,
    outputs: Mutex<HashMap<OutputKey, Output>>,
    speaker: Mutex<ActiveSpeaker>,
    control: Mutex<Control>,
    /// Current and wanted clock rate; the clock task applies changes.
    fps: AtomicU32,
    target_fps: AtomicU32,
    events: broadcast::Sender<VideoRoomEvent>,
    clock_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    started: Instant,
    ticks: AtomicU64,
    overruns: AtomicU64,
    /// Per-source decoded-frame counts at the last fps sample.
    fps_samples: Mutex<(Instant, HashMap<String, u64>)>,
    stopped: AtomicBool,
}

impl VideoRoom {
    /// Create the room and start its clock. `audio` supplies the
    /// participant list and levels; participants already in it are
    /// adopted.
    pub(crate) fn start(
        id: &str,
        settings: VideoRoomSettings,
        backend: VideoBackend,
        audio: &Arc<ConferenceRoom>,
    ) -> Arc<Self> {
        let fps = settings.fps.clamp(1, 60);
        let (events, _) = broadcast::channel(64);
        let room = Arc::new(Self {
            id: id.to_string(),
            settings: RwLock::new(settings),
            backend,
            audio: Arc::downgrade(audio),
            participants: DashMap::new(),
            join_seq: AtomicU64::new(0),
            sources: DashMap::new(),
            subscribers: DashMap::new(),
            outputs: Mutex::new(HashMap::new()),
            speaker: Mutex::new(ActiveSpeaker::default()),
            control: Mutex::new(Control::default()),
            fps: AtomicU32::new(fps),
            target_fps: AtomicU32::new(fps),
            events,
            clock_task: Mutex::new(None),
            started: Instant::now(),
            ticks: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            fps_samples: Mutex::new((Instant::now(), HashMap::new())),
            stopped: AtomicBool::new(false),
        });
        // Adopt in join order so the grid is stable from the first tick.
        let mut existing: Vec<_> = audio
            .get_all_participant_metadata()
            .into_iter()
            .filter(|m| !m.id.starts_with("__"))
            .collect();
        existing.sort_by(|a, b| a.join_time.cmp(&b.join_time).then_with(|| a.id.cmp(&b.id)));
        for meta in existing {
            room.participant_joined(&meta.id);
        }
        room.spawn_clock();
        gauge!("forge_conference_video_rooms").increment(1.0);
        info!(room = %id, fps, "video room started");
        room
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn settings(&self) -> VideoRoomSettings {
        self.settings.read().clone()
    }

    pub fn events(&self) -> broadcast::Receiver<VideoRoomEvent> {
        self.events.subscribe()
    }

    /// Stop the clock and drop every source, subscriber and encoder.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(t) = self.clock_task.lock().take() {
            t.abort();
        }
        self.sources.clear();
        self.subscribers.clear();
        self.outputs.lock().clear();
        gauge!("forge_conference_video_rooms").decrement(1.0);
        info!(room = %self.id, "video room stopped");
    }

    // ---- participants ---------------------------------------------------

    /// A participant is in the audio room; called by the audio room and
    /// at start. Idempotent.
    pub fn participant_joined(&self, id: &str) {
        self.participants
            .entry(id.to_string())
            .or_insert_with(|| Participant {
                name: id.to_string(),
                enabled: true,
                state: VideoState::Off,
                joined: self.join_seq.fetch_add(1, Ordering::Relaxed),
            });
    }

    /// The participant left: source, subscription and tile go with them.
    pub fn participant_left(&self, id: &str) {
        self.participants.remove(id);
        self.remove_source(id);
        self.unsubscribe(id);
        self.speaker.lock().remove(id);
        let mut c = self.control.lock();
        let mut changed = false;
        if c.pinned.as_deref() == Some(id) {
            c.pinned = None;
            changed = true;
        }
        if c.spotlight.as_deref() == Some(id) {
            c.spotlight = None;
            changed = true;
        }
        if changed {
            let ev = VideoRoomEvent::LayoutChanged {
                layout: c.layout.unwrap_or(self.settings.read().layout),
                pinned: c.pinned.clone(),
                spotlight: c.spotlight.clone(),
            };
            drop(c);
            let _ = self.events.send(ev);
        }
    }

    pub fn set_display_name(&self, id: &str, name: &str) {
        if let Some(mut p) = self.participants.get_mut(id) {
            p.name = name.to_string();
        }
    }

    /// Host control: hide or show a participant's video.
    pub fn set_participant_video_enabled(&self, id: &str, enabled: bool) {
        if let Some(mut p) = self.participants.get_mut(id) {
            p.enabled = enabled;
        }
        if enabled {
            if let Some(s) = self.sources.get(id) {
                s.request_keyframe();
            }
        }
    }

    // ---- ingress ----------------------------------------------------------

    /// The participant sends `codec`; start decoding it.
    pub fn add_source(&self, id: &str, codec: VideoCodec) -> Result<()> {
        if !self.participants.contains_key(id) {
            return Err(ConferenceError::Internal(format!(
                "participant {id} is not in room {}",
                self.id
            )));
        }
        let settings = self.settings.read().clone();
        let decoder = self
            .backend
            .registry
            .decoder(codec, &self.backend.device)
            .map_err(|e| ConferenceError::Internal(format!("no video decoder: {e}")))?;
        let mut limits = settings.limits.clone();
        // The room's canvas is the most a source can usefully send.
        limits.max_resolution = Resolution::new(
            limits.max_resolution.width.max(settings.resolution.width),
            limits.max_resolution.height.max(settings.resolution.height),
        );
        let source = Arc::new(VideoSource::new(
            id,
            &self.id,
            codec,
            decoder,
            limits,
            rand::random(),
        ));
        self.sources.insert(id.to_string(), source);
        gauge!("forge_conference_video_sources").increment(1.0);
        debug!(room = %self.id, participant = %id, %codec, "video source added");
        Ok(())
    }

    /// The participant stopped sending video (re-INVITE with port 0).
    pub fn remove_source(&self, id: &str) {
        if self.sources.remove(id).is_some() {
            gauge!("forge_conference_video_sources").decrement(1.0);
            self.set_state(id, VideoState::Off);
        }
    }

    pub fn has_source(&self, id: &str) -> bool {
        self.sources.contains_key(id)
    }

    /// Feed one RTP packet from a source. Returns the RTCP feedback (NACK,
    /// PLI) to send back to the sender.
    pub fn push_rtp(&self, id: &str, packet: RtpPacket) -> Vec<RtcpPacket> {
        match self.sources.get(id) {
            Some(s) => s.push(packet, &self.backend.pool, Instant::now()),
            None => Vec::new(),
        }
    }

    /// The source's decoder needs a keyframe (a new subscriber, a host
    /// re-enabled it); the next packet carries the request.
    pub fn request_source_keyframe(&self, id: &str) {
        if let Some(s) = self.sources.get(id) {
            s.request_keyframe();
        }
    }

    // ---- egress -----------------------------------------------------------

    /// The participant receives the composite in the given flavor.
    /// Replaces any existing subscription.
    pub fn subscribe(&self, id: &str, req: SubscribeRequest) -> Result<VideoSubscription> {
        if !self.participants.contains_key(id) {
            return Err(ConferenceError::Internal(format!(
                "participant {id} is not in room {}",
                self.id
            )));
        }
        let settings = self.settings.read().clone();
        let cap = settings.resolution;
        let res = req
            .resolution
            .map(|r| Resolution::new(r.width.min(cap.width), r.height.min(cap.height)))
            .unwrap_or(cap);
        let fps = req
            .fps
            .unwrap_or(settings.fps)
            .clamp(1, settings.fps.max(1));
        let kbps = req.max_kbps.unwrap_or_else(|| default_kbps(res)).max(1);
        let flavor = Flavor::new(req.codec, &req.profile, res, fps, kbps);
        let output = OutputKey {
            exclude: settings.exclude_self.then(|| id.to_string()),
            resolution: res,
        };

        self.unsubscribe(id);

        let layout = self.layout();
        let wants_keyframe = {
            let mut outputs = self.outputs.lock();
            let out = outputs.entry(output.clone()).or_insert_with(|| Output {
                compositor: HostCompositor::new(res.width, res.height, layout),
                encoders: HashMap::new(),
            });
            if let Some(enc) = out.encoders.get(&flavor) {
                // A new subscriber on a shared encoder: everyone gets a
                // keyframe (§5.4).
                enc.wants_keyframe.store(true, Ordering::Release);
                Arc::clone(&enc.wants_keyframe)
            } else {
                let es = EncoderSettings::for_flavor(
                    &flavor,
                    (settings.keyframe_interval.as_secs_f64() * fps as f64).round() as u32,
                );
                let encoder = self
                    .backend
                    .registry
                    .encoder(&es, &self.backend.device)
                    .map_err(|e| ConferenceError::Internal(format!("no video encoder: {e}")))?;
                let fe = FlavorEncoder::new(
                    flavor.clone(),
                    encoder,
                    settings.keyframe_interval,
                    settings.keyframe_min_interval,
                );
                let wk = Arc::clone(&fe.wants_keyframe);
                out.encoders.insert(flavor.clone(), fe);
                gauge!("forge_conference_video_encoders").increment(1.0);
                wk
            }
        };

        let (sub, rx) = Subscriber::new(
            id,
            flavor.clone(),
            output,
            req.payload_type,
            wants_keyframe,
            settings.rtx_cache_packets,
            settings.egress_queue_packets,
            settings.max_payload,
        );
        let ssrc = sub.ssrc;
        self.subscribers.insert(id.to_string(), sub);
        debug!(room = %self.id, participant = %id, %flavor, "video subscriber added");
        Ok(VideoSubscription {
            ssrc,
            payload_type: req.payload_type,
            flavor,
            packets: rx,
        })
    }

    /// Stop sending the composite to the participant. Drops the encoder
    /// when it was the last subscriber of its flavor.
    pub fn unsubscribe(&self, id: &str) {
        let Some((_, sub)) = self.subscribers.remove(id) else {
            return;
        };
        let still_used = self
            .subscribers
            .iter()
            .any(|s| s.output == sub.output && s.flavor == sub.flavor);
        if !still_used {
            let mut outputs = self.outputs.lock();
            if let Some(out) = outputs.get_mut(&sub.output) {
                if out.encoders.remove(&sub.flavor).is_some() {
                    gauge!("forge_conference_video_encoders").decrement(1.0);
                }
                if out.encoders.is_empty() {
                    outputs.remove(&sub.output);
                }
            }
        }
        debug!(room = %self.id, participant = %id, "video subscriber removed");
    }

    pub fn has_subscriber(&self, id: &str) -> bool {
        self.subscribers.contains_key(id)
    }

    /// RTCP feedback from a receiver of the composite: PLI/FIR re-key its
    /// encoder, a NACK is answered from its cache, REMB caps its rate.
    pub fn handle_feedback(&self, id: &str, packet: &RtcpPacket) {
        let Some(sub) = self.subscribers.get(id) else {
            return;
        };
        match packet {
            RtcpPacket::PayloadFeedback(fb) => match &fb.kind {
                PayloadFeedback::Pli | PayloadFeedback::Fir(_) => {
                    counter!("forge_conference_video_plis_received_total", "room_id" => self.id.clone())
                        .increment(1);
                    sub.request_keyframe();
                }
                PayloadFeedback::Remb { bitrate_bps, .. } => sub.set_remb(*bitrate_bps),
                PayloadFeedback::Other { .. } => {}
            },
            RtcpPacket::TransportFeedback(fb) => {
                if let TransportFeedback::Nack(entries) = &fb.kind {
                    counter!("forge_conference_video_nacks_received_total", "room_id" => self.id.clone())
                        .increment(1);
                    let mut seqs: Vec<u16> = entries.iter().flat_map(|e| e.lost()).collect();
                    seqs.sort_unstable();
                    seqs.dedup();
                    sub.retransmit(&seqs);
                }
            }
            _ => {}
        }
    }

    // ---- layout control -----------------------------------------------------

    pub fn layout(&self) -> Layout {
        self.control
            .lock()
            .layout
            .unwrap_or_else(|| self.settings.read().layout)
    }

    pub fn set_layout(&self, layout: Layout) {
        {
            let mut c = self.control.lock();
            c.layout = Some(layout);
        }
        for out in self.outputs.lock().values_mut() {
            out.compositor.set_layout(layout);
        }
        self.emit_layout();
    }

    /// Keep a participant in the first tile (`None` clears).
    pub fn pin(&self, id: Option<&str>) {
        self.control.lock().pinned = id.map(str::to_string);
        self.emit_layout();
    }

    /// Put one participant full-canvas: sets the spotlight layout.
    pub fn spotlight(&self, id: Option<&str>) {
        {
            let mut c = self.control.lock();
            c.spotlight = id.map(str::to_string);
            if id.is_some()
                && !matches!(c.layout, Some(Layout::Spotlight | Layout::PictureInPicture))
            {
                c.layout = Some(Layout::Spotlight);
            }
        }
        let layout = self.layout();
        for out in self.outputs.lock().values_mut() {
            out.compositor.set_layout(layout);
        }
        self.emit_layout();
    }

    fn emit_layout(&self) {
        let c = self.control.lock();
        let ev = VideoRoomEvent::LayoutChanged {
            layout: c.layout.unwrap_or_else(|| self.settings.read().layout),
            pinned: c.pinned.clone(),
            spotlight: c.spotlight.clone(),
        };
        drop(c);
        let _ = self.events.send(ev);
    }

    /// Change the target frame rate (applied at the next tick).
    pub fn set_fps(&self, fps: u32) {
        let fps = fps.clamp(1, 60);
        self.settings.write().fps = fps;
        self.target_fps.store(fps, Ordering::Relaxed);
    }

    pub fn fps(&self) -> u32 {
        self.fps.load(Ordering::Relaxed)
    }

    pub fn active_speaker(&self) -> Option<String> {
        self.speaker.lock().current().map(str::to_string)
    }

    // ---- status -----------------------------------------------------------

    pub fn participant(&self, id: &str) -> Option<VideoParticipantInfo> {
        let p = self.participants.get(id)?;
        Some(self.participant_info(id, &p))
    }

    pub fn participants(&self) -> Vec<VideoParticipantInfo> {
        let mut v: Vec<_> = self
            .participants
            .iter()
            .map(|e| (e.joined, self.participant_info(e.key(), e.value())))
            .collect();
        v.sort_by_key(|(j, _)| *j);
        v.into_iter().map(|(_, i)| i).collect()
    }

    fn participant_info(&self, id: &str, p: &Participant) -> VideoParticipantInfo {
        let source = self.sources.get(id).map(|s| {
            let st = &s.stats;
            VideoSourceInfo {
                codec: s.codec(),
                resolution: Resolution::new(
                    st.width.load(Ordering::Relaxed),
                    st.height.load(Ordering::Relaxed),
                ),
                fps: st.fps.load(Ordering::Relaxed),
                bitrate_kbps: st.bitrate_kbps.load(Ordering::Relaxed),
                packets_received: st.packets_received.load(Ordering::Relaxed),
                frames_decoded: st.frames_decoded.load(Ordering::Relaxed),
                frames_lost: st.frames_lost.load(Ordering::Relaxed),
                frames_dropped: st.frames_dropped.load(Ordering::Relaxed),
                decode_errors: st.decode_errors.load(Ordering::Relaxed),
                nacks_sent: st.nacks_sent.load(Ordering::Relaxed),
                plis_sent: st.plis_sent.load(Ordering::Relaxed),
            }
        });
        let subscription = self.subscribers.get(id).map(|s| {
            let st = &s.stats;
            VideoSubscriberInfo {
                flavor: s.flavor.clone(),
                ssrc: s.ssrc,
                packets_sent: st.packets_sent.load(Ordering::Relaxed),
                packets_dropped: st.packets_dropped.load(Ordering::Relaxed),
                frames_sent: st.frames_sent.load(Ordering::Relaxed),
                keyframes_sent: st.keyframes_sent.load(Ordering::Relaxed),
                nacks_received: st.nacks_received.load(Ordering::Relaxed),
                packets_retransmitted: st.packets_retransmitted.load(Ordering::Relaxed),
                plis_received: st.plis_received.load(Ordering::Relaxed),
                remb_kbps: st.remb_kbps.load(Ordering::Relaxed),
            }
        });
        VideoParticipantInfo {
            participant_id: id.to_string(),
            state: p.state,
            display_name: p.name.clone(),
            speaking: self.speaker.lock().current() == Some(id),
            source,
            subscription,
        }
    }

    pub fn status(&self) -> VideoRoomStatus {
        let c = self.control.lock().clone();
        let settings = self.settings.read().clone();
        let outputs = self.outputs.lock();
        let mut outs: Vec<VideoOutputInfo> = outputs
            .iter()
            .map(|(k, o)| {
                let mut flavors: Vec<VideoFlavorInfo> = o
                    .encoders
                    .values()
                    .map(|e| VideoFlavorInfo {
                        flavor: e.flavor.clone(),
                        subscribers: self
                            .subscribers
                            .iter()
                            .filter(|s| &s.output == k && s.flavor == e.flavor)
                            .count(),
                        target_kbps: e.target_kbps(),
                        keyframes: e.keyframes,
                        encode_errors: e.encode_errors,
                    })
                    .collect();
                flavors.sort_by(|a, b| a.flavor.cmp(&b.flavor));
                VideoOutputInfo {
                    exclude: k.exclude.clone(),
                    resolution: k.resolution,
                    flavors,
                }
            })
            .collect();
        outs.sort_by(|a, b| (&a.exclude, a.resolution).cmp(&(&b.exclude, b.resolution)));
        let encoders = outputs.values().map(|o| o.encoders.len()).sum();
        drop(outputs);
        VideoRoomStatus {
            layout: c.layout.unwrap_or(settings.layout),
            fps: self.fps(),
            target_fps: self.target_fps.load(Ordering::Relaxed),
            resolution: settings.resolution,
            pinned: c.pinned,
            spotlight: c.spotlight,
            active_speaker: self.active_speaker(),
            sources: self.sources.len(),
            encoders,
            ticks: self.ticks.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            outputs: outs,
        }
    }

    // ---- the clock and the tick ----------------------------------------------

    fn spawn_clock(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let fps = self.fps();
        let task = tokio::spawn(async move {
            let mut clock = VideoClock::new(fps);
            loop {
                let n = clock.tick().await;
                let Some(room) = weak.upgrade() else { break };
                if room.stopped.load(Ordering::Acquire) {
                    break;
                }
                let wanted = room.target_fps.load(Ordering::Relaxed);
                if wanted != clock.target_fps() {
                    clock.set_target_fps(wanted);
                    room.fps.store(clock.fps(), Ordering::Relaxed);
                }
                // One compose job in flight per room: the clock waits for
                // it, so its overrun accounting sees the real cost.
                let (tx, rx) = tokio::sync::oneshot::channel();
                let job_room = Weak::clone(&weak);
                let submitted = room.backend.pool.submit(move || {
                    if let Some(r) = job_room.upgrade() {
                        r.compose_tick(n);
                    }
                    let _ = tx.send(());
                });
                if !submitted {
                    warn!(room = %room.id, "codec pool is gone; video clock stopping");
                    break;
                }
                let _ = rx.await;
                if let Some(ev) = clock.done() {
                    let (from, to, overload) = match ev {
                        ClockEvent::FpsHalved { from, to } => (from, to, true),
                        ClockEvent::FpsRestored { from, to } => (from, to, false),
                    };
                    room.fps.store(to, Ordering::Relaxed);
                    if overload {
                        warn!(room = %room.id, from, to, "video compositor overran; frame rate halved");
                    } else {
                        info!(room = %room.id, from, to, "video frame rate restored");
                    }
                    let _ = room
                        .events
                        .send(VideoRoomEvent::FpsChanged { from, to, overload });
                }
                room.overruns.store(clock.overruns(), Ordering::Relaxed);
                gauge!("forge_conference_video_fps", "room_id" => room.id.clone())
                    .set(clock.fps() as f64);
            }
        });
        *self.clock_task.lock() = Some(task);
    }

    /// One tick's work, on a pool thread.
    fn compose_tick(&self, tick: u64) {
        let started = Instant::now();
        let now = started;
        self.ticks.store(tick, Ordering::Relaxed);
        counter!("forge_conference_video_ticks_total", "room_id" => self.id.clone()).increment(1);
        let settings = self.settings.read().clone();

        // Audio levels → speaker, muted.
        let metadata = self
            .audio
            .upgrade()
            .map(|a| a.get_all_participant_metadata())
            .unwrap_or_default();
        let mut muted: HashMap<String, bool> = HashMap::new();
        let mut levels: Vec<(String, f32, bool)> = Vec::new();
        for m in metadata {
            if m.id.starts_with("__") {
                continue;
            }
            muted.insert(
                m.id.clone(),
                m.state != forge_mixer::ParticipantState::Active,
            );
            levels.push((m.id, m.energy, m.is_speaking));
        }
        let (speaker, recent) = {
            let mut sp = self.speaker.lock();
            let lv: Vec<Level<'_>> = levels
                .iter()
                .map(|(id, e, s)| Level {
                    id,
                    energy: *e,
                    speaking: *s,
                })
                .collect();
            if let Some(new) = sp.update(&lv, now) {
                debug!(room = %self.id, speaker = %new, "active speaker changed");
                let _ = self.events.send(VideoRoomEvent::ActiveSpeaker {
                    participant_id: Some(new),
                });
            }
            (sp.current().map(str::to_string), sp.recent().to_vec())
        };

        // Participant states and the frames to draw.
        self.sample_fps(now);
        let mut ordered: Vec<(u64, String)> = self
            .participants
            .iter()
            .map(|e| (e.value().joined, e.key().clone()))
            .collect();
        ordered.sort();
        let join_order: Vec<String> = ordered.into_iter().map(|(_, id)| id).collect();

        struct Tile {
            id: String,
            name: String,
            frame: Option<Arc<VideoFrame>>,
            speaking: bool,
            muted: bool,
        }
        let mut tiles: HashMap<String, Tile> = HashMap::new();
        for id in &join_order {
            let Some(p) = self.participants.get(id) else {
                continue;
            };
            let (frame, state) = match self.sources.get(id) {
                Some(_) if !p.enabled => (None, VideoState::Disabled),
                Some(s) if s.failed() => (None, VideoState::Failed),
                Some(s) => match s.frame(now, settings.freeze_timeout) {
                    Some(f) => (Some(f), VideoState::On),
                    None => (None, VideoState::Lost),
                },
                None => (None, VideoState::Off),
            };
            let name = p.name.clone();
            drop(p);
            self.set_state(id, state);
            tiles.insert(
                id.clone(),
                Tile {
                    id: id.clone(),
                    name,
                    frame,
                    speaking: speaker.as_deref() == Some(id.as_str()),
                    muted: muted.get(id).copied().unwrap_or(false),
                },
            );
        }

        let layout = self.layout();
        let control = self.control.lock().clone();
        let order = order_tiles(
            layout,
            &join_order,
            &recent,
            speaker.as_deref(),
            control.pinned.as_deref(),
            control.spotlight.as_deref(),
            |id| tiles.get(id).map(|t| t.frame.is_some()).unwrap_or(false),
            settings.max_tiles.clamp(1, 16),
        );

        // Render each output and encode each flavor.
        let pts = (now.duration_since(self.started).as_secs_f64() * 90_000.0) as u32;
        let room_fps = self.fps();
        let mut outputs = self.outputs.lock();
        for (key, out) in outputs.iter_mut() {
            if out.encoders.is_empty() {
                continue;
            }
            let sources: Vec<TileSource<'_>> = order
                .iter()
                .filter(|id| key.exclude.as_deref() != Some(id.as_str()))
                .filter_map(|id| tiles.get(id))
                .map(|t| TileSource {
                    id: &t.id,
                    name: &t.name,
                    frame: t.frame.as_deref(),
                    speaking: t.speaking,
                    muted: t.muted,
                })
                .collect();
            if let Err(e) = out.compositor.render(&sources, pts) {
                warn!(room = %self.id, error = %e, "video composition failed");
                continue;
            }
            let canvas = out.compositor.canvas().clone();
            for enc in out.encoders.values_mut() {
                if !enc.due(room_fps) {
                    continue;
                }
                let subs: Vec<Arc<Subscriber>> = self
                    .subscribers
                    .iter()
                    .filter(|s| &s.output == key && s.flavor == enc.flavor)
                    .map(|s| Arc::clone(s.value()))
                    .collect();
                if subs.is_empty() {
                    continue;
                }
                // The encoder targets the slowest receiver (§7).
                let floor = subs
                    .iter()
                    .map(|s| s.remb_kbps())
                    .filter(|k| *k > 0)
                    .min()
                    .unwrap_or(enc.flavor.max_kbps);
                enc.set_target_kbps(floor.min(enc.flavor.max_kbps));
                let codec = enc.flavor.codec;
                for frame in enc.encode(&canvas, now, &self.id) {
                    for s in &subs {
                        s.send_frame(codec, &frame, pts);
                    }
                    counter!("forge_conference_video_packets_sent_total", "room_id" => self.id.clone())
                        .increment(subs.len() as u64);
                }
            }
        }
        drop(outputs);
        histogram!("forge_conference_video_compose_duration_seconds", "room_id" => self.id.clone())
            .record(started.elapsed().as_secs_f64());
    }

    /// Once a second, turn each source's decoded-frame count into an fps.
    fn sample_fps(&self, now: Instant) {
        let mut s = self.fps_samples.lock();
        let elapsed = now.saturating_duration_since(s.0);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let secs = elapsed.as_secs_f64();
        let mut next = HashMap::new();
        for e in self.sources.iter() {
            let total = e.frames_decoded();
            let prev = s.1.get(e.key()).copied().unwrap_or(total);
            e.set_measured_fps(((total - prev) as f64 / secs).round() as u32);
            next.insert(e.key().clone(), total);
        }
        *s = (now, next);
    }

    fn set_state(&self, id: &str, state: VideoState) {
        let changed = match self.participants.get_mut(id) {
            Some(mut p) if p.state != state => {
                p.state = state;
                true
            }
            _ => false,
        };
        if changed {
            debug!(room = %self.id, participant = %id, state = state.name(), "participant video state");
            let _ = self.events.send(VideoRoomEvent::ParticipantState {
                participant_id: id.to_string(),
                state,
            });
        }
    }
}

impl Drop for VideoRoom {
    fn drop(&mut self) {
        if let Some(t) = self.clock_task.get_mut().take() {
            t.abort();
        }
    }
}

impl std::fmt::Debug for VideoRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoRoom")
            .field("id", &self.id)
            .field("participants", &self.participants.len())
            .field("sources", &self.sources.len())
            .field("subscribers", &self.subscribers.len())
            .finish()
    }
}

/// Tile order for a layout (§8): grid in join order with the speaker
/// swapped in when it would fall off the end; active speaker first then
/// the strip by recency of speech; spotlight and PiP built around the
/// spotlit (or pinned, or speaking) participant. A pinned participant
/// takes the first tile in every layout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn order_tiles(
    layout: Layout,
    join_order: &[String],
    recent: &[String],
    speaker: Option<&str>,
    pinned: Option<&str>,
    spotlight: Option<&str>,
    has_video: impl Fn(&str) -> bool,
    max_tiles: usize,
) -> Vec<String> {
    let present = |id: &str| join_order.iter().any(|j| j == id);
    let pinned = pinned.filter(|p| present(p));
    let spotlight = spotlight.filter(|p| present(p));
    let speaker = speaker.filter(|p| present(p));
    let cap = layout.capacity().min(max_tiles).max(1);

    // Everyone by recency of speech, then join order.
    let by_recency: Vec<&str> = recent
        .iter()
        .filter(|r| present(r))
        .map(String::as_str)
        .chain(
            join_order
                .iter()
                .filter(|j| !recent.contains(j))
                .map(String::as_str),
        )
        .collect();

    let mut order: Vec<&str> = match layout {
        Layout::Grid => {
            let mut v: Vec<&str> = join_order.iter().map(String::as_str).collect();
            if let Some(p) = pinned {
                v.retain(|id| *id != p);
                v.insert(0, p);
            }
            if v.len() > cap {
                if let Some(s) = speaker {
                    if let Some(pos) = v.iter().position(|id| *id == s) {
                        if pos >= cap {
                            v.swap(pos, cap - 1);
                        }
                    }
                }
                v.truncate(cap);
            }
            v
        }
        Layout::ActiveSpeaker => {
            let first = pinned.or(speaker).or(by_recency.first().copied());
            let mut v: Vec<&str> = Vec::new();
            if let Some(f) = first {
                v.push(f);
            }
            for id in &by_recency {
                if v.len() >= cap {
                    break;
                }
                if !v.contains(id) {
                    v.push(id);
                }
            }
            v
        }
        Layout::Spotlight => {
            let subject = spotlight
                .or(pinned)
                .or(speaker)
                .or(by_recency.iter().copied().find(|id| has_video(id)))
                .or(by_recency.first().copied());
            subject.into_iter().collect()
        }
        Layout::PictureInPicture => {
            let subject = spotlight
                .or(pinned)
                .or(by_recency.iter().copied().find(|id| has_video(id)))
                .or(by_recency.first().copied());
            let mut v: Vec<&str> = subject.into_iter().collect();
            let corner = speaker
                .filter(|s| Some(*s) != subject)
                .or_else(|| by_recency.iter().copied().find(|id| Some(*id) != subject));
            if let Some(c) = corner {
                v.push(c);
            }
            v
        }
    };
    order.truncate(cap);
    order.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn grid_is_join_order_with_the_speaker_swapped_in_when_over_the_cap() {
        let join = ids(&["a", "b", "c", "d", "e"]);
        let got = order_tiles(Layout::Grid, &join, &[], Some("e"), None, None, |_| true, 4);
        assert_eq!(got, ids(&["a", "b", "c", "e"]));
        let got = order_tiles(
            Layout::Grid,
            &join,
            &[],
            Some("b"),
            Some("d"),
            None,
            |_| true,
            4,
        );
        assert_eq!(got, ids(&["d", "a", "b", "c"]));
    }

    #[test]
    fn active_speaker_leads_and_the_strip_follows_recency() {
        let join = ids(&["a", "b", "c", "d"]);
        let recent = ids(&["c", "a"]);
        let got = order_tiles(
            Layout::ActiveSpeaker,
            &join,
            &recent,
            Some("c"),
            None,
            None,
            |_| true,
            16,
        );
        assert_eq!(got, ids(&["c", "a", "b", "d"]));
        // Nobody has spoken yet: join order, capped by the layout.
        let got = order_tiles(
            Layout::ActiveSpeaker,
            &join,
            &[],
            None,
            None,
            None,
            |_| true,
            2,
        );
        assert_eq!(got, ids(&["a", "b"]));
    }

    #[test]
    fn spotlight_and_pip_pick_the_subject_and_the_speaker() {
        let join = ids(&["a", "b", "c"]);
        let got = order_tiles(
            Layout::Spotlight,
            &join,
            &[],
            Some("b"),
            None,
            Some("c"),
            |_| true,
            16,
        );
        assert_eq!(got, ids(&["c"]));
        // No spotlight: the first participant with video.
        let got = order_tiles(
            Layout::Spotlight,
            &join,
            &[],
            None,
            None,
            None,
            |id| id == "b",
            16,
        );
        assert_eq!(got, ids(&["b"]));
        let got = order_tiles(
            Layout::PictureInPicture,
            &join,
            &[],
            Some("a"),
            None,
            Some("c"),
            |_| true,
            16,
        );
        assert_eq!(got, ids(&["c", "a"]));
        // The speaker is the subject: the corner shows someone else.
        let got = order_tiles(
            Layout::PictureInPicture,
            &join,
            &ids(&["c", "b"]),
            Some("c"),
            None,
            Some("c"),
            |_| true,
            16,
        );
        assert_eq!(got, ids(&["c", "b"]));
        // A departed spotlight is ignored.
        let got = order_tiles(
            Layout::Spotlight,
            &join,
            &[],
            Some("a"),
            None,
            Some("zz"),
            |_| true,
            16,
        );
        assert_eq!(got, ids(&["a"]));
    }
}
