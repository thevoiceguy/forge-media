//! Forwarding subscribers (FCP `docs/FORWARDING_ROOMS.md` §2.1–§2.6).
//!
//! A *viewer* is a participant whose camera section takes each person's
//! own stream rather than the room's composite. It asks for streams with a
//! [`ViewRequest`]; each stream it gets has an SSRC of its own, fixed for as
//! long as the viewer asks for that person (or for the speaker), so an
//! SSRC means one person for its whole life. All of a viewer's streams
//! leave through one channel, as one RTP session.
//!
//! Which layer of a simulcast sender each stream carries is the
//! allocator's choice ([`allocate`]): the viewer's REMB is its budget,
//! spent in priority order — every stream's smallest layer first, then
//! upgrades — and a stream the budget cannot reach is paused. A switch,
//! between layers or between people, happens at the new stream's keyframe
//! ([`StreamRewriter`]), which the source is asked for through its own
//! keyframe gate, so many viewers switching cost one keyframe.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use forge_core::VideoCodec;
use forge_rtp::{RtpPacket, RtxCache, StreamRewriter};
use forge_video::frame::Resolution;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::content::SourceKey;
use super::egress::default_kbps;

/// How long a larger layer must be affordable before a stream moves up
/// to it. A move down, or a pause, is at once: a picture the link cannot
/// carry is worse than a smaller one.
pub const STREAM_UP_AFTER: Duration = Duration::from_secs(3);

/// The default view's height for the speaker's stream.
pub const DEFAULT_SPEAKER_HEIGHT: u32 = 720;
/// The default view's height for everyone else.
pub const DEFAULT_OTHER_HEIGHT: u32 = 180;
/// How many streams the default view asks for: the speaker and eight
/// others.
pub const DEFAULT_MAX_STREAMS: usize = 9;
/// The most a view may ask for.
pub const MAX_STREAMS: usize = 25;

/// What a viewer's camera section was negotiated as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRequest {
    /// A stream is forwarded only from a sender of this codec.
    pub codec: VideoCodec,
    /// The negotiated `a=fmtp`; for H.264, what a sender's profile must
    /// be forwardable to.
    pub profile: String,
    pub payload_type: u8,
}

/// Whose a stream is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamTarget {
    /// This participant's camera.
    Participant(String),
    /// Whoever is the active speaker, other than the viewer. It moves at
    /// a keyframe when the speaker changes.
    Speaker,
}

/// One stream a view asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamWant {
    pub target: StreamTarget,
    /// The tallest layer wanted; `None` is any.
    pub max_height: Option<u32>,
    /// Lower is kept first when the budget is short; `None` comes after
    /// every numbered stream, in the view's order.
    pub priority: Option<u8>,
}

/// What a viewer asks for: the `fcp-view` INFO (§2.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewRequest {
    /// `None` is the default view: the speaker at 720p and up to eight
    /// others at 180p, those who spoke last first.
    pub streams: Option<Vec<StreamWant>>,
    /// At most this many streams; the view's own default when `None`.
    pub max_streams: Option<usize>,
}

impl ViewRequest {
    /// The streams this view means, given who is in the room: `others`
    /// is everyone but the viewer, those who spoke last first.
    pub fn resolve(&self, others: &[String]) -> Vec<StreamWant> {
        let max = self
            .max_streams
            .unwrap_or(DEFAULT_MAX_STREAMS)
            .clamp(1, MAX_STREAMS);
        match &self.streams {
            Some(streams) => {
                let mut seen = std::collections::HashSet::new();
                streams
                    .iter()
                    .filter(|w| seen.insert(w.target.clone()))
                    .take(max)
                    .cloned()
                    .collect()
            }
            None => std::iter::once(StreamWant {
                target: StreamTarget::Speaker,
                max_height: Some(DEFAULT_SPEAKER_HEIGHT),
                priority: Some(1),
            })
            .chain(others.iter().take(max - 1).map(|id| StreamWant {
                target: StreamTarget::Participant(id.clone()),
                max_height: Some(DEFAULT_OTHER_HEIGHT),
                priority: None,
            }))
            .collect(),
        }
    }
}

/// One stream of a viewer, as the room reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedStreamInfo {
    pub ssrc: u32,
    pub target: StreamTarget,
    /// Whose packets the stream carries now; `None` before its first
    /// keyframe, and for a speaker stream with no speaker yet.
    pub participant: Option<String>,
    /// The simulcast layer it carries.
    pub rid: Option<String>,
    pub resolution: Resolution,
    /// Held back: the budget does not reach it, or there is nothing to
    /// send (no camera, a codec the viewer cannot take, a host's hide).
    pub paused: bool,
    pub packets_sent: u64,
}

impl ForwardedStreamInfo {
    /// What a roster needs to say about it; a change in this is what
    /// [`super::VideoRoomEvent::Streams`] reports.
    pub(crate) fn roster_key(&self) -> (u32, Option<String>, bool) {
        (self.ssrc, self.participant.clone(), self.paused)
    }
}

/// A viewer, as the room reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoViewerInfo {
    pub codec: VideoCodec,
    pub streams: Vec<ForwardedStreamInfo>,
    /// The budget the allocator spends: the latest REMB (0 = none yet,
    /// which is no limit).
    pub remb_kbps: u32,
    pub packets_sent: u64,
    pub packets_dropped: u64,
    pub nacks_received: u64,
    pub packets_retransmitted: u64,
    pub plis_received: u64,
}

/// What a viewer gets back: the packets of all its streams.
pub struct ForwardSubscription {
    pub payload_type: u8,
    /// RTP packets, unencrypted. Protect and send them.
    pub packets: mpsc::Receiver<Bytes>,
}

impl std::fmt::Debug for ForwardSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardSubscription")
            .field("payload_type", &self.payload_type)
            .finish_non_exhaustive()
    }
}

// ---- the allocator (§2.5) ---------------------------------------------------

/// One layer a stream could carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerOption {
    /// The layer's index in its source.
    pub index: usize,
    pub ssrc: u32,
    pub rid: Option<String>,
    /// 0 until a keyframe has said.
    pub height: u32,
    pub width: u32,
    /// What it costs: its measured rate, or an estimate from its height
    /// before it has one.
    pub kbps: u32,
}

impl LayerOption {
    /// The estimate for a layer whose rate is not measured yet.
    pub fn estimated_kbps(width: u32, height: u32) -> u32 {
        if height == 0 {
            default_kbps(Resolution::new(320, 180))
        } else {
            default_kbps(Resolution::new(width, height))
        }
    }

    fn rank(&self) -> (u32, u32) {
        (self.height, self.kbps)
    }
}

/// One stream asking for a layer: its priority, its height limit, and
/// its source's layers, smallest first.
#[derive(Debug, Clone)]
pub struct Ask<'a> {
    pub priority: Option<u8>,
    pub max_height: u32,
    pub layers: &'a [LayerOption],
}

/// Spend `budget_kbps` (`None` is no limit) across the asks: the index
/// into each ask's `layers` it gets, or `None` when it is paused.
///
/// 1. Sort by priority, keeping the asks' order among equals.
/// 2. Give each its smallest layer until the budget runs out; the rest
///    are paused.
/// 3. In the same order, move each up to its largest layer within its
///    height limit that the budget still covers.
pub fn allocate(budget_kbps: Option<u32>, asks: &[Ask<'_>]) -> Vec<Option<usize>> {
    let mut order: Vec<usize> = (0..asks.len()).collect();
    order.sort_by_key(|i| asks[*i].priority.unwrap_or(u8::MAX));
    let budget = budget_kbps.map(u64::from).unwrap_or(u64::MAX);
    let mut chosen: Vec<Option<usize>> = vec![None; asks.len()];
    let mut spent: u64 = 0;
    for &i in &order {
        let Some(lowest) = asks[i].layers.first() else {
            continue;
        };
        let cost = u64::from(lowest.kbps);
        if spent + cost > budget {
            break;
        }
        spent += cost;
        chosen[i] = Some(0);
    }
    for &i in &order {
        let Some(at) = chosen[i] else { continue };
        let layers = asks[i].layers;
        let now_costs = u64::from(layers[at].kbps);
        let best = (at + 1..layers.len())
            .rev()
            .find(|j| {
                layers[*j].height <= asks[i].max_height
                    && spent - now_costs + u64::from(layers[*j].kbps) <= budget
            })
            .unwrap_or(at);
        spent = spent - now_costs + u64::from(layers[best].kbps);
        chosen[i] = Some(best);
    }
    chosen
}

// ---- a viewer and its streams -----------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct ViewerStats {
    pub packets_sent: AtomicU64,
    pub packets_dropped: AtomicU64,
    pub nacks_received: AtomicU64,
    pub packets_retransmitted: AtomicU64,
    pub plis_received: AtomicU64,
}

/// A participant's camera section, taking streams.
pub(crate) struct Viewer {
    pub id: String,
    pub codec: VideoCodec,
    pub profile: String,
    pub payload_type: u8,
    pub view: Mutex<ViewRequest>,
    /// In the view's order.
    pub streams: Mutex<Vec<std::sync::Arc<Stream>>>,
    /// What was last reported, so a change is reported once.
    pub reported: Mutex<Vec<(u32, Option<String>, bool)>>,
    pub remb_kbps: AtomicU32,
    pub stats: ViewerStats,
    cache_packets: usize,
    tx: mpsc::Sender<Bytes>,
}

impl Viewer {
    pub fn new(
        id: &str,
        req: &ForwardRequest,
        cache_packets: usize,
        channel_packets: usize,
    ) -> (std::sync::Arc<Self>, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(channel_packets.max(8));
        let v = std::sync::Arc::new(Self {
            id: id.to_string(),
            codec: req.codec,
            profile: req.profile.clone(),
            payload_type: req.payload_type,
            view: Mutex::new(ViewRequest::default()),
            streams: Mutex::new(Vec::new()),
            reported: Mutex::new(Vec::new()),
            remb_kbps: AtomicU32::new(0),
            stats: ViewerStats::default(),
            cache_packets,
            tx,
        });
        (v, rx)
    }

    /// A stream for `target`, new: an SSRC no other stream of this viewer
    /// has.
    pub fn new_stream(&self, target: StreamTarget, taken: &[u32]) -> Stream {
        let ssrc = loop {
            let s: u32 = rand::random();
            if s != 0 && !taken.contains(&s) {
                break s;
            }
        };
        Stream {
            ssrc,
            target,
            state: Mutex::new(StreamState {
                rewriter: StreamRewriter::new(self.codec, ssrc, rand::random(), rand::random()),
                source: None,
                pending: None,
                layer: None,
                paused: true,
                started: false,
                up: None,
                cache: RtxCache::new(self.cache_packets),
                packets_sent: 0,
            }),
        }
    }

    pub fn stream(&self, ssrc: u32) -> Option<std::sync::Arc<Stream>> {
        self.streams.lock().iter().find(|s| s.ssrc == ssrc).cloned()
    }

    /// Hand a packet to the transport.
    pub fn deliver(&self, bytes: Bytes) -> bool {
        match self.tx.try_send(bytes) {
            Ok(()) => {
                self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(_) => {
                self.stats.packets_dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn budget_kbps(&self) -> Option<u32> {
        Some(self.remb_kbps.load(Ordering::Relaxed)).filter(|k| *k > 0)
    }
}

/// One stream of a viewer.
pub(crate) struct Stream {
    pub ssrc: u32,
    pub target: StreamTarget,
    pub state: Mutex<StreamState>,
}

pub(crate) struct StreamState {
    pub rewriter: StreamRewriter,
    /// The source whose packets go out now, and the one a switch waits
    /// on (the speaker changed).
    pub source: Option<SourceKey>,
    pub pending: Option<SourceKey>,
    /// The layer chosen.
    pub layer: Option<LayerOption>,
    pub paused: bool,
    /// Whether a layer has ever been chosen: the first is at once.
    pub started: bool,
    /// A move up waiting to hold: the layer's SSRC, and since when.
    pub up: Option<(u32, Instant)>,
    pub cache: RtxCache,
    pub packets_sent: u64,
}

/// What a stream should do this tick.
pub(crate) enum Step {
    /// Nothing changes.
    Stay,
    /// Carry this source's layer; ask it for a keyframe when the flag is
    /// set.
    Select(SourceKey, LayerOption, bool),
    /// Send nothing.
    Pause,
}

impl Stream {
    /// Offer a packet of `from`'s. Returns the rewritten packet to send,
    /// kept for NACKs, or `None` when this stream does not carry it.
    pub fn offer(&self, packet: &RtpPacket, from: &SourceKey, payload_type: u8) -> Option<Bytes> {
        let mut s = self.state.lock();
        let mut out = s.rewriter.feed(packet)?;
        if s.pending.as_ref() == Some(from) && s.rewriter.pending_source().is_none() {
            // The switch to the new person has taken.
            s.source = s.pending.take();
        }
        out.header.set_payload_type(payload_type);
        let seq = out.header.sequence_number;
        let bytes = out.to_bytes().freeze();
        s.cache.push(seq, bytes.clone());
        s.packets_sent += 1;
        Some(bytes)
    }

    /// Decide this tick's move: `want` is the source and layer the
    /// allocator gave the stream, or `None` for a pause.
    pub fn step(&self, want: Option<(SourceKey, LayerOption)>, now: Instant) -> Step {
        let mut s = self.state.lock();
        let Some((source, layer)) = want else {
            s.up = None;
            if s.paused {
                return Step::Stay;
            }
            s.paused = true;
            s.layer = None;
            s.pending = None;
            s.rewriter.clear();
            return Step::Pause;
        };
        let wanted_source = s.pending.as_ref().or(s.source.as_ref());
        let same_source = wanted_source == Some(&source);
        if same_source && s.layer.as_ref().map(|l| l.ssrc) == Some(layer.ssrc) && !s.paused {
            s.up = None;
            return Step::Stay;
        }
        // A move up — a larger layer of the same person, or out of a
        // pause — waits to see the budget hold; anything else is at once:
        // the first layer, a new person, a smaller layer, or the old
        // layer gone.
        let up = s.started
            && same_source
            && match &s.layer {
                None => true,
                Some(cur) => layer.rank() > cur.rank(),
            };
        if up {
            match s.up {
                Some((ssrc, since)) if ssrc == layer.ssrc => {
                    if now.saturating_duration_since(since) < STREAM_UP_AFTER {
                        return Step::Stay;
                    }
                }
                _ => {
                    s.up = Some((layer.ssrc, now));
                    return Step::Stay;
                }
            }
        }
        s.up = None;
        s.started = true;
        s.paused = false;
        let needs_keyframe = s.rewriter.select(layer.ssrc);
        if s.source.as_ref() != Some(&source) {
            s.pending = Some(source.clone());
        } else {
            s.pending = None;
        }
        s.layer = Some(layer.clone());
        Step::Select(source, layer, needs_keyframe)
    }

    /// The sources whose packets this stream must be offered.
    pub fn routes(&self) -> Vec<SourceKey> {
        let s = self.state.lock();
        s.source.iter().chain(s.pending.iter()).cloned().collect()
    }

    /// The layer whose keyframe a PLI from the viewer is asking for.
    pub fn keyframe_target(&self) -> Option<(SourceKey, usize)> {
        let s = self.state.lock();
        let source = s.pending.clone().or_else(|| s.source.clone())?;
        Some((source, s.layer.as_ref()?.index))
    }

    pub fn retransmit(&self, seqs: &[u16]) -> Vec<Bytes> {
        self.state
            .lock()
            .cache
            .lookup(seqs)
            .into_iter()
            .map(|(_, b)| b.clone())
            .collect()
    }

    pub fn info(&self) -> ForwardedStreamInfo {
        let s = self.state.lock();
        ForwardedStreamInfo {
            ssrc: self.ssrc,
            target: self.target.clone(),
            participant: s.source.as_ref().map(|k| k.participant.clone()),
            rid: s.layer.as_ref().and_then(|l| l.rid.clone()),
            resolution: s
                .layer
                .as_ref()
                .map(|l| Resolution::new(l.width, l.height))
                .unwrap_or(Resolution::new(0, 0)),
            paused: s.paused,
            packets_sent: s.packets_sent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(index: usize, height: u32, kbps: u32) -> LayerOption {
        LayerOption {
            index,
            ssrc: 100 + index as u32,
            rid: None,
            height,
            width: height * 16 / 9,
            kbps,
        }
    }

    fn three() -> Vec<LayerOption> {
        vec![layer(2, 180, 150), layer(1, 360, 500), layer(0, 720, 1200)]
    }

    #[test]
    fn without_a_budget_every_stream_gets_its_largest_layer_within_its_height() {
        let l = three();
        let asks = [
            Ask {
                priority: Some(1),
                max_height: 720,
                layers: &l,
            },
            Ask {
                priority: None,
                max_height: 180,
                layers: &l,
            },
            Ask {
                priority: None,
                max_height: 400,
                layers: &l,
            },
        ];
        assert_eq!(allocate(None, &asks), vec![Some(2), Some(0), Some(1)]);
    }

    #[test]
    fn a_short_budget_goes_to_the_first_priority_and_pauses_the_rest() {
        let l = three();
        let ask = |priority, max_height| Ask {
            priority,
            max_height,
            layers: &l,
        };
        // The speaker first (priority 1) though it is listed last.
        let asks = [ask(None, 180), ask(None, 180), ask(Some(1), 720)];
        // Room for every smallest layer (450), then the speaker climbs as
        // far as the rest allows: 150 + 150 + 500 = 800.
        assert_eq!(allocate(Some(900), &asks), vec![Some(0), Some(0), Some(1)]);
        // Enough for the speaker's top layer too.
        assert_eq!(allocate(Some(1500), &asks), vec![Some(0), Some(0), Some(2)]);
        // Room for two smallest layers: the speaker and the first other.
        assert_eq!(allocate(Some(300), &asks), vec![Some(0), None, Some(0)]);
        // Not even the speaker's smallest: everything paused.
        assert_eq!(allocate(Some(100), &asks), vec![None, None, None]);
    }

    #[test]
    fn a_stream_with_nothing_to_send_is_paused_and_costs_nothing() {
        let l = three();
        let none: Vec<LayerOption> = Vec::new();
        let asks = [
            Ask {
                priority: Some(1),
                max_height: 720,
                layers: &none,
            },
            Ask {
                priority: None,
                max_height: 720,
                layers: &l,
            },
        ];
        assert_eq!(allocate(Some(1200), &asks), vec![None, Some(2)]);
    }

    #[test]
    fn a_layer_taller_than_wanted_is_still_sent_when_it_is_the_only_one() {
        let single = vec![layer(0, 720, 1200)];
        let asks = [Ask {
            priority: None,
            max_height: 180,
            layers: &single,
        }];
        assert_eq!(allocate(None, &asks), vec![Some(0)]);
    }

    #[test]
    fn the_default_view_is_the_speaker_then_who_spoke_last() {
        let others: Vec<String> = (0..12).map(|i| format!("p{i}")).collect();
        let v = ViewRequest::default().resolve(&others);
        assert_eq!(v.len(), DEFAULT_MAX_STREAMS);
        assert_eq!(v[0].target, StreamTarget::Speaker);
        assert_eq!(v[0].max_height, Some(DEFAULT_SPEAKER_HEIGHT));
        assert_eq!(v[1].target, StreamTarget::Participant("p0".into()));
        assert_eq!(v[1].max_height, Some(DEFAULT_OTHER_HEIGHT));
        assert_eq!(v[8].target, StreamTarget::Participant("p7".into()));

        // An explicit view is taken as it is, repeats dropped, capped.
        let want = |id: &str| StreamWant {
            target: StreamTarget::Participant(id.into()),
            max_height: None,
            priority: None,
        };
        let v = ViewRequest {
            streams: Some(vec![want("a"), want("b"), want("a"), want("c")]),
            max_streams: Some(2),
        }
        .resolve(&others);
        assert_eq!(v, vec![want("a"), want("b")]);
    }

    fn stream(codec: VideoCodec) -> Stream {
        let (v, _rx) = Viewer::new(
            "viewer",
            &ForwardRequest {
                codec,
                profile: String::new(),
                payload_type: 96,
            },
            64,
            64,
        );
        v.new_stream(StreamTarget::Speaker, &[])
    }

    #[test]
    fn a_stream_moves_down_at_once_and_up_only_once_the_budget_holds() {
        let s = stream(VideoCodec::VP8);
        let alice = SourceKey::camera("alice");
        let t0 = Instant::now();
        let l = three();
        // The first layer is at once, and needs a keyframe.
        assert!(matches!(
            s.step(Some((alice.clone(), l[2].clone())), t0),
            Step::Select(_, _, true)
        ));
        assert_eq!(s.info().participant, None, "not before the keyframe");
        let key = RtpPacket::build(97, 1, 0, l[2].ssrc, Bytes::from_static(&[0x10, 0x00]), true);
        assert!(s.offer(&key, &alice, 96).is_some());
        assert_eq!(s.info().participant.as_deref(), Some("alice"));
        let other = RtpPacket::build(97, 1, 0, 999, Bytes::from_static(&[0x10, 0x00]), true);
        assert!(s.offer(&other, &alice, 96).is_none(), "another layer");
        // Down at once.
        assert!(matches!(
            s.step(Some((alice.clone(), l[0].clone())), t0),
            Step::Select(_, _, true)
        ));
        // Up waits.
        let up = Some((alice.clone(), l[1].clone()));
        assert!(matches!(s.step(up.clone(), t0), Step::Stay));
        assert!(matches!(
            s.step(up.clone(), t0 + STREAM_UP_AFTER / 2),
            Step::Stay
        ));
        assert!(matches!(
            s.step(up.clone(), t0 + STREAM_UP_AFTER),
            Step::Select(_, _, true)
        ));
        assert!(matches!(s.step(up, t0 + STREAM_UP_AFTER * 2), Step::Stay));
        // A pause is at once; coming out of it waits like any move up.
        assert!(matches!(s.step(None, t0), Step::Pause));
        assert!(s.info().paused);
        let back = Some((alice.clone(), l[0].clone()));
        assert!(matches!(s.step(back.clone(), t0), Step::Stay));
        assert!(matches!(
            s.step(back, t0 + STREAM_UP_AFTER),
            Step::Select(_, _, true)
        ));
        // A new person is at once.
        let bob = SourceKey::camera("bob");
        assert!(matches!(
            s.step(Some((bob.clone(), layer(0, 720, 1200))), t0),
            Step::Select(_, _, true)
        ));
        assert_eq!(s.routes(), vec![alice, bob]);
    }
}
