//! The Google Congestion Control core (draft-ietf-rmcat-gcc-02), shared by
//! the receive side ([`crate::RembEstimator`]) and the send side
//! ([`crate::SendSideEstimator`]):
//! - [`Trendline`]: how much later each group of packets arrived than it
//!   was sent; a queue building somewhere shows as a rising trend, which
//!   is overuse (§5.3, the trendline filter libwebrtc uses);
//! - [`Aimd`]: the rate, up by 8% a second while nothing says otherwise,
//!   down to 85% of what gets through on overuse (§5.5);
//! - [`LossCap`]: a cap that heavy loss brings down and light loss lets
//!   go (§6).

use std::collections::VecDeque;

/// The trend is a regression over this many deltas.
const TREND_WINDOW: usize = 20;
const TREND_SMOOTHING: f64 = 0.9;
const TREND_GAIN: f64 = 4.0;
/// The overuse threshold adapts between these, in ms (GCC §5.4).
const THRESHOLD_START: f64 = 12.5;
const THRESHOLD_MIN: f64 = 6.0;
const THRESHOLD_MAX: f64 = 600.0;
const K_UP: f64 = 0.0087;
const K_DOWN: f64 = 0.039;
/// Overuse must last this long, in ms, before it is believed.
const OVERUSE_MS: f64 = 10.0;
/// Each second without trouble, the rate may rise this much.
pub const INCREASE_PER_SECOND: f64 = 1.08;
/// On overuse, the rate is set to this share of what gets through.
pub const DECREASE: f64 = 0.85;
/// Loss above this brings the rate down; below the lower one it no longer
/// holds it back.
pub const LOSS_HIGH: f64 = 0.10;
pub const LOSS_LOW: f64 = 0.02;

/// What the delay says of the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Normal,
    /// A queue is building: send less.
    Over,
    /// A queue is draining: hold, it will be empty soon.
    Under,
}

/// The trendline delay detector. Times are milliseconds on any one clock.
#[derive(Debug, Clone)]
pub struct Trendline {
    first_arrival: Option<f64>,
    accumulated: f64,
    smoothed: f64,
    /// (arrival since the first, smoothed delay), both in ms.
    history: VecDeque<(f64, f64)>,
    deltas: usize,
    threshold: f64,
    last_threshold_update: Option<f64>,
    over_since: Option<f64>,
    previous_trend: f64,
    usage: Usage,
}

impl Default for Trendline {
    fn default() -> Self {
        Self {
            first_arrival: None,
            accumulated: 0.0,
            smoothed: 0.0,
            history: VecDeque::with_capacity(TREND_WINDOW + 1),
            deltas: 0,
            threshold: THRESHOLD_START,
            last_threshold_update: None,
            over_since: None,
            previous_trend: 0.0,
            usage: Usage::Normal,
        }
    }
}

impl Trendline {
    /// The usage as of the last delta.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Two groups: the second was sent `send_ms` after the first and
    /// arrived `arrive_ms` after it, at `arrival_ms`.
    pub fn update(&mut self, send_ms: f64, arrive_ms: f64, arrival_ms: f64) -> Usage {
        // A jump (a pause, a new stream) says nothing about a queue.
        if send_ms > 1000.0 {
            return self.usage;
        }
        let first = *self.first_arrival.get_or_insert(arrival_ms);
        self.accumulated += arrive_ms - send_ms;
        self.smoothed =
            TREND_SMOOTHING * self.smoothed + (1.0 - TREND_SMOOTHING) * self.accumulated;
        self.history.push_back((arrival_ms - first, self.smoothed));
        if self.history.len() > TREND_WINDOW {
            self.history.pop_front();
        }
        self.deltas = (self.deltas + 1).min(60);
        if self.history.len() < TREND_WINDOW {
            return self.usage;
        }
        let trend = slope(&self.history).unwrap_or(0.0) * self.deltas as f64 * TREND_GAIN;
        self.detect(trend, arrival_ms);
        self.usage
    }

    fn detect(&mut self, trend: f64, now: f64) {
        if trend > self.threshold {
            let since = *self.over_since.get_or_insert(now);
            if now - since >= OVERUSE_MS && trend >= self.previous_trend {
                self.usage = Usage::Over;
            }
        } else if trend < -self.threshold {
            self.over_since = None;
            self.usage = Usage::Under;
        } else {
            self.over_since = None;
            self.usage = Usage::Normal;
        }
        self.previous_trend = trend;
        // The threshold follows the trend, slowly up and faster down, so
        // a competing flow does not starve us (GCC §5.4).
        let dt = self
            .last_threshold_update
            .map_or(0.0, |t| (now - t).max(0.0))
            .min(100.0);
        self.last_threshold_update = Some(now);
        if trend.abs() < self.threshold + 15.0 {
            let k = if trend.abs() < self.threshold {
                K_DOWN
            } else {
                K_UP
            };
            self.threshold += k * (trend.abs() - self.threshold) * dt;
            self.threshold = self.threshold.clamp(THRESHOLD_MIN, THRESHOLD_MAX);
        }
    }
}

/// The least-squares slope of `points`.
fn slope(points: &VecDeque<(f64, f64)>) -> Option<f64> {
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = points.iter().fold((0.0, 0.0), |(num, den), (x, y)| {
        (num + (x - mx) * (y - my), den + (x - mx) * (x - mx))
    });
    (den > 0.0).then(|| num / den)
}

/// How the rate is held near what gets through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Headroom {
    /// The rate is cut to `factor` × what gets through plus `bps`: a
    /// receiver's estimate, which cannot see past what arrives.
    Cut { factor: f64, bps: f64 },
    /// The rate may not rise past `factor` × what gets through plus `bps`,
    /// but is not cut to it: a sender's, whose probes see past it.
    Hold { factor: f64, bps: f64 },
}

/// While overuse lasts, the rate comes down again this often (GCC cuts
/// once per round trip; this is a round trip and a feedback interval on
/// a long path).
pub const DECREASE_EVERY: f64 = 0.3;

/// The additive-increase, multiplicative-decrease rate.
#[derive(Debug, Clone)]
pub struct Aimd {
    pub rate: f64,
    /// The rate came down: hold until overuse ends.
    holding: bool,
    /// Seconds since the last decrease, while overuse lasts.
    since_decrease: Option<f64>,
    headroom: Headroom,
}

impl Aimd {
    pub fn new(start_bps: f64, headroom: Headroom) -> Self {
        Self {
            rate: start_bps,
            holding: false,
            since_decrease: None,
            headroom,
        }
    }

    /// `dt` seconds on, with `usage` and `through_bps` getting through.
    pub fn update(&mut self, usage: Usage, through_bps: f64, dt: f64) -> f64 {
        let before = self.rate;
        match usage {
            Usage::Over => {
                // Down at once; and down again while the queue still
                // grows, a decrease's worth of time later.
                let due = self
                    .since_decrease
                    .map_or(true, |s| s + dt >= DECREASE_EVERY);
                if due {
                    self.rate = (DECREASE * through_bps).min(self.rate);
                    self.since_decrease = Some(0.0);
                } else if let Some(s) = self.since_decrease.as_mut() {
                    *s += dt;
                }
                self.holding = true;
            }
            Usage::Under => {
                self.holding = true;
                self.since_decrease = None;
            }
            Usage::Normal => {
                self.holding = false;
                self.since_decrease = None;
                self.rate *= INCREASE_PER_SECOND.powf(dt);
            }
        }
        match self.headroom {
            Headroom::Cut { factor, bps } => self.rate = self.rate.min(factor * through_bps + bps),
            Headroom::Hold { factor, bps } => {
                let cap = factor * through_bps + bps;
                if self.rate > before && self.rate > cap {
                    self.rate = before.max(cap);
                }
            }
        }
        self.rate
    }

    /// A probe found the path carries `bps`: take it, unless a queue is
    /// building.
    pub fn probed(&mut self, bps: f64, usage: Usage) {
        if usage != Usage::Over && bps > self.rate {
            self.rate = bps;
            self.holding = false;
            self.since_decrease = None;
        }
    }
}

/// The loss-based cap.
#[derive(Debug, Clone, Default)]
pub struct LossCap {
    pub cap: Option<f64>,
}

impl LossCap {
    /// `dt` seconds on, with `loss` the share lost recently: the rate to
    /// use, `rate` (the delay-based one) or the cap below it.
    pub fn update(&mut self, loss: f64, rate: f64, through_bps: f64, dt: f64) -> f64 {
        if loss > LOSS_HIGH {
            let cap = self.cap.unwrap_or(rate).min(through_bps.max(1.0));
            // Once per update period of loss, as GCC's sender does on
            // each report.
            self.cap = Some(cap * (1.0 - 0.5 * loss * dt.max(0.2)));
        } else if loss < LOSS_LOW {
            self.cap = self
                .cap
                .map(|c| c * INCREASE_PER_SECOND.powf(dt))
                .filter(|c| *c < rate);
        }
        self.cap.map_or(rate, |c| c.min(rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steady_path_is_normal_and_a_growing_delay_is_overuse() {
        let mut t = Trendline::default();
        for i in 0..40 {
            t.update(33.0, 33.0, f64::from(i) * 33.0);
        }
        assert_eq!(t.usage(), Usage::Normal);
        for i in 40..80 {
            t.update(33.0, 37.0, f64::from(i) * 37.0);
        }
        assert_eq!(t.usage(), Usage::Over);
    }

    #[test]
    fn a_sender_s_rate_is_held_not_cut_by_what_gets_through() {
        let mut a = Aimd::new(
            2_000_000.0,
            Headroom::Hold {
                factor: 1.5,
                bps: 10_000.0,
            },
        );
        // Only 500 kb/s is being sent: the rate does not climb, nor fall.
        assert_eq!(a.update(Usage::Normal, 500_000.0, 1.0), 2_000_000.0);
        let mut r = Aimd::new(
            2_000_000.0,
            Headroom::Cut {
                factor: 1.5,
                bps: 50_000.0,
            },
        );
        assert_eq!(r.update(Usage::Normal, 500_000.0, 1.0), 800_000.0);
        // A probe lifts the sender's.
        a.probed(3_000_000.0, Usage::Normal);
        assert_eq!(a.rate, 3_000_000.0);
        a.probed(4_000_000.0, Usage::Over);
        assert_eq!(a.rate, 3_000_000.0, "not while a queue builds");
    }
}
