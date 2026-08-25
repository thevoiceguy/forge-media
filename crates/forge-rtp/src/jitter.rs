//! Jitter buffer implementation

use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tracing;

/// Adaptive jitter buffer for RTP packets
pub struct JitterBuffer {
    /// Packets stored by sequence number
    packets: BTreeMap<u16, JitterPacket>,
    /// Target buffer delay
    target_delay: Duration,
    /// Maximum buffer delay before dropping
    max_delay: Duration,
    /// Minimum buffer delay
    #[allow(dead_code)]
    min_delay: Duration,
    /// Next expected sequence number
    next_seq: Option<u16>,
    /// Base timestamp for timing calculations
    base_time: Option<Instant>,
    /// Statistics
    stats: JitterStats,
}

struct JitterPacket {
    #[allow(dead_code)]
    sequence: u16,
    #[allow(dead_code)]
    timestamp: u32,
    received_at: Instant,
    data: Vec<u8>,
}

/// Jitter buffer statistics
#[derive(Debug, Clone, Default)]
pub struct JitterStats {
    /// Total packets received
    pub packets_received: u64,
    /// Packets dropped (too late)
    pub packets_dropped: u64,
    /// Out-of-order packets
    pub packets_out_of_order: u64,
    /// Duplicate packets
    pub packets_duplicate: u64,
}

impl JitterBuffer {
    /// Create a new jitter buffer with the specified target delay
    pub fn new(target_delay: Duration) -> Self {
        Self {
            packets: BTreeMap::new(),
            target_delay,
            max_delay: target_delay * 3,
            min_delay: target_delay / 2,
            next_seq: None,
            base_time: None,
            stats: JitterStats::default(),
        }
    }

    /// Add a packet to the jitter buffer
    pub fn push(&mut self, sequence: u16, timestamp: u32, data: Vec<u8>) {
        let now = Instant::now();

        // Initialize base time on first packet
        if self.base_time.is_none() {
            self.base_time = Some(now);
            self.next_seq = Some(sequence);
        }

        // Check for duplicate
        if self.packets.contains_key(&sequence) {
            self.stats.packets_duplicate += 1;
            return;
        }

        // Check if packet is out of order
        if let Some(next) = self.next_seq {
            if !Self::is_sequence_newer(sequence, next) && sequence != next {
                self.stats.packets_out_of_order += 1;
            }
        }

        self.stats.packets_received += 1;

        // Insert packet
        self.packets.insert(
            sequence,
            JitterPacket {
                sequence,
                timestamp,
                received_at: now,
                data,
            },
        );

        // Limit buffer size to prevent unbounded growth
        const MAX_BUFFER_SIZE: usize = 100;
        if self.packets.len() > MAX_BUFFER_SIZE {
            // Drop oldest packet
            if let Some((seq, _)) = self.packets.pop_first() {
                self.stats.packets_dropped += 1;
                tracing::warn!("Buffer overflow, dropping packet seq={}", seq);
            }
        }
    }

    /// Get the next packet if it's ready to be played out
    ///
    /// This will return `Some(data)` if:
    /// - The next expected packet is available
    /// - The packet has been in the buffer for at least `target_delay`
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.base_time?;
        let now = Instant::now();

        // Discard arrivals whose playout slot has already passed. A
        // late packet left in the map is invisible to the `get`
        // below (it is behind `next_seq`) but *is* what
        // `packets.iter().next()` returns, and the unsigned distance
        // from `next_seq` back to it reads as a ~65000-packet forward
        // gap — which used to drive the skip branch one sequence
        // number at a time, forever. One reordered packet was enough.
        self.drop_stale(self.next_seq);

        // Bounded: each iteration either returns or advances
        // `next_seq` past one missing packet, and the buffer holds at
        // most MAX_BUFFER_SIZE packets to advance towards. Written as
        // a loop rather than recursion — the recursive form overflowed
        // the stack rather than merely spinning.
        loop {
            let next_seq = self.next_seq?;

            // The packet we are waiting for, held long enough?
            if let Some(packet) = self.packets.get(&next_seq) {
                if now.duration_since(packet.received_at) < self.target_delay {
                    return None;
                }
                let packet = self
                    .packets
                    .remove(&next_seq)
                    .expect("presence checked immediately above");
                self.next_seq = Some(Self::next_sequence(next_seq));
                return Some(packet.data);
            }

            // Missing. Skip it only once something newer has waited
            // long enough, or the gap is wide enough that it is not
            // coming.
            let Some((&oldest_seq, oldest_packet)) = self.packets.iter().next() else {
                return None;
            };
            let gap = Self::sequence_distance(next_seq, oldest_seq);
            let waited = now.duration_since(oldest_packet.received_at);
            if gap > 10 || waited > self.max_delay {
                tracing::debug!("Skipping missing packet seq={}, gap={}", next_seq, gap);
                self.next_seq = Some(Self::next_sequence(next_seq));
                self.stats.packets_dropped += 1;
                continue;
            }
            return None;
        }
    }

    /// Drop packets that are older than `next_seq` — their playout
    /// moment has passed, so they can never be returned, and leaving
    /// them in the map corrupts the gap arithmetic above.
    fn drop_stale(&mut self, next_seq: Option<u16>) {
        let Some(next_seq) = next_seq else { return };
        let before = self.packets.len();
        self.packets
            .retain(|&seq, _| seq == next_seq || Self::is_sequence_newer(seq, next_seq));
        let dropped = before - self.packets.len();
        if dropped > 0 {
            self.stats.packets_dropped += dropped as u64;
            tracing::debug!(dropped, next_seq, "dropping late packets");
        }
    }

    /// Check if a packet is ready to be played out
    pub fn is_ready(&self) -> bool {
        if let (Some(next_seq), Some(_base_time)) = (self.next_seq, self.base_time) {
            if let Some(packet) = self.packets.get(&next_seq) {
                let now = Instant::now();
                let buffered_time = now.duration_since(packet.received_at);
                return buffered_time >= self.target_delay;
            }
        }
        false
    }

    /// Get current buffer size
    pub fn size(&self) -> usize {
        self.packets.len()
    }

    /// Get statistics
    pub fn stats(&self) -> &JitterStats {
        &self.stats
    }

    /// Clear the jitter buffer
    pub fn clear(&mut self) {
        self.packets.clear();
        self.next_seq = None;
        self.base_time = None;
    }

    /// Check if sequence number `a` is newer than `b` (handling wraparound)
    fn is_sequence_newer(a: u16, b: u16) -> bool {
        const SEQUENCE_WRAP: u16 = u16::MAX / 2;
        a.wrapping_sub(b) < SEQUENCE_WRAP
    }

    /// Calculate distance between two sequence numbers (handling wraparound)
    fn sequence_distance(from: u16, to: u16) -> u16 {
        to.wrapping_sub(from)
    }

    /// Get the next sequence number
    fn next_sequence(seq: u16) -> u16 {
        seq.wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a single reordered packet used to abort the
    /// process. After `pop` advanced past seq 2, the late seq 1 stayed
    /// in the map; `packets.iter().next()` returned it, the unsigned
    /// distance 3 -> 1 read as 65534, and the skip branch recursed once
    /// per sequence number until the stack was gone. Reordering is
    /// ordinary on any internet path, so this was reachable by normal
    /// traffic, not just by an attacker.
    #[test]
    fn a_late_packet_does_not_blow_the_stack() {
        let mut jb = JitterBuffer::new(Duration::from_millis(0));
        jb.push(2, 320, vec![0x02; 160]);
        jb.push(1, 160, vec![0x01; 160]);

        // seq 2 established the stream, so it plays out first.
        assert_eq!(jb.pop(), Some(vec![0x02; 160]));
        // Before the fix this recursed ~65k deep and aborted.
        assert_eq!(jb.pop(), None, "the late packet is dropped, not replayed");
        assert!(jb.stats().packets_dropped >= 1);
    }

    /// The same shape one step further out: several late packets, and
    /// a live stream continuing after them. The late ones must be
    /// discarded and the stream must keep flowing.
    #[test]
    fn late_packets_are_discarded_and_the_stream_continues() {
        let mut jb = JitterBuffer::new(Duration::from_millis(0));
        jb.push(100, 0, vec![100; 8]);
        assert_eq!(jb.pop(), Some(vec![100; 8]));

        for seq in [95u16, 96, 97] {
            jb.push(seq, 0, vec![seq as u8; 8]);
        }
        jb.push(101, 0, vec![101; 8]);
        assert_eq!(
            jb.pop(),
            Some(vec![101; 8]),
            "the in-order continuation still plays"
        );
        assert_eq!(jb.size(), 0, "stale packets were not left behind");
    }

    /// A wide forward gap (a real loss burst) is skipped without
    /// spinning per sequence number.
    #[test]
    fn a_wide_forward_gap_is_skipped_promptly() {
        let mut jb = JitterBuffer::new(Duration::from_millis(0));
        jb.push(1, 0, vec![1; 8]);
        assert_eq!(jb.pop(), Some(vec![1; 8]));
        // 2..=50 are lost; 51 arrives.
        jb.push(51, 0, vec![51; 8]);
        assert_eq!(jb.pop(), Some(vec![51; 8]), "skips the hole and plays on");
    }

    #[test]
    fn test_sequence_newer() {
        assert!(JitterBuffer::is_sequence_newer(100, 50));
        assert!(!JitterBuffer::is_sequence_newer(50, 100));

        // Test wraparound
        assert!(JitterBuffer::is_sequence_newer(10, 65500));
        assert!(!JitterBuffer::is_sequence_newer(65500, 10));
    }

    #[test]
    fn test_sequence_distance() {
        assert_eq!(JitterBuffer::sequence_distance(100, 105), 5);
        assert_eq!(JitterBuffer::sequence_distance(65535, 2), 3);
    }

    #[test]
    fn test_jitter_buffer_basic() {
        let mut buffer = JitterBuffer::new(Duration::from_millis(50));

        // Add some packets
        buffer.push(1, 1000, vec![1, 2, 3]);
        buffer.push(2, 2000, vec![4, 5, 6]);
        buffer.push(3, 3000, vec![7, 8, 9]);

        assert_eq!(buffer.size(), 3);
        assert_eq!(buffer.stats().packets_received, 3);
    }

    #[test]
    fn test_jitter_buffer_timing() {
        // 100 ms / 150 ms thresholds (not 10 / 15) so the "not yet ready"
        // check stays robust under tarpaulin's ptrace instrumentation
        // overhead, which can easily eat the 10ms window between
        // `push` and `is_ready` on a busy runner.
        let mut buffer = JitterBuffer::new(Duration::from_millis(100));

        buffer.push(1, 1000, vec![1, 2, 3]);

        // Should not be ready immediately.
        assert!(!buffer.is_ready());

        // Wait past target delay.
        std::thread::sleep(Duration::from_millis(150));

        // Should be ready now.
        assert!(buffer.is_ready());

        let data = buffer.pop();
        assert!(data.is_some());
        assert_eq!(data.unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn test_duplicate_detection() {
        let mut buffer = JitterBuffer::new(Duration::from_millis(50));

        buffer.push(1, 1000, vec![1, 2, 3]);
        buffer.push(1, 1000, vec![1, 2, 3]); // Duplicate

        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.stats().packets_duplicate, 1);
    }
}
