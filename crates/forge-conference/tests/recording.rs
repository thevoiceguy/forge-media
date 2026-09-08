//! Recording a room (design §11): the composite as coded frames and the
//! room's mix as audio frames, both from a live room, neither of them a
//! participant.

use std::sync::Arc;
use std::time::Duration;

use forge_conference::video::{RecordRequest, SubscribeRequest, VideoBackend, VideoRoomSettings};
use forge_conference::{AudioFormat, ConferenceRoom};
use forge_core::VideoCodec;
use forge_rtp::video::payload::packetize;
use forge_rtp::RtpPacket;
use forge_video::codec::{EncoderSettings, VideoEncoder};
use forge_video::frame::{HostFrame, Resolution, VideoFrame};
use forge_video::layout::Layout;
use forge_video::raw::raw_registry;
use forge_video::MediaDevice;
use tokio::time::timeout;

/// A participant's camera, as in the video-room test.
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

fn audio_room(id: &str, frame_clock: bool) -> Arc<ConferenceRoom> {
    Arc::new(
        ConferenceRoom::new(
            id,
            AudioFormat::pcm_mono(),
            480,
            forge_mixer::MixerOptions {
                frame_clock,
                ..Default::default()
            },
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
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_takes_the_composite_without_joining_the_room() {
    let audio = audio_room("rec", false);
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(settings(256, 72, 30), &VideoBackend::raw());
    video.add_source("alice", VideoCodec::VP8).unwrap();

    let mut recording = video
        .record("rec-1", RecordRequest::new(VideoCodec::VP8))
        .unwrap();
    assert_eq!(recording.flavor.codec, VideoCodec::VP8);
    assert_eq!(recording.flavor.resolution, Resolution::new(256, 72));
    assert_eq!(recording.flavor.fps, 30, "the room's rate by default");
    assert!(video.is_recording("rec-1"));

    // The recording is not a participant and draws no tile of its own.
    let ids: Vec<String> = video
        .participants()
        .into_iter()
        .map(|p| p.participant_id)
        .collect();
    assert_eq!(ids, vec!["alice".to_string(), "bob".to_string()]);

    let mut alice = Camera::new(0xA11CE, 128, 72);
    let feeder = {
        let video = Arc::clone(&video);
        tokio::spawn(async move {
            for _ in 0..60 {
                for p in alice.frame(200) {
                    video.push_rtp("alice", p);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };

    // The first frame a recording gets is a keyframe, so a file can start
    // with it.
    let first = timeout(Duration::from_secs(5), recording.frames.recv())
        .await
        .expect("frames arrive")
        .expect("the recording is open");
    assert!(first.frame.keyframe, "a recording starts on a keyframe");

    // The frames decode to the room's canvas, with Alice's luma in them.
    let mut decoder = raw_registry()
        .decoder(VideoCodec::VP8, &MediaDevice::Host)
        .unwrap();
    let canvas = decoder
        .decode(&first.frame)
        .unwrap()
        .and_then(|f| f.into_host())
        .expect("the composite decodes");
    assert_eq!((canvas.width, canvas.height), (256, 72));

    // Timestamps run on the room's 90 kHz clock, and each frame says when
    // its canvas was composed, so audio can be lined up with it.
    let mut frames = vec![first];
    while frames.len() < 5 {
        let f = timeout(Duration::from_secs(5), recording.frames.recv())
            .await
            .expect("frames keep coming")
            .expect("the recording is open");
        frames.push(f);
    }
    for pair in frames.windows(2) {
        assert!(
            pair[1].frame.timestamp > pair[0].frame.timestamp,
            "timestamps advance: {} then {}",
            pair[0].frame.timestamp,
            pair[1].frame.timestamp
        );
        assert!(pair[1].at >= pair[0].at);
    }
    let elapsed = frames[4].at.duration_since(video.started_at());
    let by_timestamp = Duration::from_secs_f64(frames[4].frame.timestamp as f64 / 90_000.0);
    assert!(
        elapsed.abs_diff(by_timestamp) < Duration::from_millis(200),
        "the 90 kHz timestamps track the room clock: {elapsed:?} vs {by_timestamp:?}"
    );

    let status = video.status();
    assert_eq!(status.recordings.len(), 1);
    assert_eq!(status.recordings[0].id, "rec-1");
    assert!(status.recordings[0].frames_sent >= 5);
    assert_eq!(status.recordings[0].frames_dropped, 0);

    feeder.abort();
    video.stop_record("rec-1");
    assert!(!video.is_recording("rec-1"));
    assert!(video.status().recordings.is_empty());
    // With nothing else watching, the encoder went with it.
    assert_eq!(video.status().encoders, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_shares_a_subscriber_s_encoder() {
    let audio = audio_room("share", false);
    audio.add_participant("alice", true).unwrap();
    let video = audio.enable_video(settings(128, 72, 15), &VideoBackend::raw());
    video.add_source("alice", VideoCodec::VP8).unwrap();

    let _sub = video
        .subscribe("alice", subscribe(VideoCodec::VP8))
        .unwrap();
    assert_eq!(video.status().encoders, 1);

    // The same codec, resolution and rate: one encoder serves both.
    let recording = video
        .record("rec-1", RecordRequest::new(VideoCodec::VP8))
        .unwrap();
    assert_eq!(video.status().encoders, 1, "the flavor is shared");
    assert_eq!(recording.flavor.resolution, Resolution::new(128, 72));
    let outputs = video.status().outputs;
    assert_eq!(outputs.len(), 1, "one composite, watched by both");
    assert_eq!(outputs[0].exclude, None);

    // A smaller recording needs its own output and encoder.
    let small = video
        .record(
            "rec-2",
            RecordRequest {
                resolution: Some(Resolution::new(64, 36)),
                ..RecordRequest::new(VideoCodec::VP8)
            },
        )
        .unwrap();
    assert_eq!(small.flavor.resolution, Resolution::new(64, 36));
    assert_eq!(video.status().encoders, 2);
    assert_eq!(video.status().outputs.len(), 2);

    video.stop_record("rec-2");
    assert_eq!(video.status().encoders, 1);
    video.stop_record("rec-1");
    assert_eq!(
        video.status().encoders,
        1,
        "the subscriber still wants the shared one"
    );
    video.unsubscribe("alice");
    assert_eq!(video.status().encoders, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_sees_a_composite_nobody_is_left_out_of() {
    // `exclude_self` gives every participant a private composite; a
    // recording still gets the whole room.
    let audio = audio_room("exclude", false);
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();
    let video = audio.enable_video(
        VideoRoomSettings {
            exclude_self: true,
            ..settings(128, 72, 15)
        },
        &VideoBackend::raw(),
    );
    let _alice = video
        .subscribe("alice", subscribe(VideoCodec::VP8))
        .unwrap();
    let _bob = video.subscribe("bob", subscribe(VideoCodec::VP8)).unwrap();
    let _rec = video
        .record("rec-1", RecordRequest::new(VideoCodec::VP8))
        .unwrap();

    let outputs = video.status().outputs;
    assert_eq!(
        outputs.len(),
        3,
        "one per subscriber, one for the recording"
    );
    let excludes: Vec<Option<String>> = outputs.iter().map(|o| o.exclude.clone()).collect();
    assert!(
        excludes.contains(&None),
        "the recording's leaves nobody out"
    );
    assert!(excludes.contains(&Some("alice".to_string())));
    assert!(excludes.contains(&Some("bob".to_string())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mix_tap_carries_what_the_room_hears() {
    let audio = audio_room("mix", false);
    assert!(!audio.mix_tapped());
    audio.add_participant("alice", true).unwrap();
    audio.add_participant("bob", false).unwrap();

    let mut tap = audio.tap_mix();
    assert!(audio.mix_tapped());

    audio.write_audio("alice", &vec![1_000i16; 480]).unwrap();
    audio.write_audio("bob", &vec![500i16; 480]).unwrap();
    let mixed = audio.mix().unwrap().expect("something to mix");

    let frame = timeout(Duration::from_secs(1), tap.recv())
        .await
        .expect("the tap gets the frame")
        .expect("the tap is open");
    assert_eq!(frame.sample_rate, 48_000);
    assert_eq!(frame.channels, 1);
    assert_eq!(frame.samples.len(), 480);
    assert_eq!(*frame.samples, mixed, "the tap hears exactly the mix");
    assert!(
        frame.samples.iter().any(|s| *s != 0),
        "and it is not silence"
    );

    drop(tap);
    assert!(!audio.mix_tapped(), "the last listener left");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_frame_clock_feeds_the_mix_tap() {
    // On a frame clock nothing is drained except by `advance_frame`,
    // which must feed a listener even when nothing is being recorded.
    let audio = audio_room("clock", true);
    audio.add_participant("alice", true).unwrap();
    let mut tap = audio.tap_mix();

    for _ in 0..3 {
        audio.write_audio("alice", &vec![2_000i16; 480]).unwrap();
        audio.advance_frame();
    }

    let mut frames = 0;
    while let Ok(Ok(frame)) = timeout(Duration::from_millis(200), tap.recv()).await {
        assert_eq!(frame.samples.len(), 480);
        frames += 1;
    }
    assert!(
        frames >= 3,
        "every advanced frame reached the tap: {frames}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_sink_stops_the_recording() {
    // A recorder that goes away without saying so — its task panicked,
    // or it was simply dropped — must not leave the room encoding a
    // composite nobody reads.
    let audio = audio_room("dropped", false);
    audio.add_participant("alice", true).unwrap();
    let video = audio.enable_video(settings(128, 72, 15), &VideoBackend::raw());
    video.add_source("alice", VideoCodec::VP8).unwrap();

    let sink = video
        .record("rec-1", RecordRequest::new(VideoCodec::VP8))
        .unwrap();
    assert!(video.is_recording("rec-1"));
    assert_eq!(video.status().encoders, 1);

    drop(sink);

    // The room notices at its next composite, so feed it one.
    let mut alice = Camera::new(0xA11CE, 128, 72);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while video.is_recording("rec-1") && tokio::time::Instant::now() < deadline {
        for p in alice.frame(90) {
            video.push_rtp("alice", p);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !video.is_recording("rec-1"),
        "the room let the recording go"
    );
    assert!(video.status().recordings.is_empty());
    assert_eq!(video.status().encoders, 0, "and the encoder with it");
}
