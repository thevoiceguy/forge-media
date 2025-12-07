//! Jitter buffer implementation

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Adaptive jitter buffer for RTP packets
pub struct JitterBuffer {
    packets: BTreeMap<u16, JitterPacket>,
    target_delay: Duration,
    max_delay: Duration,
    min_delay: Duration,
}

struct JitterPacket {
    sequence: u16,
    timestamp: u32,
    received_at: Instant,
    data: Vec<u8>,
}

impl JitterBuffer {
    pub fn new(target_delay: Duration) -> Self {
        Self {
            packets: BTreeMap::new(),
            target_delay,
            max_delay: target_delay * 3,
            min_delay: target_delay / 2,
        }
    }

    /// Add a packet to the jitter buffer
    pub fn push(&mut self, sequence: u16, timestamp: u32, data: Vec<u8>) {
        self.packets.insert(
            sequence,
            JitterPacket {
                sequence,
                timestamp,
                received_at: Instant::now(),
                data,
            },
        );
    }

    /// Get the next packet if available
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        // TODO: Implement proper jitter buffer logic with timing
        self.packets.pop_first().map(|(_, packet)| packet.data)
    }

    /// Clear the jitter buffer
    pub fn clear(&mut self) {
        self.packets.clear();
    }
}
