//! The device compositor against the host's (design §15.7, block 8b):
//! every golden scene rendered on both within a PSNR threshold, the
//! graph built once for a steady scene, and the composite's cost per
//! tick measured against the host's. Skips without a device.

use forge_video::bench::{measure_compose, measure_compose_on};
use forge_video::compose::Compositor;
use forge_video::device::DeviceBackend;
use forge_video::frame::{MediaDevice, Resolution, VideoFrame};
use forge_video::metrics::psnr_luma;
use forge_video::parity::scenes;
use forge_video_hw::{DeviceCompositor, HwBackend, HwDevice};
use std::sync::Arc;

/// The scaled pictures differ by interpolation (the host's bilinear
/// against `scale_cuda`, 29 dB on noise in 8a); the chrome is the same
/// code and the same pixels.
const PARITY_DB: f64 = 27.0;

fn device() -> Option<MediaDevice> {
    let d = std::env::var("FORGE_HW_DEVICE").unwrap_or_else(|_| "cuda:0".into());
    let d = MediaDevice::parse(&d)?;
    if forge_video_hw::probe(&d).is_none() {
        eprintln!("no {d}: skipping");
        return None;
    }
    Some(d)
}

#[test]
fn every_scene_renders_on_the_device_as_on_the_host() {
    let Some(d) = device() else { return };
    let backend = HwBackend::open(&d).unwrap();
    let mut worst = f64::INFINITY;
    for scene in scenes() {
        let reference = scene.render_host();
        let frames: Vec<Option<VideoFrame>> = scene
            .tiles
            .iter()
            .map(|t| t.frame.as_ref().map(|f| backend.upload(f).unwrap()))
            .collect();
        let mut c = backend
            .compositor(
                scene.resolution.width,
                scene.resolution.height,
                scene.layout,
            )
            .unwrap();
        assert_eq!(c.device(), d);
        // Twice: the second render takes the cached underlay and bands.
        c.render(&scene.sources(&frames), 3000).unwrap();
        c.render(&scene.sources(&frames), 6000).unwrap();
        assert_eq!(c.canvas().pts(), 6000);
        assert_eq!(c.canvas().device(), d);
        let got = backend.download(c.canvas()).unwrap();
        assert_eq!(got.resolution(), scene.resolution);
        let db = psnr_luma(&reference, &got).unwrap();
        eprintln!("{:<48} {db:6.2} dB", scene.name);
        worst = worst.min(db);
        assert!(db >= PARITY_DB, "{}: {db:.2} dB < {PARITY_DB}", scene.name);
    }
    eprintln!("worst scene {worst:.2} dB");
}

#[test]
fn a_steady_scene_builds_its_graph_once_and_a_changed_shape_rebuilds_it() {
    let Some(d) = device() else { return };
    let hw = Arc::new(HwDevice::open(&d).unwrap());
    let backend = HwBackend::from_device(Arc::clone(&hw));
    let scene = &scenes()[0];
    let frames = scene
        .tiles
        .iter()
        .map(|t| t.frame.as_ref().map(|f| backend.upload(f).unwrap()))
        .collect::<Vec<_>>();
    let mut c = DeviceCompositor::new(
        hw,
        scene.resolution.width,
        scene.resolution.height,
        scene.layout,
    )
    .unwrap();
    for pts in [1, 2, 3] {
        c.render(&scene.sources(&frames), pts).unwrap();
    }
    assert_eq!(c.rebuilds(), 1, "one graph for a steady scene");
    let uploads = c.uploads();
    c.render(&scene.sources(&frames), 4).unwrap();
    assert_eq!(c.uploads(), uploads, "a steady scene uploads nothing");
    // Someone else speaks: the tiles' planes change, the graph does not.
    let mut sources = scene.sources(&frames);
    for s in &mut sources {
        s.speaking = !s.speaking;
    }
    c.render(&sources, 5).unwrap();
    assert_eq!(c.rebuilds(), 1);
    assert!(c.uploads() > uploads, "new rings went up");
    // A tile loses its picture: the shape changes.
    let mut fewer = frames.clone();
    fewer[0] = None;
    c.render(&scene.sources(&fewer), 6).unwrap();
    assert_eq!(c.rebuilds(), 2);
    // And the picture matches what the host draws for the same thing.
    let mut host_scene = scene.clone();
    host_scene.tiles[0].frame = None;
    let db = psnr_luma(
        &host_scene.render_host(),
        &backend.download(c.canvas()).unwrap(),
    )
    .unwrap();
    assert!(db >= PARITY_DB, "{db:.2} dB");
}

#[test]
fn composition_is_priced_by_saturation_on_the_device() {
    use forge_video::bench::measure_compose_saturated;
    use std::time::Duration;
    let Some(d) = device() else { return };
    let backend = HwBackend::open(&d).unwrap();
    let res = Resolution::new(1280, 720);
    let sat = measure_compose_saturated(&backend, res, 9, Duration::from_secs(1), 8).unwrap();
    let one = measure_compose_on(&backend, res, 9, 60).unwrap();
    eprintln!(
        "720p grid of 9: one graph {:.2} ms/tick; saturated at {} graphs, {:.0} ticks/s = {:.2} ns/px (one graph's tick as a constant: {:.2})",
        one * res.pixels() as f64 / 1e6,
        sat.graphs,
        sat.ticks_per_second,
        sat.ns_per_px(),
        one
    );
    assert_eq!(sat.device, d);
    assert!(sat.ticks_per_second > 30.0, "{}", sat.ticks_per_second);
    // Several graphs at once do more than one: the constant priced by
    // saturation is below one graph's tick.
    assert!(sat.ns_per_px() < one, "{} vs {}", sat.ns_per_px(), one);
}

#[test]
fn the_composite_s_cost_on_the_device_is_measured_against_the_host_s() {
    let Some(d) = device() else { return };
    let backend = HwBackend::open(&d).unwrap();
    let res = Resolution::new(1280, 720);
    for tiles in [1usize, 4, 9, 16] {
        let host = measure_compose(res, tiles, 30);
        let dev = measure_compose_on(&backend, res, tiles, 60).unwrap();
        let per_tick_ms = |ns_per_px: f64| ns_per_px * res.pixels() as f64 / 1e6;
        eprintln!(
            "720p grid of {tiles:>2}: host {:.2} ms/tick, device {:.2} ms/tick ({:.1}× )",
            per_tick_ms(host),
            per_tick_ms(dev),
            host / dev
        );
        assert!(
            per_tick_ms(dev) < 33.0,
            "a 720p composite of {tiles} on the device must fit a 30 fps tick: {:.1} ms",
            per_tick_ms(dev)
        );
    }
}
