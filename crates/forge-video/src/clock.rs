//! The room's video clock.
//!
//! Ticks at the room's frame rate; the compositor renders on each tick.
//! When rendering overruns the tick three times in a row the room is asked
//! to shed something, and if it has nothing to shed the clock halves its
//! rate and says so, so an overloaded node degrades rather than falling
//! behind; a quiet stretch lets it climb back.
//!
//! Halving the rate is the last resort because it is the one everybody in
//! the room pays: a room overrunning on one expensive composite should
//! degrade that composite, not everyone's motion (§9). What "something" is
//! belongs to whoever owns the outputs, so the clock asks a [`LoadShedder`]
//! and knows nothing about what it gives up.

use std::time::Duration;
use tokio::time::{interval, Instant, Interval, MissedTickBehavior};

/// What a tick reports besides its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    /// The rate was halved after three consecutive overruns, there being
    /// nothing left to shed.
    FpsHalved { from: u32, to: u32 },
    /// The rate was restored one step after a stretch without overruns.
    FpsRestored { from: u32, to: u32 },
}

/// What the room can give up, asked by the clock before it touches the rate.
///
/// Both methods return whether they acted. The clock calls [`shed`](Self::shed)
/// when it is about to halve, and halves only if the answer is `false`;
/// recovery unwinds in the opposite order, so [`restore`](Self::restore) is
/// called only once the rate is back at the room's target — the last thing
/// given up is the first thing returned.
pub trait LoadShedder {
    /// Overloaded: give up one step. `false` when there is nothing left.
    fn shed(&mut self) -> bool;
    /// Calm at the full rate: take one step back. `false` when nothing is
    /// currently shed.
    fn restore(&mut self) -> bool;
}

/// A shedder with nothing to shed: the frame rate is the only thing this
/// clock can give up. What [`VideoClock::done`] uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateOnly;

impl LoadShedder for RateOnly {
    fn shed(&mut self) -> bool {
        false
    }

    fn restore(&mut self) -> bool {
        false
    }
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

    /// Report that the tick's work finished, with nothing to shed but the
    /// rate. Adjusts the rate; returns an event when it changed.
    pub fn done(&mut self) -> Option<ClockEvent> {
        self.done_with(&mut RateOnly)
    }

    /// Report that the tick's work finished, offering `shedder` the chance
    /// to give up something smaller than everyone's frame rate.
    ///
    /// Three consecutive overruns ask it to shed; the rate halves only when
    /// it has nothing left. Ten calm seconds restore the rate a step, and
    /// once the rate is back at the room's target, ten more give back one
    /// thing that was shed.
    pub fn done_with(&mut self, shedder: &mut impl LoadShedder) -> Option<ClockEvent> {
        let started = self.tick_started.take()?;
        let took = started.elapsed();
        if took > self.period() {
            self.overruns_total += 1;
            self.consecutive_overruns += 1;
            self.calm_ticks = 0;
            if self.consecutive_overruns >= 3 {
                // Whatever happens now, start counting again: acting on the
                // same three overruns on every subsequent tick would shed
                // the room to nothing before the first change had a chance
                // to show in the timings.
                self.consecutive_overruns = 0;
                // Something smaller than everyone's motion, if the room has
                // one to give.
                if shedder.shed() {
                    return None;
                }
                if self.fps > self.min_fps {
                    let from = self.fps;
                    self.fps = (self.fps / 2).max(self.min_fps);
                    self.interval = make_interval(self.fps);
                    return Some(ClockEvent::FpsHalved { from, to: self.fps });
                }
            }
        } else {
            self.consecutive_overruns = 0;
            self.calm_ticks += 1;
            // Ten calm seconds: undo one step of what the overload cost,
            // most recent first — the rate, and only once it is whole
            // again, whatever was shed before it.
            if self.calm_ticks >= self.fps * 10 {
                if self.fps < self.target_fps {
                    let from = self.fps;
                    self.fps = (self.fps * 2).min(self.target_fps);
                    self.interval = make_interval(self.fps);
                    self.calm_ticks = 0;
                    return Some(ClockEvent::FpsRestored { from, to: self.fps });
                }
                if shedder.restore() {
                    self.calm_ticks = 0;
                }
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

    /// A shedder with `steps` things to give up, counting what it was asked.
    #[derive(Default)]
    struct Counting {
        available: u32,
        shed: u32,
        restored: u32,
    }

    impl Counting {
        fn with(available: u32) -> Self {
            Self {
                available,
                ..Default::default()
            }
        }
    }

    impl LoadShedder for Counting {
        fn shed(&mut self) -> bool {
            if self.shed >= self.available {
                return false;
            }
            self.shed += 1;
            true
        }

        fn restore(&mut self) -> bool {
            if self.shed == 0 {
                return false;
            }
            self.shed -= 1;
            self.restored += 1;
            true
        }
    }

    /// Three overruns at a time, as `done_with` counts them.
    async fn overrun(c: &mut VideoClock, s: &mut Counting, by: Duration) -> Option<ClockEvent> {
        let mut last = None;
        for _ in 0..3 {
            c.tick().await;
            tokio::time::advance(by).await;
            last = c.done_with(s);
        }
        last
    }

    #[tokio::test(start_paused = true)]
    async fn sheds_before_it_touches_everybody_s_frame_rate() {
        let mut c = VideoClock::new(30);
        let mut s = Counting::with(2);

        // Two rounds of overruns: two things shed, rate untouched.
        assert_eq!(
            overrun(&mut c, &mut s, Duration::from_millis(50)).await,
            None
        );
        assert_eq!(s.shed, 1);
        assert_eq!(c.fps(), 30, "the rate is the last resort");
        assert_eq!(
            overrun(&mut c, &mut s, Duration::from_millis(50)).await,
            None
        );
        assert_eq!(s.shed, 2);
        assert_eq!(c.fps(), 30);

        // Nothing left to shed: now the rate halves, as it always did.
        assert_eq!(
            overrun(&mut c, &mut s, Duration::from_millis(50)).await,
            Some(ClockEvent::FpsHalved { from: 30, to: 15 })
        );
        assert_eq!(s.shed, 2, "shedding was asked and refused, not skipped");
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_takes_the_rate_back_before_what_was_shed() {
        let mut c = VideoClock::new(30);
        let mut s = Counting::with(1);
        // One shed, then the rate halves.
        overrun(&mut c, &mut s, Duration::from_millis(50)).await;
        assert_eq!(s.shed, 1);
        assert_eq!(
            overrun(&mut c, &mut s, Duration::from_millis(50)).await,
            Some(ClockEvent::FpsHalved { from: 30, to: 15 })
        );

        // Calm: the rate comes back first, and nothing is unshed yet.
        let mut event = None;
        for _ in 0..(15 * 10) {
            c.tick().await;
            if let Some(e) = c.done_with(&mut s) {
                event = Some(e);
            }
        }
        assert_eq!(event, Some(ClockEvent::FpsRestored { from: 15, to: 30 }));
        assert_eq!(s.restored, 0, "the rate is given back before the picture");

        // Calm at the full rate: now what was shed comes back.
        for _ in 0..(30 * 10) {
            c.tick().await;
            assert_eq!(c.done_with(&mut s), None);
        }
        assert_eq!(s.restored, 1);
        assert_eq!(s.shed, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_room_with_nothing_shed_stays_at_its_rate_however_calm() {
        let mut c = VideoClock::new(30);
        let mut s = Counting::with(0);
        for _ in 0..(30 * 25) {
            c.tick().await;
            assert_eq!(c.done_with(&mut s), None);
        }
        assert_eq!(c.fps(), 30);
        assert_eq!(s.restored, 0);
    }

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
