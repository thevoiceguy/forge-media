//! Transport-cc on the room's sending side (FCP `docs/TRANSPORT_CC.md`
//! §3.2): each forwarding viewer and composite subscriber whose section
//! negotiated it numbers what it sends, reads the receiver's feedback, and
//! spends the send-side estimate while that feedback is fresh.

use bytes::Bytes;
use forge_bwe::{Prober, SendSideEstimator};
use forge_rtp::transport_cc::{stamp, TransportCcFeedback};
use forge_rtp::RtpPacket;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

/// A probe aims this far above what the upgrade it tests needs (the
/// margin R5a's simulation used: a probe exactly at the need reads back
/// a hair under it).
pub const PROBE_MARGIN: f64 = 1.25;
/// A probe that proved an upgrade lets it happen this much sooner.
pub const PROVEN_FOR: Duration = Duration::from_secs(2);
/// A probe packet on the wire: an RTP header, the transport-cc
/// extension and the padding.
const PROBE_WIRE_BYTES: usize = 12 + 8 + forge_bwe::PROBE_PADDING;

struct State {
    seq: u16,
    estimator: SendSideEstimator,
    prober: Prober,
    /// What the probe running or last run was to find, in bits a second.
    need_bps: Option<u64>,
    /// The last probe result seen, and when it proved its need.
    seen_probe: Option<u64>,
    proved_at: Option<Instant>,
}

/// One section's transport-cc.
pub struct Congestion {
    ext_id: u8,
    state: Mutex<State>,
}

impl Congestion {
    /// `ext_id`: the transport-wide sequence extension's id in this
    /// section's SDP.
    pub fn new(ext_id: u8) -> Self {
        Self {
            ext_id,
            state: Mutex::new(State {
                seq: rand::random(),
                estimator: SendSideEstimator::new(),
                prober: Prober::new(),
                need_bps: None,
                seen_probe: None,
                proved_at: None,
            }),
        }
    }

    pub fn ext_id(&self) -> u8 {
        self.ext_id
    }

    /// Number `bytes` (an RTP packet, unencrypted) as it leaves, noting
    /// it was sent; `probe` names the cluster a probe packet is in. A
    /// packet that does not parse goes as it is.
    pub fn stamp(&self, bytes: Bytes, probe: Option<u32>) -> Bytes {
        let Ok(mut packet) = RtpPacket::parse(bytes.clone()) else {
            return bytes;
        };
        let mut s = self.state.lock();
        let seq = s.seq;
        s.seq = seq.wrapping_add(1);
        stamp(&mut packet, self.ext_id, seq);
        let out = packet.to_bytes().freeze();
        s.estimator.on_sent(seq, out.len(), Instant::now(), probe);
        out
    }

    /// The receiver's feedback.
    pub fn on_feedback(&self, fb: &TransportCcFeedback, now: Instant) {
        let mut s = self.state.lock();
        s.estimator.on_feedback(fb, now);
        let result = s.estimator.last_probe();
        if result.is_some() && result != s.seen_probe {
            s.seen_probe = result;
            let (r, need) = (result.unwrap_or(0), s.need_bps.unwrap_or(u64::MAX));
            s.prober.result(r, need, now);
            if r >= need {
                s.proved_at = Some(now);
            }
        }
    }

    /// The estimate in kb/s while feedback is fresh (`None` otherwise:
    /// the REMB, if any, is the budget then).
    pub fn budget_kbps(&self, now: Instant) -> Option<u32> {
        let s = self.state.lock();
        if !s.estimator.fresh(now) {
            return None;
        }
        s.estimator
            .estimate()
            .map(|bps| (bps / 1000).min(u64::from(u32::MAX)) as u32)
    }

    /// Whether a probe proved the upgrade it was sent for within
    /// [`PROVEN_FOR`].
    pub fn proved(&self, now: Instant) -> bool {
        self.state
            .lock()
            .proved_at
            .is_some_and(|t| now.saturating_duration_since(t) < PROVEN_FOR)
    }

    /// Start a probe for an upgrade needing `need_kbps`, when the budget
    /// `budget_kbps` nearly covers it (at least half) and does not
    /// already: the cluster's id, or `None` when no probe starts.
    pub fn maybe_probe(&self, need_kbps: u32, budget_kbps: u32, now: Instant) -> Option<u32> {
        if need_kbps <= budget_kbps || u64::from(budget_kbps) * 2 < u64::from(need_kbps) {
            return None;
        }
        let mut s = self.state.lock();
        if !s.estimator.fresh(now) {
            return None;
        }
        let need = u64::from(need_kbps) * 1000;
        let target = (need as f64 * PROBE_MARGIN) as u64;
        let id = s.prober.start(target, PROBE_WIRE_BYTES, now)?;
        s.need_bps = Some(need);
        Some(id)
    }

    /// A probe packet due at `now`: its cluster.
    pub fn poll_probe(&self, now: Instant) -> Option<u32> {
        self.state.lock().prober.poll(now)
    }

    /// Give up the probe running: it could not be sent. Not a result.
    pub fn cancel_probe(&self) {
        let mut s = self.state.lock();
        s.prober.cancel();
        s.need_bps = None;
    }

    /// When the next probe packet is due, while a probe runs.
    pub fn next_probe_due(&self) -> Option<Instant> {
        self.state.lock().prober.next_due()
    }

    /// The send-side estimate, fresh or not, in bits a second.
    pub fn estimate_bps(&self) -> Option<u64> {
        self.state.lock().estimator.estimate()
    }

    /// Whether feedback is arriving.
    pub fn fresh(&self, now: Instant) -> bool {
        self.state.lock().estimator.fresh(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_bwe::FeedbackWriter;
    use forge_rtp::transport_cc::transport_sequence;

    #[test]
    fn what_leaves_is_numbered_and_the_feedback_for_it_is_the_budget() {
        let cc = Congestion::new(3);
        let mut writer = FeedbackWriter::new();
        let t0 = Instant::now();
        assert_eq!(cc.budget_kbps(t0), None, "no feedback yet");
        let mut last = None;
        for i in 0..20u16 {
            let p = RtpPacket::build(96, i, 0, 9, Bytes::from_static(&[0x41, 0x9A]), false);
            let out = cc.stamp(p.to_bytes().freeze(), None);
            let back = RtpPacket::parse(out).unwrap();
            let seq = transport_sequence(&back, 3).unwrap();
            if let Some(l) = last {
                assert_eq!(seq, u16::wrapping_add(l, 1));
            }
            last = Some(seq);
            writer.on_packet(seq, Instant::now());
        }
        let fb = writer.flush(Instant::now()).unwrap();
        cc.on_feedback(&fb, Instant::now());
        assert!(cc.budget_kbps(Instant::now()).is_some());
        assert!(cc.fresh(Instant::now()));
    }

    #[test]
    fn a_probe_starts_only_for_an_upgrade_the_budget_nearly_covers() {
        let cc = Congestion::new(3);
        let now = Instant::now();
        // No feedback: no probe.
        assert!(cc.maybe_probe(1500, 1000, now).is_none());
        let mut writer = FeedbackWriter::new();
        let out = cc.stamp(
            RtpPacket::build(96, 0, 0, 9, Bytes::from_static(&[1]), false)
                .to_bytes()
                .freeze(),
            None,
        );
        let seq = transport_sequence(&RtpPacket::parse(out).unwrap(), 3).unwrap();
        writer.on_packet(seq, now);
        cc.on_feedback(&writer.flush(now).unwrap(), now);
        assert!(cc.maybe_probe(900, 1000, now).is_none(), "covered");
        assert!(cc.maybe_probe(2500, 1000, now).is_none(), "far off");
        let id = cc.maybe_probe(1500, 1000, now).expect("nearly covered");
        assert_eq!(cc.poll_probe(now), Some(id));
    }
}
