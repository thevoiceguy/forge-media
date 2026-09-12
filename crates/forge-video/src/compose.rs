//! The compositor: a layout of sources drawn onto a canvas.
//!
//! Every tick, the room hands the compositor its tile list — each tile a
//! participant with, perhaps, a frame — and gets back the canvas: each
//! frame scaled into its tile with the aspect ratio kept, an avatar for a
//! participant without video, the display name along the bottom, a bright
//! border for whoever is speaking, and a mute mark. The canvas is kept
//! between ticks so a steady layout costs only the scaling.
//!
//! [`Compositor`] is the trait every device implements; [`HostCompositor`]
//! is the CPU reference. A device compositor must draw the same layouts
//! within a PSNR tolerance (checked with [`crate::metrics::psnr_luma`]
//! over the scenes in [`crate::parity`]). So that it can, the chrome is
//! one piece of code for every compositor: [`tile_geometry`] says where
//! the ring, the picture and the name band go, [`draw_chrome_under`]
//! paints what lies under the picture and [`draw_band`] what lies over
//! it — a device compositor paints those into host planes and uploads
//! them, and scales and overlays only the pictures on the device.

use crate::codec::CodecError;
use crate::font;
use crate::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use crate::layout::{Layout, Rect};
use crate::scale::{self, ScaleMode};

/// What a tile shows: a person, or a shared screen.
///
/// A content tile is drawn plain — no border, no name band, no avatar,
/// shrunk with a box filter so text survives — because it is a picture
/// of a document, not of a participant, and every pixel of chrome on it
/// is a pixel of the document lost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TileKind {
    #[default]
    Camera,
    Content,
}

/// One participant as the compositor sees it.
#[derive(Debug, Clone)]
pub struct TileSource<'a> {
    /// Stable id (the participant id); used for nothing but debugging.
    pub id: &'a str,
    /// Display name for the label and the avatar initials.
    pub name: &'a str,
    /// The latest decoded frame, or `None` for an audio-only participant
    /// (or one whose video is currently lost). Must be resident on the
    /// compositor's device.
    pub frame: Option<&'a VideoFrame>,
    pub speaking: bool,
    pub muted: bool,
    /// A camera (labelled, bordered, an avatar when there is no frame)
    /// or shared content (the picture alone).
    pub kind: TileKind,
}

/// Colours in Y/U/V, limited range.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub background: (u8, u8, u8),
    pub tile: (u8, u8, u8),
    pub bars: (u8, u8, u8),
    pub label_band: u8,
    pub label_text: u8,
    pub avatar_text: u8,
    pub speaking_border: (u8, u8, u8),
    pub border_px: u32,
    pub gap_px: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: (24, 128, 128),
            tile: (56, 128, 128),
            bars: (32, 128, 128),
            label_band: 28,
            label_text: 235,
            avatar_text: 220,
            speaking_border: (200, 90, 110),
            border_px: 4,
            gap_px: 4,
        }
    }
}

/// Draws layouts onto a canvas resident on one device.
pub trait Compositor: Send {
    /// Where the canvas lives and where source frames must be.
    fn device(&self) -> MediaDevice;
    fn layout(&self) -> Layout;
    fn set_layout(&mut self, layout: Layout);
    fn resolution(&self) -> Resolution;
    /// Draw `sources` in tile order. Sources beyond the layout's capacity
    /// are not drawn. Fails without drawing anything when a frame is not
    /// on this compositor's device.
    fn render(&mut self, sources: &[TileSource<'_>], pts: u32) -> Result<(), CodecError>;
    /// The canvas as of the last [`render`](Self::render).
    fn canvas(&self) -> &VideoFrame;
}

/// The CPU compositor: plain I420 work on a host canvas. Keeps its canvas
/// so unchanged regions are not repainted.
pub struct HostCompositor {
    canvas: VideoFrame,
    layout: Layout,
    theme: Theme,
    last_tiles: Vec<Rect>,
}

impl HostCompositor {
    pub fn new(width: u32, height: u32, layout: Layout) -> Self {
        let theme = Theme::default();
        let mut canvas = HostFrame::black(width, height);
        let (y, u, v) = theme.background;
        canvas.y.fill(y);
        canvas.u.fill(u);
        canvas.v.fill(v);
        Self {
            canvas: VideoFrame::Host(canvas),
            layout,
            theme,
            last_tiles: Vec::new(),
        }
    }

    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    pub fn width(&self) -> u32 {
        self.host_canvas().width
    }

    pub fn height(&self) -> u32 {
        self.host_canvas().height
    }

    /// The canvas as a host frame.
    pub fn host_canvas(&self) -> &HostFrame {
        match &self.canvas {
            VideoFrame::Host(h) => h,
            VideoFrame::Device(_) => unreachable!("host compositor canvas is a host frame"),
        }
    }

    fn canvas_mut(&mut self) -> &mut HostFrame {
        match &mut self.canvas {
            VideoFrame::Host(h) => h,
            VideoFrame::Device(_) => unreachable!("host compositor canvas is a host frame"),
        }
    }

    fn draw_tile(&mut self, src: &TileSource<'_>, frame: Option<&HostFrame>, rect: Rect) {
        let t = self.theme;
        let g = tile_geometry(rect, src.kind, &t);
        let canvas = self.canvas_mut();
        let picture = frame.map(|f| f.resolution());
        draw_chrome_under(canvas, src, &g, picture, &t);
        if let Some(frame) = frame {
            let r = g.picture(frame.resolution());
            let mode = match src.kind {
                TileKind::Camera => ScaleMode::Bilinear,
                TileKind::Content => ScaleMode::Box,
            };
            scale::scale_into_with(canvas, r, frame, mode);
        }
        draw_band(canvas, src, &g, &t);
    }
}

/// Where the parts of one tile go. The same numbers on every device:
/// the host draws them, a device compositor uploads what it drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGeometry {
    pub kind: TileKind,
    /// The whole tile. For a camera the ring around `inner` is what
    /// shows of it; a content tile has no ring and `inner` is `rect`.
    pub rect: Rect,
    /// The picture area: letterboxed picture or avatar for a camera,
    /// the document for content.
    pub inner: Rect,
    /// The name band along the bottom of `inner`, when one fits (never
    /// on content).
    pub band: Option<Rect>,
    /// The bitmap font scale of the label.
    pub label_scale: u32,
}

impl TileGeometry {
    /// Where a picture of `size` lands: the largest rectangle of its
    /// aspect ratio in `inner`, centred.
    pub fn picture(&self, size: Resolution) -> Rect {
        scale::fit(self.inner, size.width, size.height)
    }
}

/// The geometry of a tile of `kind` in `rect`.
pub fn tile_geometry(rect: Rect, kind: TileKind, theme: &Theme) -> TileGeometry {
    if kind == TileKind::Content {
        return TileGeometry {
            kind,
            rect,
            inner: rect,
            band: None,
            label_scale: 1,
        };
    }
    let border = theme.border_px.min(rect.w / 8).min(rect.h / 8);
    let inner = rect.inset(border).even();
    let label_scale = (inner.h / 90).clamp(1, 4);
    // Even height so the band reaches the tile's bottom edge exactly.
    let band_h = ((font::text_height(label_scale) + 4 * label_scale).min(inner.h / 3) + 1) & !1;
    let band = if inner.is_empty()
        || band_h < font::text_height(1) + 2
        || inner.w < font::text_width("A", 1) + 4
    {
        None
    } else {
        Some(Rect::new(inner.x, inner.y + inner.h - band_h, inner.w, band_h).even())
    };
    TileGeometry {
        kind,
        rect,
        inner,
        band,
        label_scale,
    }
}

/// Paint what lies under the picture: the ring (bright when speaking),
/// the bars a letterboxed picture of `picture`'s size leaves in
/// `inner`, or the avatar when there is no picture. Content gets the
/// background where the document does not reach.
pub fn draw_chrome_under(
    dst: &mut HostFrame,
    src: &TileSource<'_>,
    g: &TileGeometry,
    picture: Option<Resolution>,
    t: &Theme,
) {
    if g.kind == TileKind::Content {
        // The document, and nothing on top of it. Without a frame
        // there is nothing to say either: the room falls back to
        // its cameras before it draws an empty content tile, so
        // this is only ever a tick's worth of background.
        let covered = picture.map(|p| g.picture(p) == g.rect).unwrap_or(false);
        if !covered {
            scale::fill(dst, g.rect, t.background.0, t.background.1, t.background.2);
        }
        return;
    }
    // Border ring: bright when speaking, tile colour otherwise.
    let ring = if src.speaking {
        t.speaking_border
    } else {
        t.tile
    };
    scale::fill(dst, g.rect, ring.0, ring.1, ring.2);
    if g.inner.is_empty() {
        return;
    }
    match picture {
        Some(p) => {
            if g.picture(p) != g.inner {
                scale::fill(dst, g.inner, t.bars.0, t.bars.1, t.bars.2);
            }
        }
        None => {
            scale::fill(dst, g.inner, t.tile.0, t.tile.1, t.tile.2);
            let avatar = Rect::new(g.inner.x, g.inner.y, g.inner.w, g.inner.h * 3 / 4);
            let ini = font::initials(src.name);
            let max_scale = (g.inner.h / 20).max(2);
            font::draw_centered(dst, avatar, &ini, max_scale, t.avatar_text);
        }
    }
}

/// The label as it is drawn: the mute mark, the name, cut to what the
/// band holds. Empty when nothing fits (or the tile has no band).
pub fn label_text(src: &TileSource<'_>, g: &TileGeometry) -> String {
    let Some(band) = g.band else {
        return String::new();
    };
    let mut text = String::new();
    if src.muted {
        text.push_str("[M] ");
    }
    text.push_str(src.name);
    let mut chars: Vec<char> = text.chars().collect();
    let max_w = band.w.saturating_sub(4 * g.label_scale);
    while !chars.is_empty()
        && font::text_width(&chars.iter().collect::<String>(), g.label_scale) > max_w
    {
        chars.pop();
    }
    chars.into_iter().collect()
}

/// Paint the name band over the bottom of the picture: a dark band
/// with the label. Nothing when the tile has no band.
pub fn draw_band(dst: &mut HostFrame, src: &TileSource<'_>, g: &TileGeometry, t: &Theme) {
    let Some(band) = g.band else {
        return;
    };
    scale::fill(dst, band, t.label_band, 128, 128);
    let text = label_text(src, g);
    if !text.is_empty() {
        let x = band.x + 2 * g.label_scale;
        let y = band.y + (band.h - font::text_height(g.label_scale)) / 2;
        font::draw_text(dst, x, y, &text, g.label_scale, t.label_text);
    }
}

impl Compositor for HostCompositor {
    fn device(&self) -> MediaDevice {
        MediaDevice::Host
    }

    fn layout(&self) -> Layout {
        self.layout
    }

    fn set_layout(&mut self, layout: Layout) {
        if layout != self.layout {
            self.layout = layout;
            self.last_tiles.clear();
        }
    }

    fn resolution(&self) -> Resolution {
        self.host_canvas().resolution()
    }

    fn render(&mut self, sources: &[TileSource<'_>], pts: u32) -> Result<(), CodecError> {
        // Every frame must be here before anything is drawn.
        let mut frames: Vec<Option<&HostFrame>> = Vec::with_capacity(sources.len());
        for s in sources {
            frames.push(match s.frame {
                None => None,
                Some(VideoFrame::Host(h)) => Some(h),
                Some(other) => {
                    return Err(CodecError::WrongDevice {
                        expected: MediaDevice::Host,
                        actual: other.device(),
                    })
                }
            });
        }
        let t = self.theme;
        let (w, h) = (self.width(), self.height());
        let tiles = self.layout.tiles(sources.len(), w, h, t.gap_px);
        if tiles != self.last_tiles {
            // Geometry changed: clear so nothing from the old layout stays.
            let full = Rect::new(0, 0, w, h);
            scale::fill(
                self.canvas_mut(),
                full,
                t.background.0,
                t.background.1,
                t.background.2,
            );
            self.last_tiles = tiles.clone();
        }
        for ((src, frame), &rect) in sources.iter().zip(frames).zip(tiles.iter()) {
            self.draw_tile(src, frame, rect);
        }
        self.canvas_mut().pts = pts;
        Ok(())
    }

    fn canvas(&self) -> &VideoFrame {
        &self.canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::DeviceFrame;
    use std::sync::Arc;

    fn src<'a>(name: &'a str, frame: Option<&'a VideoFrame>, speaking: bool) -> TileSource<'a> {
        TileSource {
            id: name,
            name,
            frame,
            speaking,
            muted: false,
            kind: TileKind::Camera,
        }
    }

    fn content<'a>(frame: Option<&'a VideoFrame>) -> TileSource<'a> {
        TileSource {
            kind: TileKind::Content,
            ..src("screen", frame, true)
        }
    }

    fn solid(w: u32, h: u32, y: u8) -> VideoFrame {
        VideoFrame::Host(HostFrame::solid(w, h, y, 128, 128))
    }

    fn flat() -> Theme {
        Theme {
            border_px: 0,
            gap_px: 0,
            ..Theme::default()
        }
    }

    #[test]
    fn grid_places_each_source_in_its_tile() {
        let a = solid(160, 90, 200);
        let b = solid(160, 90, 100);
        let mut c = HostCompositor::new(320, 180, Layout::Grid).with_theme(flat());
        c.render(
            &[src("a", Some(&a), false), src("b", Some(&b), false)],
            9000,
        )
        .unwrap();
        let canvas = c.host_canvas();
        assert_eq!(canvas.pts, 9000);
        assert_eq!(c.canvas().pts(), 9000);
        assert_eq!(c.resolution(), Resolution::new(320, 180));
        assert!(c.device().is_host());
        // Two tiles side by side, 160×180 each; the 16:9 sources are
        // letterboxed, so the tile centre is picture and the top is bars.
        assert_eq!(canvas.luma(80, 90), 200);
        assert_eq!(canvas.luma(240, 90), 100);
        assert_eq!(canvas.luma(80, 2), 32, "bars above the letterboxed picture");
    }

    #[test]
    fn audio_only_sources_get_an_avatar_and_speakers_get_a_border() {
        let mut c = HostCompositor::new(320, 180, Layout::Spotlight);
        c.render(&[src("Alice Smith", None, true)], 0).unwrap();
        let canvas = c.host_canvas();
        let t = Theme::default();
        // Speaking border at the very edge.
        assert_eq!(canvas.luma(1, 90), t.speaking_border.0);
        // Tile fill inside the border.
        assert_eq!(canvas.luma(20, 20), t.tile.0);
        // Initials drawn somewhere in the upper three quarters.
        let lit = (0..320)
            .flat_map(|x| (0..135).map(move |y| (x, y)))
            .any(|(x, y)| canvas.luma(x, y) == t.avatar_text);
        assert!(lit, "avatar initials");
        // Label band along the bottom with text in it.
        assert_eq!(canvas.luma(160, 174), t.label_band);
        let label_lit = (0..320).any(|x| (160..180).any(|y| canvas.luma(x, y) == t.label_text));
        assert!(label_lit, "label text");
    }

    #[test]
    fn changing_the_layout_or_count_clears_stale_tiles() {
        let a = solid(64, 36, 200);
        let mut c = HostCompositor::new(128, 72, Layout::Grid).with_theme(flat());
        c.render(&[src("a", Some(&a), false), src("b", Some(&a), false)], 0)
            .unwrap();
        assert_eq!(c.host_canvas().luma(96, 36), 200, "second tile drawn");
        c.render(&[src("a", Some(&a), false)], 1).unwrap();
        // One tile now fills the canvas: the right half is the same
        // source, not stale.
        assert_eq!(c.host_canvas().luma(96, 36), 200);
        c.set_layout(Layout::PictureInPicture);
        assert_eq!(c.layout(), Layout::PictureInPicture);
        c.render(&[src("a", None, false), src("b", Some(&a), false)], 2)
            .unwrap();
        // PiP corner holds b's picture (with a label band below it).
        assert_eq!(c.host_canvas().luma(112, 56), 200);
    }

    #[test]
    fn a_content_tile_is_the_picture_alone_and_a_presentation_puts_it_first() {
        // A shared screen at 4:3 into a 16:9 room, "speaking" and all: no
        // border, no band, bars in the background colour, and the picture
        // right up to the rectangle's edge.
        let doc = solid(120, 90, 200);
        let mut c = HostCompositor::new(320, 180, Layout::Spotlight);
        c.render(&[content(Some(&doc))], 0).unwrap();
        let canvas = c.host_canvas();
        let t = Theme::default();
        assert_eq!(canvas.luma(160, 90), 200, "the document");
        assert_eq!(canvas.luma(160, 1), 200, "up to the top edge: no border");
        assert_eq!(canvas.luma(160, 178), 200, "and the bottom: no label band");
        assert_eq!(
            canvas.luma(10, 90),
            t.background.0,
            "pillar bars, in the background colour"
        );
        assert_ne!(
            canvas.luma(1, 90),
            t.speaking_border.0,
            "no speaking ring on a document"
        );

        // In a presentation the content takes the main region and the
        // cameras the strip, chrome and all.
        let cam = solid(64, 36, 100);
        let mut p = HostCompositor::new(320, 180, Layout::Presentation).with_theme(flat());
        p.render(
            &[
                content(Some(&doc)),
                src("Bob", Some(&cam), false),
                src("Cy", None, false),
            ],
            1,
        )
        .unwrap();
        let canvas = p.host_canvas();
        assert_eq!(canvas.luma(120, 90), 200, "content in the main region");
        assert_eq!(canvas.luma(280, 40), 100, "bob in the strip");
        assert_eq!(
            canvas.luma(280, 130),
            t.tile.0,
            "cy's avatar tile below him"
        );
        // No frame yet: background, and nothing drawn on it.
        let mut e = HostCompositor::new(64, 36, Layout::Spotlight);
        e.render(&[content(None)], 2).unwrap();
        assert!(e.host_canvas().y.iter().all(|&y| y == t.background.0));
    }

    #[test]
    fn more_sources_than_capacity_are_ignored_not_panicked() {
        let a = solid(32, 18, 200);
        let mut c = HostCompositor::new(64, 36, Layout::Grid);
        let many: Vec<TileSource<'_>> = (0..20).map(|_| src("x", Some(&a), false)).collect();
        c.render(&many, 0).unwrap();
        let mut s = HostCompositor::new(64, 36, Layout::Spotlight);
        s.render(&many, 0).unwrap();
        assert_eq!(s.width(), 64);
    }

    #[test]
    fn a_frame_on_another_device_is_refused_before_drawing() {
        let gpu = VideoFrame::Device(DeviceFrame {
            device: MediaDevice::parse("cuda:0").unwrap(),
            width: 64,
            height: 36,
            pts: 5,
            handle: Arc::new(()),
        });
        let a = solid(64, 36, 200);
        let mut c = HostCompositor::new(128, 72, Layout::Grid).with_theme(flat());
        let err = c
            .render(&[src("a", Some(&a), false), src("g", Some(&gpu), false)], 7)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "frame is on cuda:0, this stage runs on host"
        );
        // Nothing was drawn, not even the first tile.
        assert_eq!(c.host_canvas().luma(32, 36), Theme::default().background.0);
        assert_eq!(c.host_canvas().pts, 0);
    }
}
