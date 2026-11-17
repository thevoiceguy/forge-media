//! The audience's audio: mixed once, encoded once, sent to many
//! (FCP's `docs/WEBINARS.md` §2.2).
//!
//! A listener ([`ConferenceRoom::add_listener`](crate::ConferenceRoom::add_listener))
//! contributes nothing, so every listener hears the same thing: the mix of
//! everyone who is heard. The room makes that mix once per frame and
//! encodes it once per *flavor* — a codec at a rate and packet time, named
//! by whoever subscribes — and hands each subscriber the same payload.
//! A subscriber puts its own SSRC, sequence and timestamp on it and
//! encrypts it for its own leg, as a forwarding viewer does for video.
//!
//! A flavor lives while anyone listens to it: the first subscriber's
//! encoder is the flavor's, and the flavor goes with its last subscriber.

use bytes::Bytes;
use metrics::{gauge, histogram};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::broadcast;

/// Frames a slow subscriber may fall behind by before it misses some:
/// one second of 20 ms frames.
const AUDIENCE_BACKLOG: usize = 50;

/// Encodes the room's mix for one flavor.
pub trait AudienceEncoder: Send {
    /// One frame of the room's mix, at the room's sample rate, encoded.
    /// `None` when it could not be: subscribers then send nothing for the
    /// frame and move their timestamp on.
    fn encode(&mut self, pcm: &[i16]) -> Option<Bytes>;
}

/// One frame of the audience's audio.
#[derive(Debug, Clone)]
pub struct AudienceFrame {
    /// The room's frame-clock sequence the frame was mixed at.
    pub seq: u64,
    /// The encoded frame, or `None` when nobody was heard (nothing to
    /// send; the timestamp still moves on a frame).
    pub payload: Option<Bytes>,
}

/// A flavor as it is now, for a status page or a measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudienceFlavorInfo {
    /// The name its subscribers gave it.
    pub flavor: String,
    /// How many subscribe to it.
    pub subscribers: usize,
    /// Frames encoded for it so far.
    pub frames_encoded: u64,
}

struct Flavor {
    encoder: Box<dyn AudienceEncoder>,
    tx: broadcast::Sender<AudienceFrame>,
    frames_encoded: u64,
}

/// The room's audience feeds.
pub(crate) struct AudienceMix {
    room_id: String,
    flavors: Mutex<HashMap<String, Flavor>>,
}

impl AudienceMix {
    pub(crate) fn new(room_id: String) -> Self {
        Self {
            room_id,
            flavors: Mutex::new(HashMap::new()),
        }
    }

    /// Subscribe to `flavor`, making it with `make`'s encoder when nobody
    /// listens to it yet.
    pub(crate) fn subscribe(
        &self,
        flavor: &str,
        make: impl FnOnce() -> Box<dyn AudienceEncoder>,
    ) -> broadcast::Receiver<AudienceFrame> {
        let mut flavors = self.flavors.lock();
        if let Some(f) = flavors.get(flavor) {
            if f.tx.receiver_count() > 0 {
                return f.tx.subscribe();
            }
        }
        let (tx, rx) = broadcast::channel(AUDIENCE_BACKLOG);
        flavors.insert(
            flavor.to_string(),
            Flavor {
                encoder: make(),
                tx,
                frames_encoded: 0,
            },
        );
        rx
    }

    /// Whether anyone subscribes to any flavor.
    pub(crate) fn is_empty(&self) -> bool {
        self.flavors.lock().is_empty()
    }

    /// Feed frame `seq`: `mix` is called once, and only when someone
    /// listens; each flavor encodes it once.
    pub(crate) fn feed(&self, seq: u64, mix: impl FnOnce() -> Option<Vec<i16>>) {
        let mut flavors = self.flavors.lock();
        flavors.retain(|_, f| f.tx.receiver_count() > 0);
        let subscribers: usize = flavors.values().map(|f| f.tx.receiver_count()).sum();
        gauge!("forge_conference_audience_subscribers", "room_id" => self.room_id.clone())
            .set(subscribers as f64);
        if flavors.is_empty() {
            return;
        }
        let started = Instant::now();
        let pcm = mix();
        for f in flavors.values_mut() {
            let payload = pcm.as_deref().and_then(|pcm| f.encoder.encode(pcm));
            if payload.is_some() {
                f.frames_encoded += 1;
            }
            // A send fails only with no receivers, and those go next frame.
            let _ = f.tx.send(AudienceFrame { seq, payload });
        }
        histogram!("forge_conference_audience_frame_seconds", "room_id" => self.room_id.clone())
            .record(started.elapsed().as_secs_f64());
    }

    pub(crate) fn info(&self) -> Vec<AudienceFlavorInfo> {
        let mut out: Vec<AudienceFlavorInfo> = self
            .flavors
            .lock()
            .iter()
            .filter(|(_, f)| f.tx.receiver_count() > 0)
            .map(|(name, f)| AudienceFlavorInfo {
                flavor: name.clone(),
                subscribers: f.tx.receiver_count(),
                frames_encoded: f.frames_encoded,
            })
            .collect();
        out.sort_by(|a, b| a.flavor.cmp(&b.flavor));
        out
    }
}
