//! Per-source RTP statistics (RFC 3550 §6.4 and Appendix A): what a
//! receiver needs to fill a reception report block for each stream it
//! hears — extended highest sequence number, cumulative and interval
//! loss, interarrival jitter, last SR and the delay since it — and what
//! a sender needs to fill a sender report for each stream it sends.
//!
//! Both are primitives with no clock of their own: the caller feeds
//! packets with the `Instant` they arrived or left and asks for a report
//! when its RTCP interval elapses. A transport carrying several streams
//! keeps one [`SourceStats`] per remote SSRC and one [`SenderStats`] per
//! local SSRC.

use std::time::Instant;

use crate::rtcp::{ReceptionReportBlock, SenderReport};

/// Sequence jump beyond which a packet is a restart, not a late arrival
/// (RFC 3550 A.1).
const MAX_DROPOUT: u32 = 3000;
/// How far behind the highest sequence a packet may be and still count
/// as reordered rather than as an old restart (RFC 3550 A.1).
const MAX_MISORDER: u32 = 100;

/// Reception statistics for one remote SSRC.
#[derive(Debug, Clone)]
pub struct SourceStats {
    ssrc: u32,
    clock_rate: u32,
    started: bool,
    base_seq: u32,
    max_seq: u16,
    cycles: u32,
    bad_seq: Option<u16>,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
    first_at: Option<Instant>,
    transit: Option<i64>,
    jitter: f64,
    last_sr_mid32: u32,
    last_sr_at: Option<Instant>,
}

impl SourceStats {
    /// Statistics for `ssrc`, whose RTP timestamps run at `clock_rate` Hz
    /// (jitter is measured in those units).
    pub fn new(ssrc: u32, clock_rate: u32) -> Self {
        Self {
            ssrc,
            clock_rate: clock_rate.max(1),
            started: false,
            base_seq: 0,
            max_seq: 0,
            cycles: 0,
            bad_seq: None,
            received: 0,
            expected_prior: 0,
            received_prior: 0,
            first_at: None,
            transit: None,
            jitter: 0.0,
            last_sr_mid32: 0,
            last_sr_at: None,
        }
    }

    /// The SSRC these statistics describe.
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Change the clock rate (the stream switched payload types).
    pub fn set_clock_rate(&mut self, clock_rate: u32) {
        self.clock_rate = clock_rate.max(1);
    }

    fn init_seq(&mut self, seq: u16) {
        self.base_seq = seq as u32;
        self.max_seq = seq;
        self.cycles = 0;
        self.bad_seq = None;
        self.received = 0;
        self.expected_prior = 0;
        self.received_prior = 0;
    }

    /// Account one authenticated packet that arrived at `now`
    /// (RFC 3550 A.1 sequence tracking, A.8 jitter).
    pub fn on_packet(&mut self, seq: u16, rtp_timestamp: u32, now: Instant) {
        if !self.started {
            self.started = true;
            self.init_seq(seq);
            self.first_at = Some(now);
        } else {
            let udelta = seq.wrapping_sub(self.max_seq) as u32;
            if udelta < MAX_DROPOUT {
                // In order, with a permissible gap.
                if seq < self.max_seq {
                    self.cycles = self.cycles.wrapping_add(1 << 16);
                }
                self.max_seq = seq;
            } else if udelta <= u16::MAX as u32 - MAX_MISORDER {
                // A large jump: the source restarted, or this is a stray
                // packet. Accept it only when the next one follows on.
                if self.bad_seq == Some(seq) {
                    self.init_seq(seq);
                } else {
                    self.bad_seq = Some(seq.wrapping_add(1));
                    return;
                }
            }
            // Otherwise a duplicate or reordered packet: counted as
            // received, the sequence state untouched.
        }
        self.received = self.received.wrapping_add(1);

        // Interarrival jitter (A.8): the change in relative transit time,
        // smoothed by a factor of 16, in RTP clock units.
        let arrival = self
            .first_at
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64())
            * self.clock_rate as f64;
        let transit = arrival as i64 - rtp_timestamp as i64;
        if let Some(prev) = self.transit {
            // Reduce the difference modulo 2^32 so a wrapped timestamp
            // does not register as a huge jitter step.
            let mut d = (transit - prev).rem_euclid(1 << 32);
            if d > 1 << 31 {
                d = (1i64 << 32) - d;
            }
            self.jitter += (d as f64 - self.jitter) / 16.0;
        }
        self.transit = Some(transit);
    }

    /// Note a sender report from this source, received at `now`; the
    /// next report block carries its middle 32 NTP bits and the delay
    /// since it (RFC 3550 §6.4.1 LSR and DLSR).
    pub fn on_sender_report(&mut self, ntp_msw: u32, ntp_lsw: u32, now: Instant) {
        self.last_sr_mid32 = (ntp_msw << 16) | (ntp_lsw >> 16);
        self.last_sr_at = Some(now);
    }

    /// Extended highest sequence number received (cycles ‖ max seq).
    pub fn extended_highest_seq(&self) -> u32 {
        self.cycles.wrapping_add(self.max_seq as u32)
    }

    /// Packets counted as received since the source was first heard.
    pub fn packets_received(&self) -> u32 {
        self.received
    }

    /// Packets expected so far, from the sequence numbers.
    pub fn packets_expected(&self) -> u32 {
        if !self.started {
            return 0;
        }
        self.extended_highest_seq()
            .wrapping_sub(self.base_seq)
            .wrapping_add(1)
    }

    /// Cumulative packets lost (may be negative with duplicates).
    pub fn packets_lost(&self) -> i32 {
        self.packets_expected() as i32 - self.received as i32
    }

    /// Current interarrival jitter in RTP clock units.
    pub fn jitter(&self) -> u32 {
        self.jitter as u32
    }

    /// Current interarrival jitter in milliseconds.
    pub fn jitter_ms(&self) -> f64 {
        self.jitter * 1000.0 / self.clock_rate as f64
    }

    /// Whether any packet has been counted.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Fill a reception report block as of `now`. The fraction lost
    /// covers the interval since the previous call (RFC 3550 A.3), so
    /// call it once per report.
    pub fn report_block(&mut self, now: Instant) -> ReceptionReportBlock {
        let expected = self.packets_expected();
        let expected_interval = expected.wrapping_sub(self.expected_prior);
        let received_interval = self.received.wrapping_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_interval = expected_interval as i64 - received_interval as i64;
        let fraction_lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / expected_interval as i64).min(255) as u8
        };
        let cumulative_lost = self.packets_lost().clamp(-0x80_0000, 0x7F_FFFF);
        let delay_since_last_sr = self
            .last_sr_at
            .map(|t| (now.duration_since(t).as_secs_f64() * 65_536.0) as u32)
            .unwrap_or(0);
        ReceptionReportBlock {
            ssrc: self.ssrc,
            fraction_lost,
            cumulative_lost,
            extended_highest_seq: self.extended_highest_seq(),
            jitter: self.jitter(),
            last_sr: self.last_sr_mid32,
            delay_since_last_sr,
        }
    }
}

/// Transmission statistics for one local SSRC.
#[derive(Debug, Clone)]
pub struct SenderStats {
    ssrc: u32,
    clock_rate: u32,
    packets: u32,
    octets: u32,
    last_timestamp: u32,
    last_at: Option<Instant>,
}

impl SenderStats {
    /// Statistics for `ssrc`, whose RTP timestamps run at `clock_rate` Hz.
    pub fn new(ssrc: u32, clock_rate: u32) -> Self {
        Self {
            ssrc,
            clock_rate: clock_rate.max(1),
            packets: 0,
            octets: 0,
            last_timestamp: 0,
            last_at: None,
        }
    }

    /// The SSRC these statistics describe.
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Change the clock rate (the stream switched payload types).
    pub fn set_clock_rate(&mut self, clock_rate: u32) {
        self.clock_rate = clock_rate.max(1);
    }

    /// Account one packet sent at `now` carrying `payload_len` payload
    /// octets (RFC 3550 §6.4.1 counts payload, not headers).
    pub fn on_send(&mut self, rtp_timestamp: u32, payload_len: usize, now: Instant) {
        self.packets = self.packets.wrapping_add(1);
        self.octets = self.octets.wrapping_add(payload_len as u32);
        self.last_timestamp = rtp_timestamp;
        self.last_at = Some(now);
    }

    /// Whether any packet has been sent.
    pub fn has_sent(&self) -> bool {
        self.last_at.is_some()
    }

    /// Packets sent so far.
    pub fn packets_sent(&self) -> u32 {
        self.packets
    }

    /// Payload octets sent so far.
    pub fn octets_sent(&self) -> u32 {
        self.octets
    }

    /// The RTP timestamp the stream would carry at `now`: the last one
    /// sent advanced by the time since it at the clock rate, which is
    /// what an SR's `rtp_timestamp` must be for receivers to map RTP
    /// time to wall time (RFC 3550 §6.4.1).
    pub fn rtp_timestamp_at(&self, now: Instant) -> u32 {
        let elapsed = self
            .last_at
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last_timestamp
            .wrapping_add((elapsed * self.clock_rate as f64) as u32)
    }

    /// A sender report as of `now`, stamped with the current NTP time.
    pub fn sender_report(&self, now: Instant) -> SenderReport {
        SenderReport::with_current_time(
            self.ssrc,
            self.rtp_timestamp_at(now),
            self.packets,
            self.octets,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn counts_loss_over_an_interval_and_cumulatively() {
        let t0 = Instant::now();
        let mut s = SourceStats::new(7, 8000);
        for (i, seq) in [100u16, 101, 102, 104, 105].iter().enumerate() {
            s.on_packet(
                *seq,
                i as u32 * 160,
                t0 + Duration::from_millis(i as u64 * 20),
            );
        }
        let b = s.report_block(t0 + Duration::from_millis(100));
        assert_eq!(b.ssrc, 7);
        assert_eq!(b.extended_highest_seq, 105);
        assert_eq!(b.cumulative_lost, 1);
        // 6 expected, 5 received: 1/6 of 256.
        assert_eq!(b.fraction_lost, 42);
        // A clean second interval reports no fraction lost but the same
        // cumulative figure.
        for seq in 106u16..=110 {
            s.on_packet(seq, 0, t0);
        }
        let b = s.report_block(t0);
        assert_eq!(b.fraction_lost, 0);
        assert_eq!(b.cumulative_lost, 1);
    }

    #[test]
    fn sequence_wrap_extends_the_highest_sequence() {
        let t0 = Instant::now();
        let mut s = SourceStats::new(1, 90_000);
        s.on_packet(65_534, 0, t0);
        s.on_packet(65_535, 0, t0);
        s.on_packet(0, 0, t0);
        s.on_packet(1, 0, t0);
        assert_eq!(s.extended_highest_seq(), 0x1_0001);
        assert_eq!(s.packets_expected(), 4);
        assert_eq!(s.packets_lost(), 0);
    }

    #[test]
    fn reordered_and_duplicate_packets_count_as_received() {
        let t0 = Instant::now();
        let mut s = SourceStats::new(1, 8000);
        s.on_packet(10, 0, t0);
        s.on_packet(12, 0, t0);
        s.on_packet(11, 0, t0);
        s.on_packet(12, 0, t0);
        assert_eq!(s.packets_received(), 4);
        assert_eq!(s.packets_expected(), 3);
        assert_eq!(s.packets_lost(), -1);
        assert_eq!(s.report_block(t0).cumulative_lost, -1);
    }

    #[test]
    fn a_restart_is_accepted_once_it_continues() {
        let t0 = Instant::now();
        let mut s = SourceStats::new(1, 8000);
        s.on_packet(10, 0, t0);
        s.on_packet(11, 0, t0);
        // A stray packet far ahead is not counted…
        s.on_packet(30_000, 0, t0);
        assert_eq!(s.packets_received(), 2);
        // …until the next one follows it, which resets the baseline.
        s.on_packet(30_001, 0, t0);
        assert_eq!(s.packets_received(), 1);
        assert_eq!(s.packets_expected(), 1);
    }

    #[test]
    fn steady_arrivals_have_no_jitter_and_uneven_ones_do() {
        let t0 = Instant::now();
        let mut s = SourceStats::new(1, 8000);
        for i in 0..20u32 {
            s.on_packet(i as u16, i * 160, t0 + Duration::from_millis(i as u64 * 20));
        }
        assert!(s.jitter_ms() < 1.0, "{}", s.jitter_ms());
        let mut u = SourceStats::new(2, 8000);
        for i in 0..20u32 {
            let late = if i % 2 == 0 { 0 } else { 40 };
            u.on_packet(
                i as u16,
                i * 160,
                t0 + Duration::from_millis(i as u64 * 20 + late),
            );
        }
        assert!(u.jitter_ms() > 5.0, "{}", u.jitter_ms());
    }

    #[test]
    fn last_sr_and_delay_since_it_are_reported() {
        let t0 = Instant::now();
        let mut s = SourceStats::new(1, 8000);
        s.on_packet(1, 0, t0);
        s.on_sender_report(0x0001_0203, 0x0405_0607, t0);
        let b = s.report_block(t0 + Duration::from_millis(500));
        assert_eq!(b.last_sr, 0x0203_0405);
        assert!(
            (32_000..=33_500).contains(&b.delay_since_last_sr),
            "{}",
            b.delay_since_last_sr
        );
    }

    #[test]
    fn sender_report_extrapolates_the_rtp_timestamp() {
        let t0 = Instant::now();
        let mut s = SenderStats::new(9, 8000);
        assert!(!s.has_sent());
        s.on_send(1_000, 160, t0);
        s.on_send(1_160, 160, t0 + Duration::from_millis(20));
        let sr = s.sender_report(t0 + Duration::from_millis(120));
        assert_eq!(sr.ssrc, 9);
        assert_eq!(sr.sender_packet_count, 2);
        assert_eq!(sr.sender_octet_count, 320);
        assert!(
            (1_960..=1_965).contains(&sr.rtp_timestamp),
            "{}",
            sr.rtp_timestamp
        );
        assert_ne!(sr.ntp_timestamp_msw, 0);
    }
}
