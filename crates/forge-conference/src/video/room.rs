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
//!
//! A **recording** (design §11) is one more consumer of a flavor, but not
//! a participant: it draws no tile, sees the room whole, and takes coded
//! frames rather than packets, because a muxer wants frames and
//! packetizing one only to take it apart again is waste.
//!
//! A **shared screen** (design §15.6) is a second kind of source, keyed
//! beside the camera by [`SourceKey`]; one participant at a time holds
//! the *floor*, and every output names the [`View`] it shows — the
//! composite (with the presentation arrangement while a screen is
//! shared), the cameras alone, or the content alone.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use forge_core::VideoCodec;
use forge_rtp::rtcp::{PayloadFeedback, RtcpPacket, TransportFeedback};
use forge_rtp::{CodedFrame, RtpPacket};
use forge_video::codec::{CodecRegistry, EncoderSettings};
use forge_video::compose::{Compositor, HostCompositor, TileKind, TileSource};
use forge_video::flavor::Flavor;
use forge_video::frame::{MediaDevice, Resolution, VideoFrame};
use forge_video::ladder::{Ladder, LadderPolicy, Rung};
use forge_video::layout::Layout;
use forge_video::{ClockEvent, VideoClock};
use metrics::{counter, gauge, histogram};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use super::content::{
    ContentEvent, ContentFloor, ContentGate, ContentInfo, ContentLayout, ContentRefusal,
    ContentStop, SourceKey, StreamKind, View,
};
use super::egress::{
    default_kbps, FlavorEncoder, OutputKey, OutputScope, Subscriber, VideoSubscription,
};
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

/// When the passthrough fast path (§5.6) applies.
///
/// A forwarded frame is the sender's own picture, so it carries no name
/// banner, no speaking border and no avatar — nothing composited it.
/// That is right for a presenter at full canvas and wrong for a grid
/// where the labels are how you tell people apart, which is why `Auto`
/// is the default rather than `On`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PassthroughMode {
    /// Always compose. Every subscriber gets labels and borders.
    Off,
    /// Forward only while the room is showing one participant full
    /// canvas (`spotlight`), where a label is least missed.
    #[default]
    Auto,
    /// Forward wherever a single source allows it, labels or not.
    On,
}

impl PassthroughMode {
    /// Whether forwarding may be considered with this layout.
    pub fn allows(&self, layout: Layout) -> bool {
        match self {
            PassthroughMode::Off => false,
            PassthroughMode::Auto => layout == Layout::Spotlight,
            PassthroughMode::On => true,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PassthroughMode::Off => "off",
            PassthroughMode::Auto => "auto",
            PassthroughMode::On => "on",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Some(PassthroughMode::Off),
            "auto" => Some(PassthroughMode::Auto),
            "on" => Some(PassthroughMode::On),
            _ => None,
        }
    }
}

/// A room's video settings (the meeting's `video_*` fields plus tunables).
#[derive(Debug, Clone, PartialEq)]
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
    /// When a subscriber whose view is a single source it can already
    /// decode is served that source's own packets rather than a
    /// composite (§5.6).
    pub passthrough: PassthroughMode,
    /// Move a subscriber down the bitrate ladder when its link cannot
    /// carry its rung, and back up when it can again (§7). Off leaves
    /// every subscriber where it subscribed, and one poor link drags
    /// its flavor's encoder down for everyone sharing it.
    pub ladder: bool,
    /// How the ladder reads an estimate.
    pub ladder_policy: LadderPolicy,
    /// How long a link must ask before it is moved down, and up. Down is
    /// the shorter: a picture nobody can decode is worse than a small one.
    pub ladder_down_after: Duration,
    pub ladder_up_after: Duration,
    pub limits: SourceLimits,
    /// Whether anyone may share a screen (§15.6). Off, every content
    /// packet is refused.
    pub content: bool,
    /// How the composite shows a shared screen.
    pub content_layout: ContentLayout,
    /// A presenter that has sent nothing for this long gives up the
    /// floor. Judged on packets, not frames: a still slide is not idle.
    pub content_idle: Duration,
    /// The most a shared screen is decoded and sent at, whatever the
    /// meeting's canvas: a bigger capture is shrunk to this, and the
    /// content channel is capped here rather than at `resolution`.
    pub content_max_resolution: Resolution,
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
            passthrough: PassthroughMode::Off,
            ladder: true,
            ladder_policy: LadderPolicy::default(),
            ladder_down_after: Duration::from_secs(5),
            ladder_up_after: Duration::from_secs(15),
            limits: SourceLimits::default(),
            content: true,
            content_layout: ContentLayout::Presentation,
            content_idle: Duration::from_secs(30),
            content_max_resolution: Resolution::new(1920, 1080),
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

/// Lets the video clock ask the room to give up an output's rung before it
/// halves everybody's frame rate. The room's own state is behind
/// `DashMap`s and a `Mutex`, so a shared reference is all this needs.
struct RoomShedder<'a> {
    room: &'a VideoRoom,
}

impl forge_video::LoadShedder for RoomShedder<'_> {
    fn shed(&mut self) -> bool {
        self.room.shed_one()
    }

    fn restore(&mut self) -> bool {
        self.room.restore_one()
    }
}

/// Where a subscriber actually goes, given the rung its link wants, the
/// resolution it is on now, and the ceiling shedding has put on its scope.
///
/// A subscriber whose link has room may still be held down by a shed: the
/// ceiling is about this node's CPU, not about the link, and climbing
/// through it would undo the shedding on the very next tick. Going *down*
/// is never clamped — a link that cannot carry the ceiling is a separate
/// problem from the room overrunning, and the smaller picture is right for
/// both. `None` when the subscriber should not move at all.
fn clamp_to_cap(wants: Rung, at: Resolution, cap: Rung) -> Option<Rung> {
    if wants.resolution.height <= cap.resolution.height {
        return Some(wants);
    }
    // It wants to climb above the ceiling. Let it as far as the ceiling,
    // if it is below that; otherwise it stays where it is.
    (at.height < cap.resolution.height).then_some(cap)
}

/// What a shed ceiling is keyed by: the part of an [`OutputKey`] an output
/// keeps across a shed. Dropping a rung changes its resolution and so its
/// key; its scope and view are what stay.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ShedKey {
    scope: OutputScope,
    view: View,
}

impl ShedKey {
    fn of(output: &OutputKey) -> Self {
        Self {
            scope: output.scope.clone(),
            view: output.view,
        }
    }
}

/// Which output gives up a rung next, given each live output, the rung it
/// is held at today and the one below it on its own ladder: the largest,
/// and the key's own order to settle a tie so that two runs of the same
/// room shed the same way. The content channel goes last (§15.6): only
/// when nothing else can give up a rung is a shared screen made smaller.
/// `None` when every output is already at the bottom.
///
/// Pure, because the rule is the interesting part and a `VideoRoom` needs
/// a codec pool and an audio room to exist.
fn next_to_shed(held: &[(ShedKey, Rung, Option<Rung>)]) -> Option<(ShedKey, Rung, Rung)> {
    let pick = |content: bool| {
        held.iter()
            .filter(|(key, _, _)| (key.view == View::Content) == content)
            .filter_map(|(key, at, below)| below.map(|to| (key.clone(), *at, to)))
            .max_by(|(a_key, a_at, _), (b_key, b_at, _)| {
                (a_at.resolution.height, a_key).cmp(&(b_at.resolution.height, b_key))
            })
    };
    pick(false).or_else(|| pick(true))
}

/// Which output gets a rung back next, given each cap and the rung above
/// it: the one furthest below the top, since it has the most to gain.
/// `None` when nothing is shed. A cap that is already at the top rung is
/// stale and is reported as such by the rung above being `None`, so the
/// caller can drop it.
fn next_to_restore(
    caps: &[(ShedKey, Rung, Option<Rung>)],
) -> Option<(ShedKey, Rung, Option<Rung>)> {
    caps.iter()
        .min_by(|(a_key, a_at, _), (b_key, b_at, _)| {
            (a_at.resolution.height, a_key).cmp(&(b_at.resolution.height, b_key))
        })
        .cloned()
}

/// Name a scope for an event or a log line: `all`, `excluding:<id>` or
/// `local-only`. Chosen over `Debug` because these strings reach the
/// conference server's JSON and its events, where they are read by people.
fn scope_label(scope: &OutputScope) -> String {
    match scope {
        OutputScope::All => "all".to_string(),
        OutputScope::Excluding(id) => format!("excluding:{id}"),
        OutputScope::LocalOnly => "local-only".to_string(),
    }
}

/// Name a shed output: the scope, and the view after a colon when it is
/// not the composite — `all`, `all:cameras`, `local-only:content`.
fn shed_label(key: &ShedKey) -> String {
    match key.view {
        View::Composite => scope_label(&key.scope),
        view => format!("{}:{}", scope_label(&key.scope), view.name()),
    }
}

/// What the room tells the server about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoRoomEvent {
    /// The clock changed rate; `overload` when it was halved.
    FpsChanged { from: u32, to: u32, overload: bool },
    /// A participant's video state changed, for their camera or their
    /// shared screen.
    ParticipantState {
        participant_id: String,
        kind: StreamKind,
        state: VideoState,
    },
    /// A share started, stopped or was refused (§15.6).
    Content {
        participant_id: String,
        event: ContentEvent,
    },
    /// The active speaker changed (`None` when the speaker left).
    ActiveSpeaker {
        /// The person speaking, whichever node they are on.
        participant_id: Option<String>,
        /// The remote tile they are behind, when they are on a peer node
        /// ([`RemoteSpeaker`]); `None` for a caller on this one.
        via: Option<String>,
    },
    /// An output was dropped a rung because the room could not keep up
    /// (§9). Shedding happens before the frame rate is touched, so this
    /// event says a picture got smaller while everyone's motion was left
    /// alone; `FpsChanged { overload: true }` says shedding ran out.
    Shed {
        /// Which output: `all`, `excluding:<id>` or `local-only`, with
        /// `:cameras` or `:content` after it for a view other than the
        /// composite.
        output: String,
        from: Resolution,
        to: Resolution,
    },
    /// Layout, pin, spotlight or content layout changed.
    LayoutChanged {
        layout: Layout,
        pinned: Option<String>,
        spotlight: Option<String>,
        content_layout: ContentLayout,
    },
}

#[derive(Debug, Clone)]
struct Participant {
    name: String,
    /// Host control (§8): `false` hides the participant's camera.
    enabled: bool,
    /// Host control (§15.6): `false` keeps the participant off the floor.
    content_enabled: bool,
    state: VideoState,
    content_state: VideoState,
    joined: u64,
    /// A peer node's tile rather than a caller on this one: it draws a
    /// tile and decodes like any source, but it is not in the audio
    /// room and a trunk's own composite leaves it out
    /// ([`OutputScope::LocalOnly`]).
    remote: bool,
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
    /// Whose tiles to compose. `None` is the room's own rule: the
    /// subscriber's own tile left out when `exclude_self` is set, every
    /// tile otherwise. A trunk asks for [`OutputScope::LocalOnly`].
    pub scope: Option<OutputScope>,
    /// What to show. `None` is the channel's own default: the composite
    /// on a main section, the content alone on a content section.
    pub view: Option<View>,
}

/// What a recording asks the room for (§11). Unlike a subscription this
/// is not a participant: it draws no tile and its composite leaves
/// nobody out, whatever `exclude_self` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRequest {
    pub codec: VideoCodec,
    /// Wanted resolution; the room's is the cap and the default.
    pub resolution: Option<Resolution>,
    /// Wanted frame rate; the room's is the cap and the default.
    pub fps: Option<u32>,
    /// Bitrate cap; the ladder's default for the resolution otherwise.
    pub max_kbps: Option<u32>,
    /// Frames buffered before the recorder is taken to be too slow and
    /// frames are dropped.
    pub queue: usize,
    /// What to record: the composite, which follows a share into the
    /// presentation arrangement and back, unless told otherwise.
    pub view: View,
}

impl RecordRequest {
    /// A recording of `codec` at the room's own resolution and rate.
    pub fn new(codec: VideoCodec) -> Self {
        Self {
            codec,
            resolution: None,
            fps: None,
            max_kbps: None,
            queue: 120,
            view: View::Composite,
        }
    }
}

/// One coded frame of the composite, on its way to a recording.
#[derive(Debug, Clone)]
pub struct RecordedFrame {
    /// The frame, timestamped in the room's 90 kHz clock since it
    /// started ([`VideoRoom::started_at`]).
    pub frame: CodedFrame,
    /// When the canvas behind it was composed, for lining the video up
    /// with the audio the room was mixing at that moment.
    pub at: Instant,
}

/// What a peer node says is speaking behind its remote tile (§15.4, 5c).
///
/// A remote tile's own energy is a whole node's mix, which says nothing
/// about who is talking; the node that has the caller says instead. Every
/// node runs the same rule over its local levels and its peers' claims,
/// so they converge on one speaker within a take interval.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSpeaker {
    /// The node making the claim; it settles ties.
    pub node: String,
    /// The participant on that node.
    pub participant_id: String,
    /// What to write on the tile while they hold it.
    pub display_name: String,
    /// Their level, on the same scale every mixer produces.
    pub energy: f32,
    pub speaking: bool,
}

/// A recording's stream of coded composite frames. Dropping it stops the
/// recording at the next tick, as [`VideoRoom::stop_record`] does.
#[derive(Debug)]
pub struct RecordingSink {
    pub id: String,
    /// What the frames are: codec, resolution, rate and bitrate cap.
    pub flavor: Flavor,
    pub frames: mpsc::Receiver<RecordedFrame>,
}

/// A recording as the room holds it.
struct RecorderTap {
    output: OutputKey,
    flavor: Flavor,
    frames: mpsc::Sender<RecordedFrame>,
    sent: AtomicU64,
    dropped: AtomicU64,
}

impl RecorderTap {
    /// Hand the recorder a frame. `false` means the recording is gone —
    /// its sink was dropped — and the room should let it go.
    fn send(&self, room: &str, id: &str, frame: &CodedFrame, at: Instant) -> bool {
        match self.frames.try_send(RecordedFrame {
            frame: frame.clone(),
            at,
        }) {
            Ok(()) => {
                self.sent.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_multiple_of(100) {
                    warn!(room = %room, recording = %id, dropped = n, "recording is behind; video frames dropped");
                }
                counter!("forge_conference_video_recording_frames_dropped_total", "room_id" => room.to_string())
                    .increment(1);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// What a recording is taking from the room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRecordingInfo {
    pub id: String,
    pub flavor: Flavor,
    pub frames_sent: u64,
    pub frames_dropped: u64,
}

/// One participant's video, for the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoParticipantInfo {
    pub participant_id: String,
    /// The camera.
    pub state: VideoState,
    /// The shared screen (`Off` when the participant has no content
    /// section).
    pub content_state: VideoState,
    pub display_name: String,
    pub speaking: bool,
    /// Whether this participant holds the floor.
    pub presenting: bool,
    /// A peer node's tile, standing for the callers on that node,
    /// rather than a caller on this one.
    pub remote: bool,
    /// The camera's ingress side, when the participant sends video.
    pub source: Option<VideoSourceInfo>,
    /// The shared screen's ingress side, when the participant has a
    /// content section.
    pub content: Option<VideoSourceInfo>,
    /// The main section's egress side, when the participant receives
    /// video.
    pub subscription: Option<VideoSubscriberInfo>,
    /// The content section's egress side, when the participant has one.
    pub content_subscription: Option<VideoSubscriberInfo>,
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
    /// What the channel shows.
    pub view: View,
    pub ssrc: u32,
    /// The stream whose own packets this subscriber is being sent, when
    /// its view is a single source it can already decode (§5.6); `None`
    /// when it is watching a composite.
    pub forwarding: Option<SourceKey>,
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
    /// Whose tiles this composite is drawn from.
    pub scope: OutputScope,
    /// What it shows.
    pub view: View,
    pub resolution: Resolution,
    pub flavors: Vec<VideoFlavorInfo>,
}

/// The room's video, for the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRoomStatus {
    pub layout: Layout,
    /// How the composite shows a shared screen.
    pub content_layout: ContentLayout,
    pub fps: u32,
    pub target_fps: u32,
    pub resolution: Resolution,
    pub pinned: Option<String>,
    pub spotlight: Option<String>,
    pub active_speaker: Option<String>,
    /// The remote tile the speaker is behind, when they are on a peer
    /// node rather than here.
    pub active_speaker_via: Option<String>,
    /// The share under way, if any.
    pub content: Option<ContentInfo>,
    /// Sources decoding, cameras and shared screens both.
    pub sources: usize,
    pub encoders: usize,
    pub ticks: u64,
    pub overruns: u64,
    pub outputs: Vec<VideoOutputInfo>,
    /// Recordings taking the composite (§11).
    pub recordings: Vec<VideoRecordingInfo>,
}

pub struct VideoRoom {
    id: String,
    settings: RwLock<VideoRoomSettings>,
    backend: VideoBackend,
    audio: Weak<ConferenceRoom>,
    participants: DashMap<String, Participant>,
    join_seq: AtomicU64,
    /// Every stream being decoded, cameras and shared screens both.
    sources: DashMap<SourceKey, Arc<VideoSource>>,
    /// Every channel being sent: a participant's main section, and their
    /// content section when they have one.
    subscribers: DashMap<SourceKey, Arc<Subscriber>>,
    /// Who is sharing a screen (§15.6).
    floor: Mutex<Option<ContentFloor>>,
    /// Participants told their share was refused, so they are told once.
    refused_told: Mutex<HashSet<String>>,
    /// Whether the node can afford another share.
    content_gate: RwLock<Option<Arc<dyn ContentGate>>>,
    /// Recordings: consumers of a flavor that are not participants.
    recorders: DashMap<String, Arc<RecorderTap>>,
    outputs: Mutex<HashMap<OutputKey, Output>>,
    speaker: Mutex<ActiveSpeaker>,
    /// What each peer says is speaking behind its remote tile, by tile id.
    remote_speakers: DashMap<String, RemoteSpeaker>,
    /// Subscribers on the passthrough path, so ingress can skip the scan
    /// entirely in the ordinary case where nobody is.
    forwarders: AtomicU32,
    /// This node's name, which settles a tie against a peer's claim.
    local_node: Mutex<String>,
    control: Mutex<Control>,
    /// While the room is overrunning, the rung an output may not go
    /// above. Absent means the ladder's top, which is to say nothing is
    /// shed. Keyed by scope and view rather than by `OutputKey`, because
    /// dropping a rung changes an output's resolution and so its key:
    /// those two are what an output keeps across a shed.
    shed_caps: DashMap<ShedKey, Rung>,
    /// Current and wanted clock rate; the clock task applies changes.
    fps: AtomicU32,
    target_fps: AtomicU32,
    events: broadcast::Sender<VideoRoomEvent>,
    clock_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    started: Instant,
    ticks: AtomicU64,
    overruns: AtomicU64,
    /// Per-source decoded-frame counts at the last fps sample.
    fps_samples: Mutex<(Instant, HashMap<SourceKey, u64>)>,
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
            floor: Mutex::new(None),
            refused_told: Mutex::new(HashSet::new()),
            content_gate: RwLock::new(None),
            recorders: DashMap::new(),
            outputs: Mutex::new(HashMap::new()),
            shed_caps: DashMap::new(),
            speaker: Mutex::new(ActiveSpeaker::default()),
            remote_speakers: DashMap::new(),
            forwarders: AtomicU32::new(0),
            local_node: Mutex::new(String::new()),
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

    /// When the room's clock started. Frame timestamps are 90 kHz from
    /// this instant, which is how a recording lines video up with audio.
    pub fn started_at(&self) -> Instant {
        self.started
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
        self.recorders.clear();
        self.outputs.lock().clear();
        if self.floor.lock().take().is_some() {
            gauge!("forge_conference_video_content_rooms").decrement(1.0);
        }
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
                content_enabled: true,
                state: VideoState::Off,
                content_state: VideoState::Off,
                joined: self.join_seq.fetch_add(1, Ordering::Relaxed),
                remote: false,
            });
    }

    /// The participant left: sources, subscriptions, tile and the floor
    /// go with them.
    pub fn participant_left(&self, id: &str) {
        self.participants.remove(id);
        if self.holder_is(id) {
            self.release_floor(ContentStop::Left);
        }
        self.remove_source_kind(id, StreamKind::Camera);
        self.remove_source_kind(id, StreamKind::Content);
        self.unsubscribe_kind(id, StreamKind::Camera);
        self.unsubscribe_kind(id, StreamKind::Content);
        self.refused_told.lock().remove(id);
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
                content_layout: self.settings.read().content_layout,
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

    /// Host control: hide or show a participant's camera.
    pub fn set_participant_video_enabled(&self, id: &str, enabled: bool) {
        if let Some(mut p) = self.participants.get_mut(id) {
            p.enabled = enabled;
        }
        if enabled {
            if let Some(s) = self.sources.get(&SourceKey::camera(id)) {
                s.request_keyframe();
            }
        }
    }

    /// Host control over a share: `false` ends the participant's share
    /// if they hold the floor and keeps them from taking it again — a
    /// browser that keeps sending would otherwise have it straight back
    /// — until `true` lets them.
    pub fn set_participant_content_enabled(&self, id: &str, enabled: bool) {
        if let Some(mut p) = self.participants.get_mut(id) {
            p.content_enabled = enabled;
        }
        if enabled {
            self.refused_told.lock().remove(id);
        } else if self.holder_is(id) {
            self.release_floor(ContentStop::Host);
        }
    }

    // ---- ingress ----------------------------------------------------------

    /// A peer node's composite arriving over a trunk: one tile, drawn
    /// like any other, that is not a participant of this room.
    ///
    /// The tile carries what the peer is showing — its own callers,
    /// composed there — so a composite this node sends back to a peer
    /// must leave it out ([`OutputScope::LocalOnly`]). It is not in the
    /// audio room either, so it is never the active speaker on its own
    /// energy; the peer says who is speaking behind it.
    pub fn add_remote(&self, id: &str, codec: VideoCodec, profile: &str) -> Result<()> {
        self.add_remote_kind(id, StreamKind::Camera, codec, profile)
    }

    /// A peer node's shared screen arriving over a trunk (§15.6, 7d): a
    /// content source under the remote tile's id, which takes the floor
    /// here as a caller's would. A composite sent back down a trunk
    /// leaves it out, as it does the tile.
    pub fn add_remote_content(&self, id: &str, codec: VideoCodec, profile: &str) -> Result<()> {
        self.add_remote_kind(id, StreamKind::Content, codec, profile)
    }

    fn add_remote_kind(
        &self,
        id: &str,
        kind: StreamKind,
        codec: VideoCodec,
        profile: &str,
    ) -> Result<()> {
        self.participants
            .entry(id.to_string())
            .or_insert_with(|| Participant {
                name: id.to_string(),
                enabled: true,
                content_enabled: true,
                state: VideoState::Off,
                content_state: VideoState::Off,
                joined: self.join_seq.fetch_add(1, Ordering::Relaxed),
                remote: true,
            })
            .remote = true;
        let out = self.add_source_kind(id, kind, codec, profile);
        match &out {
            Err(_) => {
                // A tile with nothing behind it is not a tile.
                if !self.has_source(id) && !self.has_source_kind(id, StreamKind::Content) {
                    self.participants.remove(id);
                }
            }
            Ok(()) => {
                debug!(room = %self.id, remote = %id, %kind, %codec, "remote video source added")
            }
        }
        out
    }

    /// The trunk went: the tile, its decoders and the peer's claim about
    /// who is speaking behind it go with it.
    pub fn remove_remote(&self, id: &str) {
        if self.participants.get(id).is_some_and(|p| p.remote) {
            self.remote_speakers.remove(id);
            self.participant_left(id);
            debug!(room = %self.id, remote = %id, "remote video tile removed");
        }
    }

    /// The peer stopped carrying content; its tile stays.
    pub fn remove_remote_content(&self, id: &str) {
        if self.participants.get(id).is_some_and(|p| p.remote) {
            self.remove_source_kind(id, StreamKind::Content);
        }
    }

    /// Whether the id names a peer node's tile rather than a caller here.
    pub fn is_remote(&self, id: &str) -> bool {
        self.participants.get(id).is_some_and(|p| p.remote)
    }

    /// Name this node, so a tie between its caller and a peer's claim is
    /// settled the same way here and there (§15.4, 5c). Unset, the room
    /// still works; ties then fall to the id.
    pub fn set_local_node(&self, node: &str) {
        *self.local_node.lock() = node.to_string();
    }

    /// What a peer says is speaking behind its remote tile, or `None` to
    /// withdraw the claim. Fed to the same election as this node's own
    /// levels, so every node reaches the same speaker.
    pub fn set_remote_speaker(&self, remote_id: &str, speaker: Option<RemoteSpeaker>) {
        match speaker {
            Some(s) => {
                self.remote_speakers.insert(remote_id.to_string(), s);
            }
            None => {
                self.remote_speakers.remove(remote_id);
            }
        }
    }

    /// The claim standing against a remote tile, if any.
    pub fn remote_speaker(&self, remote_id: &str) -> Option<RemoteSpeaker> {
        self.remote_speakers.get(remote_id).map(|s| s.clone())
    }

    /// The participant sends `codec` with `profile` (its `a=fmtp`, `""`
    /// when the codec has none) from their camera; start decoding it.
    /// The profile is what decides whether the stream can be forwarded
    /// to someone else untouched (§5.6).
    pub fn add_source(&self, id: &str, codec: VideoCodec, profile: &str) -> Result<()> {
        self.add_source_kind(id, StreamKind::Camera, codec, profile)
    }

    /// [`add_source`](Self::add_source) for either of a participant's
    /// streams. A content source is decoded but not shown until its
    /// packets take the floor (§15.6); its cap is the room's
    /// `content_max_resolution`, and a larger screen is shrunk rather
    /// than dropped. Adding a kind the participant already sends
    /// replaces that decoder.
    pub fn add_source_kind(
        &self,
        id: &str,
        kind: StreamKind,
        codec: VideoCodec,
        profile: &str,
    ) -> Result<()> {
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
        match kind {
            // The room's canvas is the most a camera can usefully send.
            StreamKind::Camera => {
                limits.max_resolution = Resolution::new(
                    limits.max_resolution.width.max(settings.resolution.width),
                    limits.max_resolution.height.max(settings.resolution.height),
                );
            }
            // A screen is capped on its own terms, not the meeting's.
            StreamKind::Content => limits.max_resolution = settings.content_max_resolution,
        }
        let source = Arc::new(VideoSource::new(
            id,
            &self.id,
            kind,
            codec,
            profile,
            decoder,
            limits,
            rand::random(),
        ));
        if self
            .sources
            .insert(SourceKey::new(id, kind), source)
            .is_none()
        {
            gauge!("forge_conference_video_sources").increment(1.0);
        }
        debug!(room = %self.id, participant = %id, %kind, %codec, "video source added");
        Ok(())
    }

    /// The participant stopped sending from their camera (re-INVITE
    /// with port 0).
    pub fn remove_source(&self, id: &str) {
        self.remove_source_kind(id, StreamKind::Camera)
    }

    /// The participant stopped sending one of their streams. Taking the
    /// content source away ends a share it was carrying and clears a
    /// host's stop, so a fresh share starts clean.
    pub fn remove_source_kind(&self, id: &str, kind: StreamKind) {
        if self.sources.remove(&SourceKey::new(id, kind)).is_some() {
            gauge!("forge_conference_video_sources").decrement(1.0);
            if kind == StreamKind::Content {
                if self.holder_is(id) {
                    self.release_floor(ContentStop::Ended);
                }
                if let Some(mut p) = self.participants.get_mut(id) {
                    p.content_enabled = true;
                }
                self.refused_told.lock().remove(id);
            }
            self.set_state_kind(id, kind, VideoState::Off);
        }
    }

    pub fn has_source(&self, id: &str) -> bool {
        self.has_source_kind(id, StreamKind::Camera)
    }

    pub fn has_source_kind(&self, id: &str, kind: StreamKind) -> bool {
        self.sources.contains_key(&SourceKey::new(id, kind))
    }

    /// Feed one RTP packet from a participant's camera. Returns the RTCP
    /// feedback (NACK, PLI) to send back to the sender.
    pub fn push_rtp(&self, id: &str, packet: RtpPacket) -> Vec<RtcpPacket> {
        self.push_rtp_kind(id, StreamKind::Camera, packet)
    }

    /// Feed one RTP packet from either of a participant's streams. A
    /// content packet is the participant asking for the floor: the first
    /// one takes it when it is free (§15.6), and while someone else
    /// holds it the packets are dropped and the sender told once.
    pub fn push_rtp_kind(&self, id: &str, kind: StreamKind, packet: RtpPacket) -> Vec<RtcpPacket> {
        let key = SourceKey::new(id, kind);
        if kind == StreamKind::Content && !self.content_admits(id) {
            counter!("forge_conference_video_content_packets_refused_total", "room_id" => self.id.clone())
                .increment(1);
            return Vec::new();
        }
        // Anyone on the passthrough fast path is served here, before the
        // decoder and off the compose clock: forwarding a packet is the
        // whole point, and a tick of latency would undo it (§5.6).
        if self.forwarders.load(Ordering::Relaxed) > 0 {
            for sub in self.subscribers.iter() {
                if sub.forwards_from(&key) {
                    sub.send_forwarded(&packet);
                }
            }
        }
        match self.sources.get(&key) {
            Some(s) => s.push(packet, &self.backend.pool, Instant::now()),
            None => Vec::new(),
        }
    }

    /// The camera's decoder needs a keyframe (a new subscriber, a host
    /// re-enabled it); the next packet carries the request.
    pub fn request_source_keyframe(&self, id: &str) {
        self.request_source_keyframe_kind(id, StreamKind::Camera)
    }

    pub fn request_source_keyframe_kind(&self, id: &str, kind: StreamKind) {
        if let Some(s) = self.sources.get(&SourceKey::new(id, kind)) {
            s.request_keyframe();
        }
    }

    // ---- the floor (§15.6) -------------------------------------------------

    /// Who is sharing, if anyone.
    pub fn content_holder(&self) -> Option<String> {
        self.floor.lock().as_ref().map(|f| f.participant_id.clone())
    }

    fn holder_is(&self, id: &str) -> bool {
        self.floor
            .lock()
            .as_ref()
            .is_some_and(|f| f.participant_id == id)
    }

    /// Whether the node can afford a share; asked before every grant.
    pub fn set_content_gate(&self, gate: Option<Arc<dyn ContentGate>>) {
        *self.content_gate.write() = gate;
    }

    /// Ask for the floor on a participant's behalf — what a BFCP floor
    /// request comes to (7e), and what a content packet does on its own.
    /// Holding it already is not an error.
    pub fn request_content(&self, id: &str) -> std::result::Result<(), ContentRefusal> {
        self.take_floor(id)
    }

    fn take_floor(&self, id: &str) -> std::result::Result<(), ContentRefusal> {
        if !self.settings.read().content {
            return Err(ContentRefusal::Disabled);
        }
        let (enabled, remote) = match self.participants.get(id) {
            Some(p) => (p.content_enabled, p.remote),
            None => return Err(ContentRefusal::NoSource),
        };
        if !enabled {
            return Err(ContentRefusal::ParticipantDisabled);
        }
        if !self.sources.contains_key(&SourceKey::content(id)) {
            return Err(ContentRefusal::NoSource);
        }
        {
            let mut floor = self.floor.lock();
            if let Some(f) = floor.as_ref() {
                if f.participant_id == id {
                    return Ok(());
                }
                return Err(ContentRefusal::Held(f.participant_id.clone()));
            }
            // Under the lock, so two first packets cannot both be granted.
            let gate = self.content_gate.read().clone();
            if let Some(g) = gate {
                if !g.admit(&self.id, id) {
                    return Err(ContentRefusal::Budget);
                }
            }
            *floor = Some(ContentFloor {
                participant_id: id.to_string(),
                since: Instant::now(),
                remote,
            });
        }
        self.refused_told.lock().clear();
        counter!("forge_conference_video_content_started_total", "room_id" => self.id.clone())
            .increment(1);
        gauge!("forge_conference_video_content_rooms").increment(1.0);
        info!(room = %self.id, presenter = %id, remote, "screen share started");
        // Every composite changes shape: start the next one on a keyframe.
        self.request_keyframes_for_content_change();
        let _ = self.events.send(VideoRoomEvent::Content {
            participant_id: id.to_string(),
            event: ContentEvent::Started,
        });
        Ok(())
    }

    /// Give the floor up, whoever holds it. Returns who did.
    fn release_floor(&self, reason: ContentStop) -> Option<String> {
        let f = self.floor.lock().take()?;
        self.refused_told.lock().clear();
        counter!("forge_conference_video_content_stopped_total", "room_id" => self.id.clone(), "reason" => reason.name())
            .increment(1);
        gauge!("forge_conference_video_content_rooms").decrement(1.0);
        info!(room = %self.id, presenter = %f.participant_id, %reason, "screen share stopped");
        self.request_keyframes_for_content_change();
        let _ = self.events.send(VideoRoomEvent::Content {
            participant_id: f.participant_id.clone(),
            event: ContentEvent::Stopped(reason),
        });
        Some(f.participant_id)
    }

    /// The participant stops sharing (a BFCP release, a track that
    /// ended). `false` when they did not hold the floor.
    pub fn release_content(&self, id: &str) -> bool {
        self.holder_is(id) && self.release_floor(ContentStop::Ended).is_some()
    }

    /// A host ends the share. The presenter cannot take the floor back
    /// by just sending until [`set_participant_content_enabled`] lets
    /// them, or their content source is renegotiated. Returns who was
    /// stopped.
    ///
    /// [`set_participant_content_enabled`]: Self::set_participant_content_enabled
    pub fn stop_content(&self) -> Option<String> {
        let holder = self.content_holder()?;
        if let Some(mut p) = self.participants.get_mut(&holder) {
            p.content_enabled = false;
        }
        self.release_floor(ContentStop::Host)
    }

    /// Whether a content packet from `id` goes in: they hold the floor,
    /// or it is free and they take it. A refusal is reported once per
    /// participant per floor, not per packet.
    fn content_admits(&self, id: &str) -> bool {
        if self.holder_is(id) {
            return true;
        }
        match self.take_floor(id) {
            Ok(()) => true,
            Err(reason) => {
                if self.refused_told.lock().insert(id.to_string()) {
                    counter!("forge_conference_video_content_refused_total", "room_id" => self.id.clone(), "reason" => reason.name())
                        .increment(1);
                    debug!(room = %self.id, participant = %id, %reason, "screen share refused");
                    let _ = self.events.send(VideoRoomEvent::Content {
                        participant_id: id.to_string(),
                        event: ContentEvent::Refused(reason),
                    });
                }
                false
            }
        }
    }

    /// Once a tick: a holder whose source is gone, has failed, or has
    /// sent nothing for the idle timeout loses the floor. Idle is judged
    /// on packets, not frames — a slide that has not changed is still
    /// being shared, and a sender that has stopped sending is not.
    fn expire_content(&self, now: Instant, settings: &VideoRoomSettings) {
        let Some(holder) = self.content_holder() else {
            return;
        };
        let reason = match self.sources.get(&SourceKey::content(&holder)) {
            None => Some(ContentStop::Ended),
            Some(s) if s.failed() => Some(ContentStop::Failed),
            Some(s) => match s.last_packet_age(now) {
                Some(age) if age > settings.content_idle => Some(ContentStop::Idle),
                _ => None,
            },
        };
        if let Some(reason) = reason {
            self.release_floor(reason);
        }
    }

    /// Ask every encoder that shows the content — or showed it a moment
    /// ago — for a keyframe: the picture just changed shape.
    fn request_keyframes_for_content_change(&self) {
        for (key, out) in self.outputs.lock().iter() {
            if key.view == View::Cameras {
                continue;
            }
            for enc in out.encoders.values() {
                enc.wants_keyframe.store(true, Ordering::Release);
            }
        }
    }

    /// The share under way, for the API.
    pub fn content(&self) -> Option<ContentInfo> {
        let f = self.floor.lock().clone()?;
        let source = self.sources.get(&SourceKey::content(&f.participant_id));
        let (codec, resolution, fps, live) = match source.as_deref() {
            Some(s) => (
                s.codec(),
                s.measured_resolution(),
                s.stats.fps.load(Ordering::Relaxed),
                s.has_frame(),
            ),
            None => (VideoCodec::H264, Resolution::new(0, 0), 0, false),
        };
        let display_name = self
            .participants
            .get(&f.participant_id)
            .map(|p| self.tile_name(&f.participant_id, &p))
            .unwrap_or_else(|| f.participant_id.clone());
        Some(ContentInfo {
            participant_id: f.participant_id,
            display_name,
            remote: f.remote,
            held_for: f.since.elapsed(),
            codec,
            resolution,
            fps,
            live,
        })
    }

    pub fn content_layout(&self) -> ContentLayout {
        self.settings.read().content_layout
    }

    /// How the composite shows a share, changeable while one is on.
    pub fn set_content_layout(&self, layout: ContentLayout) {
        self.settings.write().content_layout = layout;
        self.request_keyframes_for_content_change();
        self.emit_layout();
    }

    // ---- egress -----------------------------------------------------------

    /// The participant receives the room on their main video section,
    /// in the given flavor. Replaces any existing main subscription.
    pub fn subscribe(&self, id: &str, req: SubscribeRequest) -> Result<VideoSubscription> {
        self.subscribe_kind(id, StreamKind::Camera, req)
    }

    /// [`subscribe`](Self::subscribe) for either of a participant's
    /// channels. A content channel (`StreamKind::Content`) shows
    /// [`View::Content`] unless the request says otherwise, is capped at
    /// the room's `content_max_resolution` rather than its canvas, and
    /// is never given its owner's own share back.
    pub fn subscribe_kind(
        &self,
        id: &str,
        kind: StreamKind,
        req: SubscribeRequest,
    ) -> Result<VideoSubscription> {
        if !self.participants.contains_key(id) {
            return Err(ConferenceError::Internal(format!(
                "participant {id} is not in room {}",
                self.id
            )));
        }
        let settings = self.settings.read().clone();
        let view = req.view.unwrap_or(match kind {
            StreamKind::Camera => View::Composite,
            StreamKind::Content => View::Content,
        });
        let cap = if view == View::Content {
            settings.content_max_resolution
        } else {
            settings.resolution
        };
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
        let scope = req.scope.clone().unwrap_or_else(|| {
            // `exclude_self` is about the participant's own tile; a
            // content channel has none in it.
            if settings.exclude_self && view != View::Content {
                OutputScope::Excluding(id.to_string())
            } else {
                OutputScope::All
            }
        });
        let output = OutputKey {
            scope,
            resolution: res,
            view,
        };

        self.unsubscribe_kind(id, kind);

        let wants_keyframe = self.ensure_encoder(&output, &flavor, &settings)?;

        let (sub, rx) = Subscriber::new(
            id,
            kind,
            flavor.clone(),
            output,
            req.payload_type,
            wants_keyframe,
            settings.rtx_cache_packets,
            settings.egress_queue_packets,
            settings.max_payload,
        );
        let ssrc = sub.ssrc;
        self.subscribers.insert(SourceKey::new(id, kind), sub);
        debug!(room = %self.id, participant = %id, %kind, %view, %flavor, "video subscriber added");
        Ok(VideoSubscription {
            ssrc,
            payload_type: req.payload_type,
            flavor,
            packets: rx,
        })
    }

    /// Stop sending the room to the participant's main section. Drops
    /// the encoder when it was the last subscriber of its flavor.
    pub fn unsubscribe(&self, id: &str) {
        self.unsubscribe_kind(id, StreamKind::Camera)
    }

    pub fn unsubscribe_kind(&self, id: &str, kind: StreamKind) {
        let Some((_, sub)) = self.subscribers.remove(&SourceKey::new(id, kind)) else {
            return;
        };
        self.release_flavor(&sub.output(), &sub.flavor());
        debug!(room = %self.id, participant = %id, %kind, "video subscriber removed");
    }

    /// The encoder for a flavor on an output, created when it is new.
    /// Whoever just arrived needs a keyframe, so one is asked for either
    /// way (§5.4); the flag it returns is the encoder's own.
    fn ensure_encoder(
        &self,
        output: &OutputKey,
        flavor: &Flavor,
        settings: &VideoRoomSettings,
    ) -> Result<Arc<AtomicBool>> {
        let layout = self.layout();
        let mut outputs = self.outputs.lock();
        let out = outputs.entry(output.clone()).or_insert_with(|| Output {
            compositor: HostCompositor::new(
                output.resolution.width,
                output.resolution.height,
                layout,
            ),
            encoders: HashMap::new(),
        });
        if let Some(enc) = out.encoders.get(flavor) {
            enc.wants_keyframe.store(true, Ordering::Release);
            return Ok(Arc::clone(&enc.wants_keyframe));
        }
        let mut es = EncoderSettings::for_flavor(
            flavor,
            (settings.keyframe_interval.as_secs_f64() * flavor.fps as f64).round() as u32,
        );
        // The content channel is a document: tune the encoder for one.
        // The composite with a document in it is mostly still faces.
        if output.view == View::Content {
            es = es.for_screen();
        }
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
        Ok(wk)
    }

    /// Drop an encoder no subscriber or recording wants any more, and the
    /// output when its last encoder goes.
    fn release_flavor(&self, output: &OutputKey, flavor: &Flavor) {
        let still_used = self.subscribers.iter().any(|s| s.watches(output, flavor))
            || self
                .recorders
                .iter()
                .any(|r| &r.output == output && &r.flavor == flavor);
        if still_used {
            return;
        }
        let mut outputs = self.outputs.lock();
        if let Some(out) = outputs.get_mut(output) {
            if out.encoders.remove(flavor).is_some() {
                gauge!("forge_conference_video_encoders").decrement(1.0);
            }
            if out.encoders.is_empty() {
                outputs.remove(output);
            }
        }
    }

    // ---- recording ----------------------------------------------------------

    /// Take the composite as coded frames, for a recording (§11). The
    /// recording is not a participant: it draws no tile, and its
    /// composite holds everyone whatever `exclude_self` says. A flavor a
    /// subscriber already watches is shared, so recording at the room's
    /// own resolution and codec costs no extra encode. Its view is
    /// [`View::Composite`] unless the request says otherwise, so a share
    /// is recorded as the room's single-picture participants saw it.
    pub fn record(&self, id: &str, req: RecordRequest) -> Result<RecordingSink> {
        let settings = self.settings.read().clone();
        let cap = if req.view == View::Content {
            settings.content_max_resolution
        } else {
            settings.resolution
        };
        let res = req
            .resolution
            .map(|r| Resolution::new(r.width.min(cap.width), r.height.min(cap.height)))
            .unwrap_or(cap);
        let fps = req
            .fps
            .unwrap_or(settings.fps)
            .clamp(1, settings.fps.max(1));
        let kbps = req.max_kbps.unwrap_or_else(|| default_kbps(res)).max(1);
        let flavor = Flavor::new(req.codec, "", res, fps, kbps);
        let output = OutputKey {
            scope: OutputScope::All,
            resolution: res,
            view: req.view,
        };

        self.stop_record(id);
        self.ensure_encoder(&output, &flavor, &settings)?;

        let (tx, rx) = mpsc::channel(req.queue.max(8));
        self.recorders.insert(
            id.to_string(),
            Arc::new(RecorderTap {
                output,
                flavor: flavor.clone(),
                frames: tx,
                sent: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
            }),
        );
        gauge!("forge_conference_video_recordings").increment(1.0);
        info!(room = %self.id, recording = %id, %flavor, view = %req.view, "video recording started");
        Ok(RecordingSink {
            id: id.to_string(),
            flavor,
            frames: rx,
        })
    }

    /// Stop a recording; its encoder goes when nothing else wants it.
    pub fn stop_record(&self, id: &str) {
        let Some((_, tap)) = self.recorders.remove(id) else {
            return;
        };
        self.release_flavor(&tap.output, &tap.flavor);
        gauge!("forge_conference_video_recordings").decrement(1.0);
        info!(
            room = %self.id,
            recording = %id,
            frames = tap.sent.load(Ordering::Relaxed),
            dropped = tap.dropped.load(Ordering::Relaxed),
            "video recording stopped"
        );
    }

    /// Whether a recording of that name is taking the composite.
    pub fn is_recording(&self, id: &str) -> bool {
        self.recorders.contains_key(id)
    }

    /// Every recording under way.
    pub fn recordings(&self) -> Vec<VideoRecordingInfo> {
        let mut v: Vec<_> = self
            .recorders
            .iter()
            .map(|e| VideoRecordingInfo {
                id: e.key().clone(),
                flavor: e.flavor.clone(),
                frames_sent: e.sent.load(Ordering::Relaxed),
                frames_dropped: e.dropped.load(Ordering::Relaxed),
            })
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn has_subscriber(&self, id: &str) -> bool {
        self.has_subscriber_kind(id, StreamKind::Camera)
    }

    pub fn has_subscriber_kind(&self, id: &str, kind: StreamKind) -> bool {
        self.subscribers.contains_key(&SourceKey::new(id, kind))
    }

    /// RTCP feedback from a receiver on their main section: PLI/FIR
    /// re-key its encoder, a NACK is answered from its cache, REMB caps
    /// its rate.
    pub fn handle_feedback(&self, id: &str, packet: &RtcpPacket) {
        self.handle_feedback_kind(id, StreamKind::Camera, packet)
    }

    /// [`handle_feedback`](Self::handle_feedback) for either channel.
    pub fn handle_feedback_kind(&self, id: &str, kind: StreamKind, packet: &RtcpPacket) {
        let Some(sub) = self.subscribers.get(&SourceKey::new(id, kind)) else {
            return;
        };
        match packet {
            RtcpPacket::PayloadFeedback(fb) => match &fb.kind {
                PayloadFeedback::Pli | PayloadFeedback::Fir(_) => {
                    counter!("forge_conference_video_plis_received_total", "room_id" => self.id.clone())
                        .increment(1);
                    // On the fast path there is no encoder of ours to
                    // re-key: the keyframe has to come from the sender
                    // whose packets this receiver is watching (§5.6).
                    match sub.forwarded_source() {
                        Some(source) => {
                            if let Some(s) = self.sources.get(&source) {
                                s.request_keyframe();
                            }
                        }
                        None => sub.request_keyframe(),
                    }
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

    /// The layout the cameras are arranged in. The compose tick hands
    /// each output the arrangement it needs, so nothing is set on the
    /// compositors here.
    pub fn set_layout(&self, layout: Layout) {
        self.control.lock().layout = Some(layout);
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
        self.emit_layout();
    }

    fn emit_layout(&self) {
        let c = self.control.lock();
        let ev = VideoRoomEvent::LayoutChanged {
            layout: c.layout.unwrap_or_else(|| self.settings.read().layout),
            pinned: c.pinned.clone(),
            spotlight: c.spotlight.clone(),
            content_layout: self.settings.read().content_layout,
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

    /// The person speaking, whichever node they are on: a caller here,
    /// or the participant a peer claims behind its remote tile.
    pub fn active_speaker(&self) -> Option<String> {
        let tile = self.speaker.lock().current().map(str::to_string)?;
        Some(self.resolve_speaker(&tile).0)
    }

    /// The remote tile the speaker is behind, when they are on a peer
    /// node; `None` when the speaker is a caller here or there is none.
    pub fn active_speaker_via(&self) -> Option<String> {
        let tile = self.speaker.lock().current().map(str::to_string)?;
        self.resolve_speaker(&tile).1
    }

    /// The tile the room is treating as the speaker — what the layouts
    /// move around, which for a peer's caller is the remote tile.
    pub fn active_speaker_tile(&self) -> Option<String> {
        self.speaker.lock().current().map(str::to_string)
    }

    /// What a tile is labelled: a caller's display name, and for a peer's
    /// remote tile whoever that peer says is speaking behind it, falling
    /// back to the node's own name.
    fn tile_name(&self, id: &str, p: &Participant) -> String {
        if p.remote {
            if let Some(claim) = self.remote_speakers.get(id) {
                if !claim.display_name.is_empty() {
                    return claim.display_name.clone();
                }
            }
        }
        p.name.clone()
    }

    /// Turn the winning tile into the person and the tile they came
    /// through.
    fn resolve_speaker(&self, tile: &str) -> (String, Option<String>) {
        match self.remote_speakers.get(tile) {
            Some(claim) => (claim.participant_id.clone(), Some(tile.to_string())),
            None => (tile.to_string(), None),
        }
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

    fn source_info(s: &VideoSource) -> VideoSourceInfo {
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
    }

    fn subscriber_info(s: &Subscriber) -> VideoSubscriberInfo {
        let st = &s.stats;
        VideoSubscriberInfo {
            flavor: s.flavor(),
            view: s.output().view,
            ssrc: s.ssrc,
            forwarding: s.forwarded_source(),
            packets_sent: st.packets_sent.load(Ordering::Relaxed),
            packets_dropped: st.packets_dropped.load(Ordering::Relaxed),
            frames_sent: st.frames_sent.load(Ordering::Relaxed),
            keyframes_sent: st.keyframes_sent.load(Ordering::Relaxed),
            nacks_received: st.nacks_received.load(Ordering::Relaxed),
            packets_retransmitted: st.packets_retransmitted.load(Ordering::Relaxed),
            plis_received: st.plis_received.load(Ordering::Relaxed),
            remb_kbps: st.remb_kbps.load(Ordering::Relaxed),
        }
    }

    fn participant_info(&self, id: &str, p: &Participant) -> VideoParticipantInfo {
        let source = self
            .sources
            .get(&SourceKey::camera(id))
            .map(|s| Self::source_info(&s));
        let content = self
            .sources
            .get(&SourceKey::content(id))
            .map(|s| Self::source_info(&s));
        let subscription = self
            .subscribers
            .get(&SourceKey::camera(id))
            .map(|s| Self::subscriber_info(&s));
        let content_subscription = self
            .subscribers
            .get(&SourceKey::content(id))
            .map(|s| Self::subscriber_info(&s));
        VideoParticipantInfo {
            participant_id: id.to_string(),
            state: p.state,
            content_state: p.content_state,
            display_name: self.tile_name(id, p),
            speaking: self.speaker.lock().current() == Some(id),
            presenting: self.holder_is(id),
            remote: p.remote,
            source,
            content,
            subscription,
            content_subscription,
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
                            .filter(|s| s.watches(k, &e.flavor))
                            .count(),
                        target_kbps: e.target_kbps(),
                        keyframes: e.keyframes,
                        encode_errors: e.encode_errors,
                    })
                    .collect();
                flavors.sort_by(|a, b| a.flavor.cmp(&b.flavor));
                VideoOutputInfo {
                    scope: k.scope.clone(),
                    view: k.view,
                    resolution: k.resolution,
                    flavors,
                }
            })
            .collect();
        outs.sort_by(|a, b| {
            (&a.scope, a.view, a.resolution).cmp(&(&b.scope, b.view, b.resolution))
        });
        let encoders = outputs.values().map(|o| o.encoders.len()).sum();
        drop(outputs);
        VideoRoomStatus {
            layout: c.layout.unwrap_or(settings.layout),
            content_layout: settings.content_layout,
            fps: self.fps(),
            target_fps: self.target_fps.load(Ordering::Relaxed),
            resolution: settings.resolution,
            pinned: c.pinned,
            spotlight: c.spotlight,
            active_speaker: self.active_speaker(),
            active_speaker_via: self.active_speaker_via(),
            content: self.content(),
            sources: self.sources.len(),
            encoders,
            ticks: self.ticks.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            outputs: outs,
            recordings: self.recordings(),
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
                // The clock asks this before it halves the rate, and again
                // when a calm stretch means something can be given back.
                let mut shedder = RoomShedder { room: &room };
                if let Some(ev) = clock.done_with(&mut shedder) {
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
        let local_node = self.local_node.lock().clone();
        let mut muted: HashMap<String, bool> = HashMap::new();
        let mut levels: Vec<(String, f32, bool, String)> = Vec::new();
        for m in metadata {
            if m.id.starts_with("__") {
                continue;
            }
            muted.insert(
                m.id.clone(),
                m.state != forge_mixer::ParticipantState::Active,
            );
            levels.push((m.id, m.energy, m.is_speaking, local_node.clone()));
        }
        // A peer's remote tile stands in the election for whoever that
        // node says is speaking behind it, under the tile's own id: the
        // tile is what the layouts move around, and the person is
        // reported. A tile with no claim never wins, since its own
        // energy is a whole node's mix (§15.4, 5c).
        for e in self.remote_speakers.iter() {
            let claim = e.value();
            levels.push((
                e.key().clone(),
                claim.energy,
                claim.speaking,
                claim.node.clone(),
            ));
        }
        // Every node feeds the same candidates in the same order, so the
        // rule and its tie-break give the same answer everywhere.
        levels.sort_by(|a, b| (&a.3, &a.0).cmp(&(&b.3, &b.0)));
        let (speaker, recent) = {
            let mut sp = self.speaker.lock();
            let lv: Vec<Level<'_>> = levels
                .iter()
                .map(|(id, e, s, node)| Level {
                    id,
                    energy: *e,
                    speaking: *s,
                    node,
                })
                .collect();
            if let Some(new) = sp.update(&lv, now) {
                let (participant_id, via) = self.resolve_speaker(&new);
                debug!(room = %self.id, speaker = %participant_id, via = ?via, "active speaker changed");
                let _ = self.events.send(VideoRoomEvent::ActiveSpeaker {
                    participant_id: Some(participant_id),
                    via,
                });
            }
            (sp.current().map(str::to_string), sp.recent().to_vec())
        };

        // Participant states and the frames to draw.
        self.sample_fps(now);
        self.expire_content(now, &settings);
        let mut ordered: Vec<(u64, String)> = self
            .participants
            .iter()
            .map(|e| (e.value().joined, e.key().clone()))
            .collect();
        ordered.sort();
        let join_order: Vec<String> = ordered.into_iter().map(|(_, id)| id).collect();

        struct Tile {
            key: SourceKey,
            name: String,
            frame: Option<Arc<VideoFrame>>,
            speaking: bool,
            muted: bool,
            /// A peer node's tile: left out of a trunk's own composite.
            remote: bool,
        }
        let mut tiles: HashMap<SourceKey, Tile> = HashMap::new();
        for id in &join_order {
            let Some(p) = self.participants.get(id) else {
                continue;
            };
            let camera = SourceKey::camera(id);
            let (frame, state) = match self.sources.get(&camera) {
                Some(_) if !p.enabled => (None, VideoState::Disabled),
                Some(s) if s.failed() => (None, VideoState::Failed),
                Some(s) => match s.frame(now, settings.freeze_timeout) {
                    Some(f) => (Some(f), VideoState::On),
                    None => (None, VideoState::Lost),
                },
                None => (None, VideoState::Off),
            };
            // The shared screen has no freeze timeout: a slide that has
            // not changed is still being shown, and the last frame is
            // what it looks like.
            let content = SourceKey::content(id);
            let (content_frame, content_state) = match self.sources.get(&content) {
                Some(_) if !p.content_enabled => (None, VideoState::Disabled),
                Some(s) if s.failed() => (None, VideoState::Failed),
                Some(s) => match s.frame(now, Duration::MAX) {
                    Some(f) => (Some(f), VideoState::On),
                    None => (None, VideoState::Lost),
                },
                None => (None, VideoState::Off),
            };
            let name = self.tile_name(id, &p);
            let remote = p.remote;
            drop(p);
            self.set_state_kind(id, StreamKind::Camera, state);
            self.set_state_kind(id, StreamKind::Content, content_state);
            tiles.insert(
                camera.clone(),
                Tile {
                    key: camera,
                    name: name.clone(),
                    frame,
                    speaking: speaker.as_deref() == Some(id.as_str()),
                    muted: muted.get(id).copied().unwrap_or(false),
                    remote,
                },
            );
            if let Some(f) = content_frame {
                tiles.insert(
                    content.clone(),
                    Tile {
                        key: content,
                        name,
                        frame: Some(f),
                        speaking: false,
                        muted: false,
                        remote,
                    },
                );
            }
        }
        // The share on show: the floor holder's screen, once a frame of
        // it has been decoded. Before that the composites carry on as
        // they were rather than draw an empty content tile.
        let content: Option<(SourceKey, bool)> = self
            .content_holder()
            .map(|h| SourceKey::content(&h))
            .and_then(|k| tiles.get(&k).map(|t| (k.clone(), t.remote)));
        let holder: Option<String> = content.as_ref().map(|(k, _)| k.participant.clone());

        let layout = self.layout();
        let control = self.control.lock().clone();
        let has_video = |id: &str| {
            tiles
                .get(&SourceKey::camera(id))
                .map(|t| t.frame.is_some())
                .unwrap_or(false)
        };
        let max_tiles = settings.max_tiles.clamp(1, 16);
        let camera_order = order_tiles(
            layout,
            &join_order,
            &recent,
            speaker.as_deref(),
            control.pinned.as_deref(),
            control.spotlight.as_deref(),
            &has_video,
            max_tiles,
        );
        // The strip beside a shared screen: whoever spoke last.
        let strip_order = if content.is_some() {
            order_tiles(
                Layout::Presentation,
                &join_order,
                &recent,
                speaker.as_deref(),
                control.pinned.as_deref(),
                control.spotlight.as_deref(),
                &has_video,
                max_tiles,
            )
        } else {
            Vec::new()
        };

        // What each output shows this tick.
        let keys: BTreeSet<OutputKey> = self
            .subscribers
            .iter()
            .map(|s| s.output())
            .chain(self.recorders.iter().map(|r| r.output.clone()))
            .collect();
        let plans: HashMap<OutputKey, Plan> = keys
            .into_iter()
            .map(|key| {
                let plan = plan_output(
                    &key,
                    layout,
                    settings.content_layout,
                    &camera_order,
                    &strip_order,
                    content.as_ref(),
                    |k| tiles.get(k).map(|t| t.remote),
                );
                (key, plan)
            })
            .collect();

        // The passthrough fast path (§5.6): a subscriber whose view is a
        // single source it can already decode is served that source's
        // own packets, and an output nobody composes is not rendered or
        // encoded at all — which is where the saving is.
        let with_frames: HashSet<SourceKey> = tiles
            .iter()
            .filter(|(_, t)| t.frame.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        if settings.ladder {
            self.walk_the_ladder(&settings, now);
        }
        let composing = self.decide_forwarding(&settings, &plans, &with_frames);

        // Render each output and encode each flavor.
        let pts = (now.duration_since(self.started).as_secs_f64() * 90_000.0) as u32;
        let room_fps = self.fps();
        let mut outputs = self.outputs.lock();
        // Recordings whose sink has gone, stopped after the loop:
        // `stop_record` wants the outputs lock this holds.
        let mut gone: Vec<String> = Vec::new();
        for (key, out) in outputs.iter_mut() {
            if out.encoders.is_empty() {
                continue;
            }
            // Nobody left to compose for: every subscriber of this
            // output is being forwarded, and a recording would have kept
            // it in `composing`.
            if !composing.contains(key) {
                continue;
            }
            let Some(plan) = plans.get(key) else {
                continue;
            };
            // A content channel with nothing shared: nothing is sent.
            if plan.tiles.is_empty() {
                continue;
            }
            out.compositor.set_layout(plan.layout);
            let sources: Vec<TileSource<'_>> = plan
                .tiles
                .iter()
                .filter_map(|k| tiles.get(k))
                .map(|t| TileSource {
                    id: &t.key.participant,
                    name: &t.name,
                    frame: t.frame.as_deref(),
                    speaking: t.speaking,
                    muted: t.muted,
                    kind: if t.key.is_content() {
                        TileKind::Content
                    } else {
                        TileKind::Camera
                    },
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
                    .filter(|s| s.watches(key, &enc.flavor))
                    // A presenter's own content channel gets nothing:
                    // what they are sharing is already on their screen.
                    .filter(|s| !(key.view == View::Content && Some(&s.id) == holder.as_ref()))
                    .map(|s| Arc::clone(s.value()))
                    .collect();
                let recorders: Vec<(String, Arc<RecorderTap>)> = self
                    .recorders
                    .iter()
                    .filter(|r| &r.output == key && r.flavor == enc.flavor)
                    .map(|r| (r.key().clone(), Arc::clone(r.value())))
                    .collect();
                if subs.is_empty() && recorders.is_empty() {
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
                    for (id, tap) in &recorders {
                        if !tap.send(&self.id, id, &frame, now) {
                            // The sink was dropped: reap the recording
                            // once the outputs are unlocked.
                            gone.push(id.clone());
                        }
                    }
                    counter!("forge_conference_video_packets_sent_total", "room_id" => self.id.clone())
                        .increment(subs.len() as u64);
                }
            }
        }
        drop(outputs);
        for id in gone {
            self.stop_record(&id);
        }
        histogram!("forge_conference_video_compose_duration_seconds", "room_id" => self.id.clone())
            .record(started.elapsed().as_secs_f64());
    }

    /// Move whoever needs moving between rungs of the bitrate ladder
    /// (§7).
    ///
    /// The encoder of a flavor targets the lowest rate any of its
    /// subscribers can take, so without this one caller on a poor link
    /// drags everybody sharing that flavor down with them. Moving them
    /// instead leaves the rest where they were, and the minimum is then
    /// taken over subscribers that belong together.
    ///
    /// A move is invisible to signaling: same SSRC, same payload type,
    /// the same sequence — the receiver sees a resolution change, which
    /// every decoder handles, and a keyframe to start it.
    ///
    /// A content channel is not on this ladder: text at 360p is useless.
    /// It moves in frame rate instead (§15.6), and only in resolution
    /// when the room sheds it, last of all.
    fn walk_the_ladder(&self, settings: &VideoRoomSettings, now: Instant) {
        let fps_moves: Vec<(SourceKey, u32)> = self
            .subscribers
            .iter()
            .filter(|s| s.output().view == View::Content && s.forwarded_source().is_none())
            .filter_map(|s| {
                let fps = s.fps_move(
                    settings.fps,
                    settings.ladder_policy,
                    settings.ladder_down_after,
                    settings.ladder_up_after,
                    now,
                )?;
                Some((Subscriber::key(s.value()), fps))
            })
            .collect();
        for (key, fps) in fps_moves {
            self.move_to_fps(&key, fps, settings);
        }

        let ladder = Ladder::for_room(settings.resolution);
        if ladder.rungs().len() < 2 {
            return;
        }
        let moves: Vec<(SourceKey, Rung)> = self
            .subscribers
            .iter()
            .filter(|s| s.output().view != View::Content && s.forwarded_source().is_none())
            .filter_map(|s| {
                let rung = s.ladder_move(
                    &ladder,
                    settings.ladder_policy,
                    settings.ladder_down_after,
                    settings.ladder_up_after,
                    now,
                )?;
                let output = s.output();
                let cap = self.cap_for(&ShedKey::of(&output), &ladder);
                let rung = clamp_to_cap(rung, output.resolution, cap)?;
                Some((Subscriber::key(s.value()), rung))
            })
            .collect();
        for (key, rung) in moves {
            self.move_to_rung(&key, rung, settings);
        }
    }

    /// Give up one step of load: the largest output drops a rung (§9).
    ///
    /// Asked by the clock before it halves the room's frame rate, because
    /// halving is the one cost everybody pays. A room overrunning on one
    /// expensive composite should degrade that composite: the largest
    /// output goes down a rung, then the next largest, and only when
    /// every output is at the bottom does the clock take the frame rate.
    /// The content channel goes last of all (§15.6): a blurred slide is
    /// worse than a slow one.
    ///
    /// The rung becomes a *ceiling* on the scope rather than a one-off
    /// move, or the ladder would put the subscribers straight back up on
    /// the next tick — their links are fine; it is this node that is not.
    ///
    /// Returns whether anything was shed.
    fn shed_one(&self) -> bool {
        let settings = self.settings.read().clone();
        let held: Vec<(ShedKey, Rung, Option<Rung>)> = self
            .live_shed_keys()
            .into_iter()
            .map(|key| {
                let ladder = self.ladder_for(&key, &settings);
                let at = self.cap_for(&key, &ladder);
                (key, at, ladder.below(at))
            })
            .collect();
        let Some((key, from, to)) = next_to_shed(&held) else {
            return false;
        };

        self.shed_caps.insert(key.clone(), to);
        self.apply_cap(&key, to, &settings);
        counter!("forge_conference_video_shed_total", "room_id" => self.id.clone()).increment(1);
        warn!(
            room = %self.id,
            output = %shed_label(&key),
            from = %from.resolution,
            to = %to.resolution,
            "video room is overrunning; dropped an output a rung"
        );
        let _ = self.events.send(VideoRoomEvent::Shed {
            output: shed_label(&key),
            from: from.resolution,
            to: to.resolution,
        });
        true
    }

    /// Take one step back: the most degraded output climbs a rung.
    ///
    /// Asked by the clock once the frame rate is whole again, so the room
    /// gives back what it took in the opposite order — everyone's motion
    /// first, then the pictures. The worst-off output goes first, on the
    /// grounds that it has the most to gain.
    ///
    /// Returns whether anything was given back.
    fn restore_one(&self) -> bool {
        let settings = self.settings.read().clone();
        let caps: Vec<(ShedKey, Rung, Option<Rung>)> = self
            .shed_caps
            .iter()
            .map(|e| {
                let ladder = self.ladder_for(e.key(), &settings);
                (e.key().clone(), *e.value(), ladder.above(*e.value()))
            })
            .collect();
        let Some((key, cap, above)) = next_to_restore(&caps) else {
            return false;
        };
        let Some(up) = above else {
            // Capped at the top already: nothing to give back, and the
            // entry is only noise.
            self.shed_caps.remove(&key);
            return false;
        };

        if up == self.ladder_for(&key, &settings).top() {
            self.shed_caps.remove(&key);
        } else {
            self.shed_caps.insert(key.clone(), up);
        }
        // With the ladder on, raising the ceiling is enough: `walk_the_ladder`
        // moves each subscriber up as its own link allows, and one whose link
        // cannot take the higher rung stays where it is. With the ladder off
        // there is no per-link judgement to wait for, so move them now. A
        // content channel is never walked in resolution, so it is moved now
        // either way.
        if !settings.ladder || key.view == View::Content {
            self.apply_cap(&key, up, &settings);
        }
        info!(
            room = %self.id,
            output = %shed_label(&key),
            from = %cap.resolution,
            to = %up.resolution,
            "video room is keeping up again; gave an output a rung back"
        );
        let _ = self.events.send(VideoRoomEvent::Shed {
            output: shed_label(&key),
            from: cap.resolution,
            to: up.resolution,
        });
        true
    }

    /// Every (scope, view) some subscriber is watching.
    fn live_shed_keys(&self) -> Vec<ShedKey> {
        self.subscribers
            .iter()
            .map(|s| ShedKey::of(&s.output()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The ladder an output climbs: the room's, or the content cap's.
    fn ladder_for(&self, key: &ShedKey, settings: &VideoRoomSettings) -> Ladder {
        Ladder::for_room(if key.view == View::Content {
            settings.content_max_resolution
        } else {
            settings.resolution
        })
    }

    /// The rung a scope is currently held at: its cap, or the ladder's top.
    fn cap_for(&self, key: &ShedKey, ladder: &Ladder) -> Rung {
        self.shed_caps
            .get(key)
            .map(|c| *c)
            .unwrap_or_else(|| ladder.top())
    }

    /// Move every subscriber of `key` that is above `cap` down onto it
    /// (or, for a content channel being restored, up to it).
    fn apply_cap(&self, key: &ShedKey, cap: Rung, settings: &VideoRoomSettings) {
        let subs: Vec<SourceKey> = self
            .subscribers
            .iter()
            .filter(|s| ShedKey::of(&s.output()) == *key)
            .filter(|s| {
                let h = s.output().resolution.height;
                if key.view == View::Content {
                    h != cap.resolution.height
                } else {
                    h > cap.resolution.height
                }
            })
            .map(|s| Subscriber::key(s.value()))
            .collect();
        for sub in subs {
            self.move_to_rung(&sub, cap, settings);
        }
    }

    /// Put one subscriber on another rung: a flavor at that size and
    /// rate, the output that goes with it, an encoder for it, and the
    /// old flavor released if nobody is left on it.
    fn move_to_rung(&self, key: &SourceKey, rung: Rung, settings: &VideoRoomSettings) {
        let Some(sub) = self.subscribers.get(key).map(|s| Arc::clone(&s)) else {
            return;
        };
        let was = sub.flavor();
        let flavor = Flavor::new(was.codec, &was.profile, rung.resolution, was.fps, rung.kbps);
        let old_output = sub.output();
        let output = OutputKey {
            scope: old_output.scope.clone(),
            resolution: rung.resolution,
            view: old_output.view,
        };
        self.move_to_flavor(&sub, flavor, output, settings, "ladder rungs");
    }

    /// Put a content channel on another frame rate (§15.6): the same
    /// picture, fewer of them.
    fn move_to_fps(&self, key: &SourceKey, fps: u32, settings: &VideoRoomSettings) {
        let Some(sub) = self.subscribers.get(key).map(|s| Arc::clone(&s)) else {
            return;
        };
        let was = sub.flavor();
        let flavor = Flavor::new(was.codec, &was.profile, was.resolution, fps, was.max_kbps);
        let output = sub.output();
        self.move_to_flavor(&sub, flavor, output, settings, "frame rates");
    }

    fn move_to_flavor(
        &self,
        sub: &Arc<Subscriber>,
        flavor: Flavor,
        output: OutputKey,
        settings: &VideoRoomSettings,
        what: &str,
    ) {
        let was = sub.flavor();
        if flavor == was {
            return;
        }
        let old_output = sub.output();
        // The encoder has to exist before anyone is pointed at it, or a
        // tick could find a subscriber with nothing to send it.
        if let Err(e) = self.ensure_encoder(&output, &flavor, settings) {
            warn!(room = %self.id, subscriber = %sub.key(), %flavor, error = %e, "could not open the encoder for the move");
            return;
        }
        sub.move_to(flavor.clone(), output);
        // Whoever it left behind may have been the last one there.
        self.release_flavor(&old_output, &was);
        counter!("forge_conference_video_ladder_moves_total", "room_id" => self.id.clone())
            .increment(1);
        debug!(
            room = %self.id,
            subscriber = %sub.key(),
            from = %was,
            to = %flavor,
            remb_kbps = sub.remb_kbps(),
            "video subscriber moved between {what}"
        );
    }

    /// Decide, for this tick, which subscribers ride the passthrough
    /// fast path, and return the outputs that still have to be composed
    /// (§5.6).
    ///
    /// A subscriber can be forwarded when its output's tiles come to
    /// exactly one source with a live frame and that source's stream is
    /// something the subscriber can already decode: same codec, a
    /// profile forwardable to its own, and a picture no larger than it
    /// asked for. A lone camera is forwarded where the passthrough mode
    /// allows it for the layout; a lone shared screen is forwarded under
    /// any mode but `Off`, since there is no banner on it to lose
    /// (§15.6). Anything else — two tiles, a codec mismatch — is
    /// composed as before.
    fn decide_forwarding(
        &self,
        settings: &VideoRoomSettings,
        plans: &HashMap<OutputKey, Plan>,
        with_frames: &HashSet<SourceKey>,
    ) -> HashSet<OutputKey> {
        let mut composing: HashSet<OutputKey> = HashSet::new();
        let mut forwarding = 0u32;
        for sub in self.subscribers.iter() {
            let key = sub.output();
            let Some(plan) = plans.get(&key) else {
                sub.stop_forwarding();
                continue;
            };
            // A presenter's own content channel is sent nothing at all.
            if key.view == View::Content
                && plan
                    .tiles
                    .iter()
                    .any(|t| t.is_content() && t.participant == sub.id)
            {
                sub.stop_forwarding();
                continue;
            }
            let sole = sole_source(&plan.tiles, with_frames).filter(|source| {
                let allowed = if source.is_content() {
                    settings.passthrough != PassthroughMode::Off
                } else {
                    settings.passthrough.allows(plan.layout)
                };
                allowed && self.forwardable(source, &sub.flavor())
            });
            match sole {
                Some(source) => {
                    let ssrc = self
                        .sources
                        .get(&source)
                        .map(|s| s.remote_ssrc())
                        .unwrap_or(0);
                    // A source we have never had a packet from has no
                    // SSRC to follow yet.
                    if ssrc == 0 {
                        sub.stop_forwarding();
                        composing.insert(key);
                        continue;
                    }
                    if sub.start_forwarding(&source, ssrc) {
                        if let Some(s) = self.sources.get(&source) {
                            s.request_keyframe();
                        }
                    }
                    forwarding += 1;
                }
                None => {
                    sub.stop_forwarding();
                    composing.insert(key);
                }
            }
        }
        // A recording takes the composite whatever the subscribers do.
        for r in self.recorders.iter() {
            composing.insert(r.output.clone());
        }
        self.forwarders.store(forwarding, Ordering::Relaxed);
        composing
    }

    /// Whether a source's own stream is something a subscriber of this
    /// flavor can decode as it stands.
    fn forwardable(&self, source: &SourceKey, flavor: &Flavor) -> bool {
        let Some(s) = self.sources.get(source) else {
            return false;
        };
        if s.codec() != flavor.codec || s.failed() {
            return false;
        }
        // The picture must fit what the subscriber asked for; scaling is
        // exactly what forwarding skips.
        let res = s.measured_resolution();
        if res.width == 0
            || res.width > flavor.resolution.width
            || res.height > flavor.resolution.height
        {
            return false;
        }
        // H.264 is the only codec here whose profile can make an
        // otherwise identical stream undecodable.
        if flavor.codec == VideoCodec::H264 {
            let from = forge_sdp::video::H264Fmtp::parse(s.profile());
            let to = forge_sdp::video::H264Fmtp::parse(&flavor.profile);
            if !from.forwardable_to(&to) {
                return false;
            }
        }
        true
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

    fn set_state_kind(&self, id: &str, kind: StreamKind, state: VideoState) {
        let changed = match self.participants.get_mut(id) {
            Some(mut p) => {
                let slot = match kind {
                    StreamKind::Camera => &mut p.state,
                    StreamKind::Content => &mut p.content_state,
                };
                if *slot != state {
                    *slot = state;
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if changed {
            debug!(room = %self.id, participant = %id, %kind, state = state.name(), "participant video state");
            let _ = self.events.send(VideoRoomEvent::ParticipantState {
                participant_id: id.to_string(),
                kind,
                state,
            });
        }
    }
}

/// What one output shows this tick: the arrangement and the tiles in it.
struct Plan {
    layout: Layout,
    tiles: Vec<SourceKey>,
}

/// The tiles an output draws, given its view and whether a screen is
/// being shared (§15.6). `remote_of` says whether a tile is a peer's, or
/// `None` for one that does not exist.
fn plan_output(
    key: &OutputKey,
    layout: Layout,
    content_layout: ContentLayout,
    camera_order: &[String],
    strip_order: &[String],
    content: Option<&(SourceKey, bool)>,
    remote_of: impl Fn(&SourceKey) -> Option<bool>,
) -> Plan {
    let cameras = |order: &[String]| -> Vec<SourceKey> {
        order
            .iter()
            .map(|id| SourceKey::camera(id))
            .filter(|k| remote_of(k).is_some_and(|remote| key.scope.admits(&k.participant, remote)))
            .collect()
    };
    let content = content
        .filter(|(_, remote)| key.scope.admits_content(*remote))
        .map(|(k, _)| k.clone());
    match (key.view, content) {
        (View::Cameras, _) | (View::Composite, None) => Plan {
            layout,
            tiles: cameras(camera_order),
        },
        (View::Content, None) => Plan {
            layout: Layout::Spotlight,
            tiles: Vec::new(),
        },
        (View::Content, Some(c)) => Plan {
            layout: Layout::Spotlight,
            tiles: vec![c],
        },
        (View::Composite, Some(c)) => match content_layout {
            ContentLayout::ContentOnly => Plan {
                layout: Layout::Spotlight,
                tiles: vec![c],
            },
            ContentLayout::Presentation => {
                let mut tiles = vec![c];
                tiles.extend(cameras(strip_order));
                Plan {
                    layout: Layout::Presentation,
                    tiles,
                }
            }
        },
    }
}

/// The one source an output's composite would show, when there is
/// exactly one tile with a live frame in it.
fn sole_source(tiles: &[SourceKey], with_frames: &HashSet<SourceKey>) -> Option<SourceKey> {
    let mut only = None;
    for k in tiles.iter().filter(|k| with_frames.contains(k)) {
        if only.is_some() {
            return None;
        }
        only = Some(k.clone());
    }
    only
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
/// takes the first tile in every layout. For a presentation the order
/// is the strip beside the shared screen — the active-speaker order,
/// one slot shorter, since the screen takes the first — and the caller
/// puts the content in front of it.
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
        Layout::ActiveSpeaker | Layout::Presentation => {
            let slots = if layout == Layout::Presentation {
                cap.saturating_sub(1)
            } else {
                cap
            };
            let first = pinned.or(speaker).or(by_recency.first().copied());
            let mut v: Vec<&str> = Vec::new();
            if let Some(f) = first {
                v.push(f);
            }
            for id in &by_recency {
                if v.len() >= slots {
                    break;
                }
                if !v.contains(id) {
                    v.push(id);
                }
            }
            v.truncate(slots);
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

    /// A 1080p room: rungs 1080, 720, 360, 180.
    fn ladder() -> Ladder {
        Ladder::for_room(Resolution::new(1920, 1080))
    }

    fn at(ladder: &Ladder, height: u32) -> Rung {
        ladder.rung_for(Resolution::new(height * 16 / 9, height))
    }

    fn key(scope: OutputScope, view: View) -> ShedKey {
        ShedKey { scope, view }
    }

    /// What `shed_one` hands `next_to_shed`: each live output, the rung
    /// it is held at, and the one below it on its ladder.
    fn held(l: &Ladder, v: &[(ShedKey, u32)]) -> Vec<(ShedKey, Rung, Option<Rung>)> {
        v.iter()
            .map(|(k, h)| (k.clone(), at(l, *h), l.below(at(l, *h))))
            .collect()
    }

    /// What `restore_one` hands `next_to_restore`: each cap and the rung
    /// above it.
    fn caps(l: &Ladder, v: &[(ShedKey, u32)]) -> Vec<(ShedKey, Rung, Option<Rung>)> {
        v.iter()
            .map(|(k, h)| (k.clone(), at(l, *h), l.above(at(l, *h))))
            .collect()
    }

    #[test]
    fn shedding_takes_the_largest_output_first_then_the_next() {
        let l = ladder();
        let h = held(
            &l,
            &[
                (key(OutputScope::LocalOnly, View::Composite), 360),
                (key(OutputScope::All, View::Composite), 1080),
                (
                    key(OutputScope::Excluding("a".into()), View::Composite),
                    720,
                ),
            ],
        );
        // The 1080p one goes first, down one rung.
        let (k, from, to) = next_to_shed(&h).expect("something to shed");
        assert_eq!(k.scope, OutputScope::All);
        assert_eq!(from.resolution.height, 1080);
        assert_eq!(to.resolution.height, 720);

        // With that one at 720 the next largest is picked, and the tie
        // between the two 720s is settled the same way every time.
        let h = held(
            &l,
            &[
                (key(OutputScope::LocalOnly, View::Composite), 360),
                (key(OutputScope::All, View::Composite), 720),
                (
                    key(OutputScope::Excluding("a".into()), View::Composite),
                    720,
                ),
            ],
        );
        let first = next_to_shed(&h).expect("something to shed");
        let again = next_to_shed(&h).expect("something to shed");
        assert_eq!(first.0, again.0, "the choice is deterministic");
        assert_eq!(first.1.resolution.height, 720);
    }

    #[test]
    fn the_content_channel_is_shed_last() {
        let l = ladder();
        // A 1080p content channel beside a 360p composite: the composite
        // still goes first, though it is the smaller picture.
        let h = held(
            &l,
            &[
                (key(OutputScope::All, View::Content), 1080),
                (key(OutputScope::All, View::Composite), 360),
            ],
        );
        let (k, _, _) = next_to_shed(&h).expect("something to shed");
        assert_eq!(k.view, View::Composite);
        // Only once every other output is at the bottom does the content
        // give up a rung.
        let h = held(
            &l,
            &[
                (key(OutputScope::All, View::Content), 1080),
                (key(OutputScope::All, View::Composite), 180),
            ],
        );
        let (k, from, to) = next_to_shed(&h).expect("the content, finally");
        assert_eq!(k.view, View::Content);
        assert_eq!((from.resolution.height, to.resolution.height), (1080, 720));
    }

    #[test]
    fn shedding_stops_when_every_output_is_at_the_bottom() {
        let l = ladder();
        let h = held(
            &l,
            &[
                (key(OutputScope::All, View::Composite), 180),
                (key(OutputScope::LocalOnly, View::Composite), 180),
                (key(OutputScope::All, View::Content), 180),
            ],
        );
        assert_eq!(next_to_shed(&h), None, "the clock takes over here");
        assert_eq!(next_to_shed(&[]), None, "a room with no subscribers");
    }

    #[test]
    fn restoring_takes_the_worst_off_output_first() {
        let l = ladder();
        let c = caps(
            &l,
            &[
                (key(OutputScope::All, View::Composite), 720),
                (key(OutputScope::LocalOnly, View::Composite), 180),
            ],
        );
        let (k, from, up) = next_to_restore(&c).expect("something to restore");
        assert_eq!(
            k.scope,
            OutputScope::LocalOnly,
            "the smallest picture first"
        );
        assert_eq!(from.resolution.height, 180);
        assert_eq!(up.expect("a rung above").resolution.height, 360);
    }

    #[test]
    fn restoring_reports_a_cap_that_is_already_at_the_top() {
        let l = ladder();
        assert_eq!(next_to_restore(&[]), None, "nothing shed");
        let c = caps(&l, &[(key(OutputScope::All, View::Composite), 1080)]);
        let (_, _, up) = next_to_restore(&c).expect("an entry");
        assert_eq!(up, None, "so the caller can drop the stale cap");
    }

    #[test]
    fn shedding_and_restoring_walk_the_same_path_in_reverse() {
        let l = ladder();
        let mut state = vec![
            (key(OutputScope::All, View::Composite), 1080),
            (key(OutputScope::LocalOnly, View::Composite), 1080),
        ];
        // Shed until there is nothing left, remembering the order.
        let mut down = Vec::new();
        while let Some((k, _, to)) = next_to_shed(&held(&l, &state)) {
            down.push((k.clone(), to));
            for entry in state.iter_mut() {
                if entry.0 == k {
                    entry.1 = to.resolution.height;
                }
            }
        }
        assert_eq!(down.len(), 6, "two outputs, three rungs each to give up");
        assert!(state.iter().all(|(_, h)| *h == 180));

        // Restore until everything is back at the top.
        let mut steps = 0;
        while let Some((k, _, Some(up))) = next_to_restore(&caps(&l, &state)) {
            steps += 1;
            for entry in state.iter_mut() {
                if entry.0 == k {
                    entry.1 = up.resolution.height;
                }
            }
            assert!(steps <= 6, "restoring must terminate");
        }
        assert_eq!(steps, 6, "everything given back, one rung at a time");
        assert!(state.iter().all(|(_, h)| *h == 1080));
    }

    #[test]
    fn a_shed_ceiling_holds_a_good_link_down_but_never_pushes_one_up() {
        let l = ladder();
        let (top, mid, low) = (at(&l, 1080), at(&l, 720), at(&l, 360));

        // Nothing shed: the ladder gets what it asked for.
        assert_eq!(
            clamp_to_cap(top, Resolution::new(1280, 720), top),
            Some(top)
        );

        // Shed to 720. A link with room for 1080 is held at 720 — and if
        // it is already there, it does not move at all, so the room is not
        // asked to rebuild the same flavor every tick.
        assert_eq!(
            clamp_to_cap(top, Resolution::new(640, 360), mid),
            Some(mid),
            "climbs as far as the ceiling"
        );
        assert_eq!(
            clamp_to_cap(top, Resolution::new(1280, 720), mid),
            None,
            "already at the ceiling: stays put"
        );

        // Down is never clamped: a link that cannot carry the ceiling gets
        // the smaller picture it asked for.
        assert_eq!(
            clamp_to_cap(low, Resolution::new(1280, 720), mid),
            Some(low)
        );
    }

    #[test]
    fn a_scope_is_named_for_the_events_people_read() {
        assert_eq!(scope_label(&OutputScope::All), "all");
        assert_eq!(scope_label(&OutputScope::LocalOnly), "local-only");
        assert_eq!(
            scope_label(&OutputScope::Excluding("alice".into())),
            "excluding:alice"
        );
        assert_eq!(shed_label(&key(OutputScope::All, View::Composite)), "all");
        assert_eq!(
            shed_label(&key(OutputScope::All, View::Content)),
            "all:content"
        );
        assert_eq!(
            shed_label(&key(OutputScope::LocalOnly, View::Cameras)),
            "local-only:cameras"
        );
    }

    #[test]
    fn a_presentation_orders_the_strip_beside_the_screen() {
        let join = ids(&["a", "b", "c", "d", "e", "f", "g"]);
        let recent = ids(&["c", "a"]);
        // Five slots beside the screen, by recency then join order.
        let got = order_tiles(
            Layout::Presentation,
            &join,
            &recent,
            Some("c"),
            None,
            None,
            |_| true,
            16,
        );
        assert_eq!(got, ids(&["c", "a", "b", "d", "e"]));
        // A room capped at one tile shows the screen alone.
        let got = order_tiles(
            Layout::Presentation,
            &join,
            &recent,
            Some("c"),
            None,
            None,
            |_| true,
            1,
        );
        assert!(got.is_empty());
        // The plan puts the content first and honours the scope.
        let content = (SourceKey::content("a"), false);
        let key_all = OutputKey {
            scope: OutputScope::All,
            resolution: Resolution::new(640, 360),
            view: View::Composite,
        };
        let plan = plan_output(
            &key_all,
            Layout::Grid,
            ContentLayout::Presentation,
            &join,
            &got,
            Some(&content),
            |_| Some(false),
        );
        assert_eq!(plan.layout, Layout::Presentation);
        assert_eq!(plan.tiles, vec![SourceKey::content("a")]);
        let strip = ids(&["c", "a"]);
        let plan = plan_output(
            &OutputKey {
                scope: OutputScope::Excluding("a".into()),
                ..key_all.clone()
            },
            Layout::Grid,
            ContentLayout::Presentation,
            &join,
            &strip,
            Some(&content),
            |_| Some(false),
        );
        assert_eq!(
            plan.tiles,
            vec![SourceKey::content("a"), SourceKey::camera("c")],
            "a's own camera is left out of a's composite, a's screen is not"
        );
        // Content only, the cameras view, the content view, and a trunk's
        // local-only composite with a peer's content.
        let plan = plan_output(
            &key_all,
            Layout::Grid,
            ContentLayout::ContentOnly,
            &join,
            &strip,
            Some(&content),
            |_| Some(false),
        );
        assert_eq!((plan.layout, plan.tiles.len()), (Layout::Spotlight, 1));
        let plan = plan_output(
            &OutputKey {
                view: View::Cameras,
                ..key_all.clone()
            },
            Layout::Grid,
            ContentLayout::Presentation,
            &join,
            &strip,
            Some(&content),
            |_| Some(false),
        );
        assert_eq!(plan.layout, Layout::Grid);
        assert!(plan.tiles.iter().all(|t| !t.is_content()));
        assert_eq!(plan.tiles.len(), 7);
        let plan = plan_output(
            &OutputKey {
                view: View::Content,
                ..key_all.clone()
            },
            Layout::Grid,
            ContentLayout::Presentation,
            &join,
            &strip,
            None,
            |_| Some(false),
        );
        assert!(plan.tiles.is_empty(), "nothing shared: nothing sent");
        let remote_content = (SourceKey::content("__trunk__b"), true);
        let plan = plan_output(
            &OutputKey {
                scope: OutputScope::LocalOnly,
                ..key_all.clone()
            },
            Layout::Grid,
            ContentLayout::Presentation,
            &join,
            &strip,
            Some(&remote_content),
            |_| Some(false),
        );
        assert_eq!(
            plan.layout,
            Layout::Grid,
            "a peer's screen does not go back to the peer"
        );
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
