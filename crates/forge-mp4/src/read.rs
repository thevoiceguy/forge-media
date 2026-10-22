//! Reading an MP4 file back.
//!
//! Enough of a parser to say what a recording holds — its brands, its
//! tracks with their sample entries and configuration records, every
//! fragment with each track's run of samples, and whether an `mfra`
//! closes the file — which is what the writer's tests assert and what a
//! tool can use to check a finished file. It walks the whole file and
//! stops at the first box it cannot complete, so a file cut short reads
//! to its last whole fragment. For verification, not playback.

use std::io;
use std::path::Path;

/// What kind of stream a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Other([u8; 4]),
}

/// One track of a recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub id: u32,
    pub kind: TrackKind,
    /// The sample entry type: `avc1`, `hvc1`, `av01`, `Opus`, …
    pub codec: String,
    /// The configuration record's payload (`avcC`, `hvcC`, `av1C`,
    /// `dOps`), empty when the entry has none this reader knows.
    pub config: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub timescale: u32,
    /// From the `tkhd`, in the movie timescale; zero until finished.
    pub duration: u64,
    /// From the `mdhd`, in the track's own `timescale`; zero until
    /// finished.
    pub media_duration: u64,
}

/// One track's run of samples in a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafInfo {
    pub track_id: u32,
    /// The `tfdt`: the first sample's decode time.
    pub base_time: u64,
    pub samples: u32,
    pub keyframes: u32,
    /// The samples' durations summed.
    pub duration: u64,
    /// Where the first sample's bytes are, from the file's start.
    pub data_start: u64,
    pub sizes: Vec<u32>,
}

/// One `moof` + `mdat` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentInfo {
    pub sequence: u32,
    pub moof_offset: u64,
    pub trafs: Vec<TrafInfo>,
}

impl FragmentInfo {
    pub fn traf(&self, track_id: u32) -> Option<&TrafInfo> {
        self.trafs.iter().find(|t| t.track_id == track_id)
    }
}

/// What a file holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mp4Summary {
    /// The `ftyp`'s major brand first, then the compatible brands.
    pub brands: Vec<String>,
    /// The `mvhd` duration in milliseconds (its timescale converted);
    /// zero when the file was never finished.
    pub duration_ms: u64,
    pub tracks: Vec<TrackInfo>,
    pub fragments: Vec<FragmentInfo>,
    /// Whether an `mfra` closes the file.
    pub mfra: bool,
    /// Entries in the `mfra`'s `tfra`.
    pub access_points: u32,
}

impl Mp4Summary {
    pub fn track(&self, kind: TrackKind) -> Option<&TrackInfo> {
        self.tracks.iter().find(|t| t.kind == kind)
    }

    /// Samples of the video track over every fragment.
    pub fn video_samples(&self) -> u64 {
        self.fragments
            .iter()
            .filter_map(|f| f.traf(crate::VIDEO_TRACK))
            .map(|t| t.samples as u64)
            .sum()
    }

    /// The bytes of the first video sample, out of `data`.
    pub fn first_video_sample(&self, data: &[u8]) -> Option<Vec<u8>> {
        let t = self.fragments.first()?.traf(crate::VIDEO_TRACK)?;
        let start = t.data_start as usize;
        let len = *t.sizes.first()? as usize;
        data.get(start..start.checked_add(len)?).map(|s| s.to_vec())
    }
}

/// Read a file's summary.
pub fn read_summary_file(path: &Path) -> io::Result<Mp4Summary> {
    let data = std::fs::read(path)?;
    read_summary(&data)
}

/// Read a summary from bytes. Never panics on any input: what cannot be
/// parsed ends the walk, and an error is only for a file that is not
/// MP4 at all.
pub fn read_summary(data: &[u8]) -> io::Result<Mp4Summary> {
    let mut s = Mp4Summary::default();
    let mut pos = 0usize;
    let mut any = false;
    while let Some((kind, payload, next)) = box_at(data, pos) {
        any = true;
        match &kind {
            b"ftyp" => {
                s.brands.clear();
                if payload.len() >= 4 {
                    s.brands.push(fourcc(&payload[..4]));
                }
                let mut i = 8;
                while i + 4 <= payload.len() {
                    s.brands.push(fourcc(&payload[i..i + 4]));
                    i += 4;
                }
            }
            b"moov" => parse_moov(payload, &mut s),
            b"moof" => {
                if let Some(f) = parse_moof(payload, pos as u64) {
                    s.fragments.push(f);
                }
            }
            b"mfra" => {
                s.mfra = true;
                s.access_points = parse_mfra(payload);
            }
            _ => {}
        }
        pos = next;
    }
    if !any {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an MP4 file",
        ));
    }
    Ok(s)
}

fn fourcc(b: &[u8]) -> String {
    b.iter()
        .map(|&c| {
            if c.is_ascii_graphic() || c == b' ' {
                c as char
            } else {
                '?'
            }
        })
        .collect()
}

/// The box at `pos`: its type, payload and the position after it, or
/// `None` when the header or the declared size does not fit.
fn box_at(data: &[u8], pos: usize) -> Option<([u8; 4], &[u8], usize)> {
    let head = data.get(pos..pos.checked_add(8)?)?;
    let mut size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
    let kind = [head[4], head[5], head[6], head[7]];
    let mut header = 8usize;
    if size == 1 {
        let large = data.get(pos + 8..pos + 16)?;
        size = u64::from_be_bytes(large.try_into().ok()?);
        header = 16;
    } else if size == 0 {
        size = (data.len() - pos) as u64;
    }
    if size < header as u64 {
        return None;
    }
    let end = pos.checked_add(usize::try_from(size).ok()?)?;
    if end > data.len() {
        return None;
    }
    Some((kind, &data[pos + header..end], end))
}

/// Every child box of a payload.
fn children(payload: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some((kind, body, next)) = box_at(payload, pos) {
        out.push((kind, body));
        if next <= pos {
            break;
        }
        pos = next;
    }
    out
}

fn child<'a>(payload: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(payload)
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, b)| b)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|x| u32::from_be_bytes(x.try_into().unwrap()))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .map(|x| u64::from_be_bytes(x.try_into().unwrap()))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2)
        .map(|x| u16::from_be_bytes(x.try_into().unwrap()))
}

fn parse_moov(payload: &[u8], s: &mut Mp4Summary) {
    let mut movie_timescale = 1000u64;
    if let Some(mvhd) = child(payload, b"mvhd") {
        let version = mvhd.first().copied().unwrap_or(0);
        let (ts, dur) = if version == 1 {
            (be32(mvhd, 20), be64(mvhd, 24))
        } else {
            (be32(mvhd, 12), be32(mvhd, 16).map(u64::from))
        };
        if let (Some(ts), Some(dur)) = (ts, dur) {
            if ts > 0 {
                movie_timescale = ts as u64;
                s.duration_ms = dur.saturating_mul(1000) / ts as u64;
            }
        }
    }
    for (kind, trak) in children(payload) {
        if &kind != b"trak" {
            continue;
        }
        if let Some(t) = parse_trak(trak, movie_timescale) {
            s.tracks.push(t);
        }
    }
}

fn parse_trak(trak: &[u8], _movie_timescale: u64) -> Option<TrackInfo> {
    let tkhd = child(trak, b"tkhd")?;
    let version = *tkhd.first()?;
    // tkhd: version/flags, creation and modification times, track id,
    // four reserved bytes, then the duration.
    let (id, duration) = if version == 1 {
        (be32(tkhd, 20)?, be64(tkhd, 28)?)
    } else {
        (be32(tkhd, 12)?, be32(tkhd, 20)? as u64)
    };
    let mdia = child(trak, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let (timescale, media_duration) = if *mdhd.first()? == 1 {
        (be32(mdhd, 20)?, be64(mdhd, 24)?)
    } else {
        (be32(mdhd, 12)?, be32(mdhd, 16)? as u64)
    };
    let hdlr = child(mdia, b"hdlr")?;
    let handler: [u8; 4] = hdlr.get(8..12)?.try_into().ok()?;
    let kind = match &handler {
        b"vide" => TrackKind::Video,
        b"soun" => TrackKind::Audio,
        other => TrackKind::Other(*other),
    };
    let stsd = child(child(child(mdia, b"minf")?, b"stbl")?, b"stsd")?;
    let entries = children(stsd.get(8..)?);
    let (entry_kind, entry) = entries.first()?;
    let codec = fourcc(entry_kind);
    let (mut width, mut height, mut channels, mut config) = (0u32, 0u32, 0u16, Vec::new());
    match kind {
        TrackKind::Video => {
            width = be16(entry, 24)? as u32;
            height = be16(entry, 26)? as u32;
            // The configuration record follows the 78-byte sample entry.
            if let Some(rest) = entry.get(78..) {
                if let Some((_, body)) = children(rest).into_iter().next() {
                    config = body.to_vec();
                }
            }
        }
        TrackKind::Audio => {
            channels = be16(entry, 16)?;
            if let Some(rest) = entry.get(28..) {
                if let Some((_, body)) = children(rest).into_iter().next() {
                    config = body.to_vec();
                }
            }
        }
        TrackKind::Other(_) => {}
    }
    Some(TrackInfo {
        id,
        kind,
        codec,
        config,
        width,
        height,
        channels,
        timescale,
        duration,
        media_duration,
    })
}

fn parse_moof(payload: &[u8], moof_offset: u64) -> Option<FragmentInfo> {
    let mfhd = child(payload, b"mfhd")?;
    let sequence = be32(mfhd, 4)?;
    let mut trafs = Vec::new();
    for (kind, traf) in children(payload) {
        if &kind != b"traf" {
            continue;
        }
        let tfhd = child(traf, b"tfhd")?;
        let track_id = be32(tfhd, 4)?;
        let base_time = match child(traf, b"tfdt") {
            Some(tfdt) if tfdt.first() == Some(&1) => be64(tfdt, 4)?,
            Some(tfdt) => be32(tfdt, 4)? as u64,
            None => 0,
        };
        let trun = child(traf, b"trun")?;
        let flags = be32(trun, 0)? & 0x00FF_FFFF;
        let count = be32(trun, 4)?;
        let mut at = 8usize;
        let mut data_offset = 0i64;
        if flags & 0x1 != 0 {
            data_offset = be32(trun, at)? as i32 as i64;
            at += 4;
        }
        if flags & 0x4 != 0 {
            at += 4; // first_sample_flags
        }
        let mut sizes = Vec::new();
        let mut duration = 0u64;
        let mut keyframes = 0u32;
        for _ in 0..count.min(1 << 20) {
            if flags & 0x100 != 0 {
                duration = duration.saturating_add(be32(trun, at)? as u64);
                at += 4;
            }
            if flags & 0x200 != 0 {
                sizes.push(be32(trun, at)?);
                at += 4;
            }
            if flags & 0x400 != 0 {
                let f = be32(trun, at)?;
                if f & 0x0001_0000 == 0 {
                    keyframes += 1;
                }
                at += 4;
            }
            if flags & 0x800 != 0 {
                at += 4; // composition time offset
            }
        }
        let data_start = (moof_offset as i64).saturating_add(data_offset).max(0) as u64;
        trafs.push(TrafInfo {
            track_id,
            base_time,
            samples: count,
            keyframes,
            duration,
            data_start,
            sizes,
        });
    }
    Some(FragmentInfo {
        sequence,
        moof_offset,
        trafs,
    })
}

fn parse_mfra(payload: &[u8]) -> u32 {
    child(payload, b"tfra")
        .and_then(|tfra| be32(tfra, 12))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_and_truncation_never_panic() {
        assert!(read_summary(&[]).is_err());
        assert!(read_summary(b"not an mp4 at all").is_err());
        // A header claiming more than there is.
        assert!(read_summary(&[0, 0, 1, 0, b'f', b't', b'y', b'p']).is_err());
        // Size 0 means "to the end"; size 1 wants a large size that is missing.
        let s = read_summary(&[0, 0, 0, 0, b'f', b'r', b'e', b'e', 1, 2]).unwrap();
        assert!(s.brands.is_empty());
        assert!(read_summary(&[0, 0, 0, 1, b'm', b'o', b'o', b'v']).is_err());
        for cut in 0..64 {
            let mut bytes = vec![
                0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 2, 0,
            ];
            bytes.extend_from_slice(&[0, 0, 0, 40, b'm', b'o', b'o', b'v']);
            bytes.extend((0..32).map(|i| (i * 7) as u8));
            let _ = read_summary(&bytes[..cut.min(bytes.len())]);
        }
    }
}
