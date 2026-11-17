//! The send side of transport-cc: what was sent when, paired with when
//! the receiver says it arrived, through the GCC core.

use crate::gcc::{Aimd, Headroom, LossCap, Trendline, Usage};
use crate::{MIN_BPS, START_BPS};
use forge_rtp::transport_cc::{TransportCcFeedback, REFERENCE_TICK_US};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Packets sent within this long of a group's first form one group (GCC
/// §5.2): a frame, or a burst of frames, leaves as one.
const GROUP_SPAN: Duration = Duration::from_millis(5);
/// A packet sent longer ago than this and never reported is forgotten.
const HISTORY: Duration = Duration::from_secs(2);
/// What got through is measured over this long of arrivals, in µs:
/// shorter than a receiver's second, since a cut is made from it.
const THROUGH_WINDOW_US: i64 = 500_000;
/// Loss is counted over this long of feedback.
const LOSS_WINDOW: Duration = Duration::from_millis(1000);
/// Feedback older than this and the estimate is stale.
pub const FRESH: Duration = Duration::from_secs(1);
/// The rate may not rise past 1.5× what gets through, plus a little — but
/// a probe lifts it past that, and it is never cut to it.
const HEADROOM: Headroom = Headroom::Hold {
    factor: 1.5,
    bps: 10_000.0,
};
/// A probe whose packets arrive slower than this share of how fast they
/// were sent found the path's limit; the result is a little under it.
const SATURATED: f64 = 0.9;
const PROBE_UTILISATION: f64 = 0.95;
/// A probe needs this many of its packets back to say anything.
const PROBE_MIN_PACKETS: usize = 4;

struct Sent {
    at: Instant,
    size: usize,
    probe: Option<u32>,
}

#[derive(Clone, Copy)]
struct Group {
    first_send: Instant,
    last_send: Instant,
    last_arrival_us: i64,
}

#[derive(Default)]
struct Cluster {
    sent: Vec<(Instant, usize)>,
    /// (arrival µs, size) of those reported received.
    arrived: Vec<(i64, usize)>,
    reported: usize,
}

/// The send-side estimate of a path, from transport-cc feedback.
pub struct SendSideEstimator {
    history: BTreeMap<i64, Sent>,
    highest_sent: Option<i64>,
    trend: Trendline,
    aimd: Aimd,
    loss_cap: LossCap,
    /// (arrival µs, bytes) of packets reported received.
    acked: VecDeque<(i64, usize)>,
    acked_bytes: usize,
    /// (when, reported, lost) per feedback.
    losses: VecDeque<(Instant, u32, u32)>,
    group: Option<Group>,
    previous: Option<Group>,
    last_feedback: Option<Instant>,
    last_update: Option<Instant>,
    /// The receiver's reference time, unwrapped, and the last one raw.
    reference: Option<(i64, i32)>,
    clusters: HashMap<u32, Cluster>,
    /// The last probe's result, in bits a second.
    last_probe: Option<u64>,
    estimate: Option<u64>,
}

impl Default for SendSideEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl SendSideEstimator {
    pub fn new() -> Self {
        Self {
            history: BTreeMap::new(),
            highest_sent: None,
            trend: Trendline::default(),
            aimd: Aimd::new(START_BPS as f64, HEADROOM),
            loss_cap: LossCap::default(),
            acked: VecDeque::new(),
            acked_bytes: 0,
            losses: VecDeque::new(),
            group: None,
            previous: None,
            last_feedback: None,
            last_update: None,
            reference: None,
            clusters: HashMap::new(),
            last_probe: None,
            estimate: None,
        }
    }

    fn unwrap(&self, seq: u16) -> i64 {
        match self.highest_sent {
            None => i64::from(seq),
            Some(h) => h + i64::from(seq.wrapping_sub(h as u16) as i16),
        }
    }

    /// Packet `seq`, `size` bytes on the wire, left at `at`; `probe` names
    /// the probe cluster it belongs to.
    pub fn on_sent(&mut self, seq: u16, size: usize, at: Instant, probe: Option<u32>) {
        let s = self.unwrap(seq);
        self.highest_sent = Some(self.highest_sent.map_or(s, |h| h.max(s)));
        self.history.insert(s, Sent { at, size, probe });
        if let Some(id) = probe {
            self.clusters.entry(id).or_default().sent.push((at, size));
        }
        while let Some((&first, sent)) = self.history.iter().next() {
            if at.saturating_duration_since(sent.at) <= HISTORY {
                break;
            }
            self.history.remove(&first);
        }
        // A probe nobody reported on is let go with its packets.
        self.clusters.retain(|_, c| {
            c.sent
                .last()
                .is_some_and(|(t, _)| at.saturating_duration_since(*t) <= HISTORY)
        });
    }

    /// The receiver's feedback arrived at `now`.
    pub fn on_feedback(&mut self, fb: &TransportCcFeedback, now: Instant) {
        // The reference time is 24 bits that wrap; keep it running on.
        let raw = fb.reference_time;
        let unwrapped = match self.reference {
            None => i64::from(raw),
            Some((prev, prev_raw)) => {
                let diff = (i64::from(raw) - i64::from(prev_raw)).rem_euclid(1 << 24);
                let diff = if diff >= 1 << 23 {
                    diff - (1 << 24)
                } else {
                    diff
                };
                prev + diff
            }
        };
        self.reference = Some((unwrapped, raw));
        let shift = (unwrapped - i64::from(raw)) * REFERENCE_TICK_US;

        let (mut reported, mut lost) = (0u32, 0u32);
        let mut finished: Vec<u32> = Vec::new();
        for (seq, arrival) in fb.arrivals() {
            let s = self.unwrap(seq);
            let Some(sent) = self.history.remove(&s) else {
                continue;
            };
            reported += 1;
            if let Some(id) = sent.probe {
                if let Some(c) = self.clusters.get_mut(&id) {
                    c.reported += 1;
                    if let Some(a) = arrival {
                        c.arrived.push((a + shift, sent.size));
                    }
                    if c.reported >= c.sent.len() {
                        finished.push(id);
                    }
                }
            }
            let Some(arrival) = arrival.map(|a| a + shift) else {
                lost += 1;
                continue;
            };
            self.acked.push_back((arrival, sent.size));
            self.acked_bytes += sent.size;
            // A probe is meant to fill the path for a moment: its own
            // delay is its result, not overuse.
            if sent.probe.is_none() {
                self.on_arrival(sent.at, arrival);
            }
        }
        if reported == 0 {
            return;
        }
        self.losses.push_back((now, reported, lost));
        while let Some(&(t, _, _)) = self.losses.front() {
            if now.saturating_duration_since(t) <= LOSS_WINDOW {
                break;
            }
            self.losses.pop_front();
        }
        let newest = self.acked.back().map_or(0, |(a, _)| *a);
        while let Some(&(a, bytes)) = self.acked.front() {
            if newest - a <= THROUGH_WINDOW_US {
                break;
            }
            self.acked_bytes -= bytes;
            self.acked.pop_front();
        }
        self.last_feedback = Some(now);
        self.update(now, finished);
    }

    /// A packet sent at `sent` arrived at `arrival_us`, in sequence order.
    fn on_arrival(&mut self, sent: Instant, arrival_us: i64) {
        match self.group {
            None => {
                self.group = Some(Group {
                    first_send: sent,
                    last_send: sent,
                    last_arrival_us: arrival_us,
                })
            }
            Some(ref mut g) if sent.saturating_duration_since(g.first_send) <= GROUP_SPAN => {
                g.last_send = g.last_send.max(sent);
                g.last_arrival_us = g.last_arrival_us.max(arrival_us);
            }
            Some(done) => {
                if let Some(prev) = self.previous {
                    let send_ms = done
                        .last_send
                        .saturating_duration_since(prev.last_send)
                        .as_secs_f64()
                        * 1000.0;
                    let arrive_ms = (done.last_arrival_us - prev.last_arrival_us) as f64 / 1000.0;
                    self.trend
                        .update(send_ms, arrive_ms, done.last_arrival_us as f64 / 1000.0);
                }
                self.previous = Some(done);
                self.group = Some(Group {
                    first_send: sent,
                    last_send: sent,
                    last_arrival_us: arrival_us,
                });
            }
        }
    }

    fn update(&mut self, now: Instant, finished: Vec<u32>) {
        let dt = self
            .last_update
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64())
            .min(1.0);
        self.last_update = Some(now);
        let through = self.acked_bytes as f64 * 8.0 / (THROUGH_WINDOW_US as f64 / 1e6);
        let usage = self.trend.usage();
        self.aimd.update(usage, through, dt);
        for id in finished {
            if let Some(c) = self.clusters.remove(&id) {
                if let Some(bps) = probe_result(&c) {
                    self.last_probe = Some(bps as u64);
                    self.aimd.probed(bps, usage);
                }
            }
        }
        self.aimd.rate = self.aimd.rate.max(MIN_BPS as f64);
        let rate = self
            .loss_cap
            .update(self.loss(), self.aimd.rate, through, dt);
        self.estimate = Some((rate as u64).max(MIN_BPS));
    }

    /// Of the packets reported over the last second, the share lost.
    pub fn loss(&self) -> f64 {
        let (n, l) = self
            .losses
            .iter()
            .fold((0u32, 0u32), |(n, l), (_, dn, dl)| (n + dn, l + dl));
        if n == 0 {
            0.0
        } else {
            f64::from(l) / f64::from(n)
        }
    }

    /// The estimate, in bits a second: `None` before any feedback.
    pub fn estimate(&self) -> Option<u64> {
        self.estimate
    }

    /// Whether feedback has arrived within [`FRESH`] of `now`.
    pub fn fresh(&self, now: Instant) -> bool {
        self.last_feedback
            .is_some_and(|t| now.saturating_duration_since(t) <= FRESH)
    }

    /// The last probe's result, in bits a second.
    pub fn last_probe(&self) -> Option<u64> {
        self.last_probe
    }

    /// What the delay says now.
    pub fn usage(&self) -> Usage {
        self.trend.usage()
    }
}

/// What a probe cluster found the path carries: the slower of how fast it
/// was sent and how fast it arrived, or a little under the arrival rate
/// when the path held it back.
fn probe_result(c: &Cluster) -> Option<f64> {
    if c.arrived.len() < PROBE_MIN_PACKETS || c.sent.len() < 2 {
        return None;
    }
    let send_span = c
        .sent
        .last()?
        .0
        .saturating_duration_since(c.sent.first()?.0)
        .as_secs_f64();
    let mut arrived = c.arrived.clone();
    arrived.sort_unstable_by_key(|(a, _)| *a);
    let recv_span = (arrived.last()?.0 - arrived.first()?.0) as f64 / 1e6;
    if send_span <= 0.001 || recv_span <= 0.001 {
        return None;
    }
    // Rates over the spans: each counts the bytes after its first packet.
    let sent_bytes: usize = c.sent.iter().skip(1).map(|(_, s)| s).sum();
    let recv_bytes: usize = arrived.iter().skip(1).map(|(_, s)| s).sum();
    let send_bps = sent_bytes as f64 * 8.0 / send_span;
    let recv_bps = recv_bytes as f64 * 8.0 / recv_span;
    Some(if recv_bps < SATURATED * send_bps {
        PROBE_UTILISATION * recv_bps
    } else {
        send_bps.min(recv_bps)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_rtp::transport_cc::PacketStatus;

    /// Feedback for packets `from..to`, each arriving `spacing_us` after
    /// the last, those `lost` says missing.
    fn feedback(
        from: u16,
        to: u16,
        spacing_us: i64,
        lost: impl Fn(u16) -> bool,
    ) -> TransportCcFeedback {
        let mut packets = Vec::new();
        let mut first = true;
        for s in from..to {
            if lost(s) {
                packets.push(PacketStatus::NotReceived);
            } else {
                packets.push(PacketStatus::Received(if first {
                    0
                } else {
                    (spacing_us / 250) as i16
                }));
                first = false;
            }
        }
        TransportCcFeedback {
            base_sequence: from,
            reference_time: 0,
            feedback_count: 0,
            packets,
        }
    }

    #[test]
    fn loss_reported_by_feedback_is_counted() {
        let mut e = SendSideEstimator::new();
        let t0 = Instant::now();
        for s in 0..10u16 {
            e.on_sent(s, 1000, t0 + Duration::from_millis(u64::from(s)), None);
        }
        e.on_feedback(
            &feedback(0, 10, 1000, |s| s % 5 == 0),
            t0 + Duration::from_millis(50),
        );
        assert!((e.loss() - 0.2).abs() < 1e-9, "{}", e.loss());
        assert!(e.fresh(t0 + Duration::from_millis(500)));
        assert!(!e.fresh(t0 + Duration::from_secs(3)));
        assert!(e.estimate().is_some());
    }

    #[test]
    fn a_probe_that_arrives_as_fast_as_it_was_sent_lifts_the_estimate() {
        let mut e = SendSideEstimator::new();
        let t0 = Instant::now();
        // 10 packets of 1250 bytes 2 ms apart: 5 Mb/s, arriving the same.
        for s in 0..10u16 {
            e.on_sent(
                s,
                1250,
                t0 + Duration::from_millis(2 * u64::from(s)),
                Some(1),
            );
        }
        e.on_feedback(
            &feedback(0, 10, 2000, |_| false),
            t0 + Duration::from_millis(60),
        );
        let p = e.last_probe().unwrap();
        assert!((4_900_000..=5_100_000).contains(&p), "{p}");
        assert!(e.estimate().unwrap() >= 4_900_000);
    }

    #[test]
    fn a_probe_the_path_held_back_finds_its_limit() {
        let mut e = SendSideEstimator::new();
        let t0 = Instant::now();
        // Sent at 5 Mb/s, arriving at 1.25 Mb/s (8 ms apart).
        for s in 0..10u16 {
            e.on_sent(
                s,
                1250,
                t0 + Duration::from_millis(2 * u64::from(s)),
                Some(7),
            );
        }
        e.on_feedback(
            &feedback(0, 10, 8000, |_| false),
            t0 + Duration::from_millis(100),
        );
        let p = e.last_probe().unwrap();
        assert!((1_150_000..=1_250_000).contains(&p), "{p}");
    }
}
