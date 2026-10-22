//! The scenes every compositor must draw alike: the golden set a device
//! compositor is checked against, rendered by the [`HostCompositor`]
//! as the reference (design §15.7, block 8b). Each scene is a layout,
//! a canvas size and a handful of tiles with deterministic synthetic
//! pictures of assorted sizes and aspect ratios, speaking and muted
//! states, avatars and a shared screen, so that scaling, placement,
//! bars, rings, bands and avatars are all exercised.

use crate::bench::synth;
use crate::compose::{Compositor, HostCompositor, TileKind, TileSource};
use crate::frame::{HostFrame, Resolution, VideoFrame};
use crate::layout::Layout;

/// One tile of a scene.
#[derive(Debug, Clone)]
pub struct SceneTile {
    pub name: String,
    pub frame: Option<HostFrame>,
    pub speaking: bool,
    pub muted: bool,
    pub kind: TileKind,
}

/// A layout on a canvas with its tiles.
#[derive(Debug, Clone)]
pub struct Scene {
    pub name: &'static str,
    pub layout: Layout,
    pub resolution: Resolution,
    pub tiles: Vec<SceneTile>,
}

impl Scene {
    /// The tiles as a compositor takes them, with `frames` standing in
    /// for the tiles' own (a device compositor's uploaded copies).
    /// `frames` must be as long as `tiles`.
    pub fn sources<'a>(&'a self, frames: &'a [Option<VideoFrame>]) -> Vec<TileSource<'a>> {
        self.tiles
            .iter()
            .zip(frames)
            .map(|(t, f)| TileSource {
                id: &t.name,
                name: &t.name,
                frame: f.as_ref(),
                speaking: t.speaking,
                muted: t.muted,
                kind: t.kind,
            })
            .collect()
    }

    /// The tiles' own frames as host frames.
    pub fn host_frames(&self) -> Vec<Option<VideoFrame>> {
        self.tiles
            .iter()
            .map(|t| t.frame.clone().map(VideoFrame::Host))
            .collect()
    }

    /// The reference: what the host compositor draws.
    pub fn render_host(&self) -> HostFrame {
        let frames = self.host_frames();
        let mut c = HostCompositor::new(self.resolution.width, self.resolution.height, self.layout);
        c.render(&self.sources(&frames), 3000)
            .expect("host frames render on the host");
        c.host_canvas().clone()
    }
}

fn camera(name: &str, frame: Option<HostFrame>, speaking: bool, muted: bool) -> SceneTile {
    SceneTile {
        name: name.to_string(),
        frame,
        speaking,
        muted,
        kind: TileKind::Camera,
    }
}

fn pic(n: u32, w: u32, h: u32) -> Option<HostFrame> {
    Some(synth(n as usize, w, h))
}

/// The golden set.
pub fn scenes() -> Vec<Scene> {
    vec![
        Scene {
            name: "grid of four, mixed sizes and aspects",
            layout: Layout::Grid,
            resolution: Resolution::new(640, 360),
            tiles: vec![
                camera("Alice Smith", pic(1, 640, 360), true, false),
                camera("Bob", pic(2, 320, 240), false, true),
                camera("Carol Jones", pic(3, 1280, 720), false, false),
                camera("Dan", pic(4, 176, 144), false, false),
            ],
        },
        Scene {
            name: "spotlight on an avatar",
            layout: Layout::Spotlight,
            resolution: Resolution::new(640, 360),
            tiles: vec![camera("Erin Vale", None, true, false)],
        },
        Scene {
            name: "spotlight on a picture wider than the canvas",
            layout: Layout::Spotlight,
            resolution: Resolution::new(640, 360),
            tiles: vec![camera("Frank", pic(5, 1920, 1080), false, false)],
        },
        Scene {
            name: "active speaker with six at 720p",
            layout: Layout::ActiveSpeaker,
            resolution: Resolution::new(1280, 720),
            tiles: vec![
                camera("Grace Hopper", pic(6, 1280, 720), true, false),
                camera("Heidi", pic(7, 640, 480), false, false),
                camera("Ivan", None, false, true),
                camera("Judy Long-Name-Here", pic(8, 320, 180), false, false),
                camera("Ken", pic(9, 640, 360), false, false),
                camera("Liu", pic(10, 352, 288), false, true),
            ],
        },
        Scene {
            name: "picture in picture",
            layout: Layout::PictureInPicture,
            resolution: Resolution::new(640, 360),
            tiles: vec![
                camera("Mallory", pic(11, 640, 360), false, false),
                camera("Niaj", pic(12, 640, 360), true, false),
            ],
        },
        Scene {
            name: "presentation: a 4:3 document with a strip",
            layout: Layout::Presentation,
            resolution: Resolution::new(1280, 720),
            tiles: vec![
                SceneTile {
                    name: "screen".into(),
                    frame: pic(13, 1024, 768),
                    speaking: true,
                    muted: false,
                    kind: TileKind::Content,
                },
                camera("Olivia", pic(14, 640, 360), true, false),
                camera("Peggy", None, false, false),
                camera("Quentin", pic(15, 320, 240), false, true),
            ],
        },
        Scene {
            name: "content alone, larger than the canvas",
            layout: Layout::Spotlight,
            resolution: Resolution::new(640, 360),
            tiles: vec![SceneTile {
                name: "screen".into(),
                frame: pic(16, 1920, 1200),
                speaking: false,
                muted: false,
                kind: TileKind::Content,
            }],
        },
        Scene {
            name: "a full grid of sixteen",
            layout: Layout::Grid,
            resolution: Resolution::new(1280, 720),
            tiles: (0..16)
                .map(|i| {
                    camera(
                        &format!("Participant {i}"),
                        if i % 5 == 4 {
                            None
                        } else {
                            pic(20 + i, 640, 360)
                        },
                        i == 3,
                        i % 4 == 1,
                    )
                })
                .collect(),
        },
        Scene {
            name: "tiny canvas",
            layout: Layout::Grid,
            resolution: Resolution::new(128, 72),
            tiles: vec![
                camera("A", pic(40, 64, 36), true, false),
                camera("B", None, false, true),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::psnr_luma;

    #[test]
    fn every_scene_renders_on_the_host_and_is_deterministic() {
        for scene in scenes() {
            let a = scene.render_host();
            let b = scene.render_host();
            assert_eq!(a.resolution(), scene.resolution, "{}", scene.name);
            assert_eq!(psnr_luma(&a, &b), Some(f64::INFINITY), "{}", scene.name);
            // Something was drawn: not the plain background.
            let bg = crate::compose::Theme::default().background.0;
            assert!(a.y.iter().any(|&y| y != bg), "{}", scene.name);
        }
    }
}
