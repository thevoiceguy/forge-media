//! A supervisor on a two-party call: who hears whom.
//!
//! A call has two parties, A and B, and a supervisor joins it in one of
//! three modes:
//!
//! - **Monitor**: the supervisor hears both parties; nobody hears them.
//! - **Whisper**: the supervisor hears both parties and is heard by one of
//!   them (the coached party) alone.
//! - **Barge**: the supervisor hears and is heard by both parties.
//!
//! [`SupervisionMix`] is the mixing core every media path shares: each
//! source's audio is written in as it arrives (mono, at the mix's rate),
//! and every frame [`SupervisionMix::tick`] says what each listener hears.
//! A party whose output is `None` keeps hearing the other party as it did
//! before (its media relayed untouched); a party given a frame is to hear
//! that frame instead — the other party with the supervisor summed in.
//! The supervisor is always given a frame.
//!
//! No clock lives here: the caller ticks once per frame. A source short of
//! a frame contributes silence for the part it lacks; a source that runs
//! ahead is trimmed so a stalled consumer cannot build up delay.

use std::collections::VecDeque;

/// One of the three voices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Voice {
    A,
    B,
    Supervisor,
}

/// A call party (the two voices the supervisor can be heard by).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallParty {
    A,
    B,
}

impl CallParty {
    fn voice(self) -> Voice {
        match self {
            Self::A => Voice::A,
            Self::B => Voice::B,
        }
    }

    fn other(self) -> Voice {
        match self {
            Self::A => Voice::B,
            Self::B => Voice::A,
        }
    }
}

/// How the supervisor takes part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionMode {
    /// Hears both parties, heard by nobody.
    Monitor,
    /// Hears both parties, heard by this one alone.
    Whisper(CallParty),
    /// Hears and is heard by both parties.
    Barge,
}

impl SupervisionMode {
    /// Whether `party` hears the supervisor, so its audio is a mix rather
    /// than the other party relayed.
    pub fn heard_by(self, party: CallParty) -> bool {
        match self {
            Self::Monitor => false,
            Self::Whisper(coached) => coached == party,
            Self::Barge => true,
        }
    }
}

/// What each listener hears for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisionFrame {
    /// Both parties, summed.
    pub supervisor: Vec<i16>,
    /// B and the supervisor, when A hears the supervisor.
    pub a: Option<Vec<i16>>,
    /// A and the supervisor, when B hears the supervisor.
    pub b: Option<Vec<i16>>,
}

/// How many frames a source may run ahead before it is trimmed, and what
/// it is trimmed to.
const MAX_AHEAD_FRAMES: usize = 5;
const TRIM_TO_FRAMES: usize = 2;

/// The three-way mix of a supervised call.
#[derive(Debug)]
pub struct SupervisionMix {
    mode: SupervisionMode,
    frame_size: usize,
    a: VecDeque<i16>,
    b: VecDeque<i16>,
    supervisor: VecDeque<i16>,
}

impl SupervisionMix {
    /// A mix in `mode` producing `frame_size` samples per tick.
    pub fn new(mode: SupervisionMode, frame_size: usize) -> Self {
        Self {
            mode,
            frame_size: frame_size.max(1),
            a: VecDeque::new(),
            b: VecDeque::new(),
            supervisor: VecDeque::new(),
        }
    }

    pub fn mode(&self) -> SupervisionMode {
        self.mode
    }

    /// Change the mode; audio already buffered is kept.
    pub fn set_mode(&mut self, mode: SupervisionMode) {
        self.mode = mode;
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Audio from one voice, mono at the mix's rate.
    pub fn write(&mut self, voice: Voice, samples: &[i16]) {
        let frame = self.frame_size;
        let buffer = self.buffer(voice);
        buffer.extend(samples.iter().copied());
        if buffer.len() > frame * MAX_AHEAD_FRAMES {
            let excess = buffer.len() - frame * TRIM_TO_FRAMES;
            buffer.drain(..excess);
        }
    }

    /// One frame: what each listener hears.
    pub fn tick(&mut self) -> SupervisionFrame {
        let a = self.take(Voice::A);
        let b = self.take(Voice::B);
        let supervisor = self.take(Voice::Supervisor);
        let for_party = |party: CallParty| {
            self.mode.heard_by(party).then(|| {
                let other = match party.other() {
                    Voice::A => &a,
                    _ => &b,
                };
                sum(&[other, &supervisor])
            })
        };
        SupervisionFrame {
            a: for_party(CallParty::A),
            b: for_party(CallParty::B),
            supervisor: sum(&[&a, &b]),
        }
    }

    fn buffer(&mut self, voice: Voice) -> &mut VecDeque<i16> {
        match voice {
            Voice::A => &mut self.a,
            Voice::B => &mut self.b,
            Voice::Supervisor => &mut self.supervisor,
        }
    }

    /// A frame of `voice`, silence for what it lacks.
    fn take(&mut self, voice: Voice) -> Vec<i16> {
        let frame = self.frame_size;
        let buffer = self.buffer(voice);
        let n = buffer.len().min(frame);
        let mut out: Vec<i16> = buffer.drain(..n).collect();
        out.resize(frame, 0);
        out
    }
}

/// Sum frames sample by sample, saturating.
fn sum(frames: &[&Vec<i16>]) -> Vec<i16> {
    let len = frames.iter().map(|f| f.len()).max().unwrap_or(0);
    (0..len)
        .map(|i| {
            let total: i32 = frames
                .iter()
                .map(|f| f.get(i).copied().unwrap_or(0) as i32)
                .sum();
            total.clamp(i16::MIN as i32, i16::MAX as i32) as i16
        })
        .collect()
}

// `CallParty::voice` is kept for callers mapping a party to its voice.
impl From<CallParty> for Voice {
    fn from(party: CallParty) -> Self {
        party.voice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(mix: &mut SupervisionMix, a: i16, b: i16, s: i16) {
        let n = mix.frame_size();
        mix.write(Voice::A, &vec![a; n]);
        mix.write(Voice::B, &vec![b; n]);
        mix.write(Voice::Supervisor, &vec![s; n]);
    }

    #[test]
    fn a_monitor_hears_both_and_is_heard_by_nobody() {
        let mut mix = SupervisionMix::new(SupervisionMode::Monitor, 4);
        filled(&mut mix, 100, 20, 7);
        let frame = mix.tick();
        assert_eq!(frame.supervisor, vec![120; 4]);
        assert_eq!(frame.a, None, "A keeps hearing B relayed");
        assert_eq!(frame.b, None);
    }

    #[test]
    fn a_whisper_is_heard_by_the_coached_party_alone() {
        let mut mix = SupervisionMix::new(SupervisionMode::Whisper(CallParty::B), 4);
        filled(&mut mix, 100, 20, 7);
        let frame = mix.tick();
        assert_eq!(frame.supervisor, vec![120; 4]);
        assert_eq!(frame.a, None, "the far party never hears the whisper");
        assert_eq!(
            frame.b,
            Some(vec![107; 4]),
            "the coached party hears A and the supervisor"
        );
    }

    #[test]
    fn a_barge_is_heard_by_both() {
        let mut mix = SupervisionMix::new(SupervisionMode::Barge, 2);
        filled(&mut mix, 100, 20, 7);
        let frame = mix.tick();
        assert_eq!(frame.a, Some(vec![27; 2]));
        assert_eq!(frame.b, Some(vec![107; 2]));
        assert_eq!(frame.supervisor, vec![120; 2]);
    }

    #[test]
    fn a_short_source_is_silence_and_sums_saturate() {
        let mut mix = SupervisionMix::new(SupervisionMode::Barge, 4);
        mix.write(Voice::A, &[i16::MAX, i16::MAX]);
        mix.write(Voice::Supervisor, &[i16::MAX; 4]);
        let frame = mix.tick();
        assert_eq!(frame.b, Some(vec![i16::MAX; 4]), "clamped, never wrapped");
        assert_eq!(frame.a, Some(vec![i16::MAX; 4]), "B said nothing");
        assert_eq!(frame.supervisor, vec![i16::MAX, i16::MAX, 0, 0]);
    }

    #[test]
    fn a_source_that_runs_ahead_is_trimmed() {
        let mut mix = SupervisionMix::new(SupervisionMode::Monitor, 2);
        mix.write(Voice::A, &[1; 20]);
        // Twenty samples is ten frames: trimmed to the last two.
        assert_eq!(mix.tick().supervisor, vec![1, 1]);
        assert_eq!(mix.tick().supervisor, vec![1, 1]);
        assert_eq!(mix.tick().supervisor, vec![0, 0]);
    }

    #[test]
    fn the_mode_changes_without_losing_audio() {
        let mut mix = SupervisionMix::new(SupervisionMode::Monitor, 2);
        filled(&mut mix, 3, 4, 5);
        mix.set_mode(SupervisionMode::Whisper(CallParty::A));
        let frame = mix.tick();
        assert_eq!(frame.a, Some(vec![9; 2]));
        assert!(frame.b.is_none());
    }
}
