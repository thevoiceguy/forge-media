//! The numbers a GPU is priced by (design §9.3, §15.7 decision 7).
//!
//! A GPU is several engines, so one encoder's speed says little about
//! how many the device runs: [`saturate`] opens more and more encoders
//! (or decoders) on their own threads until the sum of their frame rates
//! stops growing, and reports the streams the device sustains at that
//! size and rate. forge-video's `measure_codec` still gives the
//! single-stream constant; together they fill §9.4's GPU column.

use crate::device::HwDevice;
use crate::frame::upload;
use forge_core::VideoCodec;
use forge_video::bench::noisy;
use forge_video::codec::{CodecError, CodecRegistry, EncoderSettings};
use forge_video::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What to saturate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Encode,
    Decode,
}

/// How much a device sustains of one stage of one codec.
#[derive(Debug, Clone, PartialEq)]
pub struct Saturation {
    pub codec: VideoCodec,
    pub stage: Stage,
    pub device: MediaDevice,
    pub resolution: Resolution,
    pub fps: u32,
    /// Streams run at once when throughput stopped growing.
    pub streams: usize,
    /// Frames per second, summed over the streams, at that point.
    pub total_fps: f64,
    /// `total_fps / fps`: real-time streams at the given rate.
    pub realtime_streams: f64,
    /// The step where throughput stopped growing; `false` when the cap
    /// on streams was reached first.
    pub saturated: bool,
}

/// Run `stage` of `codec` on `device` with 1, 2, 4, … streams for
/// `per_step` each, until the summed frame rate grows by less than a
/// tenth or `max_streams` is reached.
pub fn saturate(
    registry: &Arc<CodecRegistry>,
    device: &MediaDevice,
    codec: VideoCodec,
    stage: Stage,
    resolution: Resolution,
    fps: u32,
    per_step: Duration,
    max_streams: usize,
) -> Result<Saturation, CodecError> {
    let settings = EncoderSettings {
        codec,
        resolution,
        fps,
        bitrate_kbps: 1200,
        keyframe_interval: fps * 2,
        profile: String::new(),
        content: Default::default(),
    };
    // One coded sequence for every decoder to chew on.
    let ring: Arc<Vec<HostFrame>> = Arc::new(
        (0..16)
            .map(|i| noisy(i, resolution.width, resolution.height))
            .collect(),
    );
    let coded: Arc<Vec<forge_rtp::CodedFrame>> = if stage == Stage::Decode {
        let mut enc = registry.encoder(&settings, device)?;
        let mut v = Vec::new();
        for (i, f) in ring.iter().enumerate() {
            v.extend(enc.encode(
                &VideoFrame::Host(f.clone().with_pts(i as u32 * 3000)),
                i == 0,
            )?);
        }
        Arc::new(v)
    } else {
        Arc::new(Vec::new())
    };
    let mut best = (0usize, 0.0f64);
    let mut streams = 1usize;
    let mut saturated = false;
    loop {
        let mut handles = Vec::new();
        for _ in 0..streams {
            let registry = Arc::clone(registry);
            let device = device.clone();
            let settings = settings.clone();
            let ring = Arc::clone(&ring);
            let coded = Arc::clone(&coded);
            handles.push(std::thread::spawn(move || -> Result<u64, CodecError> {
                let mut frames = 0u64;
                let start = Instant::now();
                match stage {
                    Stage::Encode => {
                        // The ring goes up once; what is timed is the
                        // encoder taking device frames, not the bus.
                        let hw = HwDevice::open(&device)?;
                        let pool = hw.frames(settings.resolution)?;
                        let device_ring: Vec<VideoFrame> = ring
                            .iter()
                            .map(|f| upload(&hw, &pool, f))
                            .collect::<Result<_, _>>()?;
                        let mut enc = registry.encoder(&settings, &device)?;
                        let mut i = 0u32;
                        let start = Instant::now();
                        while start.elapsed() < per_step {
                            let mut f = device_ring[i as usize % device_ring.len()].clone();
                            if let VideoFrame::Device(d) = &mut f {
                                d.pts = i * 3000;
                            }
                            enc.encode(&f, false)?;
                            frames += 1;
                            i += 1;
                        }
                    }
                    Stage::Decode => {
                        let mut dec = registry.decoder(settings.codec, &device)?;
                        let mut i = 0usize;
                        while start.elapsed() < per_step {
                            if dec.decode(&coded[i % coded.len()])?.is_some() {
                                frames += 1;
                            }
                            i += 1;
                        }
                    }
                }
                Ok(frames)
            }));
        }
        let mut total = 0u64;
        for h in handles {
            total += h
                .join()
                .map_err(|_| CodecError::Codec("bench thread panicked".into()))??;
        }
        let total_fps = total as f64 / per_step.as_secs_f64();
        if total_fps <= best.1 * 1.1 && streams > 1 {
            saturated = true;
            break;
        }
        best = (streams, total_fps);
        if streams >= max_streams {
            break;
        }
        streams = (streams * 2).min(max_streams);
    }
    Ok(Saturation {
        codec,
        stage,
        device: device.clone(),
        resolution,
        fps,
        streams: best.0,
        total_fps: best.1,
        realtime_streams: best.1 / fps.max(1) as f64,
        saturated,
    })
}
