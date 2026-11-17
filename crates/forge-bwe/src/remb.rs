//! The receive-side estimator REMB carries, as Anvil has run it since its
//! first video calls (moved here from `anvil-core/src/video/bwe.rs`):
//! - **delay**: each frame's arrival spacing against its RTP spacing, per
//!   stream, through a [`Trendline`];
//! - **loss**: packets missing on first arrival (a retransmission does
//!   not hide a loss from it);
//! - **rate**: [`Aimd`], never far above what actually arrives, and
//!   [`LossCap`].

use crate::gcc::{Aimd, Headroom, LossCap, Trendline, Usage};
use crate::{BandwidthEstimator, MIN_BPS, START_BPS};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// What arrives is measured over this long.
const RATE_WINDOW: Duration = Duration::from_millis(1000);
/// Loss is counted over this long.
const LOSS_WINDOW: Duration = Duration::from_millis(1000);
/// The rate stays within this multiple of what arrives (GCC's 1.5), plus
/// a little so a quiet sender can still climb.
const HEADROOM: Headroom = Headroom::Cut {
    factor: 1.5,
    bps: 50_000.0,
};

/// One stream's frames and delay trend.
struct Stream {
    /// The frame being received: its RTP timestamp and last arrival.
    current: Option<(u32, Instant)>,
    /// The last complete frame.
    previous: Option<(u32, Instant)>,
    trend: Trendline,
    /// The highest sequence number seen, for loss.
    highest: Option<u16>,
}

impl Stream {
    fn new() -> Self {
        Self {
            current: None,
            previous: None,
            trend: Trendline::default(),
            highest: None,
        }
    }

    /// A packet of frame `ts` arrived at `at`; `base` is the clock's zero.
    fn on_packet(&mut self, ts: u32, at: Instant, base: Instant) {
        match self.current {
            Some((cur, _)) if cur == ts => self.current = Some((ts, at)),
            // An older frame's straggler (a retransmission): no delta.
            Some((cur, _)) if (ts.wrapping_sub(cur) as i32) < 0 => {}
            Some(done) => {
                if let Some((ts0, at0)) = self.previous {
                    let (ts1, at1) = done;
                    let send_ms = f64::from(ts1.wrapping_sub(ts0)) / 90.0;
                    let arrive_ms = at1.saturating_duration_since(at0).as_secs_f64() * 1000.0;
                    let x = at1.saturating_duration_since(base).as_secs_f64() * 1000.0;
                    self.trend.update(send_ms, arrive_ms, x);
                }
                self.previous = Some(done);
                self.current = Some((ts, at));
            }
            None => self.current = Some((ts, at)),
        }
    }
}

/// The receive-side estimator REMB carries.
pub struct RembEstimator {
    streams: HashMap<u32, Stream>,
    /// (arrival, bytes) over the last [`RATE_WINDOW`].
    arrivals: VecDeque<(Instant, usize)>,
    arrived_bytes: usize,
    /// (when, expected, lost) per packet slot, over [`LOSS_WINDOW`].
    losses: VecDeque<(Instant, u32, u32)>,
    aimd: Aimd,
    loss_cap: LossCap,
    last_update: Option<Instant>,
    first_packet: Option<Instant>,
}

impl Default for RembEstimator {
    fn default() -> Self {
        Self {
            streams: HashMap::new(),
            arrivals: VecDeque::new(),
            arrived_bytes: 0,
            losses: VecDeque::new(),
            aimd: Aimd::new(START_BPS as f64, HEADROOM),
            loss_cap: LossCap::default(),
            last_update: None,
            first_packet: None,
        }
    }
}

impl RembEstimator {
    /// What arrived over the last second, in bits a second.
    fn incoming_bps(&self) -> f64 {
        self.arrived_bytes as f64 * 8.0 / RATE_WINDOW.as_secs_f64()
    }

    /// Of the packets due over the last second, the share that never came.
    pub fn loss(&self) -> f64 {
        let (expected, lost) = self
            .losses
            .iter()
            .fold((0u32, 0u32), |(e, l), (_, de, dl)| (e + de, l + dl));
        if expected == 0 {
            0.0
        } else {
            f64::from(lost) / f64::from(expected)
        }
    }

    fn usage(&self) -> Usage {
        if self
            .streams
            .values()
            .any(|d| d.trend.usage() == Usage::Over)
        {
            Usage::Over
        } else if self
            .streams
            .values()
            .any(|d| d.trend.usage() == Usage::Under)
        {
            Usage::Under
        } else {
            Usage::Normal
        }
    }

    fn expire(&mut self, now: Instant) {
        while let Some(&(t, bytes)) = self.arrivals.front() {
            if now.saturating_duration_since(t) <= RATE_WINDOW {
                break;
            }
            self.arrived_bytes -= bytes;
            self.arrivals.pop_front();
        }
        while let Some(&(t, _, _)) = self.losses.front() {
            if now.saturating_duration_since(t) <= LOSS_WINDOW {
                break;
            }
            self.losses.pop_front();
        }
    }
}

impl BandwidthEstimator for RembEstimator {
    fn on_packet(&mut self, ssrc: u32, seq: u16, timestamp: u32, size: usize, at: Instant) {
        let base = *self.first_packet.get_or_insert(at);
        self.arrivals.push_back((at, size));
        self.arrived_bytes += size;
        let d = self.streams.entry(ssrc).or_insert_with(Stream::new);
        // Loss: a jump forward leaves a gap; anything at or behind the
        // highest is a retransmission or a reordering, and counts for
        // nothing either way.
        match d.highest {
            None => {
                d.highest = Some(seq);
                self.losses.push_back((at, 1, 0));
            }
            Some(h) => {
                let ahead = seq.wrapping_sub(h);
                if ahead != 0 && ahead < 0x8000 {
                    d.highest = Some(seq);
                    // A jump of thousands is a restart, not a loss.
                    let gap = if ahead > 1000 {
                        0
                    } else {
                        u32::from(ahead) - 1
                    };
                    self.losses.push_back((at, gap + 1, gap));
                }
            }
        }
        d.on_packet(timestamp, at, base);
        self.expire(at);
    }

    fn forget(&mut self, ssrc: u32) {
        self.streams.remove(&ssrc);
    }

    fn estimate(&mut self, now: Instant) -> Option<u64> {
        self.expire(now);
        let first = self.first_packet?;
        let dt = self
            .last_update
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64())
            .min(1.0);
        self.last_update = Some(now);
        // A second of arrivals before anything is said of them.
        if now.saturating_duration_since(first) < RATE_WINDOW {
            return None;
        }
        let incoming = self.incoming_bps();
        let usage = self.usage();
        let rate = self.aimd.update(usage, incoming, dt);
        let rate = self.loss_cap.update(self.loss(), rate, incoming, dt);
        self.aimd.rate = self.aimd.rate.max(MIN_BPS as f64);
        Some((rate as u64).max(MIN_BPS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` of a 30 fps stream at `kbps`, each frame's last packet
    /// arriving `extra(i)` ms after its time.
    fn feed(
        e: &mut RembEstimator,
        start: Instant,
        frames: std::ops::Range<u32>,
        kbps: u32,
        mut extra: impl FnMut(u32) -> f64,
        mut drop: impl FnMut(u16) -> bool,
        seq: &mut u16,
    ) -> Vec<u64> {
        let per_frame = (kbps as usize * 1000 / 8 / 30).max(200);
        let packets = per_frame.div_ceil(1100);
        let mut estimates = Vec::new();
        for i in frames {
            let ts = i * 3000;
            let at = start
                + Duration::from_micros(u64::from(i) * 33_333)
                + Duration::from_secs_f64(extra(i).max(0.0) / 1000.0);
            for _ in 0..packets {
                if !drop(*seq) {
                    e.on_packet(1, *seq, ts, per_frame / packets, at);
                }
                *seq = seq.wrapping_add(1);
            }
            if i % 6 == 0 {
                if let Some(x) = e.estimate(at) {
                    estimates.push(x);
                }
            }
        }
        estimates
    }

    #[test]
    fn a_clear_path_limits_nothing_a_sender_needs() {
        let mut e = RembEstimator::default();
        let mut seq = 0;
        let est = feed(
            &mut e,
            Instant::now(),
            0..300,
            1500,
            |_| 0.0,
            |_| false,
            &mut seq,
        );
        let last = *est.last().unwrap();
        assert!(last >= 1_500_000, "a clean 1.5 Mb/s is allowed: {last}");
    }

    #[test]
    fn a_growing_queue_is_overuse_and_brings_the_estimate_under_what_arrives() {
        let mut e = RembEstimator::default();
        let mut seq = 0;
        let start = Instant::now();
        feed(&mut e, start, 0..90, 1500, |_| 0.0, |_| false, &mut seq);
        // From frame 90, each frame waits 4 ms longer than the last: a
        // bottleneck filling.
        let est = feed(
            &mut e,
            start,
            90..150,
            1500,
            |i| f64::from(i - 90) * 4.0,
            |_| false,
            &mut seq,
        );
        let last = *est.last().unwrap();
        assert!(last < 1_500_000, "overuse lowers it: {est:?}");
    }

    #[test]
    fn heavy_loss_brings_it_down_and_light_loss_does_not() {
        let mut e = RembEstimator::default();
        let mut seq = 0;
        let start = Instant::now();
        let est = feed(
            &mut e,
            start,
            0..300,
            1500,
            |_| 0.0,
            |s| s % 50 == 0,
            &mut seq,
        );
        assert!(*est.last().unwrap() >= 1_500_000, "2% loss: {est:?}");
        let est = feed(
            &mut e,
            start,
            300..450,
            1500,
            |_| 0.0,
            |s| s % 5 == 0,
            &mut seq,
        );
        assert!(*est.last().unwrap() < 1_200_000, "20% loss: {est:?}");
        assert!(e.loss() > 0.1);
    }

    #[test]
    fn a_retransmission_does_not_hide_a_loss() {
        let mut e = RembEstimator::default();
        let now = Instant::now();
        for seq in [1u16, 2, 4, 5] {
            e.on_packet(1, seq, 0, 1000, now);
        }
        e.on_packet(1, 3, 0, 1000, now);
        assert!((e.loss() - 0.2).abs() < 1e-9, "{}", e.loss());
    }
}
