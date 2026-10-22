//! The compositor on the device: an FFmpeg filter graph per output
//! (design §15.7, decision 2).
//!
//! The pictures are the only thing composed on the device: each tile's
//! frame goes through `scale_cuda` to the rectangle the host would have
//! drawn it in and `overlay_cuda` puts it there. Everything else is
//! *chrome* painted by the same code as the host compositor's
//! ([`forge_video::compose::draw_chrome_under`] and
//! [`forge_video::compose::draw_band`]) into small host planes and
//! uploaded: per tile, a plane the size of the tile with its ring, its
//! bars or its avatar, laid under the picture, and a plane the size of
//! the name band laid over it. The planes are cached by what they show
//! — a plane is uploaded when someone starts or stops speaking, a tile
//! gains or loses a picture, a name changes — so a steady scene costs
//! one graph run per tick and nothing across the bus. The tiles go onto
//! a background canvas uploaded once, in tile order, each tile's chrome,
//! picture and band in turn, which is the host's order too: a
//! picture-in-picture corner sits over the main picture with its ring.
//!
//! The graph is rebuilt when its shape changes: the tile rectangles,
//! which tiles have a picture and of what size, which have a band.
//! Frames of the same size from any pool on the device flow through the
//! same graph; a plane's pixels changing does not rebuild it.

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

/// What decides a tile's under-plane's pixels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChromeKey {
    width: u32,
    height: u32,
    kind: TileKind,
    speaking: bool,
    /// The picture's size: it decides the bars.
    picture: Option<Resolution>,
    /// The name, when the avatar is drawn (no picture).
    avatar: Option<String>,
}

/// A band's cache key: its pixels are a function of these.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BandKey {
    width: u32,
    height: u32,
    text: String,
    scale: u32,
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

/// One tile as the graph lays it: an under-plane, a picture, a band,
/// each optional, in that order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TileShape {
    chrome: Option<Rect>,
    picture: Option<Picture>,
    band: Option<Rect>,
}

/// The graph's shape: rebuilt when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shape {
    tiles: Vec<TileShape>,
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
    /// The background, uploaded once: the graph's first input.
    background: VideoFrame,
    /// The last output; the background before the first render.
    canvas: VideoFrame,
    /// Uploaded under-planes by what they show.
    chrome: HashMap<ChromeKey, VideoFrame>,
    /// Uploaded bands by what they show.
    bands: HashMap<BandKey, VideoFrame>,
    graph: Option<Graph>,
    /// The graph's clock, one per render.
    ticks: i64,
    rebuilds: u64,
    uploads: u64,
}

/// A bound on each plane cache: names and states come and go, the
/// cache is for the steady state.
const PLANE_CACHE: usize = 64;

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
        let background = Self::background(&device, &pool, resolution, &theme)?;
        Ok(DeviceCompositor {
            device,
            layout,
            theme,
            resolution,
            pool,
            canvas: background.clone(),
            background,
            chrome: HashMap::new(),
            bands: HashMap::new(),
            graph: None,
            ticks: 0,
            rebuilds: 0,
            uploads: 0,
        })
    }

    fn background(
        device: &HwDevice,
        pool: &HwFrames,
        resolution: Resolution,
        theme: &Theme,
    ) -> Result<VideoFrame, CodecError> {
        let mut bg = HostFrame::black(resolution.width, resolution.height);
        let (y, u, v) = theme.background;
        bg.y.fill(y);
        bg.u.fill(u);
        bg.v.fill(v);
        upload(device, pool, &bg)
    }

    pub fn with_theme(mut self, theme: Theme) -> Result<Self, CodecError> {
        self.theme = theme;
        self.background = Self::background(&self.device, &self.pool, self.resolution, &theme)?;
        self.canvas = self.background.clone();
        self.chrome.clear();
        self.bands.clear();
        self.graph = None;
        Ok(self)
    }

    /// How many times the graph was built: a steady scene builds it
    /// once.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Planes uploaded so far (the background included): a steady scene
    /// uploads nothing after its first render.
    pub fn uploads(&self) -> u64 {
        self.uploads
    }

    fn upload_plane(&mut self, plane: &HostFrame) -> Result<VideoFrame, CodecError> {
        let pool = self.device.frames(plane.resolution())?;
        self.uploads += 1;
        upload(&self.device, &pool, plane)
    }

    /// The tile's under-plane — ring, bars or avatar, background for
    /// content where the document does not reach — uploaded once per
    /// distinct look. `None` when nothing shows under the picture.
    fn chrome(
        &mut self,
        src: &TileSource<'_>,
        g: &TileGeometry,
    ) -> Result<Option<(Rect, VideoFrame)>, CodecError> {
        let rect = g.rect;
        if rect.is_empty() {
            return Ok(None);
        }
        let picture = src.frame.map(|f| f.resolution());
        // A content tile the document covers whole has nothing under it.
        if g.kind == TileKind::Content && picture.map(|p| g.picture(p) == rect).unwrap_or(false) {
            return Ok(None);
        }
        let key = ChromeKey {
            width: rect.w,
            height: rect.h,
            kind: g.kind,
            speaking: src.speaking && g.kind == TileKind::Camera,
            picture,
            avatar: match (g.kind, picture) {
                (TileKind::Camera, None) => Some(src.name.to_string()),
                _ => None,
            },
        };
        if let Some(f) = self.chrome.get(&key) {
            return Ok(Some((rect, f.clone())));
        }
        // Paint the tile at the origin of a plane its own size: the
        // geometry is the same, translated.
        let local = tile_geometry(Rect::new(0, 0, rect.w, rect.h), g.kind, &self.theme);
        let mut plane = HostFrame::black(rect.w, rect.h);
        let t = self.theme;
        scale::fill(
            &mut plane,
            Rect::new(0, 0, rect.w, rect.h),
            t.background.0,
            t.background.1,
            t.background.2,
        );
        draw_chrome_under(&mut plane, src, &local, picture, &t);
        let up = self.upload_plane(&plane)?;
        if self.chrome.len() >= PLANE_CACHE {
            self.chrome.clear();
        }
        self.chrome.insert(key, up.clone());
        Ok(Some((rect, up)))
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
        let mut plane = HostFrame::black(band.w, band.h);
        let local = TileGeometry {
            band: Some(Rect::new(0, 0, band.w, band.h)),
            ..*g
        };
        draw_band(&mut plane, src, &local, &self.theme);
        let up = self.upload_plane(&plane)?;
        if self.bands.len() >= PLANE_CACHE {
            self.bands.clear();
        }
        self.bands.insert(key, up.clone());
        Ok(Some((band, up)))
    }

    /// Build the graph for `shape`: the background as input 0, then per
    /// tile its under-plane, its picture through the scale filter, and
    /// its band, each a buffer source overlaid onto the chain.
    fn build(&self, shape: &Shape, inputs: &[*mut ff::AVFrame]) -> Result<FilterGraph, CodecError> {
        let kind = self.device.kind();
        let overlay_filter = match kind {
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => "overlay_cuda",
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => "overlay_vaapi",
            ff::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV => "overlay_qsv",
            _ => "overlay",
        };
        let mut graph = FilterGraph::new()?;
        let mut next = inputs.iter();
        let mut take = |graph: &mut FilterGraph, res: Resolution| -> Result<Node, CodecError> {
            let frame = *next
                .next()
                .ok_or_else(|| CodecError::Codec("compositor graph short of inputs".into()))?;
            Ok(graph.add_source(frame, res)?.1)
        };
        let mut chain = take(&mut graph, self.resolution)?;
        let overlay = |graph: &mut FilterGraph,
                       chain: Node,
                       input: Node,
                       at: Rect|
         -> Result<Node, CodecError> {
            let over = graph.add_filter(overlay_filter, &format!("x={}:y={}", at.x, at.y))?;
            graph.link(chain, 0, over, 0)?;
            graph.link(input, 0, over, 1)?;
            Ok(over)
        };
        for tile in &shape.tiles {
            if let Some(r) = tile.chrome {
                let input = take(&mut graph, Resolution::new(r.w, r.h))?;
                chain = overlay(&mut graph, chain, input, r)?;
            }
            if let Some(p) = &tile.picture {
                let input = take(&mut graph, p.src)?;
                let to = Resolution::new(p.dst.w, p.dst.h);
                let scaled = if to == p.src {
                    input
                } else {
                    let s = graph.add_filter(filter_for(kind), &scale_args(kind, to, p.mode))?;
                    graph.link(input, 0, s, 0)?;
                    s
                };
                chain = overlay(&mut graph, chain, scaled, p.dst)?;
            }
            if let Some(r) = tile.band {
                let input = take(&mut graph, Resolution::new(r.w, r.h))?;
                chain = overlay(&mut graph, chain, input, r)?;
            }
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
        for s in sources {
            if let Some(f) = s.frame {
                hw_frame(f, self.device.device())?;
            }
        }
        let t = self.theme;
        let (w, h) = (self.resolution.width, self.resolution.height);
        let tiles = self.layout.tiles(sources.len(), w, h, t.gap_px);
        let sources = &sources[..tiles.len().min(sources.len())];

        // The inputs in graph order: the background, then per tile its
        // under-plane, picture and band. Planes are held here so their
        // frames outlive the run.
        let mut shape = Shape {
            tiles: Vec::with_capacity(sources.len()),
        };
        let mut held: Vec<VideoFrame> = Vec::new();
        let mut inputs: Vec<*mut ff::AVFrame> = Vec::new();
        inputs.push(hw_frame(&self.background, self.device.device())?.raw());
        for (s, &rect) in sources.iter().zip(&tiles) {
            let g = tile_geometry(rect, s.kind, &t);
            let mut tile = TileShape {
                chrome: None,
                picture: None,
                band: None,
            };
            if let Some((r, plane)) = self.chrome(s, &g)? {
                tile.chrome = Some(r);
                inputs.push(hw_frame(&plane, self.device.device())?.raw());
                held.push(plane);
            }
            if let Some(f) = s.frame {
                let dst = g.picture(f.resolution());
                if !dst.is_empty() {
                    tile.picture = Some(Picture {
                        dst,
                        src: f.resolution(),
                        mode: match s.kind {
                            TileKind::Camera => ScaleMode::Bilinear,
                            TileKind::Content => ScaleMode::Box,
                        },
                    });
                    inputs.push(hw_frame(f, self.device.device())?.raw());
                }
            }
            if let Some((r, plane)) = self.band(s, &g)? {
                tile.band = Some(r);
                inputs.push(hw_frame(&plane, self.device.device())?.raw());
                held.push(plane);
            }
            shape.tiles.push(tile);
        }

        if self.graph.as_ref().map(|g| &g.shape) != Some(&shape) {
            let graph = self.build(&shape, &inputs)?;
            self.graph = Some(Graph { shape, graph });
            self.rebuilds += 1;
        }
        let g = self.graph.as_mut().expect("built above");
        self.ticks += 1;
        let tick = self.ticks;
        for (idx, f) in inputs.iter().enumerate() {
            g.graph.push(idx, *f, tick)?;
        }
        let out = g.graph.pull()?;
        drop(held);
        self.canvas = wrap(self.device.device(), out, pts);
        Ok(())
    }

    fn canvas(&self) -> &VideoFrame {
        &self.canvas
    }
}
