//! The room's video clock.
//!
//! Ticks at the room's frame rate; the compositor renders on each tick.
//! When rendering overruns the tick three times in a row the clock halves
//! its rate and says so, so an overloaded node degrades to a lower frame
//! rate rather than falling behind; a quiet stretch lets it climb back.

use std::time::Duration;
use tokio::time::{interval, Instant, Interval, MissedTickBehavior};

/// What a tick reports besides its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    /// The rate was halved after three consecutive overruns.
    FpsHalved { from: u32, to: u32 },
    /// The rate was restored one step after a stretch without overruns.
    FpsRestored { from: u32, to: u32 },
}

/// A frame clock. Not `Sync`: one task owns it.
#[derive(Debug)]
pub struct VideoClock {
    target_fps: u32,
    fps: u32,
    min_fps: u32,
    interval: Interval,
    tick_no: u64,
    /// Start of the current tick's work; set by `tick`, read by `done`.
    tick_started: Option<Instant>,
    consecutive_overruns: u32,
    /// Ticks since the last overrun at the current rate.
    calm_ticks: u32,
    overruns_total: u64,
}

impl VideoClock {
    /// A clock at `fps` (1..=60); it never drops below `fps / 4`, and
    /// never below 1.
    pub fn new(fps: u32) -> Self {
        let fps = fps.clamp(1, 60);
        Self {
            target_fps: fps,
            fps,
            min_fps: (fps / 4).max(1),
            interval: make_interval(fps),
            tick_no: 0,
            tick_started: None,
            consecutive_overruns: 0,
            calm_ticks: 0,
            overruns_total: 0,
        }
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    pub fn target_fps(&self) -> u32 {
        self.target_fps
    }

    pub fn period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps as f64)
    }

    pub fn overruns(&self) -> u64 {
        self.overruns_total
    }

    /// Change the target rate (a host changed the room's setting).
    pub fn set_target_fps(&mut self, fps: u32) {
        let fps = fps.clamp(1, 60);
        self.target_fps = fps;
        self.min_fps = (fps / 4).max(1);
        self.fps = fps;
        self.interval = make_interval(fps);
        self.consecutive_overruns = 0;
        self.calm_ticks = 0;
    }

    /// Wait for the next tick. Returns the tick number; the caller
    /// renders, then calls [`done`](Self::done).
    pub async fn tick(&mut self) -> u64 {
        self.interval.tick().await;
        self.tick_no += 1;
        self.tick_started = Some(Instant::now());
        self.tick_no
    }

    /// Report that the tick's work finished. Adjusts the rate; returns an
    /// event when it changed.
    pub fn done(&mut self) -> Option<ClockEvent> {
        let started = self.tick_started.take()?;
        let took = started.elapsed();
        if took > self.period() {
            self.overruns_total += 1;
            self.consecutive_overruns += 1;
            self.calm_ticks = 0;
            if self.consecutive_overruns >= 3 && self.fps > self.min_fps {
                let from = self.fps;
                self.fps = (self.fps / 2).max(self.min_fps);
                self.interval = make_interval(self.fps);
                self.consecutive_overruns = 0;
                return Some(ClockEvent::FpsHalved { from, to: self.fps });
            }
        } else {
            self.consecutive_overruns = 0;
            self.calm_ticks += 1;
            // Ten calm seconds at the reduced rate: try the next step up.
            if self.fps < self.target_fps && self.calm_ticks >= self.fps * 10 {
                let from = self.fps;
                self.fps = (self.fps * 2).min(self.target_fps);
                self.interval = make_interval(self.fps);
                self.calm_ticks = 0;
                return Some(ClockEvent::FpsRestored { from, to: self.fps });
            }
        }
        None
    }
}

fn make_interval(fps: u32) -> Interval {
    let mut i = interval(Duration::from_secs_f64(1.0 / fps as f64));
    // A late tick is late; do not burst to catch up.
    i.set_missed_tick_behavior(MissedTickBehavior::Delay);
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn ticks_at_the_rate_and_backs_off_after_three_overruns() {
        let mut c = VideoClock::new(30);
        assert_eq!(c.fps(), 30);
        let n = c.tick().await;
        assert_eq!(n, 1);
        assert!(c.done().is_none());
        // Three ticks whose work takes longer than a period.
        let mut event = None;
        for _ in 0..3 {
            c.tick().await;
            tokio::time::advance(Duration::from_millis(50)).await;
            event = c.done();
        }
        assert_eq!(event, Some(ClockEvent::FpsHalved { from: 30, to: 15 }));
        assert_eq!(c.fps(), 15);
        assert_eq!(c.overruns(), 3);
        // Three more: down to the floor of 7, then no further.
        let mut last = None;
        for _ in 0..3 {
            c.tick().await;
            tokio::time::advance(Duration::from_millis(100)).await;
            last = c.done();
        }
        assert_eq!(last, Some(ClockEvent::FpsHalved { from: 15, to: 7 }));
        for _ in 0..3 {
            c.tick().await;
            tokio::time::advance(Duration::from_millis(200)).await;
            last = c.done();
        }
        assert_eq!(last, None, "already at the floor");
        assert_eq!(c.fps(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn calm_ticks_restore_the_rate_one_step_at_a_time() {
        let mut c = VideoClock::new(30);
        for _ in 0..3 {
            c.tick().await;
            tokio::time::advance(Duration::from_millis(50)).await;
            c.done();
        }
        assert_eq!(c.fps(), 15);
        let mut events = Vec::new();
        for _ in 0..150 {
            c.tick().await;
            events.extend(c.done());
        }
        assert_eq!(events, vec![ClockEvent::FpsRestored { from: 15, to: 30 }]);
        assert_eq!(c.fps(), 30);
        // done() without a tick is a no-op.
        assert!(c.done().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn setting_the_target_resets_state() {
        let mut c = VideoClock::new(60);
        c.set_target_fps(15);
        assert_eq!(c.fps(), 15);
        assert_eq!(c.target_fps(), 15);
        assert_eq!(c.period(), Duration::from_secs_f64(1.0 / 15.0));
        assert_eq!(VideoClock::new(0).fps(), 1);
        assert_eq!(VideoClock::new(500).fps(), 60);
    }
}
