//! Reading a WebM file back.
//!
//! Enough of a parser to say what a recording holds — its tracks, every
//! block's track, timestamp and keyframe flag, and whether each Cue
//! points at a Cluster — which is what the writer's tests assert and what
//! a tool can use to check a finished file. It walks the whole file, so
//! it is for verification, not for playback.

use std::io;
use std::path::Path;

use crate::ebml::id;

/// What kind of stream a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Other(u64),
}

impl TrackKind {
    fn from_type(t: u64) -> Self {
        match t {
            1 => TrackKind::Video,
            2 => TrackKind::Audio,
            other => TrackKind::Other(other),
        }
    }
}

/// One track of a recording.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    pub number: u64,
    pub kind: TrackKind,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sample_rate: f64,
    pub channels: u64,
    pub codec_delay_ns: u64,
    pub seek_pre_roll_ns: u64,
    pub default_duration_ns: u64,
}

/// One block, with the absolute timestamp its Cluster gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockInfo {
    pub track: u64,
    pub ms: i64,
    pub keyframe: bool,
    pub bytes: usize,
}

/// One Cue, and whether the position it names really holds a Cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CueInfo {
    pub time_ms: u64,
    pub track: u64,
    pub cluster_position: u64,
    pub points_at_cluster: bool,
}

/// What a recording turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub struct WebmSummary {
    pub doc_type: String,
    pub timecode_scale_ns: u64,
    pub duration_ms: f64,
    pub tracks: Vec<TrackInfo>,
    pub clusters: usize,
    pub blocks: Vec<BlockInfo>,
    pub cues: Vec<CueInfo>,
    /// The Segment's declared size, `None` while it says "unknown".
    pub segment_size: Option<u64>,
    /// Positions the SeekHead names, by element id.
    pub seek_head: Vec<(u32, u64)>,
}

impl WebmSummary {
    /// The track of a kind, if the recording has one.
    pub fn track(&self, kind: TrackKind) -> Option<&TrackInfo> {
        self.tracks.iter().find(|t| t.kind == kind)
    }

    /// Every block of one track, in file order.
    pub fn blocks_of(&self, track: u64) -> Vec<BlockInfo> {
        self.blocks
            .iter()
            .copied()
            .filter(|b| b.track == track)
            .collect()
    }
}

/// Read a file's structure. The whole file is read into memory.
pub fn read_summary_file(path: &Path) -> io::Result<WebmSummary> {
    read_summary(&std::fs::read(path)?)
}

/// Read a recording's structure from its bytes.
pub fn read_summary(data: &[u8]) -> io::Result<WebmSummary> {
    let mut summary = WebmSummary {
        doc_type: String::new(),
        timecode_scale_ns: 1_000_000,
        duration_ms: 0.0,
        tracks: Vec::new(),
        clusters: 0,
        blocks: Vec::new(),
        cues: Vec::new(),
        segment_size: None,
        seek_head: Vec::new(),
    };
    let mut cursor = Cursor::new(data);
    let mut segment_start = None;
    let mut cluster_positions: Vec<u64> = Vec::new();

    while let Some(element) = cursor.element()? {
        match element.id {
            id::EBML => {
                let body = element.body(data);
                let mut head = Cursor::new(body);
                while let Some(e) = head.element()? {
                    if e.id == id::DOC_TYPE {
                        summary.doc_type = String::from_utf8_lossy(e.body(body)).into_owned();
                    }
                }
            }
            id::SEGMENT => {
                summary.segment_size = element.size;
                let start = element.data_start;
                segment_start = Some(start);
                let mut seg = Cursor::at(data, start, element.end(data.len()));
                while let Some(e) = seg.element()? {
                    match e.id {
                        id::SEEK_HEAD => read_seek_head(data, &e, &mut summary)?,
                        id::INFO => read_info(data, &e, &mut summary)?,
                        id::TRACKS => read_tracks(data, &e, &mut summary)?,
                        id::CLUSTER => {
                            summary.clusters += 1;
                            cluster_positions.push(e.start - start);
                            read_cluster(data, &e, &mut summary)?;
                        }
                        id::CUES => read_cues(data, &e, &mut summary)?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let _ = segment_start;
    for cue in &mut summary.cues {
        cue.points_at_cluster = cluster_positions.contains(&cue.cluster_position);
    }
    Ok(summary)
}

fn read_seek_head(data: &[u8], element: &Element, summary: &mut WebmSummary) -> io::Result<()> {
    let head = element.body(data);
    let mut c = Cursor::new(head);
    while let Some(e) = c.element()? {
        if e.id != id::SEEK {
            continue;
        }
        let entry = e.body(head);
        let mut seek = Cursor::new(entry);
        let (mut seek_id, mut position) = (0u32, 0u64);
        while let Some(f) = seek.element()? {
            let body = f.body(entry);
            match f.id {
                id::SEEK_ID => {
                    for b in body {
                        seek_id = (seek_id << 8) | *b as u32;
                    }
                }
                id::SEEK_POSITION => position = uint(body),
                _ => {}
            }
        }
        summary.seek_head.push((seek_id, position));
    }
    Ok(())
}

fn read_info(data: &[u8], element: &Element, summary: &mut WebmSummary) -> io::Result<()> {
    let body = element.body(data);
    let mut c = Cursor::new(body);
    while let Some(e) = c.element()? {
        match e.id {
            id::TIMECODE_SCALE => summary.timecode_scale_ns = uint(e.body(body)),
            id::DURATION => summary.duration_ms = float(e.body(body)),
            _ => {}
        }
    }
    Ok(())
}

fn read_tracks(data: &[u8], element: &Element, summary: &mut WebmSummary) -> io::Result<()> {
    let body = element.body(data);
    let mut c = Cursor::new(body);
    while let Some(e) = c.element()? {
        if e.id != id::TRACK_ENTRY {
            continue;
        }
        let entry_body = e.body(body);
        let mut track = TrackInfo {
            number: 0,
            kind: TrackKind::Other(0),
            codec_id: String::new(),
            codec_private: Vec::new(),
            width: 0,
            height: 0,
            sample_rate: 0.0,
            channels: 0,
            codec_delay_ns: 0,
            seek_pre_roll_ns: 0,
            default_duration_ns: 0,
        };
        let mut f = Cursor::new(entry_body);
        while let Some(g) = f.element()? {
            let gb = g.body(entry_body);
            match g.id {
                id::TRACK_NUMBER => track.number = uint(gb),
                id::TRACK_TYPE => track.kind = TrackKind::from_type(uint(gb)),
                id::CODEC_ID => track.codec_id = String::from_utf8_lossy(gb).into_owned(),
                id::CODEC_PRIVATE => track.codec_private = gb.to_vec(),
                id::CODEC_DELAY => track.codec_delay_ns = uint(gb),
                id::SEEK_PRE_ROLL => track.seek_pre_roll_ns = uint(gb),
                id::DEFAULT_DURATION => track.default_duration_ns = uint(gb),
                id::VIDEO => {
                    let mut v = Cursor::new(gb);
                    while let Some(h) = v.element()? {
                        match h.id {
                            id::PIXEL_WIDTH => track.width = uint(h.body(gb)) as u32,
                            id::PIXEL_HEIGHT => track.height = uint(h.body(gb)) as u32,
                            _ => {}
                        }
                    }
                }
                id::AUDIO => {
                    let mut a = Cursor::new(gb);
                    while let Some(h) = a.element()? {
                        match h.id {
                            id::SAMPLING_FREQUENCY => track.sample_rate = float(h.body(gb)),
                            id::CHANNELS => track.channels = uint(h.body(gb)),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        summary.tracks.push(track);
    }
    Ok(())
}

fn read_cluster(data: &[u8], element: &Element, summary: &mut WebmSummary) -> io::Result<()> {
    let body = element.body(data);
    let mut c = Cursor::new(body);
    let mut timecode = 0u64;
    while let Some(e) = c.element()? {
        let eb = e.body(body);
        match e.id {
            id::TIMECODE => timecode = uint(eb),
            id::SIMPLE_BLOCK => {
                if let Some(block) = parse_simple_block(eb, timecode) {
                    summary.blocks.push(block);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_simple_block(body: &[u8], cluster_ms: u64) -> Option<BlockInfo> {
    let mut c = Cursor::new(body);
    let track = c.vint_value()?;
    let rest = &body[c.pos..];
    if rest.len() < 3 {
        return None;
    }
    let relative = i16::from_be_bytes([rest[0], rest[1]]) as i64;
    let flags = rest[2];
    Some(BlockInfo {
        track,
        ms: cluster_ms as i64 + relative,
        keyframe: flags & 0x80 != 0,
        bytes: rest.len() - 3,
    })
}

fn read_cues(data: &[u8], element: &Element, summary: &mut WebmSummary) -> io::Result<()> {
    let body = element.body(data);
    let mut c = Cursor::new(body);
    while let Some(e) = c.element()? {
        if e.id != id::CUE_POINT {
            continue;
        }
        let point = e.body(body);
        let mut time_ms = 0u64;
        let (mut track, mut position) = (0u64, 0u64);
        let mut p = Cursor::new(point);
        while let Some(f) = p.element()? {
            let fb = f.body(point);
            match f.id {
                id::CUE_TIME => time_ms = uint(fb),
                id::CUE_TRACK_POSITIONS => {
                    let mut q = Cursor::new(fb);
                    while let Some(g) = q.element()? {
                        match g.id {
                            id::CUE_TRACK => track = uint(g.body(fb)),
                            id::CUE_CLUSTER_POSITION => position = uint(g.body(fb)),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        summary.cues.push(CueInfo {
            time_ms,
            track,
            cluster_position: position,
            points_at_cluster: false,
        });
    }
    Ok(())
}

/// An element's place in the buffer it was read from.
#[derive(Debug, Clone, Copy)]
struct Element {
    id: u32,
    /// Offset of the id.
    start: u64,
    /// Offset of the payload.
    data_start: u64,
    /// The declared size, `None` when unknown.
    size: Option<u64>,
}

impl Element {
    /// The payload, bounded by the buffer.
    fn body<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let start = self.data_start as usize;
        let end = self.end(data.len()) as usize;
        &data[start.min(data.len())..end.min(data.len())]
    }

    /// Where the element ends, taking an unknown size as the buffer's end.
    fn end(&self, len: usize) -> u64 {
        match self.size {
            Some(size) => (self.data_start + size).min(len as u64),
            None => len as u64,
        }
    }
}

/// Walks elements in a buffer.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            end: data.len(),
        }
    }

    fn at(data: &'a [u8], start: u64, end: u64) -> Self {
        Self {
            data,
            pos: start as usize,
            end: (end as usize).min(data.len()),
        }
    }

    /// The next element, or `None` at the end.
    fn element(&mut self) -> io::Result<Option<Element>> {
        if self.pos >= self.end {
            return Ok(None);
        }
        let start = self.pos as u64;
        let Some(id) = self.read_id() else {
            return Ok(None);
        };
        let Some(size) = self.read_size() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("truncated size for element {id:#x}"),
            ));
        };
        let element = Element {
            id,
            start,
            data_start: self.pos as u64,
            size,
        };
        // A master element of unknown size runs to the end; the caller
        // descends into it rather than stepping over it.
        let step = size.unwrap_or(0) as usize;
        self.pos = (self.pos + step).min(self.end);
        Ok(Some(element))
    }

    /// An id, marker bits and all.
    fn read_id(&mut self) -> Option<u32> {
        let first = *self.data.get(self.pos)?;
        let len = marker_len(first)?;
        if self.pos + len > self.end {
            return None;
        }
        let mut id = 0u32;
        for i in 0..len {
            id = (id << 8) | self.data[self.pos + i] as u32;
        }
        self.pos += len;
        Some(id)
    }

    /// A size: `Some(None)` when it is the unknown-size marker.
    fn read_size(&mut self) -> Option<Option<u64>> {
        let first = *self.data.get(self.pos)?;
        let len = marker_len(first)?;
        if self.pos + len > self.end {
            return None;
        }
        let mut value = (first & !(1u8 << (8 - len))) as u64;
        for i in 1..len {
            value = (value << 8) | self.data[self.pos + i] as u64;
        }
        self.pos += len;
        let all_ones = (1u64 << (7 * len as u32)) - 1;
        Some((value != all_ones).then_some(value))
    }

    /// A variable-length integer's value (a track number, say).
    fn vint_value(&mut self) -> Option<u64> {
        self.read_size().flatten()
    }
}

/// How many bytes a varint starting with `first` occupies.
fn marker_len(first: u8) -> Option<usize> {
    if first == 0 {
        return None;
    }
    Some(first.leading_zeros() as usize + 1)
}

fn uint(body: &[u8]) -> u64 {
    body.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64)
}

fn float(body: &[u8]) -> f64 {
    match body.len() {
        4 => f32::from_be_bytes([body[0], body[1], body[2], body[3]]) as f64,
        8 => f64::from_be_bytes([
            body[0], body[1], body[2], body[3], body[4], body[5], body[6], body[7],
        ]),
        _ => 0.0,
    }
}
