//! The receive side of transport-cc: note when each numbered packet
//! arrived and report it to the sender.

use forge_rtp::transport_cc::{
    PacketStatus, TransportCcFeedback, DELTA_TICK_US, MAX_PACKETS_PER_FEEDBACK, REFERENCE_TICK_US,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Feedback goes this often while packets arrive.
pub const FEEDBACK_INTERVAL: Duration = Duration::from_millis(100);
/// Or as soon as this many packets wait to be reported.
pub const FEEDBACK_PACKETS: usize = 100;
/// A jump this far ahead is the sender starting over, not a loss.
const RESTART_GAP: i64 = 5_000;

/// Writes transport-cc feedback for what arrives.
pub struct FeedbackWriter {
    /// Arrivals not reported yet, by unwrapped sequence number, in µs
    /// since `epoch`.
    pending: BTreeMap<i64, i64>,
    /// The next sequence number to report from.
    next: Option<i64>,
    /// The highest sequence number seen, unwrapped.
    highest: Option<i64>,
    epoch: Option<Instant>,
    last_sent: Option<Instant>,
    count: u8,
}

impl Default for FeedbackWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackWriter {
    pub fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            next: None,
            highest: None,
            epoch: None,
            last_sent: None,
            count: 0,
        }
    }

    fn unwrap(&self, seq: u16) -> i64 {
        match self.highest {
            None => i64::from(seq),
            Some(h) => {
                let delta = i64::from(seq.wrapping_sub(h as u16) as i16);
                h + delta
            }
        }
    }

    /// Packet `seq` arrived at `at`.
    pub fn on_packet(&mut self, seq: u16, at: Instant) {
        let epoch = *self.epoch.get_or_insert(at);
        let s = self.unwrap(seq);
        if let Some(h) = self.highest {
            if s - h > RESTART_GAP || h - s > RESTART_GAP {
                // The sender started over: what was waiting says nothing
                // that can still be matched.
                self.pending.clear();
                self.next = None;
                self.highest = None;
            }
        }
        let s = self.unwrap(seq);
        if self.next.is_some_and(|n| s < n) {
            // Already reported as lost; too late to say otherwise.
            return;
        }
        self.highest = Some(self.highest.map_or(s, |h| h.max(s)));
        self.next.get_or_insert(s);
        let us = at.saturating_duration_since(epoch).as_micros() as i64;
        self.pending.entry(s).or_insert(us);
    }

    /// The feedback due at `now`, if any.
    pub fn poll(&mut self, now: Instant) -> Option<TransportCcFeedback> {
        let (Some(next), Some(highest)) = (self.next, self.highest) else {
            return None;
        };
        if self.pending.is_empty() {
            return None;
        }
        let waiting = (highest - next + 1) as usize;
        let due = waiting >= FEEDBACK_PACKETS
            || self.last_sent.map_or(true, |t| {
                now.saturating_duration_since(t) >= FEEDBACK_INTERVAL
            });
        if !due {
            return None;
        }
        self.build(now)
    }

    /// Report everything waiting now, due or not.
    pub fn flush(&mut self, now: Instant) -> Option<TransportCcFeedback> {
        self.build(now)
    }

    fn build(&mut self, now: Instant) -> Option<TransportCcFeedback> {
        let next = self.next?;
        let highest = self.highest?;
        let (&first_seq, &first_us) = self.pending.iter().next()?;
        // The message starts at the first packet still unreported; a
        // run of losses before the first arrival is reported with it.
        let base = next.min(first_seq);
        let reference = first_us.div_euclid(REFERENCE_TICK_US);
        let mut last_tick = reference * (REFERENCE_TICK_US / DELTA_TICK_US);
        let mut packets = Vec::new();
        let mut s = base;
        while s <= highest && packets.len() < MAX_PACKETS_PER_FEEDBACK {
            match self.pending.get(&s) {
                None => packets.push(PacketStatus::NotReceived),
                Some(&us) => {
                    let tick = (us + DELTA_TICK_US / 2).div_euclid(DELTA_TICK_US);
                    let delta = tick - last_tick;
                    if delta < i64::from(i16::MIN) || delta > i64::from(i16::MAX) {
                        // Too far from the last one for one message: the
                        // next message starts here.
                        break;
                    }
                    packets.push(PacketStatus::Received(delta as i16));
                    last_tick = tick;
                }
            }
            s += 1;
        }
        // Trailing losses are not reported: they may yet arrive.
        while packets.last() == Some(&PacketStatus::NotReceived) {
            packets.pop();
            s -= 1;
        }
        if packets.is_empty() {
            return None;
        }
        self.pending = self.pending.split_off(&s);
        self.next = Some(s);
        self.last_sent = Some(now);
        let fb = TransportCcFeedback {
            base_sequence: base as u16,
            // 24 bits, wrapping; the sender unwraps it.
            reference_time: ((((reference & 0xFF_FFFF) as u32) << 8) as i32) >> 8,
            feedback_count: self.count,
            packets,
        };
        self.count = self.count.wrapping_add(1);
        Some(fb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrivals_and_a_loss_are_reported_on_the_interval() {
        let mut w = FeedbackWriter::new();
        let t0 = Instant::now();
        w.on_packet(65_534, t0);
        w.on_packet(65_535, t0 + Duration::from_millis(1));
        w.on_packet(1, t0 + Duration::from_millis(3));
        let fb = w
            .poll(t0 + Duration::from_millis(3))
            .expect("the first is due at once");
        assert_eq!(fb.base_sequence, 65_534);
        assert_eq!(fb.packets.len(), 4);
        assert_eq!(fb.packets[2], PacketStatus::NotReceived);
        let at: Vec<_> = fb.arrivals().into_iter().map(|(_, a)| a).collect();
        let start = at[0].unwrap();
        assert_eq!(at[1].unwrap() - start, 1_000);
        assert_eq!(at[3].unwrap() - start, 3_000);
        // Nothing new: nothing due.
        assert!(w.poll(t0 + Duration::from_millis(200)).is_none());
        w.on_packet(2, t0 + Duration::from_millis(50));
        assert!(w.poll(t0 + Duration::from_millis(60)).is_none(), "not yet");
        let fb = w.poll(t0 + Duration::from_millis(110)).unwrap();
        assert_eq!(
            (fb.base_sequence, fb.packets.len(), fb.feedback_count),
            (2, 1, 1)
        );
    }

    #[test]
    fn a_trailing_gap_waits_and_a_late_packet_after_a_report_is_let_go() {
        let mut w = FeedbackWriter::new();
        let t0 = Instant::now();
        w.on_packet(10, t0);
        w.on_packet(13, t0);
        let fb = w.flush(t0).unwrap();
        assert_eq!(
            fb.packets.len(),
            4,
            "11 and 12 are lost, between two arrivals"
        );
        w.on_packet(12, t0 + Duration::from_millis(5));
        assert!(w.flush(t0).is_none(), "reported already");
    }

    #[test]
    fn many_packets_are_due_before_the_interval() {
        let mut w = FeedbackWriter::new();
        let t0 = Instant::now();
        w.on_packet(0, t0);
        w.poll(t0).unwrap();
        for s in 1..=FEEDBACK_PACKETS as u16 {
            w.on_packet(s, t0 + Duration::from_millis(1));
        }
        let fb = w.poll(t0 + Duration::from_millis(1)).unwrap();
        assert_eq!(fb.packets.len(), FEEDBACK_PACKETS);
    }
}
