//! The video room end to end with the raw codec (design §14): sources
//! push RTP in, the room composes on its clock, and subscribers get the
//! composite back as RTP they can reassemble and decode.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use forge_conference::video::{
    SubscribeRequest, VideoBackend, VideoRoomEvent, VideoRoomSettings, VideoState,
};
use forge_conference::{AudioFormat, ConferenceRoom};
use forge_core::VideoCodec;
use forge_rtp::rtcp::{PsFeedback, RtcpPacket, RtpFeedback};
use forge_rtp::video::payload::packetize;
use forge_rtp::{AssemblerEvent, FrameAssembler, RtpPacket};
use forge_video::codec::{EncoderSettings, VideoDecoder, VideoEncoder};
use forge_video::frame::{HostFrame, Resolution, VideoFrame};
use forge_video::layout::Layout;
use forge_video::raw::raw_registry;
use forge_video::MediaDevice;
use tokio::time::timeout;

/// A participant's camera: raw encoder + packetizer + its own sequence.
struct Camera {
    enc: Box<dyn VideoEncoder>,
    seq: u16,
    ssrc: u32,
    ts: u32,
}

impl Camera {
    fn new(ssrc: u32, w: u32, h: u32) -> Self {
        let settings = EncoderSettings {
            codec: VideoCodec::VP8,
            resolution: Resolution::new(w, h),
            fps: 15,
            bitrate_kbps: 500,
            keyframe_interval: 30,
            profile: String::new(),
        };
        Self {
            enc: raw_registry()
                .encoder(&settings, &MediaDevice::Host)
                .unwrap(),
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
            let payloads = packetize(VideoCodec::VP8, &c, 1200).unwrap();
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

/// A participant's screen: reassembles the composite and decodes it.
struct Screen {
    asm: FrameAssembler,
    dec: Box<dyn VideoDecoder>,
}

impl Screen {
    fn new() -> Self {
        Self {
            asm: FrameAssembler::new(VideoCodec::VP8),
            dec: raw_registry()
                .decoder(VideoCodec::VP8, &MediaDevice::Host)
                .unwrap(),
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

fn settings(w: u32, h: u32, fps: u32) -> VideoRoomSettings {
    VideoRoomSettings {
        layout: Layout::Grid,
        resolution: Resolution::new(w, h),
        fps,
        codecs: vec![VideoCodec::VP8],
        freeze_timeout: Duration::from_millis(300),
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

fn subscribe(codec: VideoCodec, res: Option<Resolution>) -> SubscribeRequest {
    SubscribeRequest {
        codec,
        profile: String::new(),
        payload_type: 97,
        resolution: res,
        fps: None,
        max_kbps: None,
    }
}

/// Wait for a decoded composite that passes `check`.
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
async fn two_cameras_are_composited_into_a_grid_for_a_subscriber() {
    let audio = audio_room("grid");
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(settings(256, 72, 30), &VideoBackend::raw());
    video.set_display_name("alice", "Alice");
    let mut events = video.events();

    video.add_source("alice", VideoCodec::VP8).unwrap();
    video.add_source("bob", VideoCodec::VP8).unwrap();
    let mut sub = video
        .subscribe("bob", subscribe(VideoCodec::VP8, None))
        .unwrap();
    assert_eq!(sub.payload_type, 97);

    let mut alice = Camera::new(0xA11CE, 128, 72);
    let mut bob = Camera::new(0xB0B, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..60 {
                for p in alice.frame(200) {
                    // The very first packet carries the start-up PLI (a
                    // fresh decoder wants a keyframe); nothing else, as
                    // nothing is lost.
                    for fb in video.push_rtp("alice", p) {
                        assert!(
                            matches!(fb, RtcpPacket::PayloadFeedback(_)),
                            "no loss, no NACK: {fb:?}"
                        );
                    }
                }
                for p in bob.frame(60) {
                    video.push_rtp("bob", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
            // Bob's camera stops; Alice keeps going.
            for _ in 0..40 {
                for p in alice.frame(200) {
                    video.push_rtp("alice", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };

    let mut screen = Screen::new();
    let frame = wait_for_composite(&mut sub.packets, &mut screen, |f| {
        f.luma(64, 30) == 200 && f.luma(192, 30) == 60
    })
    .await;
    assert_eq!(frame.resolution(), Resolution::new(256, 72));

    // Both are "on" once frames flow, and the API sees the numbers.
    let mut seen_on = 0;
    while let Ok(ev) = events.try_recv() {
        if let VideoRoomEvent::ParticipantState {
            state: VideoState::On,
            ..
        } = ev
        {
            seen_on += 1;
        }
    }
    assert_eq!(seen_on, 2, "alice and bob each turned on once");
    let alice_info = video.participant("alice").unwrap();
    assert_eq!(alice_info.display_name, "Alice");
    assert_eq!(alice_info.state, VideoState::On);
    let src = alice_info.source.unwrap();
    assert_eq!(src.codec, VideoCodec::VP8);
    assert_eq!(src.resolution, Resolution::new(128, 72));
    assert!(src.frames_decoded >= 1, "decoded {}", src.frames_decoded);
    assert_eq!(src.frames_lost, 0);
    let bob_sub = video.participant("bob").unwrap().subscription.unwrap();
    assert!(bob_sub.frames_sent > 0);
    assert_eq!(bob_sub.ssrc, sub.ssrc);
    let status = video.status();
    assert_eq!(status.layout, Layout::Grid);
    assert_eq!(status.sources, 2);
    assert_eq!(status.encoders, 1);
    assert!(status.ticks > 0);

    // Bob stopped sending: after the freeze timeout his tile is an
    // avatar while Alice's stays live, and his state says "lost".
    let frame = wait_for_composite(&mut sub.packets, &mut screen, |f| f.luma(192, 30) != 60).await;
    assert_eq!(frame.luma(64, 30), 200, "alice's tile is still live");
    assert_eq!(video.participant("bob").unwrap().state, VideoState::Lost);
    feeder.await.unwrap();
    let src = video.participant("alice").unwrap().source.unwrap();
    assert!(src.frames_decoded >= 50, "decoded {}", src.frames_decoded);
    assert_eq!(src.frames_lost, 0);

    // Leaving the audio room removes bob's video with it.
    audio.remove_participant("bob").unwrap();
    assert!(video.participant("bob").is_none());
    assert!(!video.has_subscriber("bob"));
    assert_eq!(
        video.status().encoders,
        0,
        "the last subscriber took its encoder"
    );
    audio.disable_video();
    assert!(audio.video().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribers_with_the_same_needs_share_an_encoder_and_exclude_self_splits_them() {
    let audio = audio_room("share");
    for id in ["a", "b", "c"] {
        audio.add_participant(id, false).unwrap();
    }
    let video = audio.enable_video(settings(256, 144, 15), &VideoBackend::raw());
    let _a = video
        .subscribe("a", subscribe(VideoCodec::VP8, None))
        .unwrap();
    let _b = video
        .subscribe("b", subscribe(VideoCodec::VP8, None))
        .unwrap();
    assert_eq!(video.status().encoders, 1, "one flavor, one encoder");
    // A smaller picture is another flavor (and another canvas).
    let _c = video
        .subscribe(
            "c",
            subscribe(VideoCodec::VP8, Some(Resolution::new(128, 72))),
        )
        .unwrap();
    let status = video.status();
    assert_eq!(status.encoders, 2);
    assert_eq!(status.outputs.len(), 2);
    // Asking for more than the room's canvas is clamped to it.
    let big = video
        .subscribe(
            "c",
            subscribe(VideoCodec::VP8, Some(Resolution::new(1920, 1080))),
        )
        .unwrap();
    assert_eq!(big.flavor.resolution, Resolution::new(256, 144));
    assert_eq!(
        video.status().encoders,
        1,
        "c moved back to the shared flavor"
    );
    video.unsubscribe("a");
    video.unsubscribe("b");
    assert_eq!(video.status().encoders, 1, "c still holds it");
    audio.disable_video();

    // exclude_self: every subscriber gets a private composite.
    let audio = audio_room("private");
    audio.add_participant("a", false).unwrap();
    audio.add_participant("b", false).unwrap();
    let video = audio.enable_video(
        VideoRoomSettings {
            exclude_self: true,
            ..settings(256, 72, 30)
        },
        &VideoBackend::raw(),
    );
    video.add_source("a", VideoCodec::VP8).unwrap();
    video.add_source("b", VideoCodec::VP8).unwrap();
    let mut sub_a = video
        .subscribe("a", subscribe(VideoCodec::VP8, None))
        .unwrap();
    let _sub_b = video
        .subscribe("b", subscribe(VideoCodec::VP8, None))
        .unwrap();
    let status = video.status();
    assert_eq!(status.encoders, 2);
    assert_eq!(status.outputs.len(), 2);
    assert!(status.outputs.iter().all(|o| o.exclude.is_some()));

    let mut cam_a = Camera::new(1, 128, 72);
    let mut cam_b = Camera::new(2, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..40 {
                for p in cam_a.frame(200) {
                    video.push_rtp("a", p);
                }
                for p in cam_b.frame(60) {
                    video.push_rtp("b", p);
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        })
    };
    // A sees only B: a single tile filling the canvas.
    let mut screen = Screen::new();
    let frame =
        wait_for_composite(&mut sub_a.packets, &mut screen, |f| f.luma(128, 36) == 60).await;
    assert_eq!(frame.luma(100, 36), 60, "b's tile spans the canvas");
    assert_ne!(
        frame.luma(30, 36),
        200,
        "a's own tile is not in a's picture"
    );
    feeder.await.unwrap();
    audio.disable_video();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss_raises_a_nack_then_a_pli_and_subscriber_feedback_is_answered() {
    let audio = audio_room("loss");
    audio.add_participant("a", false).unwrap();
    audio.add_participant("b", false).unwrap();
    let video = audio.enable_video(settings(256, 72, 30), &VideoBackend::raw());
    video.add_source("a", VideoCodec::VP8).unwrap();
    let mut sub = video
        .subscribe("b", subscribe(VideoCodec::VP8, None))
        .unwrap();
    video.add_source("b", VideoCodec::VP8).unwrap();

    let mut cam = Camera::new(7, 256, 144);
    // A frame this size spans many packets; drop one in the middle.
    let mut packets = cam.frame(100);
    assert!(packets.len() > 3);
    let dropped = packets.remove(2).header.sequence_number;
    let mut feedback = Vec::new();
    for p in packets {
        feedback.extend(video.push_rtp("a", p));
    }
    let nack = feedback
        .iter()
        .find_map(|p| match p {
            RtcpPacket::TransportFeedback(fb) => Some(fb.clone()),
            _ => None,
        })
        .expect("a NACK for the gap");
    let lost: Vec<u16> = match &nack.kind {
        forge_rtp::rtcp::TransportFeedback::Nack(entries) => {
            entries.iter().flat_map(|e| e.lost()).collect()
        }
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(lost, vec![dropped]);
    assert_eq!(nack.media_ssrc, 7);

    // The gap never fills: after the reorder window the frame is given
    // up on, and a PLI goes back (the first packet already spent the
    // start-up PLI, so wait out the gate).
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut feedback = Vec::new();
    for _ in 0..3 {
        for p in cam.frame(100) {
            feedback.extend(video.push_rtp("a", p));
        }
    }
    assert!(
        feedback
            .iter()
            .any(|p| matches!(p, RtcpPacket::PayloadFeedback(_))),
        "a PLI after the loss: {feedback:?}"
    );
    let info = video.participant("a").unwrap().source.unwrap();
    assert!(info.frames_lost >= 1);
    assert!(info.nacks_sent >= 1);
    assert!(info.plis_sent >= 1);

    // Subscriber side: the composite flows, and a NACK is answered from
    // the cache with the very packet asked for. Drain what queued up
    // while we were busy so the packet we ask for is a recent one.
    while sub.packets.try_recv().is_ok() {}
    let first = timeout(Duration::from_secs(5), sub.packets.recv())
        .await
        .unwrap()
        .unwrap();
    let first_pkt = RtpPacket::parse(first.clone()).unwrap();
    let first_ssrc = first_pkt.header.ssrc;
    assert_eq!(first_ssrc, sub.ssrc);
    let seq = first_pkt.header.sequence_number;
    let before = video.participant("b").unwrap().subscription.unwrap();
    video.handle_feedback(
        "b",
        &RtcpPacket::TransportFeedback(RtpFeedback::nack(99, sub.ssrc, &[seq])),
    );
    let after = video.participant("b").unwrap().subscription.unwrap();
    assert_eq!(after.nacks_received, before.nacks_received + 1);
    assert_eq!(
        after.packets_retransmitted,
        before.packets_retransmitted + 1
    );
    // The retransmission is queued behind whatever the clock produced
    // meanwhile; it is byte-identical to the original.
    let mut found = false;
    for _ in 0..2000 {
        let b = timeout(Duration::from_secs(5), sub.packets.recv())
            .await
            .unwrap()
            .unwrap();
        if b == first {
            found = true;
            break;
        }
    }
    assert!(found, "the NACKed packet was resent");

    // A PLI from the subscriber asks the shared encoder for a keyframe.
    video.handle_feedback(
        "b",
        &RtcpPacket::PayloadFeedback(PsFeedback::pli(99, sub.ssrc)),
    );
    let info = video.participant("b").unwrap().subscription.unwrap();
    assert_eq!(info.plis_received, 1);
    // REMB caps the encoder at the slowest receiver.
    video.handle_feedback(
        "b",
        &RtcpPacket::PayloadFeedback(PsFeedback::remb(99, 120_000, vec![sub.ssrc])),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let flavor = &video.status().outputs[0].flavors[0];
    assert_eq!(flavor.target_kbps, 120);
    // Above the flavor's own cap the cap wins.
    video.handle_feedback(
        "b",
        &RtcpPacket::PayloadFeedback(PsFeedback::remb(99, 5_000_000, vec![sub.ssrc])),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let flavor = &video.status().outputs[0].flavors[0];
    assert_eq!(flavor.target_kbps, flavor.flavor.max_kbps);
    audio.disable_video();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversize_frames_are_dropped_and_layout_controls_are_reported() {
    let audio = audio_room("limits");
    audio.add_participant("a", false).unwrap();
    audio.add_participant("b", false).unwrap();
    let video = audio.enable_video(
        VideoRoomSettings {
            limits: forge_conference::video::SourceLimits {
                max_resolution: Resolution::new(320, 180),
                ..Default::default()
            },
            ..settings(256, 72, 30)
        },
        &VideoBackend::raw(),
    );
    video.add_source("a", VideoCodec::VP8).unwrap();
    let _sub = video
        .subscribe("b", subscribe(VideoCodec::VP8, None))
        .unwrap();
    let mut cam = Camera::new(3, 640, 360);
    for _ in 0..5 {
        for p in cam.frame(100) {
            video.push_rtp("a", p);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let info = video.participant("a").unwrap();
    let src = info.source.unwrap();
    assert!(src.frames_dropped >= 5, "dropped {}", src.frames_dropped);
    assert_eq!(src.frames_decoded, 0);
    assert_eq!(
        info.state,
        VideoState::Lost,
        "negotiated but nothing usable"
    );

    let mut events = video.events();
    video.set_layout(Layout::ActiveSpeaker);
    video.pin(Some("b"));
    video.spotlight(Some("a"));
    assert_eq!(video.layout(), Layout::Spotlight);
    let st = video.status();
    assert_eq!(st.pinned.as_deref(), Some("b"));
    assert_eq!(st.spotlight.as_deref(), Some("a"));
    let mut layouts = 0;
    while let Ok(ev) = events.try_recv() {
        if let VideoRoomEvent::LayoutChanged { .. } = ev {
            layouts += 1;
        }
    }
    assert_eq!(layouts, 3);
    // The spotlit participant leaving clears the spotlight.
    audio.remove_participant("a").unwrap();
    assert_eq!(video.status().spotlight, None);
    video.set_participant_video_enabled("b", false);
    video.set_fps(5);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(video.status().target_fps, 5);
    audio.disable_video();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_loudest_speaker_takes_the_spotlight_from_the_audio_mixer() {
    let audio = audio_room("speaker");
    audio.add_participant("quiet", false).unwrap();
    audio.add_participant("loud", false).unwrap();
    let video = audio.enable_video(settings(128, 72, 30), &VideoBackend::raw());
    let _sub = video
        .subscribe("quiet", subscribe(VideoCodec::VP8, None))
        .unwrap();
    let mut events = video.events();
    // The mixer's VAD needs a few loud packets in a row.
    let loud: Vec<i16> = (0..160)
        .map(|i| if i % 2 == 0 { 8000 } else { -8000 })
        .collect();
    for _ in 0..5 {
        audio.write_audio("loud", &loud).unwrap();
        audio.write_audio("quiet", &[0; 160]).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(video.active_speaker().as_deref(), Some("loud"));
    assert!(video.participant("loud").unwrap().speaking);
    let mut got = None;
    while let Ok(ev) = events.try_recv() {
        if let VideoRoomEvent::ActiveSpeaker { participant_id } = ev {
            got = participant_id;
        }
    }
    assert_eq!(got.as_deref(), Some("loud"));
    audio.disable_video();
}
