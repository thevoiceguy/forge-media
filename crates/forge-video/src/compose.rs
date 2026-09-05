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
//! within a PSNR tolerance (checked with [`crate::metrics::psnr_luma`]).

use crate::codec::CodecError;
use crate::font;
use crate::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use crate::layout::{Layout, Rect};
use crate::scale;

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
        let border = t.border_px.min(rect.w / 8).min(rect.h / 8);
        let canvas = self.canvas_mut();
        // Border ring: bright when speaking, tile colour otherwise.
        if src.speaking {
            scale::fill(
                canvas,
                rect,
                t.speaking_border.0,
                t.speaking_border.1,
                t.speaking_border.2,
            );
        } else {
            scale::fill(canvas, rect, t.tile.0, t.tile.1, t.tile.2);
        }
        let inner = rect.inset(border).even();
        if inner.is_empty() {
            return;
        }
        match frame {
            Some(frame) => scale::letterbox(canvas, inner, frame, t.bars),
            None => {
                scale::fill(canvas, inner, t.tile.0, t.tile.1, t.tile.2);
                let avatar = Rect::new(inner.x, inner.y, inner.w, inner.h * 3 / 4);
                let ini = font::initials(src.name);
                let max_scale = (inner.h / 20).max(2);
                font::draw_centered(canvas, avatar, &ini, max_scale, t.avatar_text);
            }
        }
        self.draw_label(src, inner);
    }

    /// Name (and mute mark) on a dark band along the bottom of the tile.
    fn draw_label(&mut self, src: &TileSource<'_>, inner: Rect) {
        let t = self.theme;
        let scale = (inner.h / 90).clamp(1, 4);
        // Even height so the band reaches the tile's bottom edge exactly.
        let band_h = ((font::text_height(scale) + 4 * scale).min(inner.h / 3) + 1) & !1;
        if band_h < font::text_height(1) + 2 || inner.w < font::text_width("A", 1) + 4 {
            return;
        }
        let band = Rect::new(inner.x, inner.y + inner.h - band_h, inner.w, band_h).even();
        let canvas = self.canvas_mut();
        scale::fill(canvas, band, t.label_band, 128, 128);
        let mut text = String::new();
        if src.muted {
            text.push_str("[M] ");
        }
        text.push_str(src.name);
        // Trim to what fits.
        let mut chars: Vec<char> = text.chars().collect();
        let max_w = band.w.saturating_sub(4 * scale);
        while !chars.is_empty()
            && font::text_width(&chars.iter().collect::<String>(), scale) > max_w
        {
            chars.pop();
        }
        let text: String = chars.into_iter().collect();
        if !text.is_empty() {
            let x = band.x + 2 * scale;
            let y = band.y + (band.h - font::text_height(scale)) / 2;
            font::draw_text(canvas, x, y, &text, scale, t.label_text);
        }
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
