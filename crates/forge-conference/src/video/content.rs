//! Shared content (design §15.6, phase 7a): the second kind of stream a
//! participant can send, the *floor* that says whose screen the room is
//! showing, and the *views* a channel chooses between.
//!
//! A shared screen is not another camera. It is drawn large and plain,
//! it is never a tile the speaker election or a pin can claim, there is
//! one of it per room at a time, and a static one is not a lost one. So
//! it is a [`StreamKind`] of its own, sources and channels are keyed by
//! participant *and* kind ([`SourceKey`]), and every output names the
//! [`View`] it shows: the composite, the cameras alone, or the content
//! alone.

use std::fmt;
use std::time::{Duration, Instant};

use forge_core::VideoCodec;
use forge_video::frame::Resolution;

/// Which of a participant's streams: the camera, or a shared screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamKind {
    #[default]
    Camera,
    Content,
}

impl StreamKind {
    pub fn name(&self) -> &'static str {
        match self {
            StreamKind::Camera => "camera",
            StreamKind::Content => "content",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "camera" | "main" => Some(StreamKind::Camera),
            "content" | "slides" | "screen" => Some(StreamKind::Content),
            _ => None,
        }
    }
}

impl fmt::Display for StreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A source, or a channel: a participant and which of their streams.
///
/// Displays as the participant id for a camera and `id#content` for a
/// shared screen, which is what reaches logs and the API.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceKey {
    pub participant: String,
    pub kind: StreamKind,
}

impl SourceKey {
    pub fn new(participant: &str, kind: StreamKind) -> Self {
        Self {
            participant: participant.to_string(),
            kind,
        }
    }

    pub fn camera(participant: &str) -> Self {
        Self::new(participant, StreamKind::Camera)
    }

    pub fn content(participant: &str) -> Self {
        Self::new(participant, StreamKind::Content)
    }

    pub fn is_content(&self) -> bool {
        self.kind == StreamKind::Content
    }
}

impl fmt::Display for SourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            StreamKind::Camera => f.write_str(&self.participant),
            StreamKind::Content => write!(f, "{}#content", self.participant),
        }
    }
}

/// What an output shows.
///
/// An endpoint with one video section takes `Composite`, and so does a
/// recording. An endpoint with a content section of its own takes
/// `Cameras` on its main section and `Content` on the other, which is
/// what room systems and the join page expect: people in one picture,
/// the slides in another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum View {
    /// The meeting's layout, and the presentation arrangement (or the
    /// content alone, per [`ContentLayout`]) while a screen is shared.
    #[default]
    Composite,
    /// The meeting's layout, never the content.
    Cameras,
    /// The content alone, letterboxed, no chrome. Nothing at all while
    /// nobody is sharing.
    Content,
}

impl View {
    pub fn name(&self) -> &'static str {
        match self {
            View::Composite => "composite",
            View::Cameras => "cameras",
            View::Content => "content",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "composite" => Some(View::Composite),
            "cameras" => Some(View::Cameras),
            "content" => Some(View::Content),
            _ => None,
        }
    }
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How the composite shows a shared screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ContentLayout {
    /// The content large with up to five cameras in a strip.
    #[default]
    Presentation,
    /// The content alone, full canvas.
    ContentOnly,
}

impl ContentLayout {
    pub fn name(&self) -> &'static str {
        match self {
            ContentLayout::Presentation => "presentation",
            ContentLayout::ContentOnly => "content_only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "presentation" => Some(ContentLayout::Presentation),
            "content_only" | "content" => Some(ContentLayout::ContentOnly),
            _ => None,
        }
    }
}

impl fmt::Display for ContentLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a share ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentStop {
    /// The presenter stopped: the content section went away.
    Ended,
    /// No packets for the room's idle timeout.
    Idle,
    /// A host stopped it.
    Host,
    /// Another node's presenter took the room-wide floor (§15.6, 7d).
    Replaced,
    /// The presenter left the room.
    Left,
    /// The decoder gave up on the stream.
    Failed,
}

impl ContentStop {
    pub fn name(&self) -> &'static str {
        match self {
            ContentStop::Ended => "ended",
            ContentStop::Idle => "idle",
            ContentStop::Host => "host",
            ContentStop::Replaced => "replaced",
            ContentStop::Left => "left",
            ContentStop::Failed => "failed",
        }
    }
}

impl fmt::Display for ContentStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a share was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentRefusal {
    /// Someone else holds the floor.
    Held(String),
    /// The room does not allow sharing.
    Disabled,
    /// A host stopped this participant's share and has not let them
    /// back in.
    ParticipantDisabled,
    /// The participant has no content source to grant the floor to.
    NoSource,
    /// The node's [`ContentGate`] said no (out of budget).
    Budget,
}

impl ContentRefusal {
    pub fn name(&self) -> &'static str {
        match self {
            ContentRefusal::Held(_) => "held",
            ContentRefusal::Disabled => "disabled",
            ContentRefusal::ParticipantDisabled => "participant_disabled",
            ContentRefusal::NoSource => "no_source",
            ContentRefusal::Budget => "budget",
        }
    }
}

impl fmt::Display for ContentRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentRefusal::Held(by) => write!(f, "the floor is held by {by}"),
            ContentRefusal::Disabled => f.write_str("the room does not allow sharing"),
            ContentRefusal::ParticipantDisabled => {
                f.write_str("a host stopped this participant's share")
            }
            ContentRefusal::NoSource => f.write_str("no content source"),
            ContentRefusal::Budget => f.write_str("the node cannot afford another decode"),
        }
    }
}

/// What happened to a share, for [`VideoRoomEvent::Content`].
///
/// [`VideoRoomEvent::Content`]: super::VideoRoomEvent::Content
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentEvent {
    /// The participant took the floor.
    Started,
    /// The participant gave up or lost the floor.
    Stopped(ContentStop),
    /// The participant tried to share and was refused. Sent once per
    /// participant per floor, not per packet.
    Refused(ContentRefusal),
}

/// Who holds the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFloor {
    pub participant_id: String,
    pub since: Instant,
    /// A peer node's content arriving over a trunk rather than a caller
    /// on this node.
    pub remote: bool,
}

/// The room's shared content, for the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentInfo {
    pub participant_id: String,
    pub display_name: String,
    pub remote: bool,
    /// How long the floor has been held.
    pub held_for: Duration,
    pub codec: VideoCodec,
    /// The size of the last frame decoded, or 0×0 before the first.
    pub resolution: Resolution,
    pub fps: u32,
    /// Whether a frame has been decoded: the composite is showing it.
    pub live: bool,
}

/// Whether the node can afford a share. The room asks before it grants
/// the floor, so a grant costs a decode's worth of budget rather than
/// the room slowing (§15.6). Absent, everything is admitted.
pub trait ContentGate: Send + Sync {
    fn admit(&self, room: &str, participant: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_views_and_layouts_round_trip_through_their_names() {
        for k in [StreamKind::Camera, StreamKind::Content] {
            assert_eq!(StreamKind::parse(k.name()), Some(k));
        }
        assert_eq!(StreamKind::parse("slides"), Some(StreamKind::Content));
        assert_eq!(StreamKind::parse("audio"), None);
        assert_eq!(StreamKind::default(), StreamKind::Camera);
        for v in [View::Composite, View::Cameras, View::Content] {
            assert_eq!(View::parse(v.name()), Some(v));
        }
        assert_eq!(View::parse("everything"), None);
        for l in [ContentLayout::Presentation, ContentLayout::ContentOnly] {
            assert_eq!(ContentLayout::parse(l.name()), Some(l));
        }
        assert_eq!(
            ContentLayout::parse("content-only"),
            Some(ContentLayout::ContentOnly)
        );
        assert_eq!(ContentLayout::parse("grid"), None);
    }

    #[test]
    fn a_source_key_names_the_stream_the_way_people_read_it() {
        assert_eq!(SourceKey::camera("alice").to_string(), "alice");
        assert_eq!(SourceKey::content("alice").to_string(), "alice#content");
        assert!(SourceKey::content("alice").is_content());
        assert!(!SourceKey::camera("alice").is_content());
        assert_ne!(SourceKey::camera("alice"), SourceKey::content("alice"));
        // Cameras sort before content for the same participant.
        assert!(SourceKey::camera("alice") < SourceKey::content("alice"));
        assert_eq!(ContentStop::Idle.to_string(), "idle");
        assert_eq!(ContentRefusal::Held("bob".into()).name(), "held");
        assert_eq!(
            ContentRefusal::Held("bob".into()).to_string(),
            "the floor is held by bob"
        );
    }
}
