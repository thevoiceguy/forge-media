//! A conference room on the device end to end (design §15.7, block
//! 8b): NVDEC decodes the cameras, the device compositor draws them,
//! NVENC encodes the composite for one subscriber, and a VP8
//! subscriber — a codec NVENC lacks — is fed from the canvas
//! downloaded once a tick. Skips without a device.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use forge_conference::video::{CodecPool, SubscribeRequest, VideoBackend, VideoRoomSettings};
use forge_conference::{AudioFormat, ConferenceRoom};
use forge_core::VideoCodec;
use forge_rtp::video::payload::packetize;
use forge_rtp::{AssemblerEvent, FrameAssembler, RtpPacket};
use forge_video::bench::synth;
use forge_video::codec::{CodecRegistry, EncoderSettings, VideoDecoder, VideoEncoder};
use forge_video::device::DeviceBackend;
use forge_video::frame::{HostFrame, MediaDevice, Resolution, VideoFrame};
use forge_video::layout::Layout;
use forge_video::metrics::psnr_luma;
use forge_video::raw::raw_registry;
use forge_video_hw::HwBackend;
use tokio::time::timeout;

fn device() -> Option<MediaDevice> {
    let d = std::env::var("FORGE_HW_DEVICE").unwrap_or_else(|_| "cuda:0".into());
    let d = MediaDevice::parse(&d)?;
    if forge_video_hw::probe(&d).is_none() {
        eprintln!("no {d}: skipping");
        return None;
    }
    Some(d)
}

/// The raw codec on the host for everything, the device's codecs on it.
fn registry_for(backend: &HwBackend) -> CodecRegistry {
    let mut r = raw_registry();
    backend.register(&mut r).unwrap();
    r
}

struct Camera {
    enc: Box<dyn VideoEncoder>,
    codec: VideoCodec,
    seq: u16,
    ssrc: u32,
    n: usize,
}

impl Camera {
    fn new(registry: &CodecRegistry, device: &MediaDevice, codec: VideoCodec, ssrc: u32) -> Self {
        let settings = EncoderSettings {
            codec,
            resolution: Resolution::new(640, 360),
            fps: 15,
            bitrate_kbps: 800,
            keyframe_interval: 30,
            profile: String::new(),
            content: Default::default(),
        };
        Self {
            enc: registry.encoder(&settings, device).unwrap(),
            codec,
            seq: 1,
            ssrc,
            n: 0,
        }
    }

    fn frame(&mut self) -> Vec<RtpPacket> {
        // The device encoder uploads a host frame itself.
        let f = synth(self.n, 640, 360);
        self.n += 1;
        let mut out = Vec::new();
        for c in self.enc.encode(&VideoFrame::Host(f), self.n == 1).unwrap() {
            let payloads = packetize(self.codec, &c, 1200).unwrap();
            let last = payloads.len();
            for (i, p) in payloads.into_iter().enumerate() {
                out.push(RtpPacket::build(
                    97,
                    self.seq,
                    c.timestamp,
                    self.ssrc,
                    p,
                    i + 1 == last,
                ));
                self.seq = self.seq.wrapping_add(1);
            }
        }
        out
    }
}

struct Screen {
    asm: FrameAssembler,
    dec: Box<dyn VideoDecoder>,
    backend: Arc<HwBackend>,
}

impl Screen {
    fn new(
        registry: &CodecRegistry,
        device: &MediaDevice,
        codec: VideoCodec,
        backend: Arc<HwBackend>,
    ) -> Self {
        Self {
            asm: FrameAssembler::new(codec),
            dec: registry.decoder(codec, device).unwrap(),
            backend,
        }
    }

    fn push(&mut self, bytes: Bytes) -> Option<HostFrame> {
        let packet = RtpPacket::parse(bytes).expect("valid RTP from the room");
        let mut got = None;
        for ev in self.asm.push(packet) {
            if let AssemblerEvent::Frame(f) = ev {
                if let Some(v) = self.dec.decode(&f).unwrap() {
                    got = Some(self.backend.download(&v).unwrap());
                }
            }
        }
        got
    }
}

async fn wait_for_composite(
    rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
    screen: &mut Screen,
    check: impl Fn(&HostFrame) -> bool,
) -> HostFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let bytes = timeout(deadline - tokio::time::Instant::now(), rx.recv())
            .await
            .expect("composite packets keep coming")
            .expect("subscription open");
        if let Some(frame) = screen.push(bytes) {
            if check(&frame) {
                return frame;
            }
        }
    }
}

fn subscribe(codec: VideoCodec) -> SubscribeRequest {
    SubscribeRequest {
        codec,
        profile: String::new(),
        payload_type: 97,
        resolution: None,
        fps: None,
        max_kbps: None,
        scope: None,
        view: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_mixed_on_the_device_reaches_its_subscribers() {
    let Some(d) = device() else { return };
    let backend = Arc::new(HwBackend::open(&d).unwrap());
    let registry = registry_for(&backend);
    let camera_registry = registry_for(&backend);
    let video_backend = VideoBackend::on_device(
        registry,
        CodecPool::new(2),
        Arc::clone(&backend) as Arc<dyn DeviceBackend>,
    );

    let audio = Arc::new(
        ConferenceRoom::new(
            "gpu",
            AudioFormat::pcm_mono(),
            160,
            forge_mixer::MixerOptions::default(),
        )
        .unwrap(),
    );
    for id in ["alice", "bob", "cy"] {
        audio.add_participant(id, id == "alice").unwrap();
    }
    let settings = VideoRoomSettings {
        layout: Layout::Grid,
        resolution: Resolution::new(1280, 720),
        fps: 30,
        codecs: vec![VideoCodec::H264, VideoCodec::VP8],
        ..VideoRoomSettings::default()
    };
    let video = audio.enable_video(settings, &video_backend);
    video.add_source("alice", VideoCodec::H264, "").unwrap();
    video.add_source("bob", VideoCodec::H264, "").unwrap();
    // Cy watches in H.264 (NVENC); Bob in VP8 (the host, from the
    // downloaded canvas).
    let mut cy = video.subscribe("cy", subscribe(VideoCodec::H264)).unwrap();
    let mut bob = video.subscribe("bob", subscribe(VideoCodec::VP8)).unwrap();

    let mut alice_cam = Camera::new(&camera_registry, &d, VideoCodec::H264, 0xA11CE);
    let mut bob_cam = Camera::new(&camera_registry, &d, VideoCodec::H264, 0xB0B);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..90 {
                for p in alice_cam.frame() {
                    video.push_rtp("alice", p);
                }
                for p in bob_cam.frame() {
                    video.push_rtp("bob", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };

    // What the host would have drawn for the same two pictures, as a
    // reference for both subscribers: the synthetic source at frame n
    // is deterministic, so compare against the layout with any two
    // frames by structure — both tiles lit — rather than exact pixels.
    let lit = |f: &HostFrame| {
        let tile = |x: u32| {
            let mut sum = 0u64;
            for y in (0..720).step_by(24) {
                sum += f.luma(x, y) as u64;
            }
            sum / 30
        };
        tile(320) > 40 && tile(960) > 40
    };
    let mut cy_screen = Screen::new(&camera_registry, &d, VideoCodec::H264, Arc::clone(&backend));
    let h264 = wait_for_composite(&mut cy.packets, &mut cy_screen, lit).await;
    assert_eq!(h264.resolution(), Resolution::new(1280, 720));

    struct RawScreen {
        asm: FrameAssembler,
        dec: Box<dyn VideoDecoder>,
    }
    let mut raw = RawScreen {
        asm: FrameAssembler::new(VideoCodec::VP8),
        dec: raw_registry()
            .decoder(VideoCodec::VP8, &MediaDevice::Host)
            .unwrap(),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let vp8 = loop {
        let bytes = timeout(deadline - tokio::time::Instant::now(), bob.packets.recv())
            .await
            .expect("vp8 packets keep coming")
            .expect("open");
        let packet = RtpPacket::parse(bytes).unwrap();
        let mut got = None;
        for ev in raw.asm.push(packet) {
            if let AssemblerEvent::Frame(f) = ev {
                if let Some(v) = raw.dec.decode(&f).unwrap() {
                    got = v.into_host();
                }
            }
        }
        if let Some(f) = got {
            if lit(&f) {
                break f;
            }
        }
    };
    assert_eq!(vp8.resolution(), Resolution::new(1280, 720));
    // The two pictures are the same canvas at different ticks of a
    // moving source (and one went through NVENC), so this is a report,
    // not a check: the check is that both subscribers saw both tiles.
    let db = psnr_luma(&h264, &vp8).unwrap();
    eprintln!("H.264 (NVENC) against VP8 (downloaded raw canvas), different ticks: {db:.2} dB");
    feeder.await.unwrap();

    let status = video.status();
    assert_eq!(status.device, d);
    assert!(status.ticks > 0);
    let out = &status.outputs[0];
    assert_eq!(out.device, d, "composed on the device");
    let by_codec = |c: VideoCodec| out.flavors.iter().find(|f| f.flavor.codec == c).unwrap();
    assert_eq!(
        by_codec(VideoCodec::H264).device,
        d,
        "H.264 encoded on the device"
    );
    assert!(
        by_codec(VideoCodec::VP8).device.is_host(),
        "VP8 encoded on the host"
    );
    for id in ["alice", "bob"] {
        let src = video.participant(id).unwrap().source.unwrap();
        assert_eq!(src.device, d, "{id} decoded on the device");
        assert!(
            src.frames_decoded > 10,
            "{id} decoded {}",
            src.frames_decoded
        );
        assert_eq!(src.decode_errors, 0);
    }
    eprintln!(
        "ticks {} overruns {} fps {} → {}",
        status.ticks, status.overruns, status.target_fps, status.fps
    );
}
