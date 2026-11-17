//! Bandwidth estimation for video, over one Google Congestion Control core
//! (draft-ietf-rmcat-gcc-02, `docs/TRANSPORT_CC.md` in FCP):
//! - the receive side: [`RembEstimator`], whose estimate a receiver sends
//!   as REMB; [`FeedbackWriter`], which reports arrivals as transport-cc
//!   feedback for the sender to estimate from;
//! - the send side: [`SendSideEstimator`], from that feedback, and
//!   [`Prober`], which paces probes past what is being sent;
//! - [`testing::Bottleneck`], a simulated link on a virtual clock.

pub mod feedback;
pub mod gcc;
pub mod probe;
pub mod remb;
pub mod send_side;
pub mod testing;

pub use feedback::{FeedbackWriter, FEEDBACK_INTERVAL};
pub use gcc::Usage;
pub use probe::{Prober, PROBE_INTERVAL, PROBE_PADDING};
pub use remb::RembEstimator;
pub use send_side::SendSideEstimator;

use std::time::Instant;

/// The first estimate, before anything is known: above what a camera
/// sends by default, so it limits nothing until the network says so.
pub const START_BPS: u64 = 2_500_000;
/// Never below this: a picture of some kind gets through.
pub const MIN_BPS: u64 = 100_000;

/// A receive-side estimator: watches what arrives and says what the far
/// end may send.
pub trait BandwidthEstimator: Send {
    /// An RTP packet arrived: its stream, sequence number, RTP timestamp
    /// (on a 90 kHz clock), size on the wire and arrival.
    fn on_packet(&mut self, ssrc: u32, seq: u16, timestamp: u32, size: usize, at: Instant);
    /// A stream ended or restarted (hold): its history is no guide.
    fn forget(&mut self, ssrc: u32);
    /// What the far end may send, in bits a second, as of `now`; `None`
    /// until there is enough to say.
    fn estimate(&mut self, now: Instant) -> Option<u64>;
}
