//! CMAF segments for HLS: the writer's init segment once, then one segment
//! per fragment, each handed back as bytes.
//!
//! [`CmafSegmenter`] drives an [`Mp4Writer`] into memory. A segment is cut
//! only on a video keyframe at or past the target duration, so every segment
//! starts with a keyframe and plays on its own (`EXT-X-INDEPENDENT-SEGMENTS`).
//! An audio-only stream is cut at the target duration. Each segment is an
//! `styp` followed by the fragment's `moof` and `mdat`. A segment's
//! `tfdt` carries on from the last one's, so segments play back to back.
//! The init segment is the `ftyp` and a `moov` whose durations are zero, as
//! a live stream's are.

use crate::boxes::BoxBuf;
use crate::{FragmentLimits, Mp4Config, Mp4Writer, Result};
use std::collections::VecDeque;
use std::io::Cursor;

/// A segment ready to be written out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmafSegment {
    /// From 1, in order.
    pub sequence: u32,
    /// Where it starts, in milliseconds from the stream's start.
    pub start_ms: u64,
    pub duration_ms: u64,
    /// `styp` + `moof` + `mdat`.
    pub bytes: Vec<u8>,
}

/// Cuts a stream into CMAF segments.
pub struct CmafSegmenter {
    writer: Mp4Writer<Cursor<Vec<u8>>>,
    init: Option<Vec<u8>>,
    /// Where the open fragment began, for the segment's duration.
    open_start: Option<u64>,
    ready: VecDeque<CmafSegment>,
    sequence: u32,
}

/// The `styp` a CMAF media segment starts with.
fn styp() -> Vec<u8> {
    let mut b = BoxBuf::new(*b"styp");
    b.bytes(b"msdh");
    b.u32(0);
    b.bytes(b"msdh");
    b.bytes(b"msix");
    b.finish()
}

impl CmafSegmenter {
    /// A segmenter for `config`'s tracks, cutting at `target_ms` (2,000 for
    /// HLS). `config`'s own fragment limits are replaced.
    pub fn new(config: Mp4Config, target_ms: u64) -> Result<Self> {
        let target_ms = target_ms.max(1);
        let limits = if config.video.is_some() {
            // Only on a keyframe: a segment that starts mid-picture plays
            // nothing until the next one.
            FragmentLimits {
                min_ms: target_ms,
                max_ms: u64::MAX,
            }
        } else {
            FragmentLimits {
                min_ms: target_ms,
                max_ms: target_ms,
            }
        };
        let writer = Mp4Writer::new(Cursor::new(Vec::new()), config.fragments(limits))?;
        let mut s = Self {
            writer,
            init: None,
            open_start: None,
            ready: VecDeque::new(),
            sequence: 0,
        };
        s.collect(None);
        Ok(s)
    }

    /// One coded video frame at `ms`; the first must be a keyframe.
    pub fn write_video(&mut self, ms: u64, keyframe: bool, data: &[u8]) -> Result<()> {
        self.writer.write_video(ms, keyframe, data)?;
        self.collect(Some(ms));
        Ok(())
    }

    /// One Opus packet at `ms`.
    pub fn write_audio(&mut self, ms: u64, data: &[u8]) -> Result<()> {
        self.writer.write_audio(ms, data)?;
        self.collect(Some(ms));
        Ok(())
    }

    /// The init segment, once the first keyframe has made it (at once for
    /// audio alone).
    pub fn init_segment(&self) -> Option<&[u8]> {
        self.init.as_deref()
    }

    /// The next finished segment, oldest first.
    pub fn take_segment(&mut self) -> Option<CmafSegment> {
        self.ready.pop_front()
    }

    /// End the stream: the open fragment as the last segment.
    pub fn finish(&mut self) -> Result<Option<CmafSegment>> {
        let end = self.stream_end_ms();
        self.writer.flush_fragment()?;
        self.collect(end);
        Ok(self.ready.pop_back())
    }

    /// Where the stream has reached: its last sample's end.
    fn stream_end_ms(&self) -> Option<u64> {
        let video_ms = self
            .writer
            .config
            .video
            .as_ref()
            .map_or(66, |v| 1000 / u64::from(v.fps.max(1)));
        let audio_ms = self
            .writer
            .config
            .audio
            .as_ref()
            .map_or(20, |a| u64::from(a.frame_ms));
        let v = self.writer.last_video_ms.map(|ms| ms + video_ms);
        let a = self.writer.last_audio_ms.map(|ms| ms + audio_ms);
        v.max(a)
    }

    /// Take what the writer wrote since the last call: the init segment the
    /// first time, then each fragment it closed when `now_ms` opened the
    /// next.
    fn collect(&mut self, now_ms: Option<u64>) {
        let out = self.writer.out.get_mut();
        if !out.is_empty() {
            let bytes = std::mem::take(out);
            self.writer.out.set_position(0);
            if self.init.is_none() {
                self.init = Some(bytes);
            } else {
                let start = self.open_start.unwrap_or(0);
                let end = now_ms.unwrap_or(start).max(start);
                self.sequence += 1;
                let mut seg = styp();
                seg.extend_from_slice(&bytes);
                self.ready.push_back(CmafSegment {
                    sequence: self.sequence,
                    start_ms: start,
                    duration_ms: end - start,
                    bytes: seg,
                });
            }
        }
        self.open_start = self.writer.fragment.as_ref().map(|f| f.start_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioTrack, Mp4VideoCodec, VideoTrack};

    fn boxes(mut b: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let mut out = Vec::new();
        while b.len() >= 8 {
            let size = u32::from_be_bytes(b[..4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = b[4..8].try_into().unwrap();
            out.push((kind, b[8..size].to_vec()));
            b = &b[size..];
        }
        out
    }

    /// The video `tfdt` and the first sample's flags of a segment's `moof`.
    fn video_start(moof: &[u8]) -> (u64, bool) {
        for (kind, traf) in boxes(moof) {
            if &kind != b"traf" {
                continue;
            }
            let inner = boxes(&traf);
            let track = u32::from_be_bytes(inner[0].1[4..8].try_into().unwrap());
            if track != crate::VIDEO_TRACK {
                continue;
            }
            let tfdt = u64::from_be_bytes(inner[1].1[4..12].try_into().unwrap());
            // trun: version/flags, count, data offset, then duration, size,
            // flags for the first sample.
            let flags = u32::from_be_bytes(inner[2].1[20..24].try_into().unwrap());
            return (tfdt, flags == 0x0200_0000);
        }
        panic!("no video traf");
    }

    #[test]
    fn segments_start_on_keyframes_at_the_target_and_carry_on_from_each_other() {
        let config = Mp4Config::new()
            .video(VideoTrack::new(Mp4VideoCodec::H264, 320, 180).fps(15))
            .audio(AudioTrack::opus(48_000, 1));
        let mut s = CmafSegmenter::new(config, 2_000).unwrap();
        assert!(s.init_segment().is_none(), "the init waits for a keyframe");
        // 9 s of 15 fps with a keyframe every second; Opus every 20 ms.
        let mut audio_ms = 0;
        for f in 0..135u64 {
            let ms = f * 1000 / 15;
            let key = f % 15 == 0;
            let frame = if key {
                crate::tests::h264_keyframe()
            } else {
                vec![0, 0, 0, 1, 0x41, f as u8]
            };
            s.write_video(ms, key, &frame).unwrap();
            while audio_ms <= ms {
                s.write_audio(audio_ms, &[0xFC, 1, 2]).unwrap();
                audio_ms += 20;
            }
        }
        let init = s.init_segment().expect("an init segment").to_vec();
        let kinds: Vec<[u8; 4]> = boxes(&init).iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec![*b"ftyp", *b"moov"]);

        let mut segments = Vec::new();
        while let Some(seg) = s.take_segment() {
            segments.push(seg);
        }
        segments.extend(s.finish().unwrap());
        assert_eq!(
            segments.iter().map(|s| s.sequence).collect::<Vec<_>>(),
            (1..=segments.len() as u32).collect::<Vec<_>>()
        );
        // Keyframes each second, a 2 s target: cuts at 0, 2, 4, 6, 8 s.
        assert_eq!(
            segments.iter().map(|s| s.start_ms).collect::<Vec<_>>(),
            vec![0, 2000, 4000, 6000, 8000]
        );
        for seg in &segments[..4] {
            assert_eq!(seg.duration_ms, 2000, "{seg:?}");
        }
        assert!(
            segments[4].duration_ms >= 900,
            "{:?}",
            segments[4].duration_ms
        );
        for seg in &segments {
            let b = boxes(&seg.bytes);
            assert_eq!(
                b.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
                vec![*b"styp", *b"moof", *b"mdat"],
                "segment {}",
                seg.sequence
            );
            let (tfdt, key) = video_start(&b[1].1);
            assert_eq!(tfdt, seg.start_ms, "each carries on from the last");
            assert!(key, "segment {} starts with a keyframe", seg.sequence);
        }
        assert!(s.take_segment().is_none());

        // The init and the segments back to back are one fragmented file,
        // as a player joins them: every frame, both tracks.
        let mut whole = init;
        for seg in &segments {
            whole.extend_from_slice(&seg.bytes);
        }
        let summary = crate::read_summary(&whole).unwrap();
        assert_eq!(summary.fragments.len(), segments.len());
        assert_eq!(summary.video_samples(), 135);
        assert!(summary.track(crate::TrackKind::Audio).is_some());
    }

    #[test]
    fn audio_alone_is_cut_at_the_target() {
        let mut s =
            CmafSegmenter::new(Mp4Config::new().audio(AudioTrack::opus(48_000, 1)), 2_000).unwrap();
        assert!(
            s.init_segment().is_some(),
            "audio alone has its init at once"
        );
        for i in 0..300u64 {
            s.write_audio(i * 20, &[0xFC, 1]).unwrap();
        }
        let mut starts = Vec::new();
        while let Some(seg) = s.take_segment() {
            assert_eq!(seg.duration_ms, 2000);
            starts.push(seg.start_ms);
        }
        assert_eq!(starts, vec![0, 2000]);
        let last = s.finish().unwrap().expect("the open fragment");
        assert_eq!(last.start_ms, 4000);
        assert_eq!(last.duration_ms, 2000);
    }
}
