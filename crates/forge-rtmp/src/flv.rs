//! FLV: the tag bodies RTMP carries as its audio, video and data messages,
//! and the file around them (`video/x-flv`, Adobe's FLV and F4V
//! specification, version 10.1, annex E).
//!
//! - Video is H.264: an AVC sequence header (the `avcC` record) before the
//!   first picture and whenever the parameter sets change, then each picture
//!   as length-prefixed NAL units. The composition time is always 0, since
//!   the composite has no B-frames.
//! - Audio is AAC: a sequence header (the `AudioSpecificConfig`), then raw
//!   access units.

use crate::RtmpError;
use forge_mp4::codec::{annex_b_to_sample, avcc, parameter_sets};
use forge_mp4::Mp4VideoCodec;

/// The tag types, as RTMP's message types are numbered too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Audio = 8,
    Video = 9,
    Script = 18,
}

impl TagKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            8 => Some(TagKind::Audio),
            9 => Some(TagKind::Video),
            18 => Some(TagKind::Script),
            _ => None,
        }
    }
}

/// One tag: what it is, when (milliseconds), and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub kind: TagKind,
    pub timestamp: u32,
    pub body: Vec<u8>,
}

/// AVC (codec 7), key or inter frame, sequence header or NAL units.
const AVC: u8 = 7;
/// AAC (10) at the flags AAC always carries: 44 kHz, 16-bit, stereo. The
/// real rate and channels are the `AudioSpecificConfig`'s.
const AAC_FLAGS: u8 = (10 << 4) | (3 << 2) | (1 << 1) | 1;

/// The AVC sequence header's body.
pub fn avc_sequence_header(avcc: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(5 + avcc.len());
    b.push((1 << 4) | AVC);
    b.extend_from_slice(&[0, 0, 0, 0]); // sequence header, composition time 0
    b.extend_from_slice(avcc);
    b
}

/// A picture's body: `sample` is length-prefixed NAL units.
pub fn avc_picture(keyframe: bool, sample: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(5 + sample.len());
    b.push((if keyframe { 1 } else { 2 } << 4) | AVC);
    b.extend_from_slice(&[1, 0, 0, 0]); // NAL units, composition time 0
    b.extend_from_slice(sample);
    b
}

/// The AAC sequence header's body.
pub fn aac_sequence_header(asc: &[u8]) -> Vec<u8> {
    let mut b = vec![AAC_FLAGS, 0];
    b.extend_from_slice(asc);
    b
}

/// An access unit's body.
pub fn aac_frame(unit: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + unit.len());
    b.extend_from_slice(&[AAC_FLAGS, 1]);
    b.extend_from_slice(unit);
    b
}

/// What `onMetaData` says of the stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub framerate: Option<f64>,
    /// Kilobits a second.
    pub video_kbps: Option<u32>,
    pub audio: Option<(u32, u16)>,
    pub audio_kbps: Option<u32>,
    pub encoder: String,
}

impl Metadata {
    /// The ECMA array `onMetaData` carries.
    pub fn to_amf(&self) -> crate::amf0::Value {
        use crate::amf0::Value;
        let mut p: Vec<(String, Value)> = Vec::new();
        let mut num = |k: &str, v: f64| p.push((k.into(), Value::Number(v)));
        if let Some(w) = self.width {
            num("width", w as f64);
        }
        if let Some(h) = self.height {
            num("height", h as f64);
        }
        if self.width.is_some() || self.height.is_some() {
            num("videocodecid", AVC as f64);
        }
        if let Some(f) = self.framerate {
            num("framerate", f);
        }
        if let Some(k) = self.video_kbps {
            num("videodatarate", k as f64);
        }
        if let Some((rate, _)) = self.audio {
            num("audiocodecid", 10.0);
            num("audiosamplerate", rate as f64);
            num("audiosamplesize", 16.0);
        }
        if let Some(k) = self.audio_kbps {
            num("audiodatarate", k as f64);
        }
        if let Some((_, channels)) = self.audio {
            p.push(("stereo".into(), Value::Boolean(channels > 1)));
        }
        p.push(("encoder".into(), Value::str(self.encoder.clone())));
        Value::EcmaArray(p)
    }

    /// The script tag's body: `onMetaData` and the array. (Over RTMP it is
    /// sent as `@setDataFrame` first; see the publisher.)
    pub fn script_body(&self) -> Vec<u8> {
        crate::amf0::encode(&[crate::amf0::Value::str("onMetaData"), self.to_amf()])
    }
}

/// H.264 pictures, as they come from an encoder in Annex B, made into
/// video tag bodies: the sequence header first and again whenever a
/// keyframe's parameter sets differ, and nothing before the first keyframe.
#[derive(Debug, Default)]
pub struct AvcTagger {
    config: Option<Vec<u8>>,
}

impl AvcTagger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the sequence header, so the next keyframe sends it again: a
    /// new connection needs it.
    pub fn reset(&mut self) {
        self.config = None;
    }

    /// Whether a sequence header has been made.
    pub fn started(&self) -> bool {
        self.config.is_some()
    }

    /// The bodies for one picture: none while waiting for a keyframe.
    pub fn bodies(&mut self, keyframe: bool, annex_b: &[u8]) -> Result<Vec<Vec<u8>>, RtmpError> {
        let mut out = Vec::new();
        if keyframe {
            let ps = parameter_sets(Mp4VideoCodec::H264, annex_b);
            if !ps.first.is_empty() {
                let record =
                    avcc(&ps).map_err(|e| RtmpError::Media(format!("the keyframe's {e}")))?;
                if self.config.as_ref() != Some(&record) {
                    out.push(avc_sequence_header(&record));
                    self.config = Some(record);
                }
            }
        }
        if self.config.is_none() {
            return Ok(out);
        }
        let sample = annex_b_to_sample(Mp4VideoCodec::H264, annex_b);
        if !sample.is_empty() {
            out.push(avc_picture(keyframe, &sample));
        }
        Ok(out)
    }
}

/// The 9-byte file header and the first back-pointer, for a file with
/// `audio` and `video`.
pub fn file_header(audio: bool, video: bool) -> Vec<u8> {
    let flags = (audio as u8) << 2 | video as u8;
    vec![b'F', b'L', b'V', 1, flags, 0, 0, 0, 9, 0, 0, 0, 0]
}

/// A tag in a file: its 11-byte header, body and back-pointer.
pub fn file_tag(tag: &Tag) -> Vec<u8> {
    let len = tag.body.len() as u32;
    let ts = tag.timestamp;
    let mut b = Vec::with_capacity(15 + tag.body.len());
    b.push(tag.kind as u8);
    b.extend_from_slice(&len.to_be_bytes()[1..]);
    b.extend_from_slice(&ts.to_be_bytes()[1..]);
    b.push((ts >> 24) as u8);
    b.extend_from_slice(&[0, 0, 0]);
    b.extend_from_slice(&tag.body);
    b.extend_from_slice(&(len + 11).to_be_bytes());
    b
}

/// The tags of an FLV file.
pub fn parse_file(data: &[u8]) -> Result<Vec<Tag>, RtmpError> {
    if data.len() < 13 || &data[..3] != b"FLV" {
        return Err(RtmpError::Media("not an FLV file".into()));
    }
    let header = u32::from_be_bytes([data[5], data[6], data[7], data[8]]) as usize;
    let mut pos = header + 4;
    let mut tags = Vec::new();
    while pos + 11 <= data.len() {
        let kind = data[pos];
        let len = u32::from_be_bytes([0, data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let ts = u32::from_be_bytes([data[pos + 7], data[pos + 4], data[pos + 5], data[pos + 6]]);
        let body_at = pos + 11;
        let end = body_at + len;
        if end + 4 > data.len() {
            return Err(RtmpError::Media("FLV tag runs past the file".into()));
        }
        if let Some(kind) = TagKind::from_u8(kind) {
            tags.push(Tag {
                kind,
                timestamp: ts,
                body: data[body_at..end].to_vec(),
            });
        }
        pos = end + 4;
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x01, 0x40];
    const PPS: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

    fn annex_b(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        out
    }

    #[test]
    fn pictures_wait_for_a_keyframe_and_its_sequence_header() {
        let mut t = AvcTagger::new();
        let inter = annex_b(&[&[0x41, 0x9A, 0x01]]);
        assert!(
            t.bodies(false, &inter).unwrap().is_empty(),
            "nothing before a keyframe"
        );

        let key = annex_b(&[SPS, PPS, &[0x65, 0x88, 0x84]]);
        let bodies = t.bodies(true, &key).unwrap();
        assert_eq!(bodies.len(), 2, "the sequence header, then the picture");
        assert_eq!(&bodies[0][..5], &[0x17, 0, 0, 0, 0]);
        assert_eq!(bodies[0][5], 1, "an avcC record");
        assert_eq!(
            &bodies[0][6..9],
            &SPS[1..4],
            "with the SPS's profile and level"
        );
        assert_eq!(&bodies[1][..5], &[0x17, 1, 0, 0, 0]);
        assert_eq!(
            &bodies[1][5..],
            &[0, 0, 0, 3, 0x65, 0x88, 0x84],
            "the slice, length-prefixed, without its parameter sets"
        );

        let bodies = t.bodies(false, &inter).unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(&bodies[0][..2], &[0x27, 1], "an inter frame");
        assert_eq!(
            t.bodies(true, &key).unwrap().len(),
            1,
            "the same parameter sets: no header"
        );

        t.reset();
        assert_eq!(
            t.bodies(true, &key).unwrap().len(),
            2,
            "after a reset, the header again"
        );
    }

    #[test]
    fn audio_bodies_carry_the_aac_flags() {
        assert_eq!(
            aac_sequence_header(&[0x11, 0x88]),
            vec![0xAF, 0, 0x11, 0x88]
        );
        assert_eq!(aac_frame(&[1, 2]), vec![0xAF, 1, 1, 2]);
    }

    #[test]
    fn a_file_reads_back_its_tags() {
        let tags = vec![
            Tag {
                kind: TagKind::Script,
                timestamp: 0,
                body: Metadata::default().script_body(),
            },
            Tag {
                kind: TagKind::Video,
                timestamp: 0x0123_4567,
                body: vec![0x17, 0, 0, 0, 0],
            },
            Tag {
                kind: TagKind::Audio,
                timestamp: 21,
                body: aac_frame(&[9]),
            },
        ];
        let mut file = file_header(true, true);
        for t in &tags {
            file.extend(file_tag(t));
        }
        assert_eq!(file[4], 5, "audio and video");
        assert_eq!(
            parse_file(&file).unwrap(),
            tags,
            "an extended timestamp too"
        );
        assert!(parse_file(&file[..file.len() - 3]).is_err());
    }
}
