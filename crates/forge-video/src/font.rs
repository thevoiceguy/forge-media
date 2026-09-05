//! A small bitmap font for labels and avatars, drawn straight into the
//! luma plane. Five by seven pixels, capitals, digits and a little
//! punctuation; lowercase is drawn as capitals, anything else as a box.
//! No freetype, no allocation.

use crate::frame::HostFrame;
use crate::layout::Rect;

pub const GLYPH_W: u32 = 5;
pub const GLYPH_H: u32 = 7;

/// Rows top to bottom, bit 4 is the left column.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0; 7],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '@' => [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0E],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '\'' => [0x0C, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        _ => [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F],
    }
}

/// Width in pixels of `text` at `scale`, one column of spacing per glyph.
pub fn text_width(text: &str, scale: u32) -> u32 {
    let n = text.chars().count() as u32;
    if n == 0 {
        0
    } else {
        (n * (GLYPH_W + 1) - 1) * scale
    }
}

pub fn text_height(scale: u32) -> u32 {
    GLYPH_H * scale
}

/// Draw `text` into the luma plane with its top-left at (x, y), each
/// glyph pixel a `scale`×`scale` block of luma `luma`. Clipped to the
/// frame.
pub fn draw_text(frame: &mut HostFrame, x: u32, y: u32, text: &str, scale: u32, luma: u8) {
    let scale = scale.max(1);
    let mut cx = x;
    for c in text.chars() {
        let g = glyph(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (0x10 >> col) != 0 {
                    let px = cx + col * scale;
                    let py = y + row as u32 * scale;
                    block(frame, px, py, scale, luma);
                }
            }
        }
        cx += (GLYPH_W + 1) * scale;
        if cx >= frame.width {
            break;
        }
    }
}

fn block(frame: &mut HostFrame, x: u32, y: u32, size: u32, luma: u8) {
    for yy in y..(y + size).min(frame.height) {
        let start = yy as usize * frame.y_stride;
        for xx in x..(x + size).min(frame.width) {
            frame.y[start + xx as usize] = luma;
        }
    }
}

/// The largest scale at which `text` fits in `w`×`h`, at least 1.
pub fn fitting_scale(text: &str, w: u32, h: u32) -> u32 {
    let mut s = 1;
    while text_width(text, s + 1) <= w && text_height(s + 1) <= h {
        s += 1;
    }
    s
}

/// Draw `text` centred in `r`, as large as fits (capped at `max_scale`).
pub fn draw_centered(frame: &mut HostFrame, r: Rect, text: &str, max_scale: u32, luma: u8) {
    if r.is_empty() || text.is_empty() {
        return;
    }
    let scale = fitting_scale(text, r.w, r.h).min(max_scale.max(1));
    let tw = text_width(text, scale);
    let th = text_height(scale);
    if tw > r.w || th > r.h {
        return;
    }
    draw_text(
        frame,
        r.x + (r.w - tw) / 2,
        r.y + (r.h - th) / 2,
        text,
        scale,
        luma,
    );
}

/// Up to two initials for a display name: first letters of the first and
/// last words, uppercased; `?` when there is nothing to take.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();
    let first = words
        .first()
        .and_then(|w| w.chars().find(|c| c.is_alphanumeric()));
    let last = if words.len() > 1 {
        words
            .last()
            .and_then(|w| w.chars().find(|c| c.is_alphanumeric()))
    } else {
        None
    };
    let mut s: String = first
        .into_iter()
        .chain(last)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if s.is_empty() {
        s.push('?');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_draw_where_asked_and_clip() {
        let mut f = HostFrame::black(16, 16);
        draw_text(&mut f, 2, 3, "I", 1, 235);
        // 'I' has a bar across columns 1..=3 of the glyph (x 3..=5) and a
        // stem in column 2 (x 4).
        assert_eq!(f.luma(3, 3), 235);
        assert_eq!(f.luma(5, 3), 235);
        assert_eq!(f.luma(2, 3), 16, "outside the bar");
        assert_eq!(f.luma(4, 6), 235);
        assert_eq!(f.luma(2, 6), 16, "stem only in the middle");
        assert_eq!(f.luma(4, 10), 16, "below the glyph");
        // Scale 2 doubles the footprint; drawing off the edge is clipped.
        let mut g = HostFrame::black(8, 8);
        draw_text(&mut g, 4, 4, "MM", 2, 200);
        assert_eq!(g.luma(4, 4), 200);
        assert_eq!(g.luma(7, 7), 200);
    }

    #[test]
    fn text_metrics_and_fitting() {
        assert_eq!(text_width("ABC", 1), 17);
        assert_eq!(text_width("ABC", 2), 34);
        assert_eq!(text_width("", 3), 0);
        assert_eq!(fitting_scale("AB", 100, 100), 9);
        assert_eq!(fitting_scale("AB", 4, 4), 1);
        let mut f = HostFrame::black(40, 20);
        draw_centered(&mut f, Rect::new(0, 0, 40, 20), "JF", 10, 235);
        // Something was drawn, roughly centred.
        let lit: Vec<(u32, u32)> = (0..40)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .filter(|&(x, y)| f.luma(x, y) == 235)
            .collect();
        assert!(!lit.is_empty());
        let (min_x, max_x) = (
            lit.iter().map(|p| p.0).min().unwrap(),
            lit.iter().map(|p| p.0).max().unwrap(),
        );
        assert!(min_x >= 8 && max_x <= 31, "{min_x}..{max_x}");
    }

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("James Ferris"), "JF");
        assert_eq!(initials("alice"), "A");
        assert_eq!(initials("  Mary-Ann  van der Berg "), "MB");
        assert_eq!(initials("+1 555 0100"), "10");
        assert_eq!(initials("   "), "?");
    }
}
