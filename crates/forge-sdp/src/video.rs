//! Video in SDP: codec identification, `a=rtcp-fb` feedback attributes
//! (RFC 4585 §4.2), H.264 `fmtp` parameters (RFC 6184 §8.1), the
//! `a=content` attribute that tells a camera section from a shared
//! screen (RFC 4796) and the `a=label` a floor control protocol names a
//! section by (RFC 4574).
//!
//! Forge negotiates video it forwards without decoding, so what matters
//! here is naming the codec, agreeing on the feedback a receiver may send
//! (PLI / FIR for keyframes, NACK, REMB) and reading the H.264 profile so
//! two parties are not paired across incompatible profiles.

use crate::{Attribute, CodecInfo, MediaDescription, MediaType, SessionDescription};
use forge_core::VideoCodec;
use smol_str::SmolStr;

impl CodecInfo {
    /// Convert to a forge-core video codec, if this is one.
    pub fn to_video_codec(&self) -> Option<VideoCodec> {
        VideoCodec::from_sdp_name(&self.encoding_name)
    }

    /// Whether this is a video codec forge knows.
    pub fn is_video(&self) -> bool {
        self.to_video_codec().is_some()
    }
}

/// One `a=rtcp-fb` line: `a=rtcp-fb:<pt|*> <type> [<param>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcpFeedbackAttr {
    /// Payload type the feedback applies to; `None` for `*` (all).
    pub payload_type: Option<u8>,
    /// `nack`, `ccm`, `goog-remb`, `transport-cc`, …
    pub kind: SmolStr,
    /// `pli`, `fir`, `sli`, …
    pub param: Option<SmolStr>,
}

impl RtcpFeedbackAttr {
    pub fn is_pli(&self) -> bool {
        self.kind == "nack" && self.param.as_deref() == Some("pli")
    }
    pub fn is_fir(&self) -> bool {
        self.kind == "ccm" && self.param.as_deref() == Some("fir")
    }
    /// Generic NACK (`nack` with no parameter).
    pub fn is_nack(&self) -> bool {
        self.kind == "nack" && self.param.is_none()
    }
    pub fn is_remb(&self) -> bool {
        self.kind == "goog-remb"
    }

    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split_whitespace();
        let pt = parts.next()?;
        let payload_type = if pt == "*" {
            None
        } else {
            Some(pt.parse().ok()?)
        };
        let kind = SmolStr::new(parts.next()?);
        let param = parts.next().map(SmolStr::new);
        Some(Self {
            payload_type,
            kind,
            param,
        })
    }

    fn value(&self) -> String {
        let pt = match self.payload_type {
            Some(pt) => pt.to_string(),
            None => "*".to_string(),
        };
        match &self.param {
            Some(p) => format!("{pt} {} {p}", self.kind),
            None => format!("{pt} {}", self.kind),
        }
    }
}

/// H.264 `fmtp` parameters forge cares about (RFC 6184 §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct H264Fmtp {
    /// `profile-level-id`, six hex digits: profile_idc, constraint flags,
    /// level_idc. `None` means Baseline level 1 per the RFC default.
    pub profile_level_id: Option<u32>,
    /// `packetization-mode`: 0 single NAL, 1 non-interleaved (FU-A /
    /// STAP-A), 2 interleaved. Default 0.
    pub packetization_mode: u8,
}

impl H264Fmtp {
    /// Parse the parameter string of an `a=fmtp` line.
    pub fn parse(params: &str) -> Self {
        let mut out = Self::default();
        for kv in params.split(';') {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k.trim() {
                "profile-level-id" => out.profile_level_id = u32::from_str_radix(v.trim(), 16).ok(),
                "packetization-mode" => out.packetization_mode = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
        out
    }

    /// The `profile_idc` byte (66 Baseline, 77 Main, 100 High, …).
    pub fn profile_idc(&self) -> u8 {
        self.profile_level_id.map(|p| (p >> 16) as u8).unwrap_or(66)
    }

    /// The `level_idc` byte (e.g. 31 = level 3.1).
    pub fn level_idc(&self) -> u8 {
        self.profile_level_id.map(|p| p as u8).unwrap_or(10)
    }

    /// Whether the constrained-baseline flag (constraint_set1) is set, or
    /// the profile is plain Baseline: the subset every H.264 decoder
    /// handles.
    pub fn is_baseline_compatible(&self) -> bool {
        match self.profile_level_id {
            None => true,
            Some(p) => {
                let idc = (p >> 16) as u8;
                let constraints = (p >> 8) as u8;
                idc == 66 || constraints & 0x40 != 0
            }
        }
    }

    /// Whether a stream described by `self` can be forwarded to a decoder
    /// described by `other` untouched: same packetization mode and a
    /// profile the receiver accepts (its own, or anything baseline-
    /// compatible).
    pub fn forwardable_to(&self, other: &H264Fmtp) -> bool {
        self.packetization_mode == other.packetization_mode
            && (self.profile_idc() == other.profile_idc() || self.is_baseline_compatible())
    }

    /// Render as `a=fmtp` parameters.
    pub fn to_params(&self) -> String {
        match self.profile_level_id {
            Some(p) => format!(
                "profile-level-id={p:06x};packetization-mode={}",
                self.packetization_mode
            ),
            None => format!("packetization-mode={}", self.packetization_mode),
        }
    }
}

/// What a media section carries, per RFC 4796 `a=content`. A section
/// may name more than one (`a=content:slides,main`); [`contents`] returns
/// them all and [`is_slides`] answers the question a mixer asks.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Content {
    /// A shared screen, a document, a presentation.
    Slides,
    /// The person talking (a second camera on the presenter).
    Speaker,
    /// Sign language.
    Sl,
    /// The main picture: what a single-section endpoint sends.
    Main,
    /// An alternative view of the same.
    Alt,
    /// A token this crate does not know.
    Other(SmolStr),
}

impl Content {
    pub fn parse(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "slides" => Content::Slides,
            "speaker" => Content::Speaker,
            "sl" => Content::Sl,
            "main" => Content::Main,
            "alt" => Content::Alt,
            other => Content::Other(SmolStr::new(other)),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Content::Slides => "slides",
            Content::Speaker => "speaker",
            Content::Sl => "sl",
            Content::Main => "main",
            Content::Alt => "alt",
            Content::Other(s) => s.as_str(),
        }
    }
}

/// Every `a=content` token on a section, in order; empty when there is
/// no such attribute, which is what a camera-only endpoint sends.
pub fn contents(media: &MediaDescription) -> Vec<Content> {
    media
        .attributes
        .iter()
        .filter_map(|a| match a {
            Attribute::Value { name, value } if name == "content" => Some(value),
            _ => None,
        })
        .flat_map(|v| v.split(',').map(Content::parse).collect::<Vec<_>>())
        .collect()
}

/// Whether a section is a shared screen (`a=content:slides`).
pub fn is_slides(media: &MediaDescription) -> bool {
    contents(media).contains(&Content::Slides)
}

/// Set (or replace) the section's `a=content`.
pub fn set_content(media: &mut MediaDescription, content: &[Content]) {
    media
        .attributes
        .retain(|a| !matches!(a, Attribute::Value { name, .. } if name == "content"));
    if content.is_empty() {
        return;
    }
    let value = content
        .iter()
        .map(Content::as_str)
        .collect::<Vec<_>>()
        .join(",");
    media.attributes.push(Attribute::Value {
        name: SmolStr::new("content"),
        value: SmolStr::new(value),
    });
}

/// The section's `a=label` (RFC 4574), by which BFCP's `a=floorid`
/// names the media stream a floor controls.
pub fn label(media: &MediaDescription) -> Option<&str> {
    media.attributes.iter().find_map(|a| match a {
        Attribute::Value { name, value } if name == "label" => Some(value.as_str()),
        _ => None,
    })
}

/// Set (or replace) the section's `a=label`.
pub fn set_label(media: &mut MediaDescription, label: &str) {
    media
        .attributes
        .retain(|a| !matches!(a, Attribute::Value { name, .. } if name == "label"));
    media.attributes.push(Attribute::Value {
        name: SmolStr::new("label"),
        value: SmolStr::new(label),
    });
}

/// The video sections of an offer a mixer cares about: the camera and,
/// when the endpoint can share one, the screen.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct VideoSections<'a> {
    /// The first active video section that is not `a=content:slides`.
    pub camera: Option<&'a MediaDescription>,
    /// The first active video section that is.
    pub content: Option<&'a MediaDescription>,
}

/// Sort an SDP's active video sections into a camera and a content
/// section (RFC 4796). Anything past those two is nobody's business
/// here and is for the caller to reject.
pub fn video_sections(sdp: &SessionDescription) -> VideoSections<'_> {
    let mut out = VideoSections::default();
    for m in sdp
        .media
        .iter()
        .filter(|m| m.media_type == MediaType::Video && m.port != 0)
    {
        if is_slides(m) {
            if out.content.is_none() {
                out.content = Some(m);
            }
        } else if out.camera.is_none() {
            out.camera = Some(m);
        }
    }
    out
}

/// Video-related accessors on a media description.
pub trait VideoAttributesExt {
    /// Every `a=rtcp-fb` line.
    fn rtcp_fb_iter(&self) -> Vec<RtcpFeedbackAttr>;
    /// The feedback lines that apply to `payload_type` (its own and `*`).
    fn rtcp_fb_for(&self, payload_type: u8) -> Vec<RtcpFeedbackAttr>;
    /// Whether the peer accepts PLI (`nack pli`) for `payload_type`.
    fn supports_pli(&self, payload_type: u8) -> bool;
    /// Whether the peer accepts FIR (`ccm fir`) for `payload_type`.
    fn supports_fir(&self, payload_type: u8) -> bool;
    /// Add an `a=rtcp-fb` line (no duplicates).
    fn add_rtcp_fb(&mut self, attr: RtcpFeedbackAttr);
    /// Add the feedback forge's forwarder relies on for `payload_type`:
    /// `nack`, `nack pli`, `ccm fir`.
    fn add_forwarding_feedback(&mut self, payload_type: u8);
    /// H.264 parameters for `payload_type` (defaults when there is no
    /// fmtp line).
    fn h264_fmtp(&self, payload_type: u8) -> H264Fmtp;
    /// Video codecs offered on this description, in format order.
    fn video_codecs(&self) -> Vec<(CodecInfo, VideoCodec)>;
}

impl VideoAttributesExt for MediaDescription {
    fn rtcp_fb_iter(&self) -> Vec<RtcpFeedbackAttr> {
        self.attributes
            .iter()
            .filter_map(|a| match a {
                Attribute::Value { name, value } if name == "rtcp-fb" => {
                    RtcpFeedbackAttr::parse(value)
                }
                _ => None,
            })
            .collect()
    }

    fn rtcp_fb_for(&self, payload_type: u8) -> Vec<RtcpFeedbackAttr> {
        self.rtcp_fb_iter()
            .into_iter()
            .filter(|fb| fb.payload_type.map_or(true, |pt| pt == payload_type))
            .collect()
    }

    fn supports_pli(&self, payload_type: u8) -> bool {
        self.rtcp_fb_for(payload_type).iter().any(|fb| fb.is_pli())
    }

    fn supports_fir(&self, payload_type: u8) -> bool {
        self.rtcp_fb_for(payload_type).iter().any(|fb| fb.is_fir())
    }

    fn add_rtcp_fb(&mut self, attr: RtcpFeedbackAttr) {
        if self.rtcp_fb_iter().contains(&attr) {
            return;
        }
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("rtcp-fb"),
            value: SmolStr::new(attr.value()),
        });
    }

    fn add_forwarding_feedback(&mut self, payload_type: u8) {
        for (kind, param) in [("nack", None), ("nack", Some("pli")), ("ccm", Some("fir"))] {
            self.add_rtcp_fb(RtcpFeedbackAttr {
                payload_type: Some(payload_type),
                kind: SmolStr::new(kind),
                param: param.map(SmolStr::new),
            });
        }
    }

    fn h264_fmtp(&self, payload_type: u8) -> H264Fmtp {
        self.fmtp_for(payload_type)
            .map(|f| H264Fmtp::parse(f.params.as_str()))
            .unwrap_or_default()
    }

    fn video_codecs(&self) -> Vec<(CodecInfo, VideoCodec)> {
        if self.media_type != MediaType::Video {
            return Vec::new();
        }
        crate::helpers::extract_codecs(self)
            .into_iter()
            .filter_map(|c| c.to_video_codec().map(|v| (c, v)))
            .collect()
    }
}

/// Media direction of a section, from its property attributes;
/// `sendrecv` when none is present (RFC 4566 §6).
pub fn direction_of(media: &MediaDescription) -> &'static str {
    for a in &media.attributes {
        if let Attribute::Property(p) = a {
            match p.as_str() {
                "sendonly" => return "sendonly",
                "recvonly" => return "recvonly",
                "inactive" => return "inactive",
                "sendrecv" => return "sendrecv",
                _ => {}
            }
        }
    }
    "sendrecv"
}

/// The direction an answer takes for an offered direction (RFC 3264
/// §6.1), given what we are willing to do ourselves.
pub fn answer_direction(offered: &str, ours: &str) -> &'static str {
    let can_send = matches!(ours, "sendrecv" | "sendonly");
    let can_recv = matches!(ours, "sendrecv" | "recvonly");
    match offered {
        "sendonly" => {
            if can_recv {
                "recvonly"
            } else {
                "inactive"
            }
        }
        "recvonly" => {
            if can_send {
                "sendonly"
            } else {
                "inactive"
            }
        }
        "inactive" => "inactive",
        _ => match (can_send, can_recv) {
            (true, true) => "sendrecv",
            (true, false) => "sendonly",
            (false, true) => "recvonly",
            (false, false) => "inactive",
        },
    }
}

/// Feedback types forge's forwarder and mixer act on; anything else the
/// peer offers is left out of the answer.
const SUPPORTED_FEEDBACK: [(&str, Option<&str>); 4] = [
    ("nack", None),
    ("nack", Some("pli")),
    ("ccm", Some("fir")),
    ("goog-remb", None),
];

/// Answer an offered video section with one codec (RFC 3264 §6): the
/// offer's payload type, protocol and `mid`, our port, the offered
/// `fmtp` for that codec echoed, the offered feedback types we support,
/// `rtcp-mux` if offered, and the answer direction for `ours`.
pub fn answer_video(
    offer: &MediaDescription,
    chosen: &CodecInfo,
    local_port: u16,
    ours: &str,
) -> MediaDescription {
    let mut m = MediaDescription::video(local_port);
    m.protocol = offer.protocol.clone();
    let pt = chosen.payload_type;
    m.formats.push(SmolStr::new(pt.to_string()));
    let rtpmap = crate::RtpMap {
        payload_type: pt,
        encoding_name: SmolStr::new(&chosen.encoding_name),
        clock_rate: chosen.clock_rate,
        encoding_params: None,
    };
    m.attributes.push(Attribute::Value {
        name: SmolStr::new("rtpmap"),
        value: SmolStr::new(format!(
            "{pt} {}/{}",
            chosen.encoding_name, chosen.clock_rate
        )),
    });
    m.rtpmaps.insert(pt, rtpmap);
    if let Some(f) = offer.fmtp_for(pt) {
        m.set_fmtp(pt, f.params.as_str());
    }
    for fb in offer.rtcp_fb_for(pt) {
        let supported = SUPPORTED_FEEDBACK
            .iter()
            .any(|(k, p)| fb.kind == *k && fb.param.as_deref() == *p);
        if supported {
            m.add_rtcp_fb(RtcpFeedbackAttr {
                payload_type: Some(pt),
                ..fb
            });
        }
    }
    if let Some(mid) = offer.mid() {
        m.set_mid(mid);
    }
    // What the section carries and what a floor controller calls it are
    // the offerer's to say and the answerer's to repeat (RFC 4796 §5,
    // RFC 4574 §4).
    let content = contents(offer);
    if !content.is_empty() {
        set_content(&mut m, &content);
    }
    if let Some(l) = label(offer) {
        set_label(&mut m, l);
    }
    if offer
        .attributes
        .iter()
        .any(|a| matches!(a, Attribute::Property(p) if p == "rtcp-mux"))
    {
        m.attributes
            .push(Attribute::Property(SmolStr::new("rtcp-mux")));
    }
    m.attributes
        .push(Attribute::Property(SmolStr::new(answer_direction(
            direction_of(offer),
            ours,
        ))));
    m
}

/// Reject an offered section (RFC 3264 §6, RFC 8843 §7.3): port 0, the
/// offer's protocol and formats, its `mid`, `a=inactive`. This is what an
/// answer must carry for every offered section it does not accept — an
/// answer with fewer sections than the offer is invalid.
pub fn reject_section(offer: &MediaDescription) -> MediaDescription {
    let mut m = MediaDescription {
        media_type: offer.media_type.clone(),
        port: 0,
        num_ports: None,
        protocol: offer.protocol.clone(),
        formats: if offer.formats.is_empty() {
            vec![SmolStr::new("0")]
        } else {
            offer.formats.clone()
        },
        title: None,
        connection: None,
        bandwidth: Vec::new(),
        encryption_key: None,
        attributes: Vec::new(),
        rtpmaps: Default::default(),
    };
    if let Some(mid) = offer.mid() {
        m.set_mid(mid);
    }
    m.attributes
        .push(Attribute::Property(SmolStr::new("inactive")));
    m
}

/// The first video section of an SDP that is not rejected (port 0).
pub fn active_video(sdp: &SessionDescription) -> Option<&MediaDescription> {
    sdp.media
        .iter()
        .find(|m| m.media_type == MediaType::Video && m.port != 0)
}

/// Pick the codec to use with a peer whose video section is `offer`, in
/// `preference` order; falls back to the offer's own order when nothing
/// in `preference` matches. `None` when the offer has no known video
/// codec.
pub fn choose_video_codec(
    offer: &MediaDescription,
    preference: &[VideoCodec],
) -> Option<(CodecInfo, VideoCodec)> {
    let offered = offer.video_codecs();
    preference
        .iter()
        .find_map(|want| offered.iter().find(|(_, have)| have == want).cloned())
        .or_else(|| offered.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionDescriptionExt;

    const OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\nc=IN IP4 10.0.0.1\r\nt=0 0\r\n\
m=audio 4000 RTP/AVP 0 101\r\n\
a=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\n\
m=video 4002 RTP/AVP 97 96\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 profile-level-id=42e01f;packetization-mode=1\r\n\
a=rtpmap:97 VP8/90000\r\n\
a=rtcp-fb:96 nack\r\na=rtcp-fb:96 nack pli\r\na=rtcp-fb:96 ccm fir\r\n\
a=rtcp-fb:* goog-remb\r\n\
a=sendrecv\r\n";

    #[test]
    fn video_codecs_and_feedback_are_read_from_the_video_section() {
        let sdp = SessionDescription::from_str(OFFER).unwrap();
        let video = active_video(&sdp).unwrap();
        let codecs = video.video_codecs();
        assert_eq!(
            codecs
                .iter()
                .map(|(c, v)| (c.payload_type, *v))
                .collect::<Vec<_>>(),
            vec![(97, VideoCodec::VP8), (96, VideoCodec::H264)]
        );
        assert!(video.supports_pli(96));
        assert!(video.supports_fir(96));
        assert!(!video.supports_pli(97));
        // `*` applies to every payload type.
        assert!(video.rtcp_fb_for(97).iter().any(|fb| fb.is_remb()));
        assert!(video.rtcp_fb_for(96).iter().any(|fb| fb.is_nack()));
        // The audio section has no video codecs.
        assert!(sdp.media[0].video_codecs().is_empty());
        assert!(active_video(
            &SessionDescription::from_str(
                "v=0\r\no=- 1 1 IN IP4 1.1.1.1\r\ns=-\r\nt=0 0\r\nm=video 0 RTP/AVP 96\r\n"
            )
            .unwrap()
        )
        .is_none());
    }

    #[test]
    fn codec_choice_follows_preference_then_the_offer() {
        let sdp = SessionDescription::from_str(OFFER).unwrap();
        let video = active_video(&sdp).unwrap();
        let (info, codec) =
            choose_video_codec(video, &[VideoCodec::H264, VideoCodec::VP8]).unwrap();
        assert_eq!((info.payload_type, codec), (96, VideoCodec::H264));
        let (info, codec) = choose_video_codec(video, &[VideoCodec::AV1]).unwrap();
        assert_eq!((info.payload_type, codec), (97, VideoCodec::VP8));
    }

    #[test]
    fn h264_fmtp_parses_profile_and_packetization() {
        let sdp = SessionDescription::from_str(OFFER).unwrap();
        let video = active_video(&sdp).unwrap();
        let f = video.h264_fmtp(96);
        assert_eq!(f.profile_level_id, Some(0x42e01f));
        assert_eq!(f.profile_idc(), 66);
        assert_eq!(f.level_idc(), 0x1f);
        assert_eq!(f.packetization_mode, 1);
        assert!(f.is_baseline_compatible());
        assert_eq!(
            f.to_params(),
            "profile-level-id=42e01f;packetization-mode=1"
        );
        // Missing fmtp: RFC defaults.
        let d = video.h264_fmtp(97);
        assert_eq!(d, H264Fmtp::default());
        assert_eq!(d.packetization_mode, 0);
        assert!(d.is_baseline_compatible());

        // Constrained High (64 = constraint_set1) is baseline compatible;
        // plain High is not.
        assert!(H264Fmtp::parse("profile-level-id=64401f").is_baseline_compatible());
        let high = H264Fmtp::parse("profile-level-id=640028;packetization-mode=1");
        assert!(!high.is_baseline_compatible());
        assert_eq!(high.profile_idc(), 100);
        // Forwarding: baseline → anything with the same packetization; High → only High.
        let cb = H264Fmtp::parse("profile-level-id=42e01f;packetization-mode=1");
        assert!(cb.forwardable_to(&high));
        assert!(!high.forwardable_to(&cb));
        assert!(high.forwardable_to(&high));
        assert!(!cb.forwardable_to(&H264Fmtp::parse("profile-level-id=42e01f")));
    }

    #[test]
    fn answer_mirrors_the_offer_and_keeps_only_supported_feedback() {
        let sdp = SessionDescription::from_str(OFFER).unwrap();
        let video = active_video(&sdp).unwrap();
        let (info, _) = choose_video_codec(video, &[VideoCodec::H264]).unwrap();
        let a = answer_video(video, &info, 5010, "sendrecv");
        assert_eq!(a.media_type, MediaType::Video);
        assert_eq!(a.port, 5010);
        assert_eq!(a.protocol, video.protocol);
        assert_eq!(a.formats, vec![SmolStr::new("96")]);
        assert_eq!(a.rtpmaps[&96].encoding_name, "H264");
        assert_eq!(a.h264_fmtp(96).profile_level_id, Some(0x42e01f));
        let fb: Vec<String> = a.rtcp_fb_iter().iter().map(|f| f.value()).collect();
        assert_eq!(
            fb,
            vec!["96 nack", "96 nack pli", "96 ccm fir", "96 goog-remb"]
        );
        assert_eq!(direction_of(&a), "sendrecv");
        assert!(a.mid().is_none());
        // The peer only sends: we only receive. An unsupported feedback
        // type in the offer is dropped, and mid / rtcp-mux are mirrored.
        let offer = "v=0\r\no=- 1 1 IN IP4 10.0.0.1\r\ns=-\r\nc=IN IP4 10.0.0.1\r\nt=0 0\r\n\
m=video 4002 RTP/AVP 97\r\na=rtpmap:97 VP8/90000\r\na=rtcp-fb:97 nack pli\r\n\
a=rtcp-fb:97 transport-cc\r\na=mid:1\r\na=rtcp-mux\r\na=sendonly\r\n";
        let sdp = SessionDescription::from_str(offer).unwrap();
        let video = active_video(&sdp).unwrap();
        let (info, _) = choose_video_codec(video, &[VideoCodec::VP8]).unwrap();
        let a = answer_video(video, &info, 6000, "sendrecv");
        assert_eq!(direction_of(&a), "recvonly");
        assert_eq!(a.mid(), Some("1"));
        assert!(a
            .attributes
            .iter()
            .any(|x| matches!(x, Attribute::Property(p) if p == "rtcp-mux")));
        let fb: Vec<String> = a.rtcp_fb_iter().iter().map(|f| f.value()).collect();
        assert_eq!(fb, vec!["97 nack pli"]);
        assert_eq!(
            direction_of(&answer_video(video, &info, 6000, "sendonly")),
            "inactive"
        );
    }

    #[test]
    fn answer_direction_follows_rfc_3264() {
        assert_eq!(answer_direction("sendrecv", "sendrecv"), "sendrecv");
        assert_eq!(answer_direction("sendrecv", "recvonly"), "recvonly");
        assert_eq!(answer_direction("sendonly", "sendrecv"), "recvonly");
        assert_eq!(answer_direction("recvonly", "sendrecv"), "sendonly");
        assert_eq!(answer_direction("recvonly", "recvonly"), "inactive");
        assert_eq!(answer_direction("inactive", "sendrecv"), "inactive");
    }

    #[test]
    fn rejected_sections_keep_their_place_in_the_answer() {
        let sdp = SessionDescription::from_str(OFFER).unwrap();
        let r = reject_section(&sdp.media[1]);
        assert_eq!(r.media_type, MediaType::Video);
        assert_eq!(r.port, 0);
        assert_eq!(r.protocol, sdp.media[1].protocol);
        assert_eq!(r.formats, sdp.media[1].formats);
        assert_eq!(direction_of(&r), "inactive");
        assert!(r.rtpmaps.is_empty());
        let mut bare = MediaDescription::video(9);
        bare.formats.clear();
        bare.set_mid("v");
        let r = reject_section(&bare);
        assert_eq!(r.formats, vec![SmolStr::new("0")]);
        assert_eq!(r.mid(), Some("v"));
    }

    #[test]
    fn a_shared_screen_is_told_from_a_camera_by_its_content_attribute() {
        // A room system: audio, a camera, a slides section with a label
        // for BFCP, and a fourth section nobody wants.
        let offer = "v=0\r\no=- 1 1 IN IP4 10.0.0.1\r\ns=-\r\nc=IN IP4 10.0.0.1\r\nt=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
m=video 4002 RTP/AVPF 96\r\na=rtpmap:96 H264/90000\r\na=content:main\r\na=label:1\r\n\
m=video 4004 RTP/AVPF 96\r\na=rtpmap:96 H264/90000\r\na=content:slides,main\r\na=label:3\r\n\
a=sendonly\r\n\
m=video 4006 RTP/AVPF 96\r\na=rtpmap:96 H264/90000\r\na=content:alt\r\n";
        let sdp = SessionDescription::from_str(offer).unwrap();
        let v = video_sections(&sdp);
        assert_eq!(v.camera.unwrap().port, 4002);
        assert_eq!(v.content.unwrap().port, 4004);
        assert_eq!(contents(&sdp.media[1]), vec![Content::Main]);
        assert_eq!(
            contents(&sdp.media[2]),
            vec![Content::Slides, Content::Main]
        );
        assert!(is_slides(&sdp.media[2]) && !is_slides(&sdp.media[1]));
        assert_eq!(label(&sdp.media[2]), Some("3"));
        assert_eq!(contents(&sdp.media[0]), vec![]);
        assert_eq!(
            Content::parse("Whiteboard"),
            Content::Other("whiteboard".into())
        );

        // The answer repeats content and label, and the direction rule
        // still applies: they only send, we only receive.
        let (info, _) = choose_video_codec(v.content.unwrap(), &[VideoCodec::H264]).unwrap();
        let a = answer_video(v.content.unwrap(), &info, 7000, "sendrecv");
        assert!(is_slides(&a));
        assert_eq!(label(&a), Some("3"));
        assert_eq!(direction_of(&a), "recvonly");
        // `active_video` still means the first active section, whatever it carries.
        assert_eq!(active_video(&sdp).unwrap().port, 4002);

        // A phone with no content attribute at all: camera only.
        let sdp = SessionDescription::from_str(OFFER).unwrap();
        let v = video_sections(&sdp);
        assert!(v.camera.is_some() && v.content.is_none());
        // A content-only device (no camera): the slides are still found.
        let only = "v=0\r\no=- 1 1 IN IP4 10.0.0.1\r\ns=-\r\nc=IN IP4 10.0.0.1\r\nt=0 0\r\n\
m=video 0 RTP/AVP 96\r\n\
m=video 4004 RTP/AVP 96\r\na=rtpmap:96 VP8/90000\r\na=content:slides\r\n";
        let sdp = SessionDescription::from_str(only).unwrap();
        let v = video_sections(&sdp);
        assert!(v.camera.is_none());
        assert_eq!(v.content.unwrap().port, 4004);

        // Setting and clearing.
        let mut m = MediaDescription::video(9);
        set_content(&mut m, &[Content::Slides]);
        set_content(&mut m, &[Content::Slides, Content::Alt]);
        assert_eq!(contents(&m), vec![Content::Slides, Content::Alt]);
        assert_eq!(
            m.attributes
                .iter()
                .filter(|a| matches!(a, Attribute::Value { name, .. } if name == "content"))
                .count(),
            1
        );
        set_label(&mut m, "7");
        set_label(&mut m, "8");
        assert_eq!(label(&m), Some("8"));
        set_content(&mut m, &[]);
        assert!(contents(&m).is_empty());
    }

    #[test]
    fn feedback_lines_are_added_once() {
        let mut media = MediaDescription::video(4002);
        media.add_forwarding_feedback(96);
        media.add_forwarding_feedback(96);
        let lines: Vec<String> = media
            .attributes
            .iter()
            .filter_map(|a| match a {
                Attribute::Value { name, value } if name == "rtcp-fb" => Some(value.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(lines, vec!["96 nack", "96 nack pli", "96 ccm fir"]);
        assert!(media.supports_pli(96) && media.supports_fir(96));
    }
}
