//! The bitrate ladder (design §7).
//!
//! A room encodes one flavor per distinct set of subscriber needs, and
//! that encoder targets the lowest bitrate any of its subscribers can
//! take. With one ladder rung that is all there is: a caller on a poor
//! link drags every other subscriber of the flavor down to their
//! bitrate. The ladder gives them somewhere else to go — a lower
//! resolution at a lower rate — so the rest keep the picture they can
//! afford.
//!
//! Rungs are fixed rather than per-subscriber. A subscriber asking for
//! something between two rungs gets the one at or below it, so the
//! flavors a room encodes stay few and shared.

use crate::frame::Resolution;

/// One rung: a size and the bitrate it is encoded at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    pub resolution: Resolution,
    pub kbps: u32,
}

/// The rungs, largest first (§7). A room's own resolution is the cap:
/// nothing above it is ever offered.
pub const RUNGS: [Rung; 4] = [
    Rung {
        resolution: Resolution {
            width: 1920,
            height: 1080,
        },
        kbps: 2500,
    },
    Rung {
        resolution: Resolution {
            width: 1280,
            height: 720,
        },
        kbps: 1200,
    },
    Rung {
        resolution: Resolution {
            width: 640,
            height: 360,
        },
        kbps: 500,
    },
    Rung {
        resolution: Resolution {
            width: 320,
            height: 180,
        },
        kbps: 200,
    },
];

/// The ladder a room offers: the rungs at or below its own resolution,
/// largest first. A room smaller than the lowest rung has one rung of
/// its own size, since there is always somewhere to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    rungs: Vec<Rung>,
}

impl Ladder {
    /// The ladder for a room capped at `resolution`.
    pub fn for_room(resolution: Resolution) -> Self {
        let rungs: Vec<Rung> = RUNGS
            .iter()
            .copied()
            .filter(|r| r.resolution.height <= resolution.height)
            .collect();
        if rungs.is_empty() {
            return Self {
                rungs: vec![Rung {
                    resolution,
                    // Below every rung, so the lowest rung's rate is
                    // more than the picture can need.
                    kbps: RUNGS[RUNGS.len() - 1].kbps,
                }],
            };
        }
        Self { rungs }
    }

    /// Every rung, largest first.
    pub fn rungs(&self) -> &[Rung] {
        &self.rungs
    }

    /// The top rung: what a subscriber gets before anything is known
    /// about its link.
    pub fn top(&self) -> Rung {
        self.rungs[0]
    }

    /// The rung a resolution belongs to: the first at or below it.
    pub fn rung_for(&self, resolution: Resolution) -> Rung {
        self.rungs
            .iter()
            .copied()
            .find(|r| r.resolution.height <= resolution.height)
            .unwrap_or_else(|| self.rungs[self.rungs.len() - 1])
    }

    /// Where a rung sits, 0 being the top.
    pub fn index_of(&self, rung: Rung) -> usize {
        self.rungs
            .iter()
            .position(|r| *r == rung)
            .unwrap_or(self.rungs.len() - 1)
    }

    /// One rung down, or `None` at the bottom.
    pub fn below(&self, rung: Rung) -> Option<Rung> {
        self.rungs.get(self.index_of(rung) + 1).copied()
    }

    /// One rung up, or `None` at the top.
    pub fn above(&self, rung: Rung) -> Option<Rung> {
        let i = self.index_of(rung);
        (i > 0).then(|| self.rungs[i - 1])
    }
}

/// How much headroom a link must show before a subscriber is moved.
///
/// Down is decided on the rung it is on; up on the rung it would move
/// to, with a margin, so a link that can only just afford the next rung
/// is not moved onto it and straight back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LadderPolicy {
    /// Move down when the estimate is below this share of the current
    /// rung's rate.
    pub down_at: f32,
    /// Move up when the estimate is above this share of the next rung's.
    pub up_at: f32,
}

impl Default for LadderPolicy {
    fn default() -> Self {
        Self {
            down_at: 0.7,
            up_at: 1.2,
        }
    }
}

/// Where a subscriber should be, given what its link is saying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// Stay where it is.
    Stay,
    /// Drop to this rung: the link cannot carry the current one.
    Down(Rung),
    /// Climb to this rung: the link has room to spare.
    Up(Rung),
}

impl Ladder {
    /// What the estimate says about a subscriber on `rung`. `remb_kbps`
    /// of 0 means the receiver has not said, and nothing is done.
    pub fn wants(&self, rung: Rung, remb_kbps: u32, policy: LadderPolicy) -> Move {
        if remb_kbps == 0 {
            return Move::Stay;
        }
        let estimate = remb_kbps as f32;
        if estimate < rung.kbps as f32 * policy.down_at {
            // Straight to the highest rung the link can carry, rather
            // than one step at a time: a link that halved is not helped
            // by a slow walk down.
            let target = self
                .rungs
                .iter()
                .copied()
                .find(|r| estimate >= r.kbps as f32 * policy.down_at)
                .unwrap_or_else(|| self.rungs[self.rungs.len() - 1]);
            if target != rung {
                return Move::Down(target);
            }
            return Move::Stay;
        }
        if let Some(up) = self.above(rung) {
            if estimate > up.kbps as f32 * policy.up_at {
                return Move::Up(up);
            }
        }
        Move::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(w: u32, h: u32) -> Resolution {
        Resolution::new(w, h)
    }

    #[test]
    fn a_room_offers_the_rungs_at_or_below_its_own_size() {
        let full = Ladder::for_room(res(1920, 1080));
        assert_eq!(full.rungs().len(), 4);
        assert_eq!(full.top().resolution, res(1920, 1080));

        // A 720p room never offers 1080p.
        let hd = Ladder::for_room(res(1280, 720));
        assert_eq!(hd.rungs().len(), 3);
        assert_eq!(hd.top().kbps, 1200);
        assert_eq!(hd.below(hd.top()).unwrap().resolution, res(640, 360));
        assert_eq!(hd.above(hd.top()), None);

        // Smaller than every rung: one rung of its own.
        let tiny = Ladder::for_room(res(160, 90));
        assert_eq!(tiny.rungs().len(), 1);
        assert_eq!(tiny.top().resolution, res(160, 90));
        assert_eq!(tiny.below(tiny.top()), None);
    }

    #[test]
    fn a_link_that_cannot_carry_a_rung_goes_to_one_it_can() {
        let l = Ladder::for_room(res(1280, 720));
        let p = LadderPolicy::default();
        let top = l.top(); // 720p at 1200

        // Comfortable: stay.
        assert_eq!(l.wants(top, 1200, p), Move::Stay);
        assert_eq!(l.wants(top, 900, p), Move::Stay);

        // Below 70% of 1200: down, and straight to what it can carry.
        assert_eq!(
            l.wants(top, 600, p),
            Move::Down(Rung {
                resolution: res(640, 360),
                kbps: 500
            })
        );
        assert_eq!(
            l.wants(top, 100, p),
            Move::Down(Rung {
                resolution: res(320, 180),
                kbps: 200
            }),
            "a link that collapsed goes to the bottom, not one step"
        );

        // Nothing said yet: nothing done.
        assert_eq!(l.wants(top, 0, p), Move::Stay);

        // From the bottom, room to spare climbs one rung.
        let bottom = Rung {
            resolution: res(320, 180),
            kbps: 200,
        };
        assert_eq!(l.wants(bottom, 300, p), Move::Stay, "not yet 120% of 500");
        assert_eq!(
            l.wants(bottom, 700, p),
            Move::Up(Rung {
                resolution: res(640, 360),
                kbps: 500
            })
        );
        // And never above the room's own top rung.
        assert_eq!(l.wants(top, 100_000, p), Move::Stay);
    }

    #[test]
    fn a_resolution_between_rungs_takes_the_one_below() {
        let l = Ladder::for_room(res(1920, 1080));
        assert_eq!(l.rung_for(res(1280, 720)).kbps, 1200);
        // 480p sits between 360p and 720p: the lower rung is the one
        // that can actually be carried.
        assert_eq!(l.rung_for(res(854, 480)).resolution, res(640, 360));
        assert_eq!(l.rung_for(res(64, 36)).resolution, res(320, 180));
    }
}
