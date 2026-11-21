//! The publisher against a real RTMP server: ffmpeg listening
//! (`ffmpeg -listen 1 -i rtmp://…`) and copying what it is sent to a file,
//! which ffprobe then reads. The video is x264's (baseline, an access unit
//! delimiter per picture), the audio this workspace's AAC encoder.
//!
//! Skipped unless `FORGE_RTMP_FFMPEG` is set and `ffmpeg` and `ffprobe`
//! are on the path.

use forge_codecs::aac::{AacConfig, AacEncoder};
use forge_rtmp::flv::{aac_frame, aac_sequence_header, Metadata};
use forge_rtmp::{AvcTagger, Publisher, RtmpUrl};
use std::process::Command;
use std::time::Duration;

fn access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    // Cut before each access unit delimiter (NAL type 9).
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 < stream.len() {
        if stream[i..i + 4] == [0, 0, 0, 1] && stream[i + 4] & 0x1F == 9 {
            starts.push(i);
        }
        i += 1;
    }
    starts.push(stream.len());
    starts
        .windows(2)
        .map(|w| stream[w[0]..w[1]].to_vec())
        .collect()
}

fn is_keyframe(au: &[u8]) -> bool {
    au.windows(5)
        .any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1F == 5)
        || au
            .windows(4)
            .any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1F == 5)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ffmpeg_takes_the_stream_as_h264_and_aac() {
    if std::env::var_os("FORGE_RTMP_FFMPEG").is_none() {
        eprintln!("FORGE_RTMP_FFMPEG not set; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("forge-rtmp-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("in.h264");
    let made = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=15",
            "-t",
            "4",
        ])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-g",
            "30",
            "-x264-params",
            "aud=1",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "h264",
        ])
        .arg(&h264)
        .status()
        .unwrap();
    assert!(made.success());
    let units = access_units(&std::fs::read(&h264).unwrap());
    assert!(units.len() >= 59, "{} pictures", units.len());

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let out = dir.join("out.flv");
    let mut server = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-listen",
            "1",
            "-i",
        ])
        .arg(format!("rtmp://127.0.0.1:{port}/live/key"))
        .args(["-c", "copy"])
        .arg(&out)
        .spawn()
        .unwrap();
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live")).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let p = loop {
        match Publisher::connect(&url, "key").await {
            Ok(p) => break p,
            Err(e) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "ffmpeg never listened: {e}"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let mut aac = AacEncoder::new(AacConfig::default()).unwrap();
    p.send_metadata(&Metadata {
        width: Some(640),
        height: Some(360),
        framerate: Some(15.0),
        audio: Some((48_000, 1)),
        encoder: "forge-rtmp test".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    p.send_audio(0, aac_sequence_header(aac.audio_specific_config()))
        .await
        .unwrap();
    let mut tagger = AvcTagger::new();
    let mut audio_ms = 0u64;
    let tone: Vec<i16> = (0..48_000 * 4)
        .map(|i| ((i as f64 * 2.0 * std::f64::consts::PI * 440.0 / 48_000.0).sin() * 8000.0) as i16)
        .collect();
    let mut samples = tone.chunks(960);
    for (n, au) in units.iter().enumerate() {
        let video_ms = (n as u64 * 1000) / 15;
        for body in tagger.bodies(is_keyframe(au), au).unwrap() {
            p.send_video(video_ms as u32, body).await.unwrap();
        }
        // Audio kept level with the video.
        while audio_ms <= video_ms {
            let Some(chunk) = samples.next() else { break };
            for unit in aac.encode(chunk).unwrap() {
                p.send_audio(audio_ms as u32, aac_frame(&unit))
                    .await
                    .unwrap();
                audio_ms += 1024 * 1000 / 48_000;
            }
        }
    }
    p.close().await;
    let ended = tokio::task::spawn_blocking(move || server.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(ended.success(), "ffmpeg took the stream");

    let probe = Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-show_entries",
            "stream=codec_name,width,height,sample_rate,channels:format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout).to_string();
    eprintln!("{text}");
    assert!(text.contains("codec_name=h264"), "{text}");
    assert!(
        text.contains("width=640") && text.contains("height=360"),
        "{text}"
    );
    assert!(text.contains("codec_name=aac"), "{text}");
    assert!(
        text.contains("sample_rate=48000") && text.contains("channels=1"),
        "{text}"
    );
    let duration: f64 = text
        .lines()
        .find_map(|l| l.strip_prefix("duration="))
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    assert!(
        (3.5..4.6).contains(&duration),
        "about four seconds: {duration}"
    );
    // Every picture decodes.
    let decoded = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&out)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        decoded.status.success() && decoded.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
