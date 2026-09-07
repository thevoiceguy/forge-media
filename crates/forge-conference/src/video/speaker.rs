//! Active-speaker selection with hysteresis (design §5.2).
//!
//! The audio mixer gives a smoothed energy per participant; this picks
//! the one the layouts call "the speaker". A challenger must be the
//! loudest for `take_after` (800 ms) before it takes over, and the
//! incumbent keeps the spot for at least `hold_for` (2 s) after taking
//! it, so cross-talk and a cough do not make the layout flap. When
//! nobody speaks the last speaker keeps the spot: an empty spotlight is
//! worse than a quiet one.

use std::time::{Duration, Instant};

/// One participant's audio level for a tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Level<'a> {
    pub id: &'a str,
    /// Smoothed RMS from the mixer (0 = silence).
    pub energy: f32,
    /// The mixer's voice-activity verdict.
    pub speaking: bool,
}

#[derive(Debug)]
pub struct ActiveSpeaker {
    take_after: Duration,
    hold_for: Duration,
    current: Option<String>,
    current_since: Option<Instant>,
    challenger: Option<String>,
    challenger_since: Option<Instant>,
    /// Ids that spoke, most recent first; feeds the speaker strip.
    recent: Vec<String>,
}

impl Default for ActiveSpeaker {
    fn default() -> Self {
        Self::new(Duration::from_millis(800), Duration::from_secs(2))
    }
}

impl ActiveSpeaker {
    pub fn new(take_after: Duration, hold_for: Duration) -> Self {
        Self {
            take_after,
            hold_for,
            current: None,
            current_since: None,
            challenger: None,
            challenger_since: None,
            recent: Vec::new(),
        }
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Participants ordered by recency of speech, the speaker first.
    pub fn recent(&self) -> &[String] {
        &self.recent
    }

    /// Forget a participant who left.
    pub fn remove(&mut self, id: &str) {
        if self.current.as_deref() == Some(id) {
            self.current = None;
            self.current_since = None;
        }
        if self.challenger.as_deref() == Some(id) {
            self.challenger = None;
            self.challenger_since = None;
        }
        self.recent.retain(|r| r != id);
    }

    /// Feed one tick of levels. Returns the new speaker when it changed.
    pub fn update(&mut self, levels: &[Level<'_>], now: Instant) -> Option<String> {
        // Everyone speaking moves up the recency list.
        for l in levels.iter().filter(|l| l.speaking) {
            if self.recent.first().map(String::as_str) != Some(l.id) {
                self.recent.retain(|r| r != l.id);
                self.recent.insert(0, l.id.to_string());
            }
        }

        let loudest = levels
            .iter()
            .filter(|l| l.speaking && l.energy > 0.0)
            .max_by(|a, b| a.energy.total_cmp(&b.energy))
            .map(|l| l.id);

        let Some(loudest) = loudest else {
            // Silence: the challenger loses its run, the incumbent stays.
            self.challenger = None;
            self.challenger_since = None;
            return None;
        };

        if self.current.as_deref() == Some(loudest) {
            self.challenger = None;
            self.challenger_since = None;
            return None;
        }

        // First speaker ever, or the incumbent left: take it at once.
        if self.current.is_none() {
            return Some(self.take(loudest, now));
        }

        if self.challenger.as_deref() != Some(loudest) {
            self.challenger = Some(loudest.to_string());
            self.challenger_since = Some(now);
            return None;
        }

        let dominated = self
            .challenger_since
            .map(|s| now.duration_since(s) >= self.take_after)
            .unwrap_or(false);
        let held = self
            .current_since
            .map(|s| now.duration_since(s) >= self.hold_for)
            .unwrap_or(true);
        if dominated && held {
            return Some(self.take(loudest, now));
        }
        None
    }

    fn take(&mut self, id: &str, now: Instant) -> String {
        self.current = Some(id.to_string());
        self.current_since = Some(now);
        self.challenger = None;
        self.challenger_since = None;
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lv<'a>(id: &'a str, energy: f32) -> Level<'a> {
        Level {
            id,
            energy,
            speaking: energy > 0.0,
        }
    }

    #[test]
    fn first_speaker_takes_immediately_then_a_challenger_needs_time() {
        let mut s = ActiveSpeaker::default();
        let t0 = Instant::now();
        assert_eq!(s.update(&[lv("a", 500.0)], t0).as_deref(), Some("a"));
        // Bob is louder, but only for 500 ms: no change.
        assert_eq!(
            s.update(&[lv("a", 400.0), lv("b", 900.0)], t0 + ms(100)),
            None
        );
        assert_eq!(
            s.update(&[lv("a", 400.0), lv("b", 900.0)], t0 + ms(600)),
            None
        );
        // Past 800 ms of dominance but Alice has held for under 2 s.
        assert_eq!(
            s.update(&[lv("a", 400.0), lv("b", 900.0)], t0 + ms(1000)),
            None
        );
        // After the hold, Bob takes it.
        assert_eq!(
            s.update(&[lv("a", 400.0), lv("b", 900.0)], t0 + ms(2100))
                .as_deref(),
            Some("b")
        );
        assert_eq!(s.current(), Some("b"));
        assert_eq!(s.recent(), &["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn a_dip_resets_the_challenge_and_silence_keeps_the_incumbent() {
        let mut s = ActiveSpeaker::default();
        let t0 = Instant::now();
        s.update(&[lv("a", 500.0)], t0);
        s.update(&[lv("a", 100.0), lv("b", 900.0)], t0 + ms(2500));
        // Alice is loudest again for one tick: Bob's run restarts.
        s.update(&[lv("a", 950.0), lv("b", 900.0)], t0 + ms(3000));
        assert_eq!(
            s.update(&[lv("a", 100.0), lv("b", 900.0)], t0 + ms(3500)),
            None
        );
        assert_eq!(
            s.update(&[lv("a", 100.0), lv("b", 900.0)], t0 + ms(4200)),
            None
        );
        assert_eq!(
            s.update(&[lv("a", 100.0), lv("b", 900.0)], t0 + ms(4400))
                .as_deref(),
            Some("b")
        );
        // Everyone quiet: Bob stays.
        assert_eq!(s.update(&[lv("a", 0.0), lv("b", 0.0)], t0 + ms(9000)), None);
        assert_eq!(s.current(), Some("b"));
        s.remove("b");
        assert_eq!(s.current(), None);
        assert_eq!(
            s.update(&[lv("a", 50.0)], t0 + ms(9100)).as_deref(),
            Some("a")
        );
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
}
