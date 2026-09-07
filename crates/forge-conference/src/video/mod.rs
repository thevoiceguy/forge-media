//! Conference video: the [`VideoRoom`] beside the audio room.
//!
//! Design: FCP `docs/VIDEO_CONFERENCING.md`. The room is transport-free
//! like the audio room — the conference server owns the sockets and
//! SRTP, feeds RTP in with [`VideoRoom::push_rtp`] and drains each
//! subscriber's packets from its [`VideoSubscription`] — and codec work
//! runs on the shared [`CodecPool`], never on tokio workers.

pub mod egress;
pub mod pool;
pub mod room;
pub mod source;
pub mod speaker;

pub use egress::{default_kbps, OutputKey, SubscriberStats, VideoSubscription};
pub use pool::CodecPool;
pub use room::{
    SubscribeRequest, VideoBackend, VideoFlavorInfo, VideoOutputInfo, VideoParticipantInfo,
    VideoRoom, VideoRoomEvent, VideoRoomSettings, VideoRoomStatus, VideoSourceInfo, VideoState,
    VideoSubscriberInfo,
};
pub use source::{SourceLimits, SourceStats};
pub use speaker::{ActiveSpeaker, Level};
