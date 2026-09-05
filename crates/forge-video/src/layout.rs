//! Tile geometry. A layout is a function from (how many tiles, canvas
//! size) to rectangles; the compositor is the same for all of them.

use std::fmt;

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// Snap the position and size to even numbers (I420 chroma alignment).
    pub fn even(self) -> Self {
        Rect::new(self.x & !1, self.y & !1, self.w & !1, self.h & !1)
    }

    /// The part of the rectangle inside a `w`×`h` canvas.
    pub fn clip(self, w: u32, h: u32) -> Self {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Rect::new(x, y, self.w.min(w - x), self.h.min(h - y))
    }

    /// Shrink by `m` on every side (never below zero size).
    pub fn inset(self, m: u32) -> Self {
        let m2 = m * 2;
        if self.w <= m2 || self.h <= m2 {
            return Rect::new(self.x + self.w / 2, self.y + self.h / 2, 0, 0);
        }
        Rect::new(self.x + m, self.y + m, self.w - m2, self.h - m2)
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// How tiles are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    /// The tightest grid for the tile count, up to 4×4.
    Grid,
    /// The first tile large, up to five others in a strip at the right.
    ActiveSpeaker,
    /// The first tile alone, full canvas.
    Spotlight,
    /// The first tile full canvas, the second in the bottom-right corner.
    PictureInPicture,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "grid" => Some(Layout::Grid),
            "active_speaker" | "speaker" => Some(Layout::ActiveSpeaker),
            "spotlight" => Some(Layout::Spotlight),
            "pip" | "picture_in_picture" => Some(Layout::PictureInPicture),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Layout::Grid => "grid",
            Layout::ActiveSpeaker => "active_speaker",
            Layout::Spotlight => "spotlight",
            Layout::PictureInPicture => "pip",
        }
    }

    /// Tiles this layout can show at once; further participants are
    /// audio-only in the composite.
    pub fn capacity(&self) -> usize {
        match self {
            Layout::Grid => 16,
            Layout::ActiveSpeaker => 6,
            Layout::Spotlight => 1,
            Layout::PictureInPicture => 2,
        }
    }

    /// Rectangles for `n` tiles on a `w`×`h` canvas, in tile order, with
    /// a `gap` between tiles. Returns `min(n, capacity)` rectangles, each
    /// even-aligned.
    pub fn tiles(&self, n: usize, w: u32, h: u32, gap: u32) -> Vec<Rect> {
        let n = n.min(self.capacity());
        if n == 0 || w == 0 || h == 0 {
            return Vec::new();
        }
        let canvas = Rect::new(0, 0, w, h);
        match self {
            Layout::Grid => {
                let (cols, rows) = grid_shape(n);
                grid_rects(canvas, cols, rows, gap)
                    .into_iter()
                    .take(n)
                    .collect()
            }
            Layout::Spotlight => vec![canvas.even()],
            Layout::PictureInPicture => {
                let mut v = vec![canvas.even()];
                if n > 1 {
                    let pw = w / 4;
                    let ph = h / 4;
                    v.push(Rect::new(w - pw - gap, h - ph - gap, pw, ph).even());
                }
                v
            }
            Layout::ActiveSpeaker => {
                if n == 1 {
                    return vec![canvas.even()];
                }
                let strip_w = w / 4;
                let main = Rect::new(0, 0, w - strip_w - gap, h).even();
                let mut v = vec![main];
                let others = n - 1;
                let cell_h = (h - gap * (others as u32 - 1)) / others as u32;
                for i in 0..others {
                    let y = i as u32 * (cell_h + gap);
                    v.push(Rect::new(w - strip_w, y, strip_w, cell_h).even());
                }
                v
            }
        }
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Columns and rows for the tightest grid holding `n` tiles.
pub fn grid_shape(n: usize) -> (u32, u32) {
    match n {
        0 | 1 => (1, 1),
        2 => (2, 1),
        3 | 4 => (2, 2),
        5 | 6 => (3, 2),
        7..=9 => (3, 3),
        10..=12 => (4, 3),
        _ => (4, 4),
    }
}

fn grid_rects(canvas: Rect, cols: u32, rows: u32, gap: u32) -> Vec<Rect> {
    let cell_w = (canvas.w.saturating_sub(gap * (cols - 1))) / cols;
    let cell_h = (canvas.h.saturating_sub(gap * (rows - 1))) / rows;
    let mut v = Vec::with_capacity((cols * rows) as usize);
    for r in 0..rows {
        for c in 0..cols {
            v.push(
                Rect::new(
                    canvas.x + c * (cell_w + gap),
                    canvas.y + r * (cell_h + gap),
                    cell_w,
                    cell_h,
                )
                .even(),
            );
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disjoint(rects: &[Rect]) -> bool {
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap =
                    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
                if overlap {
                    return false;
                }
            }
        }
        true
    }

    fn inside(rects: &[Rect], w: u32, h: u32) -> bool {
        rects.iter().all(|r| r.x + r.w <= w && r.y + r.h <= h)
    }

    #[test]
    fn grid_shapes_are_the_tightest() {
        assert_eq!(grid_shape(1), (1, 1));
        assert_eq!(grid_shape(2), (2, 1));
        assert_eq!(grid_shape(4), (2, 2));
        assert_eq!(grid_shape(5), (3, 2));
        assert_eq!(grid_shape(9), (3, 3));
        assert_eq!(grid_shape(10), (4, 3));
        assert_eq!(grid_shape(16), (4, 4));
    }

    #[test]
    fn every_layout_yields_disjoint_even_tiles_inside_the_canvas() {
        for layout in [
            Layout::Grid,
            Layout::ActiveSpeaker,
            Layout::Spotlight,
            Layout::PictureInPicture,
        ] {
            for n in 0..=18 {
                let t = layout.tiles(n, 1280, 720, 4);
                assert_eq!(t.len(), n.min(layout.capacity()), "{layout} n={n}");
                assert!(inside(&t, 1280, 720), "{layout} n={n}: {t:?}");
                assert!(t
                    .iter()
                    .all(|r| r.x % 2 == 0 && r.y % 2 == 0 && r.w % 2 == 0 && r.h % 2 == 0));
                if layout != Layout::PictureInPicture {
                    assert!(disjoint(&t), "{layout} n={n}: {t:?}");
                }
                if n > 0 {
                    assert!(t.iter().all(|r| !r.is_empty()), "{layout} n={n}");
                }
            }
        }
    }

    #[test]
    fn active_speaker_gives_the_first_tile_most_of_the_canvas() {
        let t = Layout::ActiveSpeaker.tiles(4, 1280, 720, 0);
        assert_eq!(t[0], Rect::new(0, 0, 960, 720));
        assert_eq!(t[1].w, 320);
        assert_eq!(t.len(), 4);
        assert_eq!(t[3].y + t[3].h, 720);
        // A lone tile is the whole canvas; a seventh participant gets no tile.
        assert_eq!(
            Layout::ActiveSpeaker.tiles(1, 640, 360, 0),
            vec![Rect::new(0, 0, 640, 360)]
        );
        assert_eq!(Layout::ActiveSpeaker.tiles(7, 640, 360, 0).len(), 6);
    }

    #[test]
    fn pip_puts_the_second_tile_in_the_corner() {
        let t = Layout::PictureInPicture.tiles(2, 640, 360, 8);
        assert_eq!(t[0], Rect::new(0, 0, 640, 360));
        assert_eq!(t[1], Rect::new(472, 262, 160, 90));
    }

    #[test]
    fn rect_helpers() {
        assert_eq!(Rect::new(3, 5, 7, 9).even(), Rect::new(2, 4, 6, 8));
        assert_eq!(
            Rect::new(10, 10, 100, 100).clip(50, 50),
            Rect::new(10, 10, 40, 40)
        );
        assert_eq!(Rect::new(0, 0, 10, 10).inset(2), Rect::new(2, 2, 6, 6));
        assert!(Rect::new(0, 0, 3, 3).inset(2).is_empty());
        assert!(Rect::new(2, 2, 4, 4).contains(5, 5));
        assert!(!Rect::new(2, 2, 4, 4).contains(6, 5));
        assert_eq!(Layout::parse("Active-Speaker"), Some(Layout::ActiveSpeaker));
        assert_eq!(Layout::parse("pip").unwrap().name(), "pip");
        assert_eq!(Layout::parse("mosaic"), None);
    }
}
