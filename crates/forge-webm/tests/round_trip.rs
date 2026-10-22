//! What the writer puts in a file is what a reader finds: tracks,
//! blocks, cluster boundaries, cues and the sizes patched in at the end.

use std::io::{self, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use forge_webm::{
    read_summary, AudioTrack, ClusterLimits, TrackKind, VideoTrack, WebmConfig, WebmError,
    WebmVideoCodec, WebmWriter, AUDIO_TRACK, VIDEO_TRACK,
};

/// A file the test can still read after the writer that owns it is
/// dropped — which is how the unfinished-recording case is checked.
#[derive(Clone, Default)]
struct SharedFile {
    data: Arc<Mutex<Vec<u8>>>,
    pos: u64,
}

impl SharedFile {
    fn bytes(&self) -> Vec<u8> {
        self.data.lock().unwrap().clone()
    }
}

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut data = self.data.lock().unwrap();
        let at = self.pos as usize;
        if data.len() < at + buf.len() {
            data.resize(at + buf.len(), 0);
        }
        data[at..at + buf.len()].copy_from_slice(buf);
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for SharedFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let len = self.data.lock().unwrap().len() as u64;
        self.pos = match from {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => (len as i64 + n) as u64,
            SeekFrom::Current(n) => (self.pos as i64 + n) as u64,
        };
        Ok(self.pos)
    }
}

fn frame(len: usize, fill: u8) -> Vec<u8> {
    vec![fill; len]
}

fn config() -> WebmConfig {
    WebmConfig::new()
        .video(VideoTrack::new(WebmVideoCodec::Vp8, 640, 360).fps(15))
        .audio(AudioTrack::opus(48_000, 1))
        .writing_app("fcp-conference test")
}

/// `seconds` of 15 fps video with a keyframe every 2 s, and 20 ms audio.
fn write_sample(w: &mut WebmWriter<SharedFile>, seconds: u64) {
    let mut audio_ms = 0u64;
    for i in 0..(seconds * 15) {
        let ms = i * 1000 / 15;
        let keyframe = i % 30 == 0;
        w.write_video(
            ms,
            keyframe,
            &frame(if keyframe { 900 } else { 200 }, i as u8),
        )
        .unwrap();
        while audio_ms <= ms {
            w.write_audio(audio_ms, &frame(80, 0xAA)).unwrap();
            audio_ms += 20;
        }
    }
}

/// A finished recording's bytes.
fn finished(seconds: u64, limits: Option<ClusterLimits>) -> Vec<u8> {
    let mut cfg = config();
    if let Some(l) = limits {
        cfg = cfg.max_cluster(l);
    }
    let file = SharedFile::default();
    let mut w = WebmWriter::new(file.clone(), cfg).unwrap();
    write_sample(&mut w, seconds);
    w.finish().unwrap();
    file.bytes()
}

#[test]
fn the_writer_counts_what_it_wrote() {
    let file = SharedFile::default();
    let mut w = WebmWriter::new(file.clone(), config()).unwrap();
    write_sample(&mut w, 4);
    let stats = w.finish().unwrap();

    assert_eq!(stats.video_frames, 60);
    assert_eq!(stats.video_keyframes, 2);
    assert_eq!(stats.audio_frames, 197);
    assert_eq!(stats.clusters, 2);
    assert!((3_900..4_000).contains(&stats.duration_ms), "{stats:?}");
    assert_eq!(stats.bytes, file.bytes().len() as u64);
}

#[test]
fn tracks_blocks_and_cues_come_back() {
    let data = finished(4, None);
    let s = read_summary(&data).unwrap();

    assert_eq!(s.doc_type, "webm");
    assert_eq!(s.timecode_scale_ns, 1_000_000);
    assert!(s.duration_ms >= 3_900.0, "{}", s.duration_ms);
    let segment_payload = s.segment_size.expect("the segment size was patched in");
    assert_eq!(
        segment_payload as usize + segment_header_len(&data),
        data.len()
    );

    let video = s.track(TrackKind::Video).expect("a video track");
    assert_eq!(video.number, VIDEO_TRACK);
    assert_eq!(video.codec_id, "V_VP8");
    assert_eq!((video.width, video.height), (640, 360));
    assert_eq!(video.default_duration_ns, 1_000_000_000 / 15);

    let audio = s.track(TrackKind::Audio).expect("an audio track");
    assert_eq!(audio.number, AUDIO_TRACK);
    assert_eq!(audio.codec_id, "A_OPUS");
    assert_eq!(audio.channels, 1);
    assert_eq!(audio.sample_rate, 48_000.0);
    assert_eq!(&audio.codec_private[..8], b"OpusHead");
    assert_eq!(audio.codec_private.len(), 19);
    assert_eq!(
        u16::from_le_bytes([audio.codec_private[10], audio.codec_private[11]]),
        3_840
    );
    assert_eq!(audio.codec_delay_ns, 3_840 * 1_000_000_000 / 48_000);
    assert_eq!(audio.seek_pre_roll_ns, 80_000_000);

    let video_blocks = s.blocks_of(VIDEO_TRACK);
    assert_eq!(video_blocks.len(), 60);
    assert_eq!(video_blocks.iter().filter(|b| b.keyframe).count(), 2);
    assert!(video_blocks[0].keyframe);
    assert_eq!(video_blocks[0].ms, 0);
    assert_eq!(video_blocks[0].bytes, 900);
    assert_eq!(video_blocks[15].ms, 1_000);
    let mut last = i64::MIN;
    for b in &video_blocks {
        assert!(b.ms >= last, "{b:?} went backwards");
        last = b.ms;
    }

    let audio_blocks = s.blocks_of(AUDIO_TRACK);
    assert_eq!(audio_blocks.len(), 197);
    assert!(
        audio_blocks.iter().all(|b| b.keyframe),
        "audio blocks are keyframes"
    );
    assert_eq!(audio_blocks[0].ms, 0);
    assert_eq!(audio_blocks[1].ms, 20);

    assert_eq!(s.cues.len(), 2);
    assert!(s.cues.iter().all(|c| c.points_at_cluster), "{:?}", s.cues);
    assert!(s.cues.iter().all(|c| c.track == VIDEO_TRACK));
    assert_eq!(s.cues[0].time_ms, 0);
    assert_eq!(s.cues[1].time_ms, 2_000);

    // The SeekHead names Info, Tracks and Cues, each inside the file.
    let ids: Vec<u32> = s.seek_head.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&0x1549_A966), "info: {ids:x?}");
    assert!(ids.contains(&0x1654_AE6B), "tracks: {ids:x?}");
    assert!(ids.contains(&0x1C53_BB6B), "cues: {ids:x?}");
    for (_, position) in &s.seek_head {
        assert!(*position > 0 && (*position as usize) < data.len());
    }
}

/// Everything before the Segment's payload.
fn segment_header_len(data: &[u8]) -> usize {
    let pos = data[..data.len().min(128)]
        .windows(4)
        .position(|w| w == [0x18, 0x53, 0x80, 0x67])
        .expect("a segment near the start");
    pos + 4 + 8
}

#[test]
fn clusters_close_on_keyframes_and_at_the_limits() {
    // A keyframe every 2 s against a 1 s minimum: one cluster each.
    let s = read_summary(&finished(6, None)).unwrap();
    assert_eq!(s.clusters, 3, "one cluster per keyframe");
    assert_eq!(s.cues.len(), 3);

    // A short maximum closes clusters between keyframes too.
    let s = read_summary(&finished(
        6,
        Some(ClusterLimits {
            min_ms: 1_000,
            max_ms: 1_000,
            max_bytes: 4 * 1024 * 1024,
        }),
    ))
    .unwrap();
    assert!(s.clusters >= 6, "{} clusters", s.clusters);
    let video = s.blocks_of(VIDEO_TRACK);
    assert_eq!(video.len(), 90);
    assert_eq!(video[45].ms, 3_000);

    // And so does a byte cap.
    let s = read_summary(&finished(
        4,
        Some(ClusterLimits {
            min_ms: 1_000,
            max_ms: 60_000,
            max_bytes: 4_096,
        }),
    ))
    .unwrap();
    assert!(s.clusters > 3, "{} clusters", s.clusters);
    assert_eq!(s.blocks_of(VIDEO_TRACK).len(), 60);
}

#[test]
fn a_recording_of_one_kind_carries_only_that_track() {
    let file = SharedFile::default();
    let mut w = WebmWriter::new(
        file.clone(),
        WebmConfig::new().audio(AudioTrack::opus(48_000, 2)),
    )
    .unwrap();
    assert!(matches!(
        w.write_video(0, true, &frame(10, 1)),
        Err(WebmError::NoTrack("video"))
    ));
    for i in 0..50u64 {
        w.write_audio(i * 20, &frame(60, 7)).unwrap();
    }
    w.finish().unwrap();
    let s = read_summary(&file.bytes()).unwrap();
    assert_eq!(s.tracks.len(), 1);
    assert_eq!(s.track(TrackKind::Audio).unwrap().channels, 2);
    assert!(s.track(TrackKind::Video).is_none());
    assert_eq!(s.blocks_of(AUDIO_TRACK).len(), 50);
    // No video keyframe, so no cue; the file is well formed regardless.
    assert!(s.cues.is_empty());
    assert_eq!(s.clusters, 1);

    let file = SharedFile::default();
    let mut w = WebmWriter::new(
        file.clone(),
        WebmConfig::new().video(VideoTrack::new(WebmVideoCodec::Vp9, 320, 180)),
    )
    .unwrap();
    assert!(matches!(
        w.write_audio(0, &frame(10, 1)),
        Err(WebmError::NoTrack("audio"))
    ));
    w.write_video(0, true, &frame(100, 3)).unwrap();
    w.finish().unwrap();
    let s = read_summary(&file.bytes()).unwrap();
    let video = s.track(TrackKind::Video).unwrap();
    assert_eq!(video.codec_id, "V_VP9");
    assert_eq!(video.default_duration_ns, 0, "no nominal frame rate");
}

#[test]
fn a_recording_left_unfinished_still_reads() {
    let file = SharedFile::default();
    let mut w = WebmWriter::new(file.clone(), config()).unwrap();
    write_sample(&mut w, 6);
    drop(w); // no finish: as a crash would leave it

    let s = read_summary(&file.bytes()).unwrap();
    assert_eq!(s.doc_type, "webm");
    assert!(
        s.segment_size.is_none(),
        "the segment is still unknown-size"
    );
    assert_eq!(s.duration_ms, 0.0, "the duration was never patched in");
    assert_eq!(s.tracks.len(), 2);
    // Every block written is there, the last cluster included.
    assert_eq!(s.blocks_of(VIDEO_TRACK).len(), 90);
    assert!(s.cues.is_empty(), "cues are written on finish");
}

#[test]
fn a_frame_far_behind_the_cluster_is_refused() {
    let file = SharedFile::default();
    let mut w = WebmWriter::new(file, config()).unwrap();
    w.write_video(60_000, true, &frame(10, 1)).unwrap();
    // Behind the cluster but within a 16-bit relative timestamp.
    w.write_audio(59_000, &frame(10, 1)).unwrap();
    // Too far behind to express, and not ahead enough to open a new one.
    let err = w.write_audio(1_000, &frame(10, 1)).unwrap_err();
    assert!(matches!(err, WebmError::Backwards { .. }), "{err:?}");
}
