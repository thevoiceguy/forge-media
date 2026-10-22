//! Seed corpus for the `mp4_read` fuzz target (see `fuzz/README.md`).
//!
//! The reader's job is to make sense of files this crate's writer produced,
//! so the writer is where the corpus comes from: a real fragmented MP4, with
//! its box nesting, its fragments and its access table, is a far better
//! starting point than random bytes, and it cannot drift out of date the
//! way a checked-in blob would.
//!
//! Nothing is written unless `FORGE_FUZZ_SEED_DIR` names a directory, so a
//! normal `cargo test` run only checks that the corpus can still be built.
//! The nightly fuzzing job sets it.

use forge_mp4::{
    read_summary, AudioTrack, FragmentLimits, Mp4Config, Mp4VideoCodec, Mp4Writer, VideoTrack,
};
use std::io::Cursor;
use std::path::PathBuf;

fn write_seed(name: &str, bytes: &[u8]) {
    let Some(root) = std::env::var_os("FORGE_FUZZ_SEED_DIR").map(PathBuf::from) else {
        return;
    };
    let dir = root.join("mp4_read");
    std::fs::create_dir_all(&dir).expect("create seed directory");
    std::fs::write(dir.join(name), bytes).expect("write seed");
}

fn h264_keyframe() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x02]);
    v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80]);
    v.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33]);
    v
}

/// A recording of `frames` frames with a keyframe every eight, fragments
/// short enough that `frames` of them make several, audio alongside when
/// asked.
fn recording(codec: Mp4VideoCodec, frames: u32, with_audio: bool, finish: bool) -> Vec<u8> {
    let mut config = Mp4Config::new()
        .video(VideoTrack::new(codec, 640, 360).fps(15))
        .fragments(FragmentLimits {
            min_ms: 200,
            max_ms: 2000,
        });
    if with_audio {
        config = config.audio(AudioTrack::opus(48_000, 2));
    }
    let mut out = Cursor::new(Vec::new());
    let mut writer = Mp4Writer::new(&mut out, config).expect("open writer");
    for i in 0..frames {
        let ms = u64::from(i) * 66;
        let keyframe = i % 8 == 0;
        let payload = match (codec, keyframe) {
            (Mp4VideoCodec::Av1, true) => {
                let mut tu = vec![0x0A, 5, 0, 0, 0, 0b0100_0000, 0];
                tu.extend_from_slice(&[0x32, 3, 0xAA, 0xBB, i as u8]);
                tu
            }
            (Mp4VideoCodec::Av1, false) => vec![0x32, 2, 0xDD, i as u8],
            (Mp4VideoCodec::Hevc, true) => {
                let mut vps = vec![0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF];
                vps.extend_from_slice(&[0x01, 0x60, 0, 0, 0x03, 0, 0, 0, 0, 0, 0, 0x5D]);
                let mut key = Vec::new();
                for n in [
                    &vps[..],
                    &[0x42, 0x01, 0x01],
                    &[0x44, 0x01, 0xC1],
                    &[0x26, 0x01, i as u8],
                ] {
                    key.extend_from_slice(&[0, 0, 0, 1]);
                    key.extend_from_slice(n);
                }
                key
            }
            (Mp4VideoCodec::Hevc, false) => vec![0, 0, 0, 1, 0x02, 0x01, i as u8],
            (Mp4VideoCodec::H264, true) => h264_keyframe(),
            (Mp4VideoCodec::H264, false) => vec![0, 0, 0, 1, 0x41, 0x9A, i as u8, i as u8],
        };
        writer
            .write_video(ms, keyframe, &payload)
            .expect("write video frame");
        if with_audio {
            let audio: Vec<u8> = (0..40u32).map(|n| ((n * 3 + i) % 251) as u8).collect();
            writer.write_audio(ms, &audio).expect("write audio frame");
        }
    }
    if finish {
        writer.finish().expect("finish recording");
    } else {
        drop(writer);
    }
    out.into_inner()
}

#[test]
fn seeds_are_valid_recordings_and_written_when_asked() {
    let seeds: [(&str, Vec<u8>); 6] = [
        (
            "h264_audio.mp4",
            recording(Mp4VideoCodec::H264, 40, true, true),
        ),
        (
            "h264_video_only.mp4",
            recording(Mp4VideoCodec::H264, 24, false, true),
        ),
        (
            "h264_unfinished.mp4",
            recording(Mp4VideoCodec::H264, 40, true, false),
        ),
        ("hevc.mp4", recording(Mp4VideoCodec::Hevc, 24, true, true)),
        ("av1.mp4", recording(Mp4VideoCodec::Av1, 24, true, true)),
        ("short.mp4", recording(Mp4VideoCodec::H264, 1, true, true)),
    ];
    for (name, bytes) in &seeds {
        let summary = read_summary(bytes).expect("a seed reads back");
        assert!(!summary.tracks.is_empty(), "{name}");
        assert!(!summary.fragments.is_empty(), "{name}");
        write_seed(name, bytes);
    }
    // A truncated seed too, so the corpus starts where crashes live.
    let cut = &seeds[0].1[..seeds[0].1.len() * 2 / 3];
    assert!(read_summary(cut).is_ok());
    write_seed("truncated.mp4", cut);
}
