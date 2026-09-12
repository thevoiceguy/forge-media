//! The compositor on the device: an FFmpeg filter graph per output
//! (design §15.7, decision 2).
//!
//! The pictures are the only thing composed on the device: each tile's
//! frame goes through `scale_cuda` to the rectangle the host would have
//! drawn it in and `overlay_cuda` puts it there. Everything else — the
//! background, the rings, the bars, the avatars — is the *underlay*: a
//! host canvas painted by the same code as the host compositor's chrome
//! ([`forge_video::compose::draw_chrome_under`]) and uploaded when
//! anything in it changes, which is when someone starts or stops
//! speaking, a tile gains or loses a picture, or the layout moves. The
//! name bands go over the pictures, so they are small host planes of
//! their own ([`forge_video::compose::draw_band`]), cached by their
//! text and size and uploaded once. A steady scene costs one graph run
//! per tick and nothing across the bus.
//!
//! The graph is rebuilt when its shape changes: the tile rectangles,
//! the size of the picture each takes, or the bands. Frames of the same
//! size from any pool on the device flow through the same graph.

use crate::device::{HwDevice, HwFrames};
use crate::frame::{hw_frame, upload, wrap};
use crate::graph::{FilterGraph, Node};
use crate::scale::{filter_for, scale_args};
use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use forge_video::compose::{
    draw_band, draw_chrome_under, label_text, tile_geometry, Compositor, Theme, TileGeometry,
    TileKind, TileSource,
};
use forge_video::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use forge_video::layout::{Layout, Rect};
use forge_video::scale::{self, ScaleMode};
use std::collections::HashMap;
use std::sync::Arc;

/// What decides the underlay's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnderlayKey {
    layout: Layout,
    tiles: Vec<UnderlayTile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnderlayTile {
    rect: Rect,
    kind: TileKind,
    speaking: bool,
    /// The picture's size: it decides the bars.
    picture: Option<Resolution>,
    /// The name, when the avatar is drawn (no picture).
    avatar: Option<String>,
}

/// One picture the graph places.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Picture {
    /// Where on the canvas.
    dst: Rect,
    /// The frame's size, which the buffer source is built for.
    src: Resolution,
    mode: ScaleMode,
}

/// A band's cache key: its pixels are a function of these.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BandKey {
    width: u32,
    height: u32,
    text: String,
    scale: u32,
}

/// The graph's shape: rebuilt when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shape {
    pictures: Vec<Picture>,
    bands: Vec<Rect>,
}

struct Graph {
    shape: Shape,
    graph: FilterGraph,
}

/// The device compositor.
pub struct DeviceCompositor {
    device: Arc<HwDevice>,
    layout: Layout,
    theme: Theme,
    resolution: Resolution,
    pool: HwFrames,
    /// The last output; the underlay alone before the first render.
    canvas: VideoFrame,
    underlay_key: Option<UnderlayKey>,
    underlay: Option<VideoFrame>,
    /// Uploaded bands by what they show.
    bands: HashMap<BandKey, VideoFrame>,
    graph: Option<Graph>,
    /// The graph's clock, one per render.
    ticks: i64,
    rebuilds: u64,
}

impl DeviceCompositor {
    pub fn new(
        device: Arc<HwDevice>,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<DeviceCompositor, CodecError> {
        let resolution = Resolution::new(width, height);
        let pool = device.frames(resolution)?;
        let theme = Theme::default();
        let mut bg = HostFrame::black(resolution.width, resolution.height);
        let (y, u, v) = theme.background;
        bg.y.fill(y);
        bg.u.fill(u);
        bg.v.fill(v);
        let canvas = upload(&device, &pool, &bg)?;
        Ok(DeviceCompositor {
            device,
            layout,
            theme,
            resolution,
            pool,
            canvas,
            underlay_key: None,
            underlay: None,
            bands: HashMap::new(),
            graph: None,
            ticks: 0,
            rebuilds: 0,
        })
    }

    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self.underlay_key = None;
        self.bands.clear();
        self
    }

    /// How many times the graph was built: a steady scene builds it
    /// once.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// The underlay for `key`, uploaded: the same one while nothing in
    /// it changes.
    fn underlay(
        &mut self,
        key: UnderlayKey,
        sources: &[TileSource<'_>],
        geometry: &[TileGeometry],
    ) -> Result<VideoFrame, CodecError> {
        if self.underlay_key.as_ref() == Some(&key) {
            if let Some(u) = &self.underlay {
                return Ok(u.clone());
            }
        }
        let t = self.theme;
        let mut host = HostFrame::black(self.resolution.width, self.resolution.height);
        let full = Rect::new(0, 0, host.width, host.height);
        scale::fill(
            &mut host,
            full,
            t.background.0,
            t.background.1,
            t.background.2,
        );
        for ((src, g), k) in sources.iter().zip(geometry).zip(&key.tiles) {
            draw_chrome_under(&mut host, src, g, k.picture, &t);
        }
        let up = upload(&self.device, &self.pool, &host)?;
        self.underlay_key = Some(key);
        self.underlay = Some(up.clone());
        Ok(up)
    }

    /// The band for a tile, uploaded once per distinct band.
    fn band(
        &mut self,
        src: &TileSource<'_>,
        g: &TileGeometry,
    ) -> Result<Option<(Rect, VideoFrame)>, CodecError> {
        let Some(band) = g.band else {
            return Ok(None);
        };
        if band.is_empty() {
            return Ok(None);
        }
        let key = BandKey {
            width: band.w,
            height: band.h,
            text: label_text(src, g),
            scale: g.label_scale,
        };
        if let Some(f) = self.bands.get(&key) {
            return Ok(Some((band, f.clone())));
        }
        // Paint the band at the origin of a plane its own size.
        let mut plane = HostFrame::black(band.w, band.h);
        let local = TileGeometry {
            band: Some(Rect::new(0, 0, band.w, band.h)),
            ..*g
        };
        draw_band(&mut plane, src, &local, &self.theme);
        let pool = self.device.frames(plane.resolution())?;
        let up = upload(&self.device, &pool, &plane)?;
        if self.bands.len() >= 64 {
            // Names come and go; the cache is for the steady state.
            self.bands.clear();
        }
        self.bands.insert(key, up.clone());
        Ok(Some((band, up)))
    }

    /// Build the graph for `shape`: the underlay as input 0, then a
    /// buffer source per picture through the scale filter into a chain
    /// of overlays, then one per band on top.
    fn build(
        &self,
        shape: &Shape,
        underlay: *mut ff::AVFrame,
        pictures: &[*mut ff::AVFrame],
        bands: &[*mut ff::AVFrame],
    ) -> Result<FilterGraph, CodecError> {
        let kind = self.device.kind();
        let overlay_filter = match kind {
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => "overlay_cuda",
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => "overlay_vaapi",
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV => "overlay_qsv",
            _ => "overlay",
        };
        let mut graph = FilterGraph::new()?;
        let (_, base) = graph.add_source(underlay, self.resolution)?;
        let mut chain: Node = base;
        for (p, frame) in shape.pictures.iter().zip(pictures) {
            let (_, input) = graph.add_source(*frame, p.src)?;
            let to = Resolution::new(p.dst.w, p.dst.h);
            let scaled = if to == p.src {
                input
            } else {
                let s = graph.add_filter(filter_for(kind), &scale_args(kind, to, p.mode))?;
                graph.link(input, 0, s, 0)?;
                s
            };
            let over = graph.add_filter(overlay_filter, &format!("x={}:y={}", p.dst.x, p.dst.y))?;
            graph.link(chain, 0, over, 0)?;
            graph.link(scaled, 0, over, 1)?;
            chain = over;
        }
        for (r, frame) in shape.bands.iter().zip(bands) {
            let (_, input) = graph.add_source(*frame, Resolution::new(r.w, r.h))?;
            let over = graph.add_filter(overlay_filter, &format!("x={}:y={}", r.x, r.y))?;
            graph.link(chain, 0, over, 0)?;
            graph.link(input, 0, over, 1)?;
            chain = over;
        }
        graph.add_sink(chain)?;
        graph.configure()?;
        Ok(graph)
    }
}

impl Compositor for DeviceCompositor {
    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn layout(&self) -> Layout {
        self.layout
    }

    fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    fn resolution(&self) -> Resolution {
        self.resolution
    }

    fn render(&mut self, sources: &[TileSource<'_>], pts: u32) -> Result<(), CodecError> {
        // Every frame must be here before anything is done.
        let mut frames: Vec<Option<*mut ff::AVFrame>> = Vec::with_capacity(sources.len());
        for s in sources {
            frames.push(match s.frame {
                None => None,
                Some(f) => Some(hw_frame(f, self.device.device())?.raw()),
            });
        }
        let t = self.theme;
        let (w, h) = (self.resolution.width, self.resolution.height);
        let tiles = self.layout.tiles(sources.len(), w, h, t.gap_px);
        let n = tiles.len();
        let sources = &sources[..n.min(sources.len())];
        let frames = &frames[..sources.len()];

        let geometry: Vec<TileGeometry> = sources
            .iter()
            .zip(&tiles)
            .map(|(s, &r)| tile_geometry(r, s.kind, &t))
            .collect();
        let key = UnderlayKey {
            layout: self.layout,
            tiles: sources
                .iter()
                .zip(&geometry)
                .map(|(s, g)| UnderlayTile {
                    rect: g.rect,
                    kind: s.kind,
                    speaking: s.speaking && s.kind == TileKind::Camera,
                    picture: s.frame.map(|f| f.resolution()),
                    avatar: match (s.kind, s.frame) {
                        (TileKind::Camera, None) => Some(s.name.to_string()),
                        _ => None,
                    },
                })
                .collect(),
        };
        let underlay = self.underlay(key, sources, &geometry)?;

        let mut pictures: Vec<Picture> = Vec::new();
        let mut picture_frames: Vec<*mut ff::AVFrame> = Vec::new();
        let mut bands: Vec<Rect> = Vec::new();
        let mut band_frames: Vec<VideoFrame> = Vec::new();
        for ((s, g), f) in sources.iter().zip(&geometry).zip(frames) {
            if let (Some(f), Some(frame)) = (s.frame, f) {
                let dst = g.picture(f.resolution());
                if !dst.is_empty() {
                    pictures.push(Picture {
                        dst,
                        src: f.resolution(),
                        mode: match s.kind {
                            TileKind::Camera => ScaleMode::Bilinear,
                            TileKind::Content => ScaleMode::Box,
                        },
                    });
                    picture_frames.push(*frame);
                }
            }
            if let Some((r, plane)) = self.band(s, g)? {
                bands.push(r);
                band_frames.push(plane);
            }
        }
        let shape = Shape { pictures, bands };
        let underlay_raw = hw_frame(&underlay, self.device.device())?.raw();
        let band_raws: Vec<*mut ff::AVFrame> = band_frames
            .iter()
            .map(|f| hw_frame(f, self.device.device()).map(|h| h.raw()))
            .collect::<Result<_, _>>()?;
        if self.graph.as_ref().map(|g| &g.shape) != Some(&shape) {
            let graph = self.build(&shape, underlay_raw, &picture_frames, &band_raws)?;
            self.graph = Some(Graph { shape, graph });
            self.rebuilds += 1;
        }
        let g = self.graph.as_mut().expect("built above");
        self.ticks += 1;
        let tick = self.ticks;
        g.graph.push(0, underlay_raw, tick)?;
        let mut idx = 1;
        for f in &picture_frames {
            g.graph.push(idx, *f, tick)?;
            idx += 1;
        }
        for f in &band_raws {
            g.graph.push(idx, *f, tick)?;
            idx += 1;
        }
        let out = g.graph.pull()?;
        self.canvas = wrap(self.device.device(), out, pts);
        Ok(())
    }

    fn canvas(&self) -> &VideoFrame {
        &self.canvas
    }
}
