//! A Matroska (WebM) writer for conference recordings.
//!
//! A conference recording is one composite video track and the room's
//! mixed audio, both on one timebase, in a file every browser and player
//! opens. That is a small, well-specified corner of Matroska — an EBML
//! header, a Segment holding Info, Tracks, Clusters of SimpleBlocks and
//! Cues — so it is written here rather than pulled in, as the Ogg/Opus
//! writer beside it in `forge-recorder` is.
//!
//! What it writes: **V_VP8** or **V_VP9** video and **A_OPUS** audio,
//! timestamps in milliseconds (a 1 ms `TimecodeScale`), one Cluster per
//! keyframe (bounded by [`WebmConfig::max_cluster`]), a Cue per Cluster
//! that starts on a keyframe, and, on [`WebmWriter::finish`], the
//! Segment's size, the Info duration and the SeekHead positions patched
//! in. A file left unfinished by a crash still plays: the Segment's size
//! is written up front as the largest 8-byte value, which readers treat
//! as "to the end of the file", and Clusters are flushed as they close.
//!
//! What it does not do: lacing, BlockGroups, subtitles, chapters, tags,
//! or a live (non-seekable) stream. The writer needs `Seek`.
//!
//! ```no_run
//! use forge_webm::{AudioTrack, VideoTrack, WebmConfig, WebmVideoCodec, WebmWriter};
//! # fn main() -> Result<(), forge_webm::WebmError> {
//! let file = std::fs::File::create("room.webm")?;
//! let mut w = WebmWriter::new(
//!     file,
//!     WebmConfig::new()
//!         .video(VideoTrack::new(WebmVideoCodec::Vp8, 1280, 720).fps(15))
//!         .audio(AudioTrack::opus(48_000, 1)),
//! )?;
//! w.write_video(0, true, &[/* coded frame */])?;
//! w.write_audio(0, &[/* opus packet */])?;
//! let summary = w.finish()?;
//! println!("{} ms, {} bytes", summary.duration_ms, summary.bytes);
//! # Ok(()) }
//! ```

mod ebml;
pub mod read;

use std::io::{self, Seek, SeekFrom, Write};

use ebml::id;
pub use read::{read_summary, BlockInfo, TrackInfo, TrackKind, WebmSummary};

/// The video track's number; audio is [`AUDIO_TRACK`].
pub const VIDEO_TRACK: u64 = 1;
/// The audio track's number.
pub const AUDIO_TRACK: u64 = 2;

/// Timestamps are milliseconds: one `TimecodeScale` of 1 000 000 ns.
const TIMECODE_SCALE_NS: u64 = 1_000_000;

/// Room for the SeekHead's entries, patched on finish.
const SEEK_HEAD_RESERVE: usize = 96;

/// What went wrong writing a recording.
#[derive(Debug, thiserror::Error)]
pub enum WebmError {
    #[error("webm i/o: {0}")]
    Io(#[from] io::Error),
    /// A frame's timestamp went backwards past what a Cluster can hold.
    #[error("frame timestamp {ms} ms is before the current cluster at {cluster_ms} ms")]
    Backwards { ms: u64, cluster_ms: u64 },
    /// A track was written to that the file does not have.
    #[error("this recording has no {0} track")]
    NoTrack(&'static str),
}

type Result<T> = std::result::Result<T, WebmError>;

/// The video codecs a recording can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebmVideoCodec {
    Vp8,
    Vp9,
}

impl WebmVideoCodec {
    /// The Matroska `CodecID`.
    pub fn codec_id(&self) -> &'static str {
        match self {
            WebmVideoCodec::Vp8 => "V_VP8",
            WebmVideoCodec::Vp9 => "V_VP9",
        }
    }
}

/// The composite video track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrack {
    pub codec: WebmVideoCodec,
    pub width: u32,
    pub height: u32,
    /// Nominal frame rate, written as `DefaultDuration`; `None` leaves it
    /// out, which is right for a variable rate.
    pub fps: Option<u32>,
}

impl VideoTrack {
    pub fn new(codec: WebmVideoCodec, width: u32, height: u32) -> Self {
        Self {
            codec,
            width,
            height,
            fps: None,
        }
    }

    pub fn fps(mut self, fps: u32) -> Self {
        self.fps = (fps > 0).then_some(fps);
        self
    }
}

/// The mixed-audio track. Opus is the only codec WebM carries that this
/// writer produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub sample_rate: u32,
    pub channels: u8,
    /// Samples the decoder discards at the start, at 48 kHz (RFC 7845
    /// §4.1); libopus asks for 3 840 at its default.
    pub pre_skip: u16,
}

impl AudioTrack {
    /// An Opus track. `sample_rate` is the rate the encoder was given;
    /// Opus always decodes at 48 kHz, which is what the track advertises.
    pub fn opus(sample_rate: u32, channels: u8) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
            pre_skip: 3_840,
        }
    }

    pub fn pre_skip(mut self, pre_skip: u16) -> Self {
        self.pre_skip = pre_skip;
        self
    }

    /// The `CodecPrivate` an Opus track carries: an OpusHead (RFC 7845
    /// §5.1).
    pub fn opus_head(&self) -> Vec<u8> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(self.channels);
        head.extend_from_slice(&self.pre_skip.to_le_bytes());
        head.extend_from_slice(&self.sample_rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family
        head
    }

    /// The decoder delay in nanoseconds, from the pre-skip.
    fn codec_delay_ns(&self) -> u64 {
        self.pre_skip as u64 * 1_000_000_000 / 48_000
    }
}

/// How the writer closes Clusters. A Cluster's blocks carry timestamps
/// relative to it in a signed 16-bit field, so one can never span more
/// than ~32 seconds; the defaults are the WebM guidelines' 5 seconds and
/// a few megabytes, with a new Cluster at each keyframe past `min_ms` so
/// seeking lands close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterLimits {
    pub min_ms: u64,
    pub max_ms: u64,
    pub max_bytes: usize,
}

impl Default for ClusterLimits {
    fn default() -> Self {
        Self {
            min_ms: 1_000,
            max_ms: 5_000,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

/// What a recording holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebmConfig {
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
    pub max_cluster: ClusterLimits,
    /// The `WritingApp` string; the muxing app is always this crate.
    pub writing_app: Option<String>,
}

impl WebmConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn video(mut self, track: VideoTrack) -> Self {
        self.video = Some(track);
        self
    }

    pub fn audio(mut self, track: AudioTrack) -> Self {
        self.audio = Some(track);
        self
    }

    pub fn max_cluster(mut self, limits: ClusterLimits) -> Self {
        self.max_cluster = limits;
        self
    }

    pub fn writing_app(mut self, app: impl Into<String>) -> Self {
        self.writing_app = Some(app.into());
        self
    }
}

/// What a finished recording came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WebmStats {
    pub duration_ms: u64,
    pub bytes: u64,
    pub video_frames: u64,
    pub video_keyframes: u64,
    pub audio_frames: u64,
    pub clusters: u64,
}

/// A Cue: where in the file a keyframe is.
struct CuePoint {
    time_ms: u64,
    track: u64,
    cluster_position: u64,
}

/// The Cluster being filled, held in memory until it closes.
struct ClusterBuf {
    timecode_ms: u64,
    body: Vec<u8>,
    last_ms: u64,
    /// The keyframe this Cluster starts on, for its Cue.
    cue: Option<(u64, u64)>,
}

/// Writes one WebM file.
pub struct WebmWriter<W: Write + Seek> {
    out: W,
    config: WebmConfig,
    /// Byte offset of the Segment's payload; every stored position is
    /// relative to it.
    segment_start: u64,
    /// Where the Segment's 8-byte size sits.
    segment_size_pos: u64,
    /// Where Info's 8-byte Duration float sits.
    duration_pos: u64,
    /// Where each SeekHead entry's position sits, in the order written.
    seek_positions: Vec<(u32, u64)>,
    info_pos: u64,
    tracks_pos: u64,
    cluster: Option<ClusterBuf>,
    cues: Vec<CuePoint>,
    stats: WebmStats,
}

impl<W: Write + Seek> WebmWriter<W> {
    /// Start a recording: header, Segment, Info and Tracks are written at
    /// once, so a file is playable from the first Cluster onwards.
    pub fn new(mut out: W, config: WebmConfig) -> Result<Self> {
        write_ebml_header(&mut out)?;

        // A Segment whose size is the largest an 8-byte size can hold:
        // readers take that as "to the end of the file", so a recording
        // cut short by a crash still plays. `finish` patches the real one.
        out.write_all(&ebml::id_bytes(id::SEGMENT))?;
        let segment_size_pos = out.stream_position()?;
        ebml::write_vint_len(&mut out, unknown_size_8(), 8)?;
        let segment_start = out.stream_position()?;

        // Room for the SeekHead, filled in on finish.
        let seek_head_pos = out.stream_position()?;
        ebml::write_void(&mut out, SEEK_HEAD_RESERVE)?;

        let info_pos = out.stream_position()?;
        let duration_pos = write_info(&mut out, &config)?;
        let tracks_pos = out.stream_position()?;
        write_tracks(&mut out, &config)?;

        Ok(Self {
            out,
            config,
            segment_start,
            segment_size_pos,
            duration_pos,
            seek_positions: vec![(id::SEEK_HEAD, seek_head_pos)],
            info_pos,
            tracks_pos,
            cluster: None,
            cues: Vec::new(),
            stats: WebmStats::default(),
        })
    }

    /// Write one coded video frame at `ms` from the start of the
    /// recording. A keyframe starts a new Cluster once the current one
    /// has run for [`ClusterLimits::min_ms`], and gets a Cue.
    pub fn write_video(&mut self, ms: u64, keyframe: bool, data: &[u8]) -> Result<()> {
        if self.config.video.is_none() {
            return Err(WebmError::NoTrack("video"));
        }
        self.ensure_cluster(ms, keyframe)?;
        self.write_block(VIDEO_TRACK, ms, keyframe, data)?;
        self.stats.video_frames += 1;
        if keyframe {
            self.stats.video_keyframes += 1;
        }
        Ok(())
    }

    /// Write one coded audio frame at `ms`. Every audio block is a
    /// keyframe, as Matroska expects of a lossy audio track.
    pub fn write_audio(&mut self, ms: u64, data: &[u8]) -> Result<()> {
        if self.config.audio.is_none() {
            return Err(WebmError::NoTrack("audio"));
        }
        self.ensure_cluster(ms, false)?;
        self.write_block(AUDIO_TRACK, ms, true, data)?;
        self.stats.audio_frames += 1;
        Ok(())
    }

    /// Close the recording: the last Cluster, the Cues, and the Segment
    /// size, duration and SeekHead patched in.
    pub fn finish(mut self) -> Result<WebmStats> {
        self.flush_cluster()?;

        let cues_pos = self.out.stream_position()?;
        if !self.cues.is_empty() {
            self.write_cues()?;
            self.seek_positions.push((id::CUES, cues_pos));
        }

        let end = self.out.stream_position()?;
        self.stats.bytes = end;

        // The Segment's real size, now that we know it.
        self.out.seek(SeekFrom::Start(self.segment_size_pos))?;
        ebml::write_vint_len(&mut self.out, end - self.segment_start, 8)?;

        // The duration, in TimecodeScale units.
        self.out.seek(SeekFrom::Start(self.duration_pos))?;
        self.out
            .write_all(&(self.stats.duration_ms as f64).to_be_bytes())?;

        // The SeekHead, over the Void reserved for it.
        let (seek_head_pos, info_pos, tracks_pos) =
            (self.seek_positions[0].1, self.info_pos, self.tracks_pos);
        let mut entries = vec![
            (id::INFO, info_pos - self.segment_start),
            (id::TRACKS, tracks_pos - self.segment_start),
        ];
        if let Some((_, pos)) = self.seek_positions.iter().find(|(i, _)| *i == id::CUES) {
            entries.push((id::CUES, pos - self.segment_start));
        }
        self.out.seek(SeekFrom::Start(seek_head_pos))?;
        write_seek_head(&mut self.out, &entries, SEEK_HEAD_RESERVE)?;

        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()?;
        Ok(self.stats)
    }

    /// What has been written so far.
    pub fn stats(&self) -> WebmStats {
        self.stats
    }

    /// The bytes written so far, for a size cap.
    pub fn position(&mut self) -> Result<u64> {
        Ok(self.out.stream_position()?)
    }

    // ---- clusters and blocks ---------------------------------------------

    /// Start a Cluster when there is none, when this keyframe should open
    /// one, or when the current one is full.
    fn ensure_cluster(&mut self, ms: u64, keyframe: bool) -> Result<()> {
        let limits = self.config.max_cluster;
        let start_new = match &self.cluster {
            None => true,
            Some(c) => {
                // Only a forward jump opens a Cluster. A block a little
                // behind the Cluster's timestamp is ordinary interleaving
                // and rides in it as a negative relative; one far behind
                // is the caller's error, which `write_block` reports.
                let relative = ms as i64 - c.timecode_ms as i64;
                relative > i16::MAX as i64
                    || relative >= limits.max_ms as i64
                    || c.body.len() >= limits.max_bytes
                    || (keyframe && relative >= limits.min_ms as i64)
            }
        };
        if start_new {
            self.flush_cluster()?;
            self.cluster = Some(ClusterBuf {
                timecode_ms: ms,
                body: Vec::with_capacity(64 * 1024),
                last_ms: ms,
                cue: keyframe.then_some((ms, VIDEO_TRACK)),
            });
        } else if keyframe {
            // A keyframe inside a young Cluster still deserves a Cue when
            // the Cluster has none yet.
            if let Some(c) = self.cluster.as_mut() {
                c.cue.get_or_insert((ms, VIDEO_TRACK));
            }
        }
        Ok(())
    }

    fn write_block(&mut self, track: u64, ms: u64, keyframe: bool, data: &[u8]) -> Result<()> {
        let cluster = self.cluster.as_mut().expect("ensure_cluster ran");
        let relative = ms as i64 - cluster.timecode_ms as i64;
        if relative < i16::MIN as i64 || relative > i16::MAX as i64 {
            return Err(WebmError::Backwards {
                ms,
                cluster_ms: cluster.timecode_ms,
            });
        }
        // SimpleBlock: the track as a varint, the timestamp relative to
        // the Cluster, one flags byte, then the frame (Matroska §12.4).
        let mut block = Vec::with_capacity(data.len() + 8);
        ebml::write_vint(&mut block, track)?;
        block.extend_from_slice(&(relative as i16).to_be_bytes());
        block.push(if keyframe { 0x80 } else { 0x00 });
        block.extend_from_slice(data);
        ebml::write_binary(&mut cluster.body, id::SIMPLE_BLOCK, &block)?;

        cluster.last_ms = cluster.last_ms.max(ms);
        self.stats.duration_ms = self.stats.duration_ms.max(ms);
        Ok(())
    }

    /// Write the buffered Cluster out and note its Cue.
    fn flush_cluster(&mut self) -> Result<()> {
        let Some(cluster) = self.cluster.take() else {
            return Ok(());
        };
        if cluster.body.is_empty() {
            return Ok(());
        }
        let position = self.out.stream_position()?;
        let mut payload = Vec::with_capacity(cluster.body.len() + 16);
        ebml::write_uint(&mut payload, id::TIMECODE, cluster.timecode_ms)?;
        payload.extend_from_slice(&cluster.body);
        ebml::write_binary(&mut self.out, id::CLUSTER, &payload)?;
        self.stats.clusters += 1;
        if let Some((time_ms, track)) = cluster.cue {
            self.cues.push(CuePoint {
                time_ms,
                track,
                cluster_position: position - self.segment_start,
            });
        }
        Ok(())
    }

    fn write_cues(&mut self) -> Result<()> {
        let mut payload = Vec::with_capacity(self.cues.len() * 24);
        for cue in &self.cues {
            let mut point = Vec::with_capacity(24);
            ebml::write_uint(&mut point, id::CUE_TIME, cue.time_ms)?;
            let mut positions = Vec::with_capacity(16);
            ebml::write_uint(&mut positions, id::CUE_TRACK, cue.track)?;
            ebml::write_uint(
                &mut positions,
                id::CUE_CLUSTER_POSITION,
                cue.cluster_position,
            )?;
            ebml::write_binary(&mut point, id::CUE_TRACK_POSITIONS, &positions)?;
            ebml::write_binary(&mut payload, id::CUE_POINT, &point)?;
        }
        ebml::write_binary(&mut self.out, id::CUES, &payload)?;
        Ok(())
    }
}

impl<W: Write + Seek> Drop for WebmWriter<W> {
    /// A recording abandoned without [`WebmWriter::finish`] keeps every
    /// block written: the Cluster in hand is flushed, and the Segment's
    /// size is still the unknown-size marker, which readers follow to the
    /// end of the file.
    fn drop(&mut self) {
        if self.cluster.is_some() {
            if let Err(e) = self.flush_cluster() {
                tracing::warn!(error = %e, "webm: could not flush the last cluster");
            }
            let _ = self.out.flush();
        }
    }
}

/// The unknown-size marker for an 8-byte EBML size: every value bit set.
/// A reader takes it as "this element runs to the end of the file", which
/// is what keeps a recording cut short by a crash playable.
fn unknown_size_8() -> u64 {
    (1u64 << 56) - 1
}

fn write_ebml_header<W: Write>(w: &mut W) -> Result<()> {
    let mut head = Vec::with_capacity(32);
    ebml::write_uint(&mut head, id::EBML_VERSION, 1)?;
    ebml::write_uint(&mut head, id::EBML_READ_VERSION, 1)?;
    ebml::write_uint(&mut head, id::EBML_MAX_ID_LENGTH, 4)?;
    ebml::write_uint(&mut head, id::EBML_MAX_SIZE_LENGTH, 8)?;
    ebml::write_str(&mut head, id::DOC_TYPE, "webm")?;
    ebml::write_uint(&mut head, id::DOC_TYPE_VERSION, 2)?;
    ebml::write_uint(&mut head, id::DOC_TYPE_READ_VERSION, 2)?;
    ebml::write_binary(w, id::EBML, &head)?;
    Ok(())
}

/// Write Info; returns where its Duration float sits, for patching.
fn write_info<W: Write + Seek>(w: &mut W, config: &WebmConfig) -> Result<u64> {
    let app = config
        .writing_app
        .clone()
        .unwrap_or_else(|| format!("forge-webm {}", env!("CARGO_PKG_VERSION")));
    let mut info = Vec::with_capacity(96);
    ebml::write_uint(&mut info, id::TIMECODE_SCALE, TIMECODE_SCALE_NS)?;
    ebml::write_str(
        &mut info,
        id::MUXING_APP,
        &format!("forge-webm {}", env!("CARGO_PKG_VERSION")),
    )?;
    ebml::write_str(&mut info, id::WRITING_APP, &app)?;
    // The duration's own offset inside Info, so it can be patched.
    let duration_offset_in_info = info.len() + ebml::id_bytes(id::DURATION).len() + 1;
    ebml::write_f64(&mut info, id::DURATION, 0.0)?;

    let info_header = ebml::id_bytes(id::INFO).len() + ebml::vint_len(info.len() as u64);
    let info_start = w.stream_position()?;
    ebml::write_binary(w, id::INFO, &info)?;
    Ok(info_start + info_header as u64 + duration_offset_in_info as u64)
}

fn write_tracks<W: Write>(w: &mut W, config: &WebmConfig) -> Result<()> {
    let mut tracks = Vec::with_capacity(128);
    if let Some(v) = &config.video {
        let mut entry = Vec::with_capacity(64);
        ebml::write_uint(&mut entry, id::TRACK_NUMBER, VIDEO_TRACK)?;
        ebml::write_uint(&mut entry, id::TRACK_UID, VIDEO_TRACK)?;
        ebml::write_uint(&mut entry, id::TRACK_TYPE, 1)?;
        ebml::write_uint(&mut entry, id::FLAG_LACING, 0)?;
        ebml::write_str(&mut entry, id::CODEC_ID, v.codec.codec_id())?;
        if let Some(fps) = v.fps {
            ebml::write_uint(&mut entry, id::DEFAULT_DURATION, 1_000_000_000 / fps as u64)?;
        }
        let mut video = Vec::with_capacity(16);
        ebml::write_uint(&mut video, id::PIXEL_WIDTH, v.width as u64)?;
        ebml::write_uint(&mut video, id::PIXEL_HEIGHT, v.height as u64)?;
        ebml::write_binary(&mut entry, id::VIDEO, &video)?;
        ebml::write_binary(&mut tracks, id::TRACK_ENTRY, &entry)?;
    }
    if let Some(a) = &config.audio {
        let mut entry = Vec::with_capacity(64);
        ebml::write_uint(&mut entry, id::TRACK_NUMBER, AUDIO_TRACK)?;
        ebml::write_uint(&mut entry, id::TRACK_UID, AUDIO_TRACK)?;
        ebml::write_uint(&mut entry, id::TRACK_TYPE, 2)?;
        ebml::write_uint(&mut entry, id::FLAG_LACING, 0)?;
        ebml::write_str(&mut entry, id::CODEC_ID, "A_OPUS")?;
        ebml::write_binary(&mut entry, id::CODEC_PRIVATE, &a.opus_head())?;
        ebml::write_uint(&mut entry, id::CODEC_DELAY, a.codec_delay_ns())?;
        // RFC 7845 §4: 80 ms of pre-roll after a seek.
        ebml::write_uint(&mut entry, id::SEEK_PRE_ROLL, 80_000_000)?;
        let mut audio = Vec::with_capacity(16);
        // Opus always decodes at 48 kHz whatever it was fed.
        ebml::write_f64(&mut audio, id::SAMPLING_FREQUENCY, 48_000.0)?;
        ebml::write_uint(&mut audio, id::CHANNELS, a.channels as u64)?;
        ebml::write_binary(&mut entry, id::AUDIO, &audio)?;
        ebml::write_binary(&mut tracks, id::TRACK_ENTRY, &entry)?;
    }
    ebml::write_binary(w, id::TRACKS, &tracks)?;
    Ok(())
}

/// Write a SeekHead occupying exactly `total` bytes — the entries, then a
/// Void inside it for the slack — so it fits the space reserved for it at
/// the start of the Segment.
fn write_seek_head<W: Write>(w: &mut W, entries: &[(u32, u64)], total: usize) -> Result<()> {
    let mut payload = Vec::with_capacity(total);
    for (element, position) in entries {
        let mut seek = Vec::with_capacity(24);
        ebml::write_binary(&mut seek, id::SEEK_ID, &ebml::id_bytes(*element))?;
        // A fixed width, so an entry's size never changes when patched.
        ebml::write_uint_padded(&mut seek, id::SEEK_POSITION, *position, 8)?;
        ebml::write_binary(&mut payload, id::SEEK, &seek)?;
    }
    let id_len = ebml::id_bytes(id::SEEK_HEAD).len();
    // The payload fills what is left after the id and the size; the
    // size's own width depends on the payload, and the slack must be
    // either nothing or enough for a Void (two bytes).
    let mut chosen = None;
    for size_len in 1..=8usize {
        let Some(want) = total.checked_sub(id_len + size_len) else {
            break;
        };
        if ebml::vint_len(want as u64) > size_len || want < payload.len() {
            continue;
        }
        let slack = want - payload.len();
        if slack == 0 || slack >= 2 {
            chosen = Some((size_len, slack));
            break;
        }
    }
    let (size_len, slack) = chosen.ok_or_else(|| {
        io::Error::other(format!(
            "a SeekHead of {} bytes does not fit the {total} reserved for it",
            payload.len()
        ))
    })?;
    if slack >= 2 {
        ebml::write_void(&mut payload, slack)?;
    }
    w.write_all(&ebml::id_bytes(id::SEEK_HEAD))?;
    ebml::write_vint_len(w, payload.len() as u64, size_len)?;
    w.write_all(&payload)?;
    Ok(())
}
