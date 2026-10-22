//! A fragmented MP4 (ISO base media, ISO/IEC 14496-12) writer for
//! conference recordings.
//!
//! A recording is one composite video track and the room's mixed audio
//! in a file every player opens. Written fragmented — an `ftyp`, a
//! `moov` whose sample tables are empty and whose `mvex` says the
//! samples come in fragments, then a `moof` + `mdat` pair per fragment —
//! so a file a node stopped writing mid-way still plays to its last
//! complete fragment, and a player can start on one before it is done.
//! On [`Mp4Writer::finish`] the movie's duration is patched in and an
//! `mfra` (fragment random access) appended for fast seeking.
//!
//! What it writes: **H.264** (`avc1` + `avcC`), **HEVC** (`hvc1` +
//! `hvcC`) or **AV1** (`av01` + `av1C`) video, and **Opus** audio
//! (`Opus` + `dOps`, Opus in ISOBMFF), both on a millisecond timescale.
//! H.264 and HEVC frames are taken as the Annex B byte streams the
//! depacketizers and encoders produce and stored as length-prefixed NAL
//! units with the parameter sets lifted into the configuration record;
//! AV1 temporal units are stored as they are. The record needs the first
//! keyframe's parameter sets, so the header is written when that frame
//! arrives; audio before it is held for the first fragment. A fragment
//! closes at each video keyframe once [`FragmentLimits::min_ms`] has
//! passed, and at [`FragmentLimits::max_ms`] regardless.
//!
//! What it does not do: AAC, VP8 or VP9 (those are WebM's), edit lists,
//! subtitles, chapters, encryption, a whole-file (`faststart`) layout.
//! The writer needs `Seek` only to patch the durations at the end.
//!
//! ```no_run
//! use forge_mp4::{AudioTrack, Mp4Config, Mp4VideoCodec, Mp4Writer, VideoTrack};
//! # fn main() -> Result<(), forge_mp4::Mp4Error> {
//! let file = std::fs::File::create("room.mp4")?;
//! let mut w = Mp4Writer::new(
//!     file,
//!     Mp4Config::new()
//!         .video(VideoTrack::new(Mp4VideoCodec::H264, 1280, 720).fps(15))
//!         .audio(AudioTrack::opus(48_000, 1)),
//! )?;
//! w.write_video(0, true, &[/* Annex B keyframe */])?;
//! w.write_audio(0, &[/* opus packet */])?;
//! let summary = w.finish()?;
//! println!("{} ms, {} bytes", summary.duration_ms, summary.bytes);
//! # Ok(()) }
//! ```

mod boxes;
pub mod codec;
pub mod read;

use std::io::{self, Seek, SeekFrom, Write};

use boxes::{fixed_16_16, simple, BoxBuf, IDENTITY_MATRIX};
pub use read::{
    read_summary, read_summary_file, FragmentInfo, Mp4Summary, TrackInfo, TrackKind, TrafInfo,
};

/// The video track's id; audio is [`AUDIO_TRACK`].
pub const VIDEO_TRACK: u32 = 1;
/// The audio track's id.
pub const AUDIO_TRACK: u32 = 2;

/// Both tracks' timescale: milliseconds.
pub const TIMESCALE: u32 = 1000;

/// What went wrong writing a recording.
#[derive(Debug, thiserror::Error)]
pub enum Mp4Error {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("no {0} track in this recording")]
    NoTrack(&'static str),
    #[error("the first video frame must be a keyframe")]
    NotAKeyframe,
    #[error("the keyframe carries no {0}")]
    NoParameterSets(&'static str),
    #[error("timestamp {0} ms is before the fragment's start {1} ms")]
    Backwards(u64, u64),
}

pub type Result<T> = std::result::Result<T, Mp4Error>;

/// The video codecs an MP4 recording carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl Mp4VideoCodec {
    /// The sample entry type.
    pub fn sample_entry(&self) -> [u8; 4] {
        match self {
            Mp4VideoCodec::H264 => *b"avc1",
            Mp4VideoCodec::Hevc => *b"hvc1",
            Mp4VideoCodec::Av1 => *b"av01",
        }
    }

    fn config_box(&self) -> [u8; 4] {
        match self {
            Mp4VideoCodec::H264 => *b"avcC",
            Mp4VideoCodec::Hevc => *b"hvcC",
            Mp4VideoCodec::Av1 => *b"av1C",
        }
    }
}

/// The video track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrack {
    pub codec: Mp4VideoCodec,
    pub width: u32,
    pub height: u32,
    /// The nominal rate, for the last sample's duration in a fragment
    /// (the others are the gaps between them).
    pub fps: u32,
}

impl VideoTrack {
    pub fn new(codec: Mp4VideoCodec, width: u32, height: u32) -> Self {
        Self {
            codec,
            width,
            height,
            fps: 15,
        }
    }

    pub fn fps(mut self, fps: u32) -> Self {
        self.fps = fps.max(1);
        self
    }
}

/// The audio track: Opus, as the recorder produces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub sample_rate: u32,
    pub channels: u8,
    /// Samples the decoder discards at the start, at 48 kHz (RFC 7845
    /// §4.1); libopus asks for 3 840 at its default.
    pub pre_skip: u16,
    /// One packet's duration in milliseconds: the recorder's 20.
    pub frame_ms: u32,
}

impl AudioTrack {
    pub fn opus(sample_rate: u32, channels: u8) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
            pre_skip: 3_840,
            frame_ms: 20,
        }
    }

    pub fn pre_skip(mut self, pre_skip: u16) -> Self {
        self.pre_skip = pre_skip;
        self
    }

    pub fn frame_ms(mut self, frame_ms: u32) -> Self {
        self.frame_ms = frame_ms.max(1);
        self
    }
}

/// How the writer closes fragments: at a keyframe once `min_ms` has
/// passed, and at `max_ms` whatever comes (a fragment is held in
/// memory until it closes, and a player seeks to fragment starts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentLimits {
    pub min_ms: u64,
    pub max_ms: u64,
}

impl Default for FragmentLimits {
    fn default() -> Self {
        Self {
            min_ms: 1_000,
            max_ms: 30_000,
        }
    }
}

/// What a recording is made of.
#[derive(Debug, Clone, Default)]
pub struct Mp4Config {
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
    pub fragments: FragmentLimits,
}

impl Mp4Config {
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

    pub fn fragments(mut self, limits: FragmentLimits) -> Self {
        self.fragments = limits;
        self
    }
}

/// What a finished recording came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mp4Stats {
    pub duration_ms: u64,
    pub bytes: u64,
    pub video_frames: u64,
    pub video_keyframes: u64,
    pub audio_frames: u64,
    pub fragments: u64,
}

/// One sample waiting in the open fragment.
struct Sample {
    ms: u64,
    keyframe: bool,
    data: Vec<u8>,
}

/// The fragment being filled.
struct Fragment {
    start_ms: u64,
    video: Vec<Sample>,
    audio: Vec<Sample>,
}

/// Where a fragment started with a keyframe, for the `mfra`.
struct Access {
    time_ms: u64,
    moof_offset: u64,
}

/// Writes one MP4 file.
pub struct Mp4Writer<W: Write + Seek> {
    out: W,
    config: Mp4Config,
    /// The `moov` is written on the first keyframe (its configuration
    /// record comes from there); `None` until then.
    header: Option<Header>,
    /// Audio that arrived before the header could be written.
    pending_audio: Vec<Sample>,
    fragment: Option<Fragment>,
    sequence: u32,
    access: Vec<Access>,
    last_video_ms: Option<u64>,
    last_audio_ms: Option<u64>,
    stats: Mp4Stats,
}

/// Where the `moov`'s durations sit, to be patched on finish.
struct Header {
    mvhd_duration_pos: u64,
    tkhd_duration_pos: Vec<u64>,
    mdhd_duration_pos: Vec<u64>,
}

impl<W: Write + Seek> Mp4Writer<W> {
    /// Open a recording. Nothing is written until the first video
    /// keyframe (or at once, for an audio-only recording).
    pub fn new(out: W, config: Mp4Config) -> Result<Self> {
        if config.video.is_none() && config.audio.is_none() {
            return Err(Mp4Error::NoTrack("video or audio"));
        }
        let mut w = Self {
            out,
            config,
            header: None,
            pending_audio: Vec::new(),
            fragment: None,
            sequence: 0,
            access: Vec::new(),
            last_video_ms: None,
            last_audio_ms: None,
            stats: Mp4Stats::default(),
        };
        if w.config.video.is_none() {
            w.write_header(None)?;
        }
        Ok(w)
    }

    /// Write one coded video frame at `ms` from the start of the
    /// recording: an Annex B byte stream for H.264 and HEVC, a temporal
    /// unit for AV1. The first must be a keyframe.
    pub fn write_video(&mut self, ms: u64, keyframe: bool, data: &[u8]) -> Result<()> {
        let track = self
            .config
            .video
            .clone()
            .ok_or(Mp4Error::NoTrack("video"))?;
        if self.header.is_none() {
            if !keyframe {
                return Err(Mp4Error::NotAKeyframe);
            }
            let ps = codec::parameter_sets(track.codec, data);
            let record = match track.codec {
                Mp4VideoCodec::H264 => codec::avcc(&ps)?,
                Mp4VideoCodec::Hevc => codec::hvcc(&ps)?,
                Mp4VideoCodec::Av1 => codec::av1c(&ps)?,
            };
            self.write_header(Some(record))?;
        }
        let sample = match track.codec {
            Mp4VideoCodec::Av1 => data.to_vec(),
            _ => codec::annex_b_to_sample(track.codec, data),
        };
        self.ensure_fragment(ms, keyframe)?;
        let f = self.fragment.as_mut().expect("opened above");
        if ms < f.start_ms {
            return Err(Mp4Error::Backwards(ms, f.start_ms));
        }
        f.video.push(Sample {
            ms,
            keyframe,
            data: sample,
        });
        self.last_video_ms = Some(ms);
        self.stats.video_frames += 1;
        if keyframe {
            self.stats.video_keyframes += 1;
        }
        self.stats.duration_ms = self.stats.duration_ms.max(ms);
        Ok(())
    }

    /// Write one Opus packet at `ms`.
    pub fn write_audio(&mut self, ms: u64, data: &[u8]) -> Result<()> {
        if self.config.audio.is_none() {
            return Err(Mp4Error::NoTrack("audio"));
        }
        let sample = Sample {
            ms,
            keyframe: true,
            data: data.to_vec(),
        };
        if self.header.is_none() {
            // Held for the first fragment, which the first keyframe opens.
            self.pending_audio.push(sample);
            return Ok(());
        }
        self.ensure_fragment(ms, false)?;
        let f = self.fragment.as_mut().expect("opened above");
        let sample = if ms < f.start_ms {
            // A little behind the keyframe that opened the fragment:
            // ordinary interleaving, clamped to the fragment's start.
            Sample {
                ms: f.start_ms,
                ..sample
            }
        } else {
            sample
        };
        f.audio.push(sample);
        self.last_audio_ms = Some(ms);
        self.stats.audio_frames += 1;
        self.stats.duration_ms = self.stats.duration_ms.max(ms);
        Ok(())
    }

    /// Close the recording: the last fragment, the durations patched
    /// into the `moov`, and the `mfra`.
    pub fn finish(mut self) -> Result<Mp4Stats> {
        if self.header.is_none() {
            // Audio only ever arrived, or nothing: write what there is.
            if self.config.video.is_some() && !self.pending_audio.is_empty() {
                // Video that never came: the file is its audio.
                self.config.video = None;
                self.write_header(None)?;
            } else if self.config.video.is_some() {
                return Ok(self.stats);
            }
        }
        self.flush_fragment()?;
        // The movie's duration, in the moov's three places.
        let duration = self.total_duration_ms();
        if let Some(h) = &self.header {
            let positions: Vec<u64> = std::iter::once(h.mvhd_duration_pos)
                .chain(h.tkhd_duration_pos.iter().copied())
                .chain(h.mdhd_duration_pos.iter().copied())
                .collect();
            for pos in positions {
                self.out.seek(SeekFrom::Start(pos))?;
                self.out.write_all(&(duration as u32).to_be_bytes())?;
            }
            self.out.seek(SeekFrom::End(0))?;
        }
        self.write_mfra()?;
        let end = self.out.stream_position()?;
        self.stats.bytes = end;
        self.stats.duration_ms = duration;
        self.out.flush()?;
        Ok(self.stats)
    }

    /// What has been written so far.
    pub fn stats(&self) -> Mp4Stats {
        self.stats
    }

    /// The bytes written so far, for a size cap. Fragments still open
    /// are not counted.
    pub fn position(&mut self) -> Result<u64> {
        Ok(self.out.stream_position()?)
    }

    fn total_duration_ms(&self) -> u64 {
        let video = self
            .last_video_ms
            .map(|ms| ms + 1000 / self.config.video.as_ref().map(|v| v.fps).unwrap_or(15) as u64);
        let audio = self
            .last_audio_ms
            .map(|ms| ms + self.config.audio.as_ref().map(|a| a.frame_ms).unwrap_or(20) as u64);
        video.into_iter().chain(audio).max().unwrap_or(0)
    }

    // ---- the header ------------------------------------------------------

    fn write_header(&mut self, video_record: Option<Vec<u8>>) -> Result<()> {
        // ftyp: isom, with the brands players look for.
        let mut ftyp = BoxBuf::new(*b"ftyp");
        ftyp.bytes(b"isom").u32(512).bytes(b"isom").bytes(b"iso2");
        if let Some(v) = &self.config.video {
            ftyp.bytes(match v.codec {
                Mp4VideoCodec::H264 => b"avc1",
                Mp4VideoCodec::Hevc => b"hvc1",
                Mp4VideoCodec::Av1 => b"av01",
            });
        }
        ftyp.bytes(b"mp41");
        let ftyp = ftyp.finish();
        self.out.write_all(&ftyp)?;

        let moov_start = self.out.stream_position()?;
        let mut moov = BoxBuf::new(*b"moov");
        let mut positions = Header {
            mvhd_duration_pos: 0,
            tkhd_duration_pos: Vec::new(),
            mdhd_duration_pos: Vec::new(),
        };

        // mvhd
        let mut mvhd = BoxBuf::full(*b"mvhd", 0, 0);
        mvhd.u32(0).u32(0).u32(TIMESCALE);
        positions.mvhd_duration_pos = moov_start + moov.len() as u64 + mvhd.len() as u64;
        mvhd.u32(0); // duration, patched on finish
        mvhd.u32(0x0001_0000).u16(0x0100).u16(0).u32(0).u32(0);
        for m in IDENTITY_MATRIX {
            mvhd.u32(m);
        }
        mvhd.zeros(24); // pre_defined
        let next_track =
            1 + self.config.video.is_some() as u32 + self.config.audio.is_some() as u32;
        mvhd.u32(next_track);
        moov.child(mvhd);

        let mut tracks: Vec<Vec<u8>> = Vec::new();
        if let Some(v) = self.config.video.clone() {
            let record = video_record.ok_or(Mp4Error::NoParameterSets("parameter sets"))?;
            tracks.push(self.trak(
                VIDEO_TRACK,
                TrackKindW::Video(&v, &record),
                moov_start + moov.len() as u64 + tracks.iter().map(|t| t.len() as u64).sum::<u64>(),
                &mut positions,
            ));
        }
        if let Some(a) = self.config.audio.clone() {
            tracks.push(self.trak(
                AUDIO_TRACK,
                TrackKindW::Audio(&a),
                moov_start + moov.len() as u64 + tracks.iter().map(|t| t.len() as u64).sum::<u64>(),
                &mut positions,
            ));
        }
        for t in tracks {
            moov.bytes(&t);
        }

        // mvex: the samples are in fragments.
        let mut mvex = BoxBuf::new(*b"mvex");
        for id in self.track_ids() {
            let mut trex = BoxBuf::full(*b"trex", 0, 0);
            trex.u32(id).u32(1).u32(0).u32(0).u32(0);
            mvex.child(trex);
        }
        moov.child(mvex);
        self.out.write_all(&moov.finish())?;
        self.header = Some(positions);

        // Audio that waited for the header.
        let pending = std::mem::take(&mut self.pending_audio);
        for s in pending {
            self.write_audio(s.ms, &s.data)?;
        }
        Ok(())
    }

    fn track_ids(&self) -> Vec<u32> {
        let mut ids = Vec::new();
        if self.config.video.is_some() {
            ids.push(VIDEO_TRACK);
        }
        if self.config.audio.is_some() {
            ids.push(AUDIO_TRACK);
        }
        ids
    }

    /// One `trak`, its duration positions recorded relative to `at`
    /// (the file offset the trak will start at).
    fn trak(&self, id: u32, kind: TrackKindW<'_>, at: u64, positions: &mut Header) -> Vec<u8> {
        let mut trak = BoxBuf::new(*b"trak");
        // tkhd: enabled, in movie, in preview.
        let mut tkhd = BoxBuf::full(*b"tkhd", 0, 0x7);
        tkhd.u32(0).u32(0).u32(id).u32(0);
        positions
            .tkhd_duration_pos
            .push(at + trak.len() as u64 + tkhd.len() as u64);
        tkhd.u32(0); // duration, patched
        tkhd.u32(0).u32(0); // reserved
        tkhd.i16(0); // layer
        tkhd.i16(0); // alternate_group
        tkhd.u16(match kind {
            TrackKindW::Audio(_) => 0x0100,
            TrackKindW::Video(..) => 0,
        });
        tkhd.u16(0);
        for m in IDENTITY_MATRIX {
            tkhd.u32(m);
        }
        match kind {
            TrackKindW::Video(v, _) => {
                tkhd.u32(fixed_16_16(v.width)).u32(fixed_16_16(v.height));
            }
            TrackKindW::Audio(_) => {
                tkhd.u32(0).u32(0);
            }
        }
        trak.child(tkhd);

        let mut mdia = BoxBuf::new(*b"mdia");
        let mut mdhd = BoxBuf::full(*b"mdhd", 0, 0);
        mdhd.u32(0).u32(0).u32(TIMESCALE);
        positions
            .mdhd_duration_pos
            .push(at + trak.len() as u64 + mdia.len() as u64 + mdhd.len() as u64);
        mdhd.u32(0); // duration, patched
        mdhd.u16(0x55C4); // language: und
        mdhd.u16(0);
        mdia.child(mdhd);
        let mut hdlr = BoxBuf::full(*b"hdlr", 0, 0);
        hdlr.u32(0);
        match kind {
            TrackKindW::Video(..) => hdlr.bytes(b"vide").zeros(12).bytes(b"VideoHandler\0"),
            TrackKindW::Audio(_) => hdlr.bytes(b"soun").zeros(12).bytes(b"SoundHandler\0"),
        };
        mdia.child(hdlr);

        let mut minf = BoxBuf::new(*b"minf");
        match kind {
            TrackKindW::Video(..) => {
                let mut vmhd = BoxBuf::full(*b"vmhd", 0, 1);
                vmhd.u16(0).u16(0).u16(0).u16(0);
                minf.child(vmhd);
            }
            TrackKindW::Audio(_) => {
                let mut smhd = BoxBuf::full(*b"smhd", 0, 0);
                smhd.u16(0).u16(0);
                minf.child(smhd);
            }
        }
        let mut dinf = BoxBuf::new(*b"dinf");
        let mut dref = BoxBuf::full(*b"dref", 0, 0);
        dref.u32(1);
        dref.child(BoxBuf::full(*b"url ", 0, 1)); // self-contained
        dinf.child(dref);
        minf.child(dinf);

        let mut stbl = BoxBuf::new(*b"stbl");
        let mut stsd = BoxBuf::full(*b"stsd", 0, 0);
        stsd.u32(1);
        match kind {
            TrackKindW::Video(v, record) => {
                let mut entry = BoxBuf::new(v.codec.sample_entry());
                entry.zeros(6).u16(1); // reserved, data_reference_index
                entry.u16(0).u16(0).zeros(12); // pre_defined, reserved, pre_defined
                entry.u16(v.width as u16).u16(v.height as u16);
                entry.u32(0x0048_0000).u32(0x0048_0000); // 72 dpi
                entry.u32(0);
                entry.u16(1); // frame_count
                let mut name = [0u8; 32];
                let label: &[u8] = b"\x0bforge-media";
                name[..label.len()].copy_from_slice(label);
                entry.bytes(&name);
                entry.u16(0x0018); // depth
                entry.i16(-1);
                entry.bytes(&simple(v.codec.config_box(), record));
                stsd.child(entry);
            }
            TrackKindW::Audio(a) => {
                let mut entry = BoxBuf::new(*b"Opus");
                entry.zeros(6).u16(1);
                entry.u32(0).u32(0); // reserved
                entry.u16(a.channels as u16).u16(16); // channelcount, samplesize
                entry.u16(0).u16(0); // pre_defined, reserved
                entry.u32(48_000 << 16); // samplerate 16.16: Opus decodes at 48 kHz
                entry.bytes(&simple(
                    *b"dOps",
                    &codec::dops(a.channels, a.pre_skip, a.sample_rate),
                ));
                stsd.child(entry);
            }
        }
        stbl.child(stsd);
        for kind in [*b"stts", *b"stsc", *b"stsz", *b"stco"] {
            let mut b = BoxBuf::full(kind, 0, 0);
            if kind == *b"stsz" {
                b.u32(0);
            }
            b.u32(0);
            stbl.child(b);
        }
        minf.child(stbl);
        mdia.child(minf);
        trak.child(mdia);
        trak.finish()
    }

    // ---- fragments ---------------------------------------------------------

    /// Open a fragment when there is none, when this keyframe should
    /// start one, or when the open one has run its course.
    fn ensure_fragment(&mut self, ms: u64, keyframe: bool) -> Result<()> {
        let limits = self.config.fragments;
        let start_new = match &self.fragment {
            None => true,
            Some(f) => {
                let ran = ms.saturating_sub(f.start_ms);
                (keyframe && ran >= limits.min_ms) || ran >= limits.max_ms
            }
        };
        if start_new {
            self.flush_fragment()?;
            self.fragment = Some(Fragment {
                start_ms: ms,
                video: Vec::new(),
                audio: Vec::new(),
            });
        }
        Ok(())
    }

    /// Write the open fragment as `moof` + `mdat`.
    fn flush_fragment(&mut self) -> Result<()> {
        let Some(f) = self.fragment.take() else {
            return Ok(());
        };
        if f.video.is_empty() && f.audio.is_empty() {
            return Ok(());
        }
        self.sequence += 1;
        let moof_offset = self.out.stream_position()?;

        // Durations: the gap to the next sample, the nominal for the last.
        let video_fps = self.config.video.as_ref().map(|v| v.fps).unwrap_or(15) as u64;
        let audio_ms = self.config.audio.as_ref().map(|a| a.frame_ms).unwrap_or(20) as u64;
        let durations = |samples: &[Sample], nominal: u64| -> Vec<u32> {
            samples
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let d = match samples.get(i + 1) {
                        Some(next) => next.ms.saturating_sub(s.ms),
                        None => nominal,
                    };
                    d.max(1) as u32
                })
                .collect()
        };
        let video_durations = durations(&f.video, 1000 / video_fps.max(1));
        let audio_durations = durations(&f.audio, audio_ms);

        // moof, with the data offsets patched once its size is known.
        let mut moof = BoxBuf::new(*b"moof");
        let mut mfhd = BoxBuf::full(*b"mfhd", 0, 0);
        mfhd.u32(self.sequence);
        moof.child(mfhd);
        let mut offset_patches: Vec<(usize, u64)> = Vec::new(); // (position in moof, byte offset within mdat payload)
        let mut mdat_len = 0u64;
        let tracks: Vec<(u32, &Vec<Sample>, &Vec<u32>)> = [
            (VIDEO_TRACK, &f.video, &video_durations),
            (AUDIO_TRACK, &f.audio, &audio_durations),
        ]
        .into_iter()
        .filter(|(_, s, _)| !s.is_empty())
        .collect();
        for (id, samples, durs) in &tracks {
            let mut traf = BoxBuf::new(*b"traf");
            // tfhd: default-base-is-moof.
            let mut tfhd = BoxBuf::full(*b"tfhd", 0, 0x0002_0000);
            tfhd.u32(*id);
            traf.child(tfhd);
            let mut tfdt = BoxBuf::full(*b"tfdt", 1, 0);
            tfdt.u64(samples[0].ms);
            traf.child(tfdt);
            // trun: data offset, durations, sizes, flags per sample.
            let mut trun = BoxBuf::full(*b"trun", 0, 0x0001 | 0x0100 | 0x0200 | 0x0400);
            trun.u32(samples.len() as u32);
            let offset_pos = moof.len() + traf.len() + trun.len();
            trun.i32(0); // data_offset, patched
            for (s, d) in samples.iter().zip(durs.iter()) {
                trun.u32(*d);
                trun.u32(s.data.len() as u32);
                trun.u32(if s.keyframe { 0x0200_0000 } else { 0x0101_0000 });
            }
            traf.child(trun);
            offset_patches.push((offset_pos, mdat_len));
            mdat_len += samples.iter().map(|s| s.data.len() as u64).sum::<u64>();
            moof.child(traf);
        }
        let moof_size = moof.len() as u64;
        for (pos, within) in offset_patches {
            let data_offset = moof_size + 8 + within;
            moof.patch_u32(pos, data_offset as u32);
        }
        self.out.write_all(&moof.finish())?;
        self.out.write_all(&((mdat_len + 8) as u32).to_be_bytes())?;
        self.out.write_all(b"mdat")?;
        for (_, samples, _) in &tracks {
            for s in samples.iter() {
                self.out.write_all(&s.data)?;
            }
        }
        self.out.flush()?;
        if f.video.first().map(|s| s.keyframe).unwrap_or(false) {
            self.access.push(Access {
                time_ms: f.video[0].ms,
                moof_offset,
            });
        }
        self.stats.fragments += 1;
        Ok(())
    }

    /// The `mfra`: where each keyframe-led fragment is, for seeking.
    fn write_mfra(&mut self) -> Result<()> {
        if self.access.is_empty() {
            return Ok(());
        }
        let mut mfra = BoxBuf::new(*b"mfra");
        // tfra version 1: 64-bit times and offsets; traf/trun/sample
        // numbers one byte each.
        let mut tfra = BoxBuf::full(*b"tfra", 1, 0);
        tfra.u32(VIDEO_TRACK);
        tfra.u32(0); // length_size_of_traf_num/trun_num/sample_num = 0: one byte each
        tfra.u32(self.access.len() as u32);
        for a in &self.access {
            tfra.u64(a.time_ms).u64(a.moof_offset).u8(1).u8(1).u8(1);
        }
        mfra.child(tfra);
        let mut mfro = BoxBuf::full(*b"mfro", 0, 0);
        mfro.u32(0); // patched: the mfra's size
        mfra.child(mfro);
        let mut bytes = mfra.finish();
        let size = bytes.len() as u32;
        let n = bytes.len();
        bytes[n - 4..].copy_from_slice(&size.to_be_bytes());
        self.out.write_all(&bytes)?;
        Ok(())
    }
}

/// A track as the header writer sees it.
enum TrackKindW<'a> {
    Video(&'a VideoTrack, &'a [u8]),
    Audio(&'a AudioTrack),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    pub fn h264_keyframe() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x02]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80]);
        v.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33]);
        v
    }

    fn h264_frame(n: u8) -> Vec<u8> {
        vec![0, 0, 0, 1, 0x41, 0x9A, n, n, n]
    }

    /// A recording of `frames` frames at 15 fps with 20 ms audio, a
    /// keyframe every `gop`, fragments at least `min_ms` long.
    pub fn recording(frames: u32, gop: u32, min_ms: u64, audio: bool, finish: bool) -> Vec<u8> {
        let mut config = Mp4Config::new()
            .video(VideoTrack::new(Mp4VideoCodec::H264, 640, 360).fps(15))
            .fragments(FragmentLimits {
                min_ms,
                max_ms: 30_000,
            });
        if audio {
            config = config.audio(AudioTrack::opus(48_000, 1));
        }
        let mut out = Cursor::new(Vec::new());
        {
            let mut w = Mp4Writer::new(&mut out, config).unwrap();
            let mut audio_ms = 0u64;
            for i in 0..frames {
                let ms = i as u64 * 1000 / 15;
                if audio {
                    while audio_ms <= ms {
                        w.write_audio(audio_ms, &[0xFC, 0xFF, 0xFE, (audio_ms % 251) as u8])
                            .unwrap();
                        audio_ms += 20;
                    }
                }
                let key = i % gop == 0;
                let data = if key {
                    h264_keyframe()
                } else {
                    h264_frame(i as u8)
                };
                w.write_video(ms, key, &data).unwrap();
            }
            if finish {
                let stats = w.finish().unwrap();
                assert_eq!(stats.video_frames, frames as u64);
                assert!(stats.fragments >= 1);
            }
        }
        out.into_inner()
    }

    #[test]
    fn a_recording_round_trips_with_its_tracks_fragments_and_index() {
        let bytes = recording(45, 15, 1000, true, false);
        let s = read_summary(&bytes).unwrap();
        assert_eq!(s.brands[0], "isom");
        assert!(s.brands.contains(&"avc1".to_string()));
        let v = s.track(TrackKind::Video).unwrap();
        assert_eq!(v.codec, "avc1");
        assert_eq!((v.width, v.height), (640, 360));
        assert_eq!(v.timescale, TIMESCALE);
        assert_eq!(
            &v.config[..5],
            &[1, 0x42, 0xC0, 0x1E, 0xFF],
            "avcC from the SPS"
        );
        let a = s.track(TrackKind::Audio).unwrap();
        assert_eq!(a.codec, "Opus");
        assert_eq!(a.config[1], 1, "mono");
        // Three keyframes a second apart: a fragment per keyframe, each
        // led by it with audio interleaved. Unfinished, the open third
        // fragment was never flushed — a crash loses what is in flight —
        // and there is no mfra.
        assert_eq!(s.fragments.len(), 2);
        let first = &s.fragments[0];
        let vt = first.traf(VIDEO_TRACK).unwrap();
        assert_eq!(vt.samples, 15);
        assert_eq!(vt.keyframes, 1);
        assert_eq!(vt.base_time, 0);
        assert!(first.traf(AUDIO_TRACK).unwrap().samples >= 49);
        assert_eq!(s.fragments[1].traf(VIDEO_TRACK).unwrap().base_time, 1000);
        assert!(!s.mfra);
        assert_eq!(s.duration_ms, 0, "not patched: never finished");
        for t in &s.tracks {
            assert_eq!((t.duration, t.media_duration), (0, 0), "track {}", t.id);
        }
        assert_eq!(s.video_samples(), 30);

        let done = recording(45, 15, 1000, true, true);
        let s = read_summary(&done).unwrap();
        assert_eq!(s.fragments.len(), 3, "finished: the last fragment flushed");
        assert_eq!(s.video_samples(), 45);
        assert!(s.mfra, "finished: an mfra");
        assert_eq!(s.access_points, 3);
        assert!(s.duration_ms >= 2_990, "{}", s.duration_ms);
        // Finishing patches the movie's duration into every header:
        // the mvhd and each track's tkhd and mdhd, all on the
        // millisecond timescale.
        assert_eq!(s.tracks.len(), 2);
        for t in &s.tracks {
            assert_eq!(t.timescale, TIMESCALE);
            assert_eq!(t.duration, s.duration_ms, "tkhd of track {}", t.id);
            assert_eq!(t.media_duration, s.duration_ms, "mdhd of track {}", t.id);
        }
        // The samples are AVCC: length-prefixed, parameter sets lifted.
        let sample = s.first_video_sample(&done).unwrap();
        assert_eq!(&sample[..4], &[0, 0, 0, 5]);
        assert_eq!(sample[4], 0x65);
    }

    #[test]
    fn a_file_cut_mid_fragment_reads_to_its_last_complete_one() {
        let bytes = recording(45, 15, 1000, true, true);
        let s = read_summary(&bytes).unwrap();
        let cut = s.fragments[2].moof_offset as usize + 40;
        let s = read_summary(&bytes[..cut]).unwrap();
        assert_eq!(s.fragments.len(), 2);
        assert!(!s.mfra);
        assert_eq!(s.video_samples(), 30);
    }

    #[test]
    fn the_first_frame_must_be_a_keyframe_and_audio_before_it_is_kept() {
        let mut out = Cursor::new(Vec::new());
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new()
                .video(VideoTrack::new(Mp4VideoCodec::H264, 64, 36))
                .audio(AudioTrack::opus(48_000, 2)),
        )
        .unwrap();
        w.write_audio(0, &[1]).unwrap();
        w.write_audio(20, &[2]).unwrap();
        assert!(matches!(
            w.write_video(0, false, &h264_frame(1)),
            Err(Mp4Error::NotAKeyframe)
        ));
        assert_eq!(
            w.position().unwrap(),
            0,
            "nothing written before the keyframe"
        );
        drop(w);
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new()
                .video(VideoTrack::new(Mp4VideoCodec::H264, 64, 36))
                .audio(AudioTrack::opus(48_000, 2)),
        )
        .unwrap();
        w.write_audio(0, &[1]).unwrap();
        w.write_audio(20, &[2]).unwrap();
        w.write_video(30, true, &h264_keyframe()).unwrap();
        let stats = w.finish().unwrap();
        assert_eq!(stats.audio_frames, 2);
        let s = read_summary(out.get_ref()).unwrap();
        assert_eq!(s.fragments.len(), 1);
        assert_eq!(s.fragments[0].traf(AUDIO_TRACK).unwrap().samples, 2);
        // A keyframe without parameter sets cannot open a file.
        let mut out = Cursor::new(Vec::new());
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new().video(VideoTrack::new(Mp4VideoCodec::H264, 64, 36)),
        )
        .unwrap();
        assert!(matches!(
            w.write_video(0, true, &[0, 0, 0, 1, 0x65, 1, 2]),
            Err(Mp4Error::NoParameterSets(_))
        ));
    }

    #[test]
    fn audio_only_writes_its_header_at_once_and_long_gops_close_at_max_ms() {
        let mut out = Cursor::new(Vec::new());
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new().audio(AudioTrack::opus(48_000, 1)),
        )
        .unwrap();
        assert!(w.position().unwrap() > 0);
        for i in 0..10u64 {
            w.write_audio(i * 20, &[i as u8]).unwrap();
        }
        w.finish().unwrap();
        let s = read_summary(out.get_ref()).unwrap();
        assert!(s.track(TrackKind::Video).is_none());
        assert_eq!(s.fragments.len(), 1);
        assert_eq!(s.duration_ms, 200);

        // One keyframe only, three seconds of frames, max_ms 1000: three
        // fragments, only the first with an access point.
        let mut out = Cursor::new(Vec::new());
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new()
                .video(VideoTrack::new(Mp4VideoCodec::H264, 64, 36).fps(15))
                .fragments(FragmentLimits {
                    min_ms: 500,
                    max_ms: 1000,
                }),
        )
        .unwrap();
        for i in 0..45u64 {
            let ms = i * 1000 / 15;
            if i == 0 {
                w.write_video(ms, true, &h264_keyframe()).unwrap();
            } else {
                w.write_video(ms, false, &h264_frame(i as u8)).unwrap();
            }
        }
        let stats = w.finish().unwrap();
        assert_eq!(stats.fragments, 3);
        let s = read_summary(out.get_ref()).unwrap();
        assert_eq!(s.fragments.len(), 3);
        assert_eq!(s.access_points, 1);
    }

    #[test]
    fn hevc_and_av1_recordings_carry_their_records() {
        let mut vps = vec![0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF];
        vps.extend_from_slice(&[0x01, 0x60, 0, 0, 0x03, 0, 0, 0, 0, 0, 0, 0x5D]);
        let mut key = Vec::new();
        for n in [
            &vps[..],
            &[0x42, 0x01, 0x01],
            &[0x44, 0x01, 0xC1],
            &[0x26, 0x01, 0xAF],
        ] {
            key.extend_from_slice(&[0, 0, 0, 1]);
            key.extend_from_slice(n);
        }
        let mut out = Cursor::new(Vec::new());
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new().video(VideoTrack::new(Mp4VideoCodec::Hevc, 64, 36)),
        )
        .unwrap();
        w.write_video(0, true, &key).unwrap();
        w.write_video(66, false, &[0, 0, 0, 1, 0x02, 0x01, 0xB0])
            .unwrap();
        w.finish().unwrap();
        let s = read_summary(out.get_ref()).unwrap();
        let v = s.track(TrackKind::Video).unwrap();
        assert_eq!(v.codec, "hvc1");
        assert_eq!(v.config[12], 0x5D);

        let mut tu = vec![0x0A, 5, 0, 0, 0, 0b0100_0000, 0];
        tu.extend_from_slice(&[0x32, 3, 0xAA, 0xBB, 0xCC]);
        let mut out = Cursor::new(Vec::new());
        let mut w = Mp4Writer::new(
            &mut out,
            Mp4Config::new().video(VideoTrack::new(Mp4VideoCodec::Av1, 64, 36)),
        )
        .unwrap();
        w.write_video(0, true, &tu).unwrap();
        w.write_video(66, false, &[0x32, 2, 0xDD, 0xEE]).unwrap();
        w.finish().unwrap();
        let s = read_summary(out.get_ref()).unwrap();
        let v = s.track(TrackKind::Video).unwrap();
        assert_eq!(v.codec, "av01");
        assert_eq!(v.config[1], 8);
        // AV1 samples are the temporal units whole.
        assert_eq!(s.first_video_sample(out.get_ref()).unwrap(), tu);
    }
}
