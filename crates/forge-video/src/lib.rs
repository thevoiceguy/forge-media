//! Forge Video — what a video mixer is made of, above RTP and below the
//! codecs.
//!
//! - [`frame`]: the [`VideoFrame`] type (host I420, or a handle to memory
//!   on a device) and the [`MediaDevice`] it lives on.
//! - [`scale`]: I420 scaling, letterboxing and blitting, and the
//!   [`Scaler`] trait a device backend implements.
//! - [`layout`]: grid, active-speaker, spotlight and picture-in-picture
//!   tile geometry.
//! - [`compose`]: the [`Compositor`] trait and the [`HostCompositor`]
//!   that draws a layout of sources onto a canvas, with name labels,
//!   speaking indicators and avatars.
//! - [`clock`]: the per-room [`VideoClock`] with overrun back-off.
//! - [`flavor`]: [`Flavor`], what a receiver consumes, and the table that
//!   lets subscribers with the same needs share one encoder.
//! - [`codec`]: the [`VideoDecoder`] / [`VideoEncoder`] traits every codec
//!   binding implements, and the registry that picks one per device.
//! - [`raw`]: an uncompressed "codec" so the whole pipeline is testable
//!   without native libraries.
//!
//! Design: FCP `docs/VIDEO_CONFERENCING.md`.

pub mod clock;
pub mod codec;
pub mod compose;
pub mod flavor;
pub mod font;
pub mod frame;
pub mod layout;
pub mod metrics;
pub mod raw;
pub mod scale;

pub use clock::{ClockEvent, VideoClock};
pub use codec::{
    CodecError, CodecRegistry, DecoderFactory, EncoderFactory, EncoderSettings, VideoDecoder,
    VideoEncoder,
};
pub use compose::{Compositor, HostCompositor, Theme, TileSource};
pub use flavor::{Flavor, FlavorTable};
pub use frame::{DeviceFrame, HostFrame, MediaDevice, Resolution, VideoFrame};
pub use layout::{Layout, Rect};
pub use raw::{RawDecoder, RawEncoder};
pub use scale::{HostScaler, Scaler};
