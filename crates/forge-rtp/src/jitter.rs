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
        let next_seq = self.next_seq?;
        let _base_time = self.base_time?;
        let now = Instant::now();

        // Check if we have the next expected packet
        if let Some(packet) = self.packets.get(&next_seq) {
            // Check if packet has been buffered long enough
            let buffered_time = now.duration_since(packet.received_at);

            if buffered_time >= self.target_delay {
                // Remove and return the packet
                let packet = self.packets.remove(&next_seq).unwrap();

                // Update next expected sequence
                self.next_seq = Some(Self::next_sequence(next_seq));

                return Some(packet.data);
            }
        } else {
            // Missing packet - check if we should skip it
            // If the next packet in the buffer is much newer, skip the missing one
            if let Some((&oldest_seq, oldest_packet)) = self.packets.iter().next() {
                let gap = Self::sequence_distance(next_seq, oldest_seq);
                let buffered_time = now.duration_since(oldest_packet.received_at);

                // If gap is large or we've waited too long, skip the missing packet
                if gap > 10 || buffered_time > self.max_delay {
                    tracing::debug!("Skipping missing packet seq={}, gap={}", next_seq, gap);
                    self.next_seq = Some(Self::next_sequence(next_seq));
                    self.stats.packets_dropped += 1;

                    // Try to pop again with the updated sequence
                    return self.pop();
                }
            }
        }

        None
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
