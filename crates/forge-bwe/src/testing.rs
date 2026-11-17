//! A simulated bottleneck on a virtual clock, for the estimators' tests
//! and the comparison in `tests/simulation.rs`.

use std::time::{Duration, Instant};

/// A link with a rate, a drop-tail queue, a one-way delay and random loss.
pub struct Bottleneck {
    rate_bps: f64,
    queue_bytes: usize,
    delay: Duration,
    loss: f64,
    rng: u64,
    /// When the link finishes sending what is queued.
    free_at: Option<Instant>,
}

impl Bottleneck {
    pub fn new(rate_bps: u64, queue_bytes: usize, delay: Duration) -> Self {
        Self {
            rate_bps: rate_bps as f64,
            queue_bytes,
            delay,
            loss: 0.0,
            rng: 0x9E37_79B9_7F4A_7C15,
            free_at: None,
        }
    }

    /// Lose this share of packets at random (a fixed seed: runs repeat).
    pub fn with_loss(mut self, loss: f64) -> Self {
        self.loss = loss;
        self
    }

    pub fn set_rate(&mut self, rate_bps: u64) {
        self.rate_bps = rate_bps as f64;
    }

    pub fn rate(&self) -> u64 {
        self.rate_bps as u64
    }

    /// How long a packet entering at `at` waits before it is sent.
    pub fn queue_delay(&self, at: Instant) -> Duration {
        self.free_at
            .map_or(Duration::ZERO, |f| f.saturating_duration_since(at))
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A packet of `size` bytes enters at `at` (in time order): when it
    /// arrives, or `None` when the queue is full or it is lost.
    pub fn send(&mut self, size: usize, at: Instant) -> Option<Instant> {
        let wait = self.queue_delay(at);
        let queued = wait.as_secs_f64() * self.rate_bps / 8.0;
        if queued + size as f64 > self.queue_bytes as f64 {
            return None;
        }
        let start = at + wait;
        let done = start + Duration::from_secs_f64(size as f64 * 8.0 / self.rate_bps);
        self.free_at = Some(done);
        if self.loss > 0.0 && self.random() < self.loss {
            return None;
        }
        Some(done + self.delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_queues_at_the_link_rate_and_a_full_queue_drops() {
        let t0 = Instant::now();
        // 1 Mb/s: 1250 bytes take 10 ms. Room for three.
        let mut b = Bottleneck::new(1_000_000, 3750, Duration::from_millis(20));
        let a: Vec<_> = (0..4).map(|_| b.send(1250, t0)).collect();
        assert_eq!(a[0], Some(t0 + Duration::from_millis(30)));
        assert_eq!(a[2], Some(t0 + Duration::from_millis(50)));
        assert_eq!(a[3], None, "the queue is full");
        assert_eq!(b.queue_delay(t0), Duration::from_millis(30));
    }
}
