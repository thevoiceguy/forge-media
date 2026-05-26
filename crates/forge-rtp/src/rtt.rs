//! RTT tracking from RTCP SR/RR exchanges (RFC 3550 §A.7).
//!
//! Computes round-trip time by matching the `last_sr` field of an incoming
//! Receiver Report against the middle-32 NTP timestamp of a previously-sent
//! Sender Report, subtracting the receiver's `delay_since_last_sr`. All
//! arithmetic is in the standard 1/65536-second NTP units; wrapping
//! subtraction handles the 18-hour wrap.
//!
//! ## Wiring
//!
//! 1. On every outgoing SR, call [`RttTracker::record_outgoing_sr`] with
//!    the 64-bit NTP timestamp the SR carried.
//! 2. On every incoming RR, call [`RttTracker::observe_incoming_rr`] with
//!    the RR's `last_sr` + `delay_since_last_sr` + the current 64-bit NTP
//!    time.
//! 3. Poll [`RttTracker::mean_ms`] when emitting a periodic stats event
//!    to get the rolling-mean RTT across the configured window.
//!
//! This module is a primitive; it does not subscribe to any event bus or
//! drive any emit cadence. That is the consumer's job (siphon-ai
//! `rtp_stats` tap, forge-engine quality emitter, etc.).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One observed RTT sample. Stored with the wall-clock moment we computed
/// it so [`RttTracker::mean_ms`] can evict samples older than the window.
#[derive(Debug, Clone, Copy)]
struct RttSample {
    rtt_ms: f32,
    observed_at: Instant,
}

/// Rolling-mean RTT tracker driven by RTCP SR/RR exchanges per RFC 3550 §A.7.
///
/// The tracker keeps a small ring of recently-sent SR timestamps so an RR
/// that references an older SR (rare but legal — the remote may skip RRs
/// for a few SR cycles) still produces a sample. The default ring size is
/// 8 SRs, which at typical RTCP cadence (every 5 s) covers ~40 s of slack.
#[derive(Debug)]
pub struct RttTracker {
    /// Middle-32 NTP timestamps of recently-sent SRs, oldest first.
    recent_sr_middle32: VecDeque<u32>,
    sr_ring_cap: usize,
    /// Window over which [`mean_ms`] averages samples.
    window: Duration,
    /// Accumulated samples, oldest first.
    samples: VecDeque<RttSample>,
}

impl RttTracker {
    /// Create a tracker that retains samples for `window` and a fixed-size
    /// ring of 8 SR timestamps.
    pub fn new(window: Duration) -> Self {
        Self::with_capacity(window, 8)
    }

    /// Same as [`new`](Self::new) but lets callers override the SR ring size.
    /// Useful for very aggressive RTCP cadence or for tests that need to
    /// observe ring-overflow behaviour.
    pub fn with_capacity(window: Duration, sr_ring_cap: usize) -> Self {
        let cap = sr_ring_cap.max(1);
        Self {
            recent_sr_middle32: VecDeque::with_capacity(cap),
            sr_ring_cap: cap,
            window,
            samples: VecDeque::new(),
        }
    }

    /// Record that we sent an SR carrying NTP timestamp `ntp_ts`. The
    /// tracker stores only the middle-32 representation that the remote
    /// will echo back in the RR's `last_sr` field.
    pub fn record_outgoing_sr(&mut self, ntp_ts: u64) {
        let m32 = ntp_middle32(ntp_ts);
        if self.recent_sr_middle32.back().copied() == Some(m32) {
            // Duplicate of the most-recent SR — skip to keep the ring useful.
            return;
        }
        self.recent_sr_middle32.push_back(m32);
        while self.recent_sr_middle32.len() > self.sr_ring_cap {
            self.recent_sr_middle32.pop_front();
        }
    }

    /// Observe an incoming RR. Returns `Some(rtt_ms)` when the RR's
    /// `last_sr` matches a SR we previously recorded, and the arithmetic
    /// passes sanity checks. Returns `None` otherwise — the sample is
    /// silently dropped so observability code can keep going.
    ///
    /// `now_ntp_ts` is the local 64-bit NTP timestamp at which the RR
    /// arrived. Callers should use [`crate::rtcp::ntp::now`] in production;
    /// tests pass a fixed value.
    pub fn observe_incoming_rr(
        &mut self,
        last_sr: u32,
        delay_since_last_sr: u32,
        now_ntp_ts: u64,
    ) -> Option<f32> {
        // RFC 3550 §6.4.1: LSR = 0 means "no SR has been received from
        // this source yet" — not a valid RTT computation.
        if last_sr == 0 {
            return None;
        }
        if !self.recent_sr_middle32.contains(&last_sr) {
            return None;
        }
        let now_m32 = ntp_middle32(now_ntp_ts);
        // Wrapping subtraction handles the 18.2-hour middle-32 wrap.
        let elapsed_units = now_m32.wrapping_sub(last_sr);
        // Guard against a clock skew or a malicious DLSR that would
        // produce a negative RTT.
        if elapsed_units < delay_since_last_sr {
            return None;
        }
        let rtt_units = elapsed_units - delay_since_last_sr;
        // 65536 middle-32 units per second → 65.536 units per ms.
        let rtt_ms = (rtt_units as f32) / 65.536_f32;
        self.samples.push_back(RttSample {
            rtt_ms,
            observed_at: Instant::now(),
        });
        self.prune();
        Some(rtt_ms)
    }

    /// Mean RTT across the window in milliseconds, or `None` if no samples
    /// are present after pruning. Pruning is performed eagerly here so the
    /// returned value never reflects stale samples.
    pub fn mean_ms(&mut self) -> Option<f32> {
        self.prune();
        if self.samples.is_empty() {
            return None;
        }
        let sum: f32 = self.samples.iter().map(|s| s.rtt_ms).sum();
        Some(sum / self.samples.len() as f32)
    }

    /// Number of samples currently retained. Useful for tests and for
    /// `siphon_ai_rtp_rtt_samples` gauges.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Configured window.
    pub fn window(&self) -> Duration {
        self.window
    }

    fn prune(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.samples.front() {
            if now.duration_since(front.observed_at) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Extract the middle 32 bits of a 64-bit NTP timestamp per RFC 3550 §A.7.
///
/// The middle 32 = `[low 16 of NTP seconds] [high 16 of NTP fraction]`,
/// which is what SR carries in its `last_sr` echo field. Each unit is
/// 1/65536 second.
#[inline]
pub fn ntp_middle32(ntp_ts: u64) -> u32 {
    ((ntp_ts >> 16) & 0xFFFF_FFFF) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 64-bit NTP timestamp from seconds + fraction (both u32).
    fn ntp(seconds: u32, fraction: u32) -> u64 {
        ((seconds as u64) << 32) | (fraction as u64)
    }

    // ---------------------------------------------------------------------
    // ntp_middle32 vectors
    // ---------------------------------------------------------------------

    #[test]
    fn ntp_middle32_extracts_low16_seconds_high16_fraction() {
        // Seconds = 0x0001_0203, fraction = 0x0405_0607.
        // Middle 32 = (low 16 of seconds) (high 16 of fraction)
        //          = 0x0203 0405
        let ts = ntp(0x0001_0203, 0x0405_0607);
        assert_eq!(ntp_middle32(ts), 0x0203_0405);
    }

    #[test]
    fn ntp_middle32_extreme_values() {
        assert_eq!(ntp_middle32(0), 0);
        assert_eq!(ntp_middle32(u64::MAX), 0xFFFF_FFFF);
    }

    // ---------------------------------------------------------------------
    // observe_incoming_rr — happy path
    // ---------------------------------------------------------------------

    #[test]
    fn matching_last_sr_produces_expected_rtt() {
        let mut t = RttTracker::new(Duration::from_secs(60));

        // We sent an SR with seconds=100, fraction=0 → middle32=100<<16.
        let sr_ts = ntp(100, 0);
        let sr_m32 = ntp_middle32(sr_ts); // = 100 * 65536
        t.record_outgoing_sr(sr_ts);

        // The remote took 5 ms to process the SR → DLSR = 5 * 65.536 ≈ 328.
        let dlsr: u32 = (5.0 * 65.536) as u32; // 327
                                               // The RR arrives 105 ms after the SR was sent.
        let now_m32 = sr_m32 + ((105.0 * 65.536) as u32); // +6881
        let now_ts = ntp(now_m32 >> 16, (now_m32 & 0xFFFF) << 16);

        let rtt = t.observe_incoming_rr(sr_m32, dlsr, now_ts).unwrap();
        // Expected: 105 ms total - 5 ms remote dwell = 100 ms RTT.
        assert!((rtt - 100.0).abs() < 0.5, "rtt = {rtt}; expected ~100 ms");
        assert_eq!(t.sample_count(), 1);
    }

    #[test]
    fn mean_across_multiple_samples() {
        let mut t = RttTracker::new(Duration::from_secs(60));

        for (i, target_rtt_ms) in [50_u32, 100, 150].iter().enumerate() {
            let sr_m32 = (1000 + i as u32 * 100_000) << 4; // distinct values
            let sr_ts = ntp(sr_m32 >> 16, (sr_m32 & 0xFFFF) << 16);
            t.record_outgoing_sr(sr_ts);

            let dlsr = (10.0 * 65.536) as u32; // 10 ms dwell
            let total_units = ((target_rtt_ms + 10) as f32 * 65.536) as u32;
            let now_m32 = sr_m32 + total_units;
            let now_ts = ntp(now_m32 >> 16, (now_m32 & 0xFFFF) << 16);

            let _ = t.observe_incoming_rr(sr_m32, dlsr, now_ts).unwrap();
        }

        // Mean of 50/100/150 = 100, allow ±1 ms for fixed-point rounding.
        let mean = t.mean_ms().unwrap();
        assert!((mean - 100.0).abs() < 1.0, "mean = {mean}; expected ~100");
        assert_eq!(t.sample_count(), 3);
    }

    #[test]
    fn ring_holds_multiple_pending_srs() {
        let mut t = RttTracker::new(Duration::from_secs(60));

        // Send 3 SRs.
        let sr_ts_a = ntp(50, 0);
        let sr_ts_b = ntp(51, 0);
        let sr_ts_c = ntp(52, 0);
        t.record_outgoing_sr(sr_ts_a);
        t.record_outgoing_sr(sr_ts_b);
        t.record_outgoing_sr(sr_ts_c);

        // RR references the *first* (oldest) SR — should still match.
        let sr_a_m32 = ntp_middle32(sr_ts_a);
        let now_m32 = sr_a_m32 + (50.0 * 65.536) as u32; // 50 ms later
        let now_ts = ntp(now_m32 >> 16, (now_m32 & 0xFFFF) << 16);
        let rtt = t.observe_incoming_rr(sr_a_m32, 0, now_ts).unwrap();
        assert!((rtt - 50.0).abs() < 0.5);
    }

    #[test]
    fn ring_eviction_drops_oldest() {
        let mut t = RttTracker::with_capacity(Duration::from_secs(60), 2);
        let sr_a = ntp(50, 0);
        let sr_b = ntp(51, 0);
        let sr_c = ntp(52, 0);
        t.record_outgoing_sr(sr_a);
        t.record_outgoing_sr(sr_b);
        t.record_outgoing_sr(sr_c); // evicts sr_a

        // RR referencing the evicted SR no longer produces a sample.
        let sr_a_m32 = ntp_middle32(sr_a);
        let later = sr_a_m32 + 65536;
        let now_ts = ntp(later >> 16, (later & 0xFFFF) << 16);
        assert!(t.observe_incoming_rr(sr_a_m32, 0, now_ts).is_none());
    }

    #[test]
    fn duplicate_sr_record_is_no_op() {
        let mut t = RttTracker::with_capacity(Duration::from_secs(60), 8);
        let sr = ntp(50, 0);
        t.record_outgoing_sr(sr);
        t.record_outgoing_sr(sr);
        // Internal observation via behaviour: after dedupe, only one entry,
        // so adding 7 unique additional SRs and one more keeps room.
        for s in 51..58 {
            t.record_outgoing_sr(ntp(s, 0));
        }
        // The original sr=50 should still be matchable.
        let sr_m32 = ntp_middle32(sr);
        let later = sr_m32 + 65536;
        let now_ts = ntp(later >> 16, (later & 0xFFFF) << 16);
        assert!(t.observe_incoming_rr(sr_m32, 0, now_ts).is_some());
    }

    // ---------------------------------------------------------------------
    // observe_incoming_rr — rejection paths
    // ---------------------------------------------------------------------

    #[test]
    fn unmatched_last_sr_returns_none() {
        let mut t = RttTracker::new(Duration::from_secs(60));
        t.record_outgoing_sr(ntp(50, 0));

        // Some random last_sr value the tracker has never seen.
        assert!(t
            .observe_incoming_rr(0xDEAD_BEEF, 100, ntp(60, 0))
            .is_none());
        assert_eq!(t.sample_count(), 0);
    }

    #[test]
    fn last_sr_zero_returns_none() {
        // RFC 3550 §6.4.1: LSR=0 means "no SR seen yet" — must not match.
        let mut t = RttTracker::new(Duration::from_secs(60));
        t.record_outgoing_sr(ntp(50, 0));
        assert!(t.observe_incoming_rr(0, 0, ntp(60, 0)).is_none());
    }

    #[test]
    fn dlsr_larger_than_elapsed_returns_none() {
        // Guards against a remote that lies (or has a wildly skewed clock).
        let mut t = RttTracker::new(Duration::from_secs(60));
        let sr_ts = ntp(50, 0);
        let sr_m32 = ntp_middle32(sr_ts);
        t.record_outgoing_sr(sr_ts);

        let now_m32 = sr_m32 + 100; // 100 units elapsed
        let dlsr = 5000; // remote claims a 5000-unit dwell — impossible
        let now_ts = ntp(now_m32 >> 16, (now_m32 & 0xFFFF) << 16);
        assert!(t.observe_incoming_rr(sr_m32, dlsr, now_ts).is_none());
    }

    // ---------------------------------------------------------------------
    // mean_ms behaviour
    // ---------------------------------------------------------------------

    #[test]
    fn mean_returns_none_when_empty() {
        let mut t = RttTracker::new(Duration::from_secs(60));
        assert!(t.mean_ms().is_none());
        assert_eq!(t.sample_count(), 0);
    }

    #[test]
    fn mean_after_single_sample() {
        let mut t = RttTracker::new(Duration::from_secs(60));
        let sr_ts = ntp(100, 0);
        let sr_m32 = ntp_middle32(sr_ts);
        t.record_outgoing_sr(sr_ts);
        let now_m32 = sr_m32 + (40.0 * 65.536) as u32;
        let now_ts = ntp(now_m32 >> 16, (now_m32 & 0xFFFF) << 16);
        t.observe_incoming_rr(sr_m32, 0, now_ts).unwrap();
        let mean = t.mean_ms().unwrap();
        assert!((mean - 40.0).abs() < 0.5);
    }

    #[test]
    fn window_is_exposed() {
        let t = RttTracker::new(Duration::from_secs(10));
        assert_eq!(t.window(), Duration::from_secs(10));
    }

    // ---------------------------------------------------------------------
    // Wrap-around — middle-32 NTP wraps every ~18.2 hours.
    // ---------------------------------------------------------------------

    #[test]
    fn handles_middle32_wrap() {
        let mut t = RttTracker::new(Duration::from_secs(60));
        // Pick a last_sr near the u32 ceiling so the elapsed subtraction wraps.
        let last_sr: u32 = u32::MAX - 1000;
        // Synthesize an NTP timestamp whose middle-32 IS last_sr, then add 2000.
        let now_m32 = last_sr.wrapping_add(2000);
        // Build a fake ntp_ts with that middle-32. The low 16 of seconds
        // and high 16 of fraction encode the middle-32.
        let seconds = (now_m32 >> 16) & 0xFFFF; // low 16 -> goes back here
        let fraction_high = (now_m32 & 0xFFFF) << 16;
        let now_ts = ntp(seconds, fraction_high);
        let now_back_m32 = ntp_middle32(now_ts);
        assert_eq!(now_back_m32, now_m32, "round-trip middle32 must hold");

        // Pre-load the ring with the wrapped last_sr value.
        let pre_seconds = (last_sr >> 16) & 0xFFFF;
        let pre_fraction = (last_sr & 0xFFFF) << 16;
        t.record_outgoing_sr(ntp(pre_seconds, pre_fraction));

        // 2000 units elapsed, 500 units dwell → 1500 units RTT ≈ 22.9 ms.
        let rtt = t.observe_incoming_rr(last_sr, 500, now_ts).unwrap();
        let expected = 1500.0_f32 / 65.536_f32;
        assert!(
            (rtt - expected).abs() < 0.1,
            "rtt = {rtt}; expected {expected}"
        );
    }
}
