//! A room on a device (design §15.7, block 8b), on a machine without
//! one: the fake device of `forge_video::testing` holds host frames
//! behind device handles and counts what crosses its bus, so the
//! placement of each stage and the fallbacks to the host are checked
//! here, in CI, and the real device's pixels on the GPU box.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use forge_conference::video::{
    CodecPool, RecordRequest, StreamKind, SubscribeRequest, VideoBackend, VideoRoomSettings,
    VideoState,
};
use forge_conference::{AudioFormat, ConferenceRoom};
use forge_core::VideoCodec;
use forge_rtp::video::payload::packetize;
use forge_rtp::{AssemblerEvent, FrameAssembler, RtpPacket};
use forge_video::codec::{EncoderSettings, VideoDecoder, VideoEncoder};
use forge_video::device::DeviceBackend;
use forge_video::frame::{HostFrame, Resolution, VideoFrame};
use forge_video::layout::Layout;
use forge_video::raw::raw_registry;
use forge_video::testing::{fake_registry, FakeBackend};
use forge_video::MediaDevice;
use tokio::time::timeout;

struct Camera {
    enc: Box<dyn VideoEncoder>,
    codec: VideoCodec,
    seq: u16,
    ssrc: u32,
    ts: u32,
}

impl Camera {
    fn new(codec: VideoCodec, ssrc: u32, w: u32, h: u32) -> Self {
        let settings = EncoderSettings {
            codec,
            resolution: Resolution::new(w, h),
            fps: 15,
            bitrate_kbps: 500,
            keyframe_interval: 30,
            profile: String::new(),
            content: Default::default(),
        };
        Self {
            enc: raw_registry()
                .encoder(&settings, &MediaDevice::Host)
                .unwrap(),
            codec,
            seq: 1000,
            ssrc,
            ts: 0,
        }
    }

    fn frame(&mut self, luma: u8) -> Vec<RtpPacket> {
        let res = self.enc.settings().resolution;
        let f = HostFrame::solid(res.width, res.height, luma, 128, 128).with_pts(self.ts);
        self.ts = self.ts.wrapping_add(6000);
        let mut out = Vec::new();
        for c in self.enc.encode(&VideoFrame::Host(f), false).unwrap() {
            let payloads = packetize(self.codec, &c, 1200).unwrap();
            let n = payloads.len();
            for (i, p) in payloads.into_iter().enumerate() {
                out.push(RtpPacket::build(
                    97,
                    self.seq,
                    c.timestamp,
                    self.ssrc,
                    p,
                    i + 1 == n,
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
}

impl Screen {
    fn new(codec: VideoCodec) -> Self {
        Self {
            asm: FrameAssembler::new(codec),
            dec: raw_registry().decoder(codec, &MediaDevice::Host).unwrap(),
        }
    }

    fn push(&mut self, bytes: Bytes) -> Option<HostFrame> {
        let packet = RtpPacket::parse(bytes).expect("valid RTP from the room");
        let mut got = None;
        for ev in self.asm.push(packet) {
            if let AssemblerEvent::Frame(f) = ev {
                if let Some(v) = self.dec.decode(&f).unwrap() {
                    got = v.into_host();
                }
            }
        }
        got
    }
}

fn settings(w: u32, h: u32) -> VideoRoomSettings {
    VideoRoomSettings {
        layout: Layout::Grid,
        resolution: Resolution::new(w, h),
        fps: 30,
        codecs: vec![VideoCodec::VP8, VideoCodec::VP9],
        freeze_timeout: Duration::from_millis(300),
        content_max_resolution: Resolution::new(256, 144),
        ..VideoRoomSettings::default()
    }
}

fn audio_room(id: &str) -> Arc<ConferenceRoom> {
    Arc::new(
        ConferenceRoom::new(
            id,
            AudioFormat::pcm_mono(),
            160,
            forge_mixer::MixerOptions::default(),
        )
        .unwrap(),
    )
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

/// A backend on a fake device that decodes VP8 and VP9 but encodes
/// only VP9 — the shape of an NVIDIA GPU, which decodes more than it
/// encodes (the raw codec's frames go through the VP8 and VP9
/// packetizers untouched, which is why those two stand in here).
fn gpu_like() -> (Arc<FakeBackend>, VideoBackend) {
    let hw = Arc::new(FakeBackend::new("0"));
    let registry = fake_registry(
        &hw.device(),
        &[VideoCodec::VP8, VideoCodec::VP9],
        &[VideoCodec::VP9],
    );
    let backend = VideoBackend::on_device(registry, CodecPool::new(2), Arc::clone(&hw) as _);
    (hw, backend)
}

async fn wait_for_composite(
    rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
    screen: &mut Screen,
    check: impl Fn(&HostFrame) -> bool,
) -> HostFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_on_a_device_keeps_its_frames_there_and_says_so() {
    let (hw, backend) = gpu_like();
    let device = hw.device();
    let audio = audio_room("dev");
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(settings(256, 72), &backend);
    video.add_source("alice", VideoCodec::VP8, "").unwrap();
    let mut sub = video.subscribe("bob", subscribe(VideoCodec::VP9)).unwrap();

    let mut alice = Camera::new(VideoCodec::VP8, 0xA11CE, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..40 {
                for p in alice.frame(200) {
                    video.push_rtp("alice", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };
    let mut screen = Screen::new(VideoCodec::VP9);
    let frame = wait_for_composite(&mut sub.packets, &mut screen, |f| f.luma(64, 30) == 200).await;
    assert_eq!(frame.resolution(), Resolution::new(256, 72));
    feeder.await.unwrap();

    // Decoded, composed and encoded on the device: nothing crossed the
    // bus.
    let status = video.status();
    assert_eq!(status.device, device);
    let out = &status.outputs[0];
    assert_eq!(out.device, device, "composed on the device");
    assert_eq!(out.flavors[0].device, device, "encoded on the device");
    let src = video.participant("alice").unwrap().source.unwrap();
    assert_eq!(src.device, device, "decoded on the device");
    assert_eq!(hw.uploads(), 0, "no upload");
    assert_eq!(hw.downloads(), 0, "no download");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_codec_the_device_cannot_encode_takes_the_canvas_downloaded_once_a_tick() {
    let (hw, backend) = gpu_like();
    let device = hw.device();
    let audio = audio_room("vp8");
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    audio.add_participant("cy", false).unwrap();
    let video = audio.enable_video(settings(256, 72), &backend);
    video.add_source("alice", VideoCodec::VP8, "").unwrap();
    // Two VP8 subscribers of the same flavor share the host encoder;
    // a recording in VP8 (what a WebM asks for) shares it too.
    let mut bob = video.subscribe("bob", subscribe(VideoCodec::VP8)).unwrap();
    let mut cy = video.subscribe("cy", subscribe(VideoCodec::VP8)).unwrap();
    let mut rec = video
        .record("rec-1", RecordRequest::new(VideoCodec::VP8))
        .unwrap();

    let mut alice = Camera::new(VideoCodec::VP8, 0xA11CE, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..40 {
                for p in alice.frame(200) {
                    video.push_rtp("alice", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };
    let mut screen = Screen::new(VideoCodec::VP8);
    // Three participants: a 2×2 grid, alice's picture in the first tile.
    wait_for_composite(&mut bob.packets, &mut screen, |f| f.luma(64, 14) == 200).await;
    let mut screen2 = Screen::new(VideoCodec::VP8);
    wait_for_composite(&mut cy.packets, &mut screen2, |f| f.luma(64, 14) == 200).await;
    let first = timeout(Duration::from_secs(5), rec.frames.recv())
        .await
        .expect("recording frames")
        .expect("open");
    assert!(first.frame.keyframe);
    feeder.await.unwrap();

    let status = video.status();
    assert_eq!(status.device, device);
    let out = &status.outputs[0];
    assert_eq!(out.device, device, "still composed on the device");
    assert_eq!(
        out.flavors.len(),
        1,
        "one VP8 flavor for both subscribers and the recording"
    );
    assert!(
        out.flavors[0].device.is_host(),
        "VP8 is encoded on the host"
    );
    assert_eq!(out.flavors[0].subscribers, 2);
    // One download per tick, not per subscriber: fewer downloads than
    // frames sent to the two of them together.
    let sent = video
        .participant("bob")
        .unwrap()
        .subscription
        .unwrap()
        .frames_sent
        + video
            .participant("cy")
            .unwrap()
            .subscription
            .unwrap()
            .frames_sent;
    let downloads = hw.downloads();
    assert!(downloads > 0, "the canvas came down");
    assert!(
        downloads * 2 <= sent + 2,
        "downloads {downloads} for {sent} frames sent to two subscribers"
    );
    assert_eq!(hw.uploads(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_codec_the_device_cannot_decode_is_decoded_on_the_host_and_uploaded() {
    let hw = Arc::new(FakeBackend::new("0"));
    let device = hw.device();
    // The device decodes nothing but VP8; a VP9 camera joins.
    let registry = fake_registry(&device, &[VideoCodec::VP8], &[VideoCodec::VP9]);
    let backend = VideoBackend::on_device(registry, CodecPool::new(2), Arc::clone(&hw) as _);
    let audio = audio_room("dec");
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(settings(256, 72), &backend);
    video.add_source("alice", VideoCodec::VP9, "").unwrap();
    let mut sub = video.subscribe("bob", subscribe(VideoCodec::VP9)).unwrap();

    let mut alice = Camera::new(VideoCodec::VP9, 0xA11CE, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..40 {
                for p in alice.frame(200) {
                    video.push_rtp("alice", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };
    let mut screen = Screen::new(VideoCodec::VP9);
    wait_for_composite(&mut sub.packets, &mut screen, |f| f.luma(64, 30) == 200).await;
    feeder.await.unwrap();

    let src = video.participant("alice").unwrap().source.unwrap();
    assert!(src.device.is_host(), "decoded on the host");
    assert!(src.frames_decoded > 0);
    assert_eq!(video.participant("alice").unwrap().state, VideoState::On);
    let uploads = hw.uploads();
    assert!(uploads > 0, "decoded frames went up");
    assert!(
        uploads <= src.frames_decoded,
        "one upload per decoded frame at most"
    );
    assert_eq!(hw.downloads(), 0, "composed and encoded on the device");
    let status = video.status();
    assert_eq!(status.outputs[0].device, device);
    assert_eq!(status.outputs[0].flavors[0].device, device);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_screen_over_the_cap_is_shrunk_on_the_device_not_dropped() {
    let (hw, backend) = gpu_like();
    let audio = audio_room("screen");
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(settings(256, 144), &backend);
    video
        .add_source_kind("alice", StreamKind::Content, VideoCodec::VP8, "")
        .unwrap();
    let mut sub = video.subscribe("bob", subscribe(VideoCodec::VP9)).unwrap();

    // A 512×288 screen into a room that caps content at 256×144.
    let mut monitor = Camera::new(VideoCodec::VP8, 0x5C4EE, 512, 288);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..40 {
                for p in monitor.frame(180) {
                    video.push_rtp_kind("alice", StreamKind::Content, p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };
    let mut screen = Screen::new(VideoCodec::VP9);
    wait_for_composite(&mut sub.packets, &mut screen, |f| f.luma(128, 72) == 180).await;
    feeder.await.unwrap();
    let alice = video.participant("alice").unwrap();
    let content = alice.content.expect("the screen is a source");
    assert_eq!(
        content.resolution,
        Resolution::new(256, 144),
        "shrunk to the cap"
    );
    assert_eq!(content.frames_dropped, 0, "nothing dropped");
    assert!(content.frames_decoded > 0);
    assert_eq!(hw.downloads(), 0, "shrunk on the device");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_without_a_compositor_composes_on_the_host_from_downloaded_tiles() {
    let (hw, backend) = gpu_like();
    let device = hw.device();
    hw.set_no_compositor(true);
    let audio = audio_room("nocomp");
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(settings(256, 72), &backend);
    video.add_source("alice", VideoCodec::VP8, "").unwrap();
    let mut sub = video.subscribe("bob", subscribe(VideoCodec::VP9)).unwrap();

    let mut alice = Camera::new(VideoCodec::VP8, 0xA11CE, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..40 {
                for p in alice.frame(200) {
                    video.push_rtp("alice", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };
    let mut screen = Screen::new(VideoCodec::VP9);
    wait_for_composite(&mut sub.packets, &mut screen, |f| f.luma(64, 30) == 200).await;
    feeder.await.unwrap();

    let status = video.status();
    assert_eq!(status.device, device, "the room is still on the device");
    let out = &status.outputs[0];
    assert!(out.device.is_host(), "composed on the host");
    assert_eq!(out.flavors[0].device, device, "encoded on the device");
    assert!(hw.downloads() > 0, "tiles came down");
    assert!(
        hw.uploads() > 0,
        "the host canvas went up to the device encoder"
    );
}
