//! Probing: a short burst of padding at a target rate, whose arrival times
//! say whether the path carries that rate before a layer is committed to
//! it.

use std::time::{Duration, Instant};

/// A probe is padding-only RTP packets of this much padding (the most one
/// packet's padding length byte can say, RFC 3550 §5.1).
pub const PROBE_PADDING: usize = 255;
/// A cluster is at least this many packets…
pub const PROBE_MIN_PACKETS: usize = 5;
/// …and lasts at least this long.
pub const PROBE_MIN_SPAN: Duration = Duration::from_millis(15);
/// Probes are no closer together than this.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// A probe that fell short of its target waits twice as long as the last
/// before that target is tried again, up to this.
pub const PROBE_BACKOFF_MAX: Duration = Duration::from_secs(30);

struct Active {
    id: u32,
    remaining: usize,
    next_at: Instant,
    spacing: Duration,
}

/// A target that a probe fell short of: not tried again until `until`.
struct Failed {
    target_bps: u64,
    wait: Duration,
    until: Instant,
}

/// Paces probe clusters.
#[derive(Default)]
pub struct Prober {
    active: Option<Active>,
    next_id: u32,
    last_start: Option<Instant>,
    last_target: Option<u64>,
    failed: Option<Failed>,
}

impl Prober {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a cluster at `target_bps` of packets `packet_bytes` long on
    /// the wire: its id, or `None` while one runs or the last began within
    /// [`PROBE_INTERVAL`].
    pub fn start(&mut self, target_bps: u64, packet_bytes: usize, now: Instant) -> Option<u32> {
        if self.active.is_some()
            || target_bps == 0
            || self
                .last_start
                .is_some_and(|t| now.saturating_duration_since(t) < PROBE_INTERVAL)
        {
            return None;
        }
        // A target at or above one that just failed waits out its backoff.
        if let Some(f) = &self.failed {
            if target_bps >= f.target_bps && now < f.until {
                return None;
            }
        }
        self.last_target = Some(target_bps);
        let spacing = Duration::from_secs_f64(packet_bytes as f64 * 8.0 / target_bps as f64);
        let over_span = (PROBE_MIN_SPAN.as_secs_f64() / spacing.as_secs_f64()).ceil() as usize + 1;
        let count = PROBE_MIN_PACKETS.max(over_span);
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.active = Some(Active {
            id,
            remaining: count,
            next_at: now,
            spacing,
        });
        self.last_start = Some(now);
        Some(id)
    }

    /// The last cluster found `result_bps`: under what it was sent to
    /// find, it backs off; at or over it, the backoff ends.
    pub fn result(&mut self, result_bps: u64, needed_bps: u64, now: Instant) {
        let Some(target) = self.last_target else {
            return;
        };
        if result_bps >= needed_bps {
            self.failed = None;
            return;
        }
        let wait = match &self.failed {
            Some(f) if f.target_bps <= target => (f.wait * 2).min(PROBE_BACKOFF_MAX),
            _ => PROBE_INTERVAL * 2,
        };
        self.failed = Some(Failed {
            target_bps: target,
            wait,
            until: now + wait,
        });
    }

    /// Stop the cluster running (it could not be sent). The interval
    /// still counts from when it began.
    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// Whether a cluster is running.
    pub fn probing(&self) -> bool {
        self.active.is_some()
    }

    /// When the next probe packet is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.active.as_ref().map(|a| a.next_at)
    }

    /// A probe packet due at `now`: its cluster's id. Call until `None`.
    pub fn poll(&mut self, now: Instant) -> Option<u32> {
        let a = self.active.as_mut()?;
        if now < a.next_at {
            return None;
        }
        let id = a.id;
        a.remaining -= 1;
        a.next_at += a.spacing;
        if a.remaining == 0 {
            self.active = None;
        }
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cluster_is_paced_at_its_rate_and_spaced_from_the_next() {
        let mut p = Prober::new();
        let t0 = Instant::now();
        // 267-byte packets at 2 Mb/s: about 1.07 ms apart, 16 to span 15 ms.
        let id = p.start(2_000_000, 267, t0).unwrap();
        let mut sent = Vec::new();
        let mut t = t0;
        while p.probing() {
            while let Some(c) = p.poll(t) {
                assert_eq!(c, id);
                sent.push(t);
            }
            t += Duration::from_micros(100);
        }
        assert!(sent.len() >= 15, "{}", sent.len());
        let span = sent.last().unwrap().duration_since(sent[0]);
        assert!(span >= PROBE_MIN_SPAN, "{span:?}");
        assert!(p.start(2_000_000, 267, t).is_none(), "too soon");
        assert!(p.start(2_000_000, 267, t0 + PROBE_INTERVAL).is_some());
    }

    #[test]
    fn a_probe_that_falls_short_backs_off_and_one_that_reaches_ends_it() {
        let mut p = Prober::new();
        let t0 = Instant::now();
        let mut t = t0;
        let mut run = |p: &mut Prober, t: Instant| {
            while p.poll(t + Duration::from_millis(100)).is_some() {}
        };
        assert!(p.start(3_000_000, 267, t).is_some());
        run(&mut p, t);
        p.result(2_000_000, 2_600_000, t);
        t += PROBE_INTERVAL;
        assert!(p.start(3_000_000, 267, t).is_none(), "backing off");
        assert!(
            p.start(1_000_000, 267, t).is_some(),
            "a lower target may go"
        );
        run(&mut p, t);
        p.result(1_200_000, 900_000, t);
        t += PROBE_INTERVAL * 2;
        assert!(p.start(3_000_000, 267, t).is_some(), "the backoff ended");
    }
}
