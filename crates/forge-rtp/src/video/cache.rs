//! Outgoing-side helpers: a retransmission cache that answers NACKs, and
//! a gate that keeps keyframe requests from becoming keyframe floods.

use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// The last N packets sent on one stream, by sequence number, so a NACK
/// can be answered with the original bytes (same SSRC, RFC 4585 §6.2.1).
#[derive(Debug)]
pub struct RtxCache {
    capacity: usize,
    order: VecDeque<u16>,
    packets: HashMap<u16, Bytes>,
}

impl RtxCache {
    /// Keep the last `capacity` packets. At 1.2 Mb/s and 1200-byte
    /// payloads, 512 packets is about four seconds.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            packets: HashMap::with_capacity(capacity),
        }
    }

    /// Remember a packet as sent (the full RTP packet bytes, before SRTP).
    pub fn push(&mut self, seq: u16, packet: Bytes) {
        if self.packets.insert(seq, packet).is_none() {
            self.order.push_back(seq);
        }
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.packets.remove(&old);
            }
        }
    }

    /// The packet with `seq`, if still cached.
    pub fn get(&self, seq: u16) -> Option<&Bytes> {
        self.packets.get(&seq)
    }

    /// The cached packets for a NACK's sequence numbers, in the order
    /// asked; missing ones are skipped.
    pub fn lookup<'a>(&'a self, seqs: &[u16]) -> Vec<(u16, &'a Bytes)> {
        seqs.iter()
            .filter_map(|&s| self.packets.get(&s).map(|p| (s, p)))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.packets.clear();
    }
}

/// Lets a keyframe request (PLI / FIR, or an encoder's forced intra
/// frame) through at most once per interval. Requests in between are
/// absorbed: the keyframe already on its way answers them.
#[derive(Debug)]
pub struct KeyframeRequestGate {
    min_interval: Duration,
    last: Option<Instant>,
    absorbed: u64,
}

impl KeyframeRequestGate {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: None,
            absorbed: 0,
        }
    }

    /// Whether a request now should go through.
    pub fn allow(&mut self) -> bool {
        self.allow_at(Instant::now())
    }

    /// [`allow`](Self::allow) at a given instant.
    pub fn allow_at(&mut self, now: Instant) -> bool {
        match self.last {
            Some(t) if now.duration_since(t) < self.min_interval => {
                self.absorbed += 1;
                false
            }
            _ => {
                self.last = Some(now);
                true
            }
        }
    }

    /// Requests absorbed since start.
    pub fn absorbed(&self) -> u64 {
        self.absorbed
    }

    /// Forget the last request (the stream restarted).
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keeps_the_newest_and_answers_lookups_in_asked_order() {
        let mut c = RtxCache::new(3);
        for s in [10u16, 11, 12] {
            c.push(s, Bytes::from(vec![s as u8]));
        }
        assert_eq!(c.len(), 3);
        c.push(13, Bytes::from_static(b"\x0d"));
        assert!(c.get(10).is_none(), "evicted");
        assert_eq!(c.get(13).unwrap().as_ref(), b"\x0d");
        let found = c.lookup(&[13, 10, 11]);
        assert_eq!(
            found.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![13, 11]
        );
        // Re-pushing a sequence number replaces without growing.
        c.push(13, Bytes::from_static(b"\xff"));
        assert_eq!(c.len(), 3);
        assert_eq!(c.get(13).unwrap().as_ref(), b"\xff");
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn gate_passes_one_request_per_interval() {
        let mut g = KeyframeRequestGate::new(Duration::from_millis(500));
        let t0 = Instant::now();
        assert!(g.allow_at(t0));
        assert!(!g.allow_at(t0 + Duration::from_millis(100)));
        assert!(!g.allow_at(t0 + Duration::from_millis(499)));
        assert_eq!(g.absorbed(), 2);
        assert!(g.allow_at(t0 + Duration::from_millis(500)));
        g.reset();
        assert!(g.allow_at(t0 + Duration::from_millis(501)));
    }
}
