//! Seed corpus for the `webm_read_summary` fuzz target (see `fuzz/README.md`).
//!
//! The reader's job is to make sense of files this crate's writer produced,
//! so the writer is where the corpus comes from: a real Matroska file, with
//! its EBML nesting, its cluster boundaries and its cue points, is a far
//! better starting point than random bytes, and it cannot drift out of date
//! the way a checked-in blob would.
//!
//! Nothing is written unless `FORGE_FUZZ_SEED_DIR` names a directory, so a
//! normal `cargo test` run only checks that the corpus can still be built.
//! The weekly fuzzing job sets it.

use forge_webm::{
    read_summary, AudioTrack, ClusterLimits, VideoTrack, WebmConfig, WebmVideoCodec, WebmWriter,
};
use std::io::Cursor;
use std::path::PathBuf;

fn write_seed(name: &str, bytes: &[u8]) {
    let Some(root) = std::env::var_os("FORGE_FUZZ_SEED_DIR").map(PathBuf::from) else {
        return;
    };
    let dir = root.join("webm_read_summary");
    std::fs::create_dir_all(&dir).expect("create seed directory");
    std::fs::write(dir.join(name), bytes).expect("write seed");
}

/// A recording of `frames` video frames with audio alongside, at a cluster
/// limit low enough that `frames` of them produce several clusters.
fn recording(codec: WebmVideoCodec, frames: u32, with_audio: bool) -> Vec<u8> {
    let mut config = WebmConfig::new()
        .video(VideoTrack::new(codec, 640, 360).fps(15))
        .max_cluster(ClusterLimits {
            max_ms: 200,
            ..Default::default()
        });
    if with_audio {
        config = config.audio(AudioTrack::opus(48_000, 2));
    }

    let mut out = Cursor::new(Vec::new());
    let mut writer = WebmWriter::new(&mut out, config).expect("open writer");
    for i in 0..frames {
        let ms = u64::from(i) * 66;
        // A keyframe every eight frames, so the cues have something to point
        // at and the reader's cluster walk has more than one entry.
        let keyframe = i % 8 == 0;
        let payload: Vec<u8> = (0..64u32).map(|n| ((n + i) % 251) as u8).collect();
        writer
            .write_video(ms, keyframe, &payload)
            .expect("write video frame");
        if with_audio {
            let audio: Vec<u8> = (0..40u32).map(|n| ((n * 3 + i) % 251) as u8).collect();
            writer.write_audio(ms, &audio).expect("write audio frame");
        }
    }
    writer.finish().expect("finish recording");
    out.into_inner()
}

#[test]
fn webm_seeds() {
    let cases = [
        ("vp8_av", recording(WebmVideoCodec::Vp8, 40, true)),
        ("vp9_av", recording(WebmVideoCodec::Vp9, 40, true)),
        ("vp8_video_only", recording(WebmVideoCodec::Vp8, 12, false)),
    ];

    for (name, bytes) in &cases {
        // A corpus entry the reader rejects would start the fuzzer at the
        // first branch instead of inside the parser.
        read_summary(bytes).unwrap_or_else(|e| panic!("seed {name} reads back: {e}"));
        write_seed(name, bytes);
    }

    // Truncation is the failure this reader actually meets in the field — a
    // recording whose node died mid-write — so the corpus carries a few, cut
    // at points that land inside the EBML nesting rather than between
    // elements.
    let (_, full) = &cases[0];
    for cut in [full.len() / 4, full.len() / 2, full.len() - 1] {
        let truncated = &full[..cut];
        // No expectation of success: the point is that it must not panic.
        let _ = read_summary(truncated);
        write_seed(&format!("vp8_av_truncated_{cut}"), truncated);
    }
}
