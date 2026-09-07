//! SDP construction and inspection for the endpoint-shaped peer connection.
//!
//! The peer connection negotiates one audio section (Opus, G.722 and/or
//! G.711, optional telephone-event) and, when configured, one video
//! section (H.264, VP8, VP9, AV1 with `nack`, `nack pli`, `ccm fir` and
//! `goog-remb` feedback), BUNDLE + rtcp-mux, DTLS-SRTP and trickle ICE —
//! the shape every browser produces and the shape the DSIP WebRTC Media
//! Binding pins. Offers are built from scratch; answers mirror the remote
//! offer (payload types, `a=mid`, protocol, `fmtp`) and reject every
//! section they do not accept with port 0 so the answer stays a valid
//! RFC 3264 answer to a browser offer that carries more sections than we
//! take.
//!
//! Codec selection: the local preference list ([`LocalParams::codecs`],
//! [`LocalVideo::codecs`]) decides. An answer accepts exactly one codec
//! per section — the first local preference the remote offered — so the
//! negotiated codec is pinned deterministically rather than left to the
//! sender. G.711 (PCMU/PCMA) is mandatory-to-implement in WebRTC (RFC 7874
//! §3) and G.722 ships in every browser, so against a browser either can
//! be preferred to skip transcoding toward a SIP leg or a mixer that
//! speaks them.

use forge_core::{AudioCodec, VideoCodec};
use forge_ice::IceCandidate;
use forge_sdp::{
    Attribute, Connection, DtlsAttributesExt, DtlsSetup, IceAttributesExt, MediaDescription,
    MediaDtlsAttributesExt, MediaIceAttributesExt, MediaType, Protocol, RtcpFeedbackAttr, SdpError,
    SessionDescription, SessionDescriptionExt, VideoAttributesExt,
};
use smol_str::SmolStr;

use crate::{Result, WebRtcError};

/// Media direction (RFC 3264 §5.1 / RFC 4566 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Send and receive.
    #[default]
    SendRecv,
    /// Send only.
    SendOnly,
    /// Receive only (a DSIP screening answer).
    RecvOnly,
    /// Neither.
    Inactive,
}

impl Direction {
    /// SDP attribute name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::SendRecv => "sendrecv",
            Direction::SendOnly => "sendonly",
            Direction::RecvOnly => "recvonly",
            Direction::Inactive => "inactive",
        }
    }

    /// Parse an SDP direction attribute name.
    pub fn parse(s: &str) -> Option<Direction> {
        match s {
            "sendrecv" => Some(Direction::SendRecv),
            "sendonly" => Some(Direction::SendOnly),
            "recvonly" => Some(Direction::RecvOnly),
            "inactive" => Some(Direction::Inactive),
            _ => None,
        }
    }

    /// Whether this endpoint transmits under this direction.
    pub fn sends(&self) -> bool {
        matches!(self, Direction::SendRecv | Direction::SendOnly)
    }

    /// Whether this endpoint receives under this direction.
    pub fn receives(&self) -> bool {
        matches!(self, Direction::SendRecv | Direction::RecvOnly)
    }

    /// The direction an answer may carry given the offer's direction and what
    /// the answerer wants (RFC 3264 §6.1): an offer of `sendonly` can only be
    /// answered `recvonly`/`inactive`, and so on.
    pub fn answer_for(offer: Direction, want: Direction) -> Direction {
        let can_send = matches!(offer, Direction::SendRecv | Direction::RecvOnly) && want.sends();
        let can_recv = matches!(offer, Direction::SendRecv | Direction::SendOnly)
            && matches!(want, Direction::SendRecv | Direction::RecvOnly);
        match (can_send, can_recv) {
            (true, true) => Direction::SendRecv,
            (true, false) => Direction::SendOnly,
            (false, true) => Direction::RecvOnly,
            (false, false) => Direction::Inactive,
        }
    }
}

/// The RTP clock rate of an audio codec — its sample rate, except G.722,
/// which is clocked at 8 kHz in RTP for historical reasons (RFC 3551
/// §4.5.2) while sampling at 16 kHz.
pub fn rtp_clock(codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::G722 => 8_000,
        other => other.sample_rate(),
    }
}

/// The feedback messages this endpoint sends and acts on (RFC 4585 NACK
/// and PLI, RFC 5104 FIR, REMB). Anything else a peer offers — notably
/// `transport-cc`, which needs the RFC 8285 header extension this
/// endpoint does not write — is left out of the answer.
const SUPPORTED_FEEDBACK: [(&str, Option<&str>); 4] = [
    ("nack", None),
    ("nack", Some("pli")),
    ("ccm", Some("fir")),
    ("goog-remb", None),
];

fn feedback_supported(fb: &RtcpFeedbackAttr) -> bool {
    SUPPORTED_FEEDBACK
        .iter()
        .any(|(k, p)| fb.kind == *k && fb.param.as_deref() == *p)
}

fn feedback_attr(pt: u8, kind: &str, param: Option<&str>) -> RtcpFeedbackAttr {
    RtcpFeedbackAttr {
        payload_type: Some(pt),
        kind: SmolStr::new(kind),
        param: param.map(SmolStr::new),
    }
}

/// The video half of what goes into an offer or answer.
#[derive(Debug, Clone)]
pub struct LocalVideo<'a> {
    /// Our video sending SSRC.
    pub ssrc: u32,
    /// Codecs we offer (or are willing to answer with), in preference
    /// order, each with the payload type we use when offering it.
    pub codecs: &'a [(VideoCodec, u8)],
    /// Desired direction.
    pub direction: Direction,
    /// `a=mid` for our video section in an offer.
    pub mid: &'a str,
    /// The H.264 `profile-level-id` we offer (six hex digits).
    pub h264_profile_level_id: &'a str,
    /// Bitrate cap advertised as `b=AS` on the section, in kb/s.
    pub max_kbps: Option<u32>,
}

/// Everything local that goes into an offer or answer.
#[derive(Debug, Clone)]
pub struct LocalParams<'a> {
    /// ICE username fragment.
    pub ufrag: &'a str,
    /// ICE password.
    pub pwd: &'a str,
    /// SHA-256 DTLS certificate fingerprint (`AA:BB:…`).
    pub fingerprint: &'a str,
    /// `a=setup` value.
    pub setup: DtlsSetup,
    /// Candidates gathered so far (inlined as `a=candidate`).
    pub candidates: &'a [IceCandidate],
    /// Whether gathering has finished (`a=end-of-candidates`).
    pub end_of_candidates: bool,
    /// Our audio sending SSRC.
    pub ssrc: u32,
    /// RTCP CNAME.
    pub cname: &'a str,
    /// The `a=msid` stream id both sections share.
    pub msid: &'a str,
    /// Desired direction for the audio section.
    pub direction: Direction,
    /// Codecs we offer (or are willing to answer with), in preference
    /// order, each with the payload type we use when offering it. Only
    /// [`AudioCodec::Opus`], [`AudioCodec::G722`], [`AudioCodec::PCMU`]
    /// and [`AudioCodec::PCMA`] are supported here.
    pub codecs: &'a [(AudioCodec, u8)],
    /// telephone-event payload type we offer, if any.
    pub dtmf_pt: Option<u8>,
    /// `a=mid` for our audio section in an offer.
    pub mid: &'a str,
    /// The video section, if we offer or accept one.
    pub video: Option<LocalVideo<'a>>,
    /// `o=` session id.
    pub session_id: u64,
    /// `o=` session version (incremented per description).
    pub session_version: u64,
}

/// The audio section of a parsed remote description.
#[derive(Debug, Clone)]
pub struct RemoteAudio {
    /// Index of the section in the remote `m=` list.
    pub index: usize,
    /// `a=mid`, if present.
    pub mid: Option<String>,
    /// Codecs the remote listed that we can speak (Opus, G.722, PCMU,
    /// PCMA), with the remote's payload type for each, in the remote's
    /// `m=` format order. Static payload types 0/8/9 are recognised
    /// without an `a=rtpmap` line (RFC 3551 §6).
    pub codecs: Vec<(AudioCodec, u8)>,
    /// telephone-event payload types the remote listed, with their clock
    /// rates, in `m=` format order. RFC 4733 clocks telephone-event at the
    /// audio codec's rate, so browsers list one per distinct codec clock
    /// (Chrome: `110 telephone-event/48000`, `126 telephone-event/8000`).
    pub dtmf_pts: Vec<(u8, u32)>,
    /// Remote direction.
    pub direction: Direction,
    /// First `a=ssrc`, if present.
    pub ssrc: Option<u32>,
    /// Transport protocol of the section.
    pub protocol: Protocol,
}

/// The video section of a parsed remote description.
#[derive(Debug, Clone)]
pub struct RemoteVideo {
    /// Index of the section in the remote `m=` list.
    pub index: usize,
    /// `a=mid`, if present.
    pub mid: Option<String>,
    /// Codecs the remote listed that we can carry, with the remote's
    /// payload type for each, in the remote's `m=` format order. H.264
    /// entries count only with `packetization-mode=1` (RFC 6184 §6.3, the
    /// mode every browser and this stack's packetizer use); RTX, RED and
    /// FEC formats are skipped.
    pub codecs: Vec<(VideoCodec, u8)>,
    /// Remote direction.
    pub direction: Direction,
    /// First `a=ssrc`, if present.
    pub ssrc: Option<u32>,
    /// Transport protocol of the section.
    pub protocol: Protocol,
    /// The section itself, for `fmtp` and `rtcp-fb` lookups.
    pub media: MediaDescription,
}

/// What the transport and the answer builder need from a remote description.
#[derive(Debug, Clone)]
pub struct RemoteDescription {
    /// ICE username fragment.
    pub ufrag: String,
    /// ICE password.
    pub pwd: String,
    /// Fingerprint algorithm (`sha-256`).
    pub fingerprint_alg: String,
    /// Fingerprint hash.
    pub fingerprint: String,
    /// `a=setup`.
    pub setup: DtlsSetup,
    /// Inline candidates of the first active section.
    pub candidates: Vec<IceCandidate>,
    /// `a=end-of-candidates` present.
    pub end_of_candidates: bool,
    /// `a=ice-options:trickle` present at either level.
    pub trickle: bool,
    /// `a=ice-lite` present.
    pub ice_lite: bool,
    /// The accepted audio section, if the remote offered/answered one.
    pub audio: Option<RemoteAudio>,
    /// The active video section, if the remote offered/answered one.
    pub video: Option<RemoteVideo>,
    /// All remote sections, in order (used to mirror rejected ones).
    pub media: Vec<MediaDescription>,
    /// BUNDLE mids, in order.
    pub bundle: Vec<String>,
}

impl RemoteAudio {
    /// The remote's payload type for `codec`, if it listed it.
    pub fn pt_of(&self, codec: AudioCodec) -> Option<u8> {
        self.codecs
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|&(_, pt)| pt)
    }

    /// The remote's telephone-event `(payload type, clock)` best matched to
    /// `codec`: the one clocked at the codec's RTP rate (RFC 4733 §2.1),
    /// falling back to whichever it listed first (mirrored at the remote's
    /// own clock — never re-declared at a different one).
    pub fn dtmf_for(&self, codec: AudioCodec) -> Option<(u8, u32)> {
        let clock = rtp_clock(codec);
        self.dtmf_pts
            .iter()
            .find(|(_, c)| *c == clock)
            .or_else(|| self.dtmf_pts.first())
            .copied()
    }
}

impl RemoteVideo {
    /// The remote's payload type for `codec`, if it listed it.
    pub fn pt_of(&self, codec: VideoCodec) -> Option<u8> {
        self.codecs
            .iter()
            .find(|(c, _)| *c == codec)
            .map(|&(_, pt)| pt)
    }

    /// The `a=rtcp-fb` lines the remote attached to `payload_type` (its
    /// own and `*`).
    pub fn feedback_for(&self, payload_type: u8) -> Vec<RtcpFeedbackAttr> {
        self.media.rtcp_fb_for(payload_type)
    }

    /// The remote's `fmtp` parameters for `payload_type`, if any.
    pub fn fmtp_of(&self, payload_type: u8) -> Option<String> {
        self.media
            .fmtp_for(payload_type)
            .map(|f| f.params.to_string())
    }
}

/// The video codec pinned by offer/answer, with the payload type the
/// remote expects and the feedback the remote agreed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedVideo {
    /// The codec.
    pub codec: VideoCodec,
    /// Payload type on the wire, both directions.
    pub payload_type: u8,
    /// Our direction for the section.
    pub direction: Direction,
    /// The remote's first `a=ssrc` for the section, if signalled.
    pub remote_ssrc: Option<u32>,
    /// The remote's `fmtp` parameters for the codec, if any.
    pub fmtp: Option<String>,
    /// Generic NACK (RFC 4585 §6.2.1) negotiated.
    pub nack: bool,
    /// PLI (RFC 4585 §6.3.1) negotiated.
    pub pli: bool,
    /// FIR (RFC 5104 §4.3.1) negotiated.
    pub fir: bool,
    /// REMB negotiated.
    pub remb: bool,
}

impl NegotiatedVideo {
    fn from_feedback(
        codec: VideoCodec,
        payload_type: u8,
        direction: Direction,
        remote: &RemoteVideo,
        feedback: &[RtcpFeedbackAttr],
    ) -> Self {
        Self {
            codec,
            payload_type,
            direction,
            remote_ssrc: remote.ssrc,
            fmtp: remote.fmtp_of(payload_type),
            nack: feedback
                .iter()
                .any(|f| f.kind == "nack" && f.param.is_none()),
            pli: feedback.iter().any(RtcpFeedbackAttr::is_pli),
            fir: feedback.iter().any(RtcpFeedbackAttr::is_fir),
            remb: feedback.iter().any(RtcpFeedbackAttr::is_remb),
        }
    }
}

/// What offer/answer pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// The audio codec, with the payload type the remote expects.
    pub audio: (AudioCodec, u8),
    /// The video section, when both sides accepted one.
    pub video: Option<NegotiatedVideo>,
}

/// The first local preference the remote listed, with the **remote's**
/// payload type (an answer must mirror the offer's payload types, RFC 3264
/// §6.1). `None` means no codec in common.
pub fn select_codec(prefs: &[(AudioCodec, u8)], remote: &RemoteAudio) -> Option<(AudioCodec, u8)> {
    prefs
        .iter()
        .find_map(|&(c, _)| remote.pt_of(c).map(|pt| (c, pt)))
}

/// The first local video preference the remote listed, with the remote's
/// payload type.
pub fn select_video_codec(
    prefs: &[(VideoCodec, u8)],
    remote: &RemoteVideo,
) -> Option<(VideoCodec, u8)> {
    prefs
        .iter()
        .find_map(|&(c, _)| remote.pt_of(c).map(|pt| (c, pt)))
}

/// What an answer to our offer pinned for video: the codec of ours the
/// answer kept (at our payload type, which a conformant answer mirrors)
/// and the feedback it agreed to. `None` when the answer rejected video.
pub fn video_from_answer(
    prefs: &[(VideoCodec, u8)],
    want: Direction,
    answer: &RemoteVideo,
) -> Option<NegotiatedVideo> {
    let &(codec, pt) = prefs.iter().find(|(c, pt)| answer.pt_of(*c) == Some(*pt))?;
    let feedback: Vec<RtcpFeedbackAttr> = answer
        .feedback_for(pt)
        .into_iter()
        .filter(feedback_supported)
        .collect();
    // The answer's direction is from the remote's point of view; ours is
    // what we offered, narrowed by what the answer allows.
    let direction = Direction::answer_for(answer.direction, want);
    Some(NegotiatedVideo::from_feedback(
        codec, pt, direction, answer, &feedback,
    ))
}

fn missing(what: &str) -> WebRtcError {
    WebRtcError::SdpError(SdpError::MissingField(what.to_string()))
}

fn value_attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs.iter().find_map(|a| match a {
        Attribute::Value { name: n, value } if n == name => Some(value.as_str()),
        _ => None,
    })
}

fn has_property(attrs: &[Attribute], name: &str) -> bool {
    attrs
        .iter()
        .any(|a| matches!(a, Attribute::Property(p) if p == name))
}

fn direction_of(attrs: &[Attribute]) -> Option<Direction> {
    attrs.iter().find_map(|a| match a {
        Attribute::Property(p) => Direction::parse(p.as_str()),
        _ => None,
    })
}

fn first_ssrc(attrs: &[Attribute]) -> Option<u32> {
    value_attr(attrs, "ssrc")
        .and_then(|v| v.split(' ').next())
        .and_then(|v| v.parse().ok())
}

/// Parse a remote offer or answer.
pub fn parse_remote(sdp: &str) -> Result<RemoteDescription> {
    let desc = SessionDescription::from_str(sdp)?;

    let audio_index = desc
        .media
        .iter()
        .position(|m| m.media_type == MediaType::Audio && m.port != 0);
    let audio_media = audio_index.map(|i| &desc.media[i]);
    let video_index = desc
        .media
        .iter()
        .position(|m| m.media_type == MediaType::Video && m.port != 0);
    let video_media = video_index.map(|i| &desc.media[i]);
    // With BUNDLE every section carries the same transport attributes;
    // read them from the first active one.
    let first_media = audio_media.or(video_media);

    // Credentials, fingerprint and setup: media level first (Chrome), then
    // session level (Firefox, SIP-style endpoints).
    let (ufrag, pwd) = first_media
        .and_then(|m| m.get_media_ice_credentials())
        .or_else(|| desc.get_ice_credentials())
        .ok_or_else(|| missing("ICE credentials"))?;
    let (fingerprint_alg, fingerprint) = first_media
        .and_then(|m| m.get_media_dtls_fingerprint())
        .or_else(|| desc.get_dtls_fingerprint())
        .ok_or_else(|| missing("DTLS fingerprint"))?;
    if !fingerprint_alg.eq_ignore_ascii_case("sha-256") {
        return Err(WebRtcError::SdpError(SdpError::Internal(format!(
            "unsupported fingerprint algorithm {fingerprint_alg}"
        ))));
    }
    let setup = first_media
        .and_then(|m| m.get_media_dtls_setup())
        .or_else(|| desc.get_dtls_setup())
        .ok_or_else(|| missing("DTLS setup"))?;

    let candidates = first_media
        .map(|m| {
            MediaIceAttributesExt::get_ice_candidates(m)
                .iter()
                .filter_map(|s| IceCandidate::from_sdp_attribute(s).ok())
                .collect()
        })
        .unwrap_or_default();

    let trickle = desc.has_trickle_ice()
        || first_media
            .map(|m| {
                m.attributes.iter().any(|a| {
                    matches!(a, Attribute::Value { name, value } if name == "ice-options" && value.split(' ').any(|v| v == "trickle"))
                })
            })
            .unwrap_or(false);
    let end_of_candidates = first_media
        .map(|m| has_property(&m.attributes, "end-of-candidates"))
        .unwrap_or(false)
        || has_property(&desc.attributes, "end-of-candidates");
    let ice_lite = has_property(&desc.attributes, "ice-lite");

    let audio = audio_index.map(|index| {
        let m = &desc.media[index];
        // Walk the m= format list in order (the remote's preference order,
        // RFC 3264 §5.1), resolving each payload type through its rtpmap —
        // or, absent one, through the RFC 3551 static assignments.
        let mut codecs = Vec::new();
        let mut dtmf_pts = Vec::new();
        for f in &m.formats {
            let Ok(pt) = f.parse::<u8>() else { continue };
            match m.rtpmaps.get(&pt) {
                Some(r) if r.encoding_name.eq_ignore_ascii_case("opus") => {
                    codecs.push((AudioCodec::Opus, pt));
                }
                Some(r) if r.encoding_name.eq_ignore_ascii_case("g722") => {
                    codecs.push((AudioCodec::G722, pt));
                }
                Some(r) if r.encoding_name.eq_ignore_ascii_case("pcmu") => {
                    codecs.push((AudioCodec::PCMU, pt));
                }
                Some(r) if r.encoding_name.eq_ignore_ascii_case("pcma") => {
                    codecs.push((AudioCodec::PCMA, pt));
                }
                Some(r) if r.encoding_name.eq_ignore_ascii_case("telephone-event") => {
                    dtmf_pts.push((pt, r.clock_rate));
                }
                Some(_) => {}
                None => match pt {
                    0 => codecs.push((AudioCodec::PCMU, pt)),
                    8 => codecs.push((AudioCodec::PCMA, pt)),
                    9 => codecs.push((AudioCodec::G722, pt)),
                    _ => {}
                },
            }
        }
        RemoteAudio {
            index,
            mid: value_attr(&m.attributes, "mid").map(str::to_string),
            codecs,
            dtmf_pts,
            direction: direction_of(&m.attributes)
                .or_else(|| direction_of(&desc.attributes))
                .unwrap_or_default(),
            ssrc: first_ssrc(&m.attributes),
            protocol: m.protocol.clone(),
        }
    });

    let video = video_index.map(|index| {
        let m = &desc.media[index];
        let mut codecs = Vec::new();
        for f in &m.formats {
            let Ok(pt) = f.parse::<u8>() else { continue };
            let Some(r) = m.rtpmaps.get(&pt) else {
                continue;
            };
            let Some(codec) = VideoCodec::from_sdp_name(&r.encoding_name) else {
                continue;
            };
            if codec == VideoCodec::H264 && m.h264_fmtp(pt).packetization_mode != 1 {
                continue;
            }
            codecs.push((codec, pt));
        }
        RemoteVideo {
            index,
            mid: value_attr(&m.attributes, "mid").map(str::to_string),
            codecs,
            direction: direction_of(&m.attributes)
                .or_else(|| direction_of(&desc.attributes))
                .unwrap_or_default(),
            ssrc: first_ssrc(&m.attributes),
            protocol: m.protocol.clone(),
            media: m.clone(),
        }
    });

    let bundle = value_attr(&desc.attributes, "group")
        .and_then(|v| v.strip_prefix("BUNDLE"))
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();

    Ok(RemoteDescription {
        ufrag,
        pwd,
        fingerprint_alg,
        fingerprint,
        setup,
        candidates,
        end_of_candidates,
        trickle,
        ice_lite,
        audio,
        video,
        media: desc.media.clone(),
        bundle,
    })
}

fn push_value(attrs: &mut Vec<Attribute>, name: &str, value: String) {
    attrs.push(Attribute::Value {
        name: SmolStr::new(name),
        value: SmolStr::new(value),
    });
}

fn push_prop(attrs: &mut Vec<Attribute>, name: &str) {
    attrs.push(Attribute::Property(SmolStr::new(name)));
}

/// The address advertised on `c=`/`m=`: the first IPv4 host candidate, or the
/// trickle placeholder `0.0.0.0:9` (RFC 8840 §4.1.2).
fn advertised(candidates: &[IceCandidate]) -> (String, u16) {
    candidates
        .iter()
        .find(|c| c.ip.is_ipv4() && c.typ == forge_ice::CandidateType::Host)
        .map(|c| (c.ip.to_string(), c.port))
        .unwrap_or_else(|| ("0.0.0.0".to_string(), 9))
}

/// The `a=rtpmap` encoding for a supported audio codec.
fn rtpmap_of(codec: AudioCodec) -> (&'static str, u32, Option<&'static str>) {
    match codec {
        AudioCodec::Opus => ("opus", 48_000, Some("2")),
        AudioCodec::G722 => ("G722", 8_000, None),
        AudioCodec::PCMU => ("PCMU", 8_000, None),
        AudioCodec::PCMA => ("PCMA", 8_000, None),
        // LocalParams::codecs documents the supported set; peer.rs
        // constructs it from the same enum, so this is unreachable
        // without a code change here.
        other => unreachable!("unsupported WebRTC codec {other:?}"),
    }
}

/// The transport attributes every section carries (RFC 8843 §7.1 puts
/// them on each BUNDLE'd section).
fn transport_attrs(a: &mut Vec<Attribute>, p: &LocalParams<'_>, port: u16) {
    if port == 9 {
        push_value(a, "rtcp", "9 IN IP4 0.0.0.0".into());
    }
    push_value(a, "ice-ufrag", p.ufrag.into());
    push_value(a, "ice-pwd", p.pwd.into());
    push_value(a, "ice-options", "trickle".into());
    push_value(a, "fingerprint", format!("sha-256 {}", p.fingerprint));
    push_value(a, "setup", p.setup.as_str().into());
}

/// The source and candidate attributes that close every section.
fn source_attrs(
    media: &mut MediaDescription,
    p: &LocalParams<'_>,
    direction: Direction,
    ssrc: u32,
    track: &str,
) {
    if direction.sends() {
        let a = &mut media.attributes;
        push_value(a, "msid", format!("{} {track}", p.msid));
        push_value(a, "ssrc", format!("{ssrc} cname:{}", p.cname));
        push_value(a, "ssrc", format!("{ssrc} msid:{} {track}", p.msid));
    }
    for c in p.candidates {
        media.add_ice_candidate_from_forge(c);
    }
    if p.end_of_candidates {
        push_prop(&mut media.attributes, "end-of-candidates");
    }
}

fn audio_section(
    p: &LocalParams<'_>,
    port: u16,
    protocol: Protocol,
    mid: &str,
    direction: Direction,
    codecs: &[(AudioCodec, u8)],
    dtmf: Option<(u8, u32)>,
) -> MediaDescription {
    let mut media = MediaDescription::audio(port);
    media.protocol = protocol;
    for &(codec, pt) in codecs {
        let (name, clock, params) = rtpmap_of(codec);
        media.formats.push(SmolStr::new(pt.to_string()));
        media.rtpmaps.insert(
            pt,
            forge_sdp::RtpMap {
                payload_type: pt,
                encoding_name: SmolStr::new(name),
                clock_rate: clock,
                encoding_params: params.map(SmolStr::new),
            },
        );
    }
    if let Some((pt, clock)) = dtmf {
        media.formats.push(SmolStr::new(pt.to_string()));
        media.rtpmaps.insert(
            pt,
            forge_sdp::RtpMap {
                payload_type: pt,
                encoding_name: SmolStr::new("telephone-event"),
                clock_rate: clock,
                encoding_params: None,
            },
        );
    }

    let a = &mut media.attributes;
    transport_attrs(a, p, port);
    push_value(a, "mid", mid.into());
    push_prop(a, direction.as_str());
    push_prop(a, "rtcp-mux");
    for &(codec, pt) in codecs {
        let (name, clock, params) = rtpmap_of(codec);
        let params = params.map(|p| format!("/{p}")).unwrap_or_default();
        push_value(a, "rtpmap", format!("{pt} {name}/{clock}{params}"));
        if codec == AudioCodec::Opus {
            push_value(a, "fmtp", format!("{pt} minptime=10;useinbandfec=1"));
        }
    }
    if let Some((pt, clock)) = dtmf {
        push_value(a, "rtpmap", format!("{pt} telephone-event/{clock}"));
        push_value(a, "fmtp", format!("{pt} 0-16"));
    }
    source_attrs(&mut media, p, direction, p.ssrc, "audio0");
    media
}

/// One format of a video section: the codec, its payload type, its
/// `fmtp` line and the feedback attached to it.
struct VideoFormat {
    codec: VideoCodec,
    pt: u8,
    fmtp: Option<String>,
    feedback: Vec<RtcpFeedbackAttr>,
}

fn video_section(
    p: &LocalParams<'_>,
    lv: &LocalVideo<'_>,
    port: u16,
    protocol: Protocol,
    mid: &str,
    direction: Direction,
    formats: &[VideoFormat],
) -> MediaDescription {
    let mut media = MediaDescription::video(port);
    media.protocol = protocol;
    for f in formats {
        media.formats.push(SmolStr::new(f.pt.to_string()));
        media.rtpmaps.insert(
            f.pt,
            forge_sdp::RtpMap {
                payload_type: f.pt,
                encoding_name: SmolStr::new(f.codec.sdp_name()),
                clock_rate: VideoCodec::CLOCK_RATE,
                encoding_params: None,
            },
        );
    }
    if let Some(kbps) = lv.max_kbps {
        media.bandwidth.push(forge_sdp::Bandwidth {
            bw_type: SmolStr::new("AS"),
            bandwidth: kbps,
        });
    }

    let a = &mut media.attributes;
    transport_attrs(a, p, port);
    push_value(a, "mid", mid.into());
    push_prop(a, direction.as_str());
    push_prop(a, "rtcp-mux");
    for f in formats {
        push_value(
            a,
            "rtpmap",
            format!("{} {}/{}", f.pt, f.codec.sdp_name(), VideoCodec::CLOCK_RATE),
        );
        if let Some(fmtp) = &f.fmtp {
            push_value(a, "fmtp", format!("{} {fmtp}", f.pt));
        }
        for fb in &f.feedback {
            let param = fb
                .param
                .as_deref()
                .map(|s| format!(" {s}"))
                .unwrap_or_default();
            push_value(a, "rtcp-fb", format!("{} {}{param}", f.pt, fb.kind));
        }
    }
    source_attrs(&mut media, p, direction, lv.ssrc, "video0");
    media
}

/// The formats we offer for video: every configured codec at its payload
/// type, H.264 with our profile and packetization mode, and the full
/// supported feedback set on each.
fn offered_video_formats(lv: &LocalVideo<'_>) -> Vec<VideoFormat> {
    lv.codecs
        .iter()
        .map(|&(codec, pt)| VideoFormat {
            codec,
            pt,
            fmtp: (codec == VideoCodec::H264).then(|| {
                format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                    lv.h264_profile_level_id
                )
            }),
            feedback: SUPPORTED_FEEDBACK
                .iter()
                .map(|(k, param)| feedback_attr(pt, k, *param))
                .collect(),
        })
        .collect()
}

fn session(p: &LocalParams<'_>, bundle: &[&str], media: Vec<MediaDescription>) -> String {
    let (addr, _) = advertised(p.candidates);
    let mut sdp = SessionDescription::default();
    sdp.origin = forge_sdp::Origin::new("-", &p.session_id.to_string(), "127.0.0.1")
        .unwrap_or_else(|_| sdp.origin.clone());
    sdp.origin.session_version = SmolStr::new(p.session_version.to_string());
    sdp.session_name = SmolStr::new("-");
    sdp.connection = Connection::new(&addr).ok();
    sdp.times = vec![forge_sdp::TimeDescription {
        start_time: 0,
        stop_time: 0,
        repeats: vec![],
    }];
    if !bundle.is_empty() {
        push_value(
            &mut sdp.attributes,
            "group",
            format!("BUNDLE {}", bundle.join(" ")),
        );
    }
    push_value(
        &mut sdp.attributes,
        "msid-semantic",
        format!(" WMS {}", p.msid),
    );
    sdp.media = media;
    forge_sdp::serialize::serialize_sdp(&sdp)
}

/// Build an SDP offer carrying every codec in [`LocalParams::codecs`], in
/// preference order, and a video section when [`LocalParams::video`] is
/// set. telephone-event, when offered, is clocked at the first (preferred)
/// audio codec's rate (RFC 4733 §2.1).
pub fn build_offer(p: &LocalParams<'_>) -> String {
    let (_, port) = advertised(p.candidates);
    let dtmf = p.dtmf_pt.map(|pt| {
        let clock = p
            .codecs
            .first()
            .map(|&(c, _)| rtp_clock(c))
            .unwrap_or(8_000);
        (pt, clock)
    });
    let audio = audio_section(
        p,
        port,
        Protocol::UdpTlsRtpSavpf,
        p.mid,
        p.direction,
        p.codecs,
        dtmf,
    );
    let mut media = vec![audio];
    let mut bundle = vec![p.mid];
    if let Some(lv) = &p.video {
        let formats = offered_video_formats(lv);
        media.push(video_section(
            p,
            lv,
            port,
            Protocol::UdpTlsRtpSavpf,
            lv.mid,
            lv.direction,
            &formats,
        ));
        bundle.push(lv.mid);
    }
    session(p, &bundle, media)
}

/// Reject an offered section: port 0, same protocol and formats, mid
/// mirrored (RFC 3264 §6, RFC 8843 §7.3).
fn rejected_section(m: &MediaDescription) -> MediaDescription {
    let mut rejected = MediaDescription {
        media_type: m.media_type.clone(),
        port: 0,
        num_ports: None,
        protocol: m.protocol.clone(),
        formats: if m.formats.is_empty() {
            vec![SmolStr::new("0")]
        } else {
            m.formats.clone()
        },
        title: None,
        connection: None,
        bandwidth: vec![],
        encryption_key: None,
        attributes: vec![],
        rtpmaps: Default::default(),
    };
    if let Some(mid) = value_attr(&m.attributes, "mid") {
        push_value(&mut rejected.attributes, "mid", mid.to_string());
    }
    push_prop(&mut rejected.attributes, "inactive");
    rejected
}

/// Build an SDP answer to `remote`: accept the audio section with exactly
/// one codec — the first local preference the remote offered, at the
/// remote's payload type — accept the video section the same way when
/// [`LocalParams::video`] is set and a codec is common, and reject
/// everything else. Returns the answer and what it pinned.
pub fn build_answer(
    p: &LocalParams<'_>,
    remote: &RemoteDescription,
) -> Result<(String, Negotiated)> {
    let audio = remote
        .audio
        .as_ref()
        .ok_or_else(|| missing("audio section"))?;
    let selected =
        select_codec(p.codecs, audio).ok_or(WebRtcError::SdpError(SdpError::NoCommonCodec))?;
    let dtmf = match p.dtmf_pt {
        Some(_) => audio.dtmf_for(selected.0),
        None => None,
    };
    let audio_mid = audio.mid.clone().unwrap_or_else(|| "0".to_string());
    let (_, port) = advertised(p.candidates);
    let direction = Direction::answer_for(audio.direction, p.direction);
    let accepted_audio = audio_section(
        p,
        port,
        audio.protocol.clone(),
        &audio_mid,
        direction,
        &[selected],
        dtmf,
    );

    // Video: accepted when we have a configuration for it, want more than
    // `inactive`, and the offer lists a codec we carry.
    let mut accepted_video: Option<(usize, MediaDescription, String, NegotiatedVideo)> = None;
    if let (Some(lv), Some(rv)) = (&p.video, &remote.video) {
        if lv.direction != Direction::Inactive {
            if let Some((codec, pt)) = select_video_codec(lv.codecs, rv) {
                let feedback: Vec<RtcpFeedbackAttr> = rv
                    .feedback_for(pt)
                    .into_iter()
                    .filter(feedback_supported)
                    .map(|fb| RtcpFeedbackAttr {
                        payload_type: Some(pt),
                        ..fb
                    })
                    .collect();
                let direction = Direction::answer_for(rv.direction, lv.direction);
                let mid = rv.mid.clone().unwrap_or_else(|| "1".to_string());
                let format = VideoFormat {
                    codec,
                    pt,
                    fmtp: rv.fmtp_of(pt),
                    feedback: feedback.clone(),
                };
                let section =
                    video_section(p, lv, port, rv.protocol.clone(), &mid, direction, &[format]);
                let negotiated =
                    NegotiatedVideo::from_feedback(codec, pt, direction, rv, &feedback);
                accepted_video = Some((rv.index, section, mid, negotiated));
            }
        }
    }

    let mut media = Vec::with_capacity(remote.media.len());
    let mut bundle: Vec<&str> = Vec::with_capacity(2);
    for (i, m) in remote.media.iter().enumerate() {
        if i == audio.index {
            media.push(accepted_audio.clone());
            bundle.push(audio_mid.as_str());
            continue;
        }
        if let Some((index, section, mid, _)) = &accepted_video {
            if i == *index {
                media.push(section.clone());
                bundle.push(mid.as_str());
                continue;
            }
        }
        media.push(rejected_section(m));
    }
    let sdp = session(p, &bundle, media);
    drop(bundle);
    let negotiated = Negotiated {
        audio: selected,
        video: accepted_video.map(|(_, _, _, v)| v),
    };
    Ok((sdp, negotiated))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_OFFER: &str = "v=0\r\n\
o=- 4611728142112323737 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 1\r\n\
a=extmap-allow-mixed\r\n\
a=msid-semantic: WMS stream0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111 63 9 0 8 13 110 126\r\n\
c=IN IP4 0.0.0.0\r\n\
a=rtcp:9 IN IP4 0.0.0.0\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:efghijklmnopqrstuvwxyz0123\r\n\
a=ice-options:trickle\r\n\
a=fingerprint:sha-256 12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0\r\n\
a=setup:actpass\r\n\
a=mid:0\r\n\
a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n\
a=sendrecv\r\n\
a=msid:stream0 track0\r\n\
a=rtcp-mux\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=rtcp-fb:111 transport-cc\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n\
a=rtpmap:63 red/48000/2\r\n\
a=fmtp:63 111/111\r\n\
a=rtpmap:9 G722/8000\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:13 CN/8000\r\n\
a=rtpmap:110 telephone-event/48000\r\n\
a=rtpmap:126 telephone-event/8000\r\n\
a=ssrc:3735928559 cname:user@example.com\r\n\
a=ssrc:3735928559 msid:stream0 track0\r\n\
a=candidate:1 1 udp 2130706431 192.168.1.5 52000 typ host generation 0 network-id 1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
c=IN IP4 0.0.0.0\r\n\
a=rtcp:9 IN IP4 0.0.0.0\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:efghijklmnopqrstuvwxyz0123\r\n\
a=ice-options:trickle\r\n\
a=fingerprint:sha-256 12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0\r\n\
a=setup:actpass\r\n\
a=mid:1\r\n\
a=sendrecv\r\n\
a=rtcp-mux\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rtpmap:97 rtx/90000\r\n\
a=fmtp:97 apt=96\r\n";

    fn local<'a>(cands: &'a [IceCandidate]) -> LocalParams<'a> {
        LocalParams {
            ufrag: "0123456789abcdef0123456789abcdef",
            pwd: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            fingerprint: "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99",
            setup: DtlsSetup::Active,
            candidates: cands,
            end_of_candidates: false,
            ssrc: 42,
            cname: "forge",
            msid: "stream",
            direction: Direction::SendRecv,
            codecs: &[
                (AudioCodec::Opus, 111),
                (AudioCodec::PCMU, 0),
                (AudioCodec::PCMA, 8),
            ],
            dtmf_pt: Some(101),
            mid: "0",
            video: None,
            session_id: 1,
            session_version: 1,
        }
    }

    fn local_video<'a>(codecs: &'a [(VideoCodec, u8)]) -> LocalVideo<'a> {
        LocalVideo {
            ssrc: 4242,
            codecs,
            direction: Direction::SendRecv,
            mid: "1",
            h264_profile_level_id: "42e01f",
            max_kbps: Some(1200),
        }
    }

    /// A Chrome offer whose video section lists VP8 and two H.264 entries
    /// (packetization modes 0 and 1) with RTX, RED and ULPFEC beside them
    /// and the full browser feedback set on each codec.
    const CHROME_VIDEO_OFFER: &str = "v=0\r\n\
o=- 4611728142112323737 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 1\r\n\
a=msid-semantic: WMS stream0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111 0 8 126\r\n\
c=IN IP4 0.0.0.0\r\n\
a=rtcp:9 IN IP4 0.0.0.0\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:efghijklmnopqrstuvwxyz0123\r\n\
a=ice-options:trickle\r\n\
a=fingerprint:sha-256 12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0\r\n\
a=setup:actpass\r\n\
a=mid:0\r\n\
a=sendrecv\r\n\
a=rtcp-mux\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:126 telephone-event/8000\r\n\
a=ssrc:3735928559 cname:user@example.com\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99 100 101 102\r\n\
c=IN IP4 0.0.0.0\r\n\
a=rtcp:9 IN IP4 0.0.0.0\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:efghijklmnopqrstuvwxyz0123\r\n\
a=ice-options:trickle\r\n\
a=fingerprint:sha-256 12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0\r\n\
a=setup:actpass\r\n\
a=mid:1\r\n\
a=sendrecv\r\n\
a=rtcp-mux\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rtcp-fb:96 goog-remb\r\n\
a=rtcp-fb:96 transport-cc\r\n\
a=rtcp-fb:96 ccm fir\r\n\
a=rtcp-fb:96 nack\r\n\
a=rtcp-fb:96 nack pli\r\n\
a=rtpmap:97 rtx/90000\r\n\
a=fmtp:97 apt=96\r\n\
a=rtpmap:98 H264/90000\r\n\
a=rtcp-fb:98 goog-remb\r\n\
a=rtcp-fb:98 transport-cc\r\n\
a=rtcp-fb:98 ccm fir\r\n\
a=rtcp-fb:98 nack\r\n\
a=rtcp-fb:98 nack pli\r\n\
a=fmtp:98 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42001f\r\n\
a=rtpmap:99 H264/90000\r\n\
a=rtcp-fb:99 goog-remb\r\n\
a=rtcp-fb:99 transport-cc\r\n\
a=rtcp-fb:99 ccm fir\r\n\
a=rtcp-fb:99 nack\r\n\
a=rtcp-fb:99 nack pli\r\n\
a=fmtp:99 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n\
a=rtpmap:100 rtx/90000\r\n\
a=fmtp:100 apt=99\r\n\
a=rtpmap:101 red/90000\r\n\
a=rtpmap:102 ulpfec/90000\r\n\
a=ssrc-group:FID 1111 2222\r\n\
a=ssrc:1111 cname:user@example.com\r\n\
a=ssrc:2222 cname:user@example.com\r\n";

    #[test]
    fn parses_chrome_offer_media_level_attributes() {
        let r = parse_remote(CHROME_OFFER).unwrap();
        assert_eq!(r.ufrag, "abcd");
        assert_eq!(r.pwd, "efghijklmnopqrstuvwxyz0123");
        assert_eq!(r.setup, DtlsSetup::Actpass);
        assert!(r.trickle);
        assert_eq!(r.candidates.len(), 1);
        let a = r.audio.as_ref().unwrap();
        // Recognised codecs in the offer's m= order: opus 111, G722 9,
        // PCMU 0, PCMA 8 (CN is skipped; red is an unknown encoding).
        assert_eq!(
            a.codecs,
            vec![
                (AudioCodec::Opus, 111),
                (AudioCodec::G722, 9),
                (AudioCodec::PCMU, 0),
                (AudioCodec::PCMA, 8),
            ]
        );
        // The video section is parsed too: VP8 at 96, RTX skipped.
        let v = r.video.as_ref().unwrap();
        assert_eq!(v.codecs, vec![(VideoCodec::VP8, 96)]);
        assert_eq!(v.mid.as_deref(), Some("1"));
        assert_eq!(a.pt_of(AudioCodec::Opus), Some(111));
        // Both telephone-event clocks, in m= order; matching picks by the
        // codec's clock.
        assert_eq!(a.dtmf_pts, vec![(110, 48_000), (126, 8_000)]);
        assert_eq!(a.dtmf_for(AudioCodec::Opus), Some((110, 48_000)));
        assert_eq!(a.dtmf_for(AudioCodec::PCMU), Some((126, 8_000)));
        assert_eq!(a.mid.as_deref(), Some("0"));
        assert_eq!(a.direction, Direction::SendRecv);
        assert_eq!(a.ssrc, Some(3735928559));
        assert_eq!(r.bundle, vec!["0", "1"]);
        assert_eq!(r.media.len(), 2);
    }

    #[test]
    fn answer_mirrors_offer_and_rejects_video() {
        let r = parse_remote(CHROME_OFFER).unwrap();
        let cands = vec![IceCandidate::new_host(
            "1".into(),
            1,
            forge_ice::Protocol::Udp,
            "10.0.0.2".parse().unwrap(),
            40000,
            65535,
        )];
        let mut p = local(&cands);
        p.direction = Direction::RecvOnly;
        let (answer, negotiated) = build_answer(&p, &r).unwrap();
        // Opus is our first preference and the offer has it; the answer's
        // telephone-event mirrors the 48 kHz-clocked one (110, not 126).
        assert_eq!(negotiated.audio, (AudioCodec::Opus, 111));
        assert!(negotiated.video.is_none());
        assert!(answer.contains("a=group:BUNDLE 0\r\n"), "{answer}");
        assert!(
            answer.contains("m=audio 40000 UDP/TLS/RTP/SAVPF 111 110\r\n"),
            "{answer}"
        );
        assert!(answer.contains("a=rtpmap:110 telephone-event/48000\r\n"));
        assert!(
            answer.contains("m=video 0 UDP/TLS/RTP/SAVPF 96 97\r\n"),
            "{answer}"
        );
        assert!(answer.contains("a=setup:active\r\n"));
        assert!(answer.contains("a=recvonly\r\n"));
        assert!(answer.contains("a=mid:0\r\n"));
        assert!(answer.contains("a=mid:1\r\n"));
        assert!(answer.contains("a=candidate:1 1 UDP"));
        // The answer parses back as a remote description with one audio section.
        let back = parse_remote(&answer).unwrap();
        assert_eq!(back.audio.unwrap().pt_of(AudioCodec::Opus), Some(111));
        assert_eq!(back.media.len(), 2);
    }

    #[test]
    fn offer_without_candidates_uses_trickle_placeholders() {
        let p = local(&[]);
        let offer = build_offer(&p);
        assert!(
            offer.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111 0 8 101\r\n"),
            "{offer}"
        );
        assert!(offer.contains("a=rtpmap:111 opus/48000/2\r\n"));
        assert!(offer.contains("a=rtpmap:0 PCMU/8000\r\n"));
        assert!(offer.contains("a=rtpmap:8 PCMA/8000\r\n"));
        // telephone-event clocked at the preferred codec's rate.
        assert!(offer.contains("a=rtpmap:101 telephone-event/48000\r\n"));
        // fmtp only for Opus (and the telephone-event event range).
        assert!(offer.contains("a=fmtp:111 minptime=10;useinbandfec=1\r\n"));
        assert!(!offer.contains("a=fmtp:0 "));
        assert!(!offer.contains("a=fmtp:8 "));
        assert!(offer.contains("c=IN IP4 0.0.0.0\r\n"));
        assert!(offer.contains("a=rtcp:9 IN IP4 0.0.0.0\r\n"));
        assert!(offer.contains("a=ice-options:trickle\r\n"));
        assert!(offer.contains("a=ssrc:42 cname:forge\r\n"));
        let back = parse_remote(&offer).unwrap();
        assert_eq!(back.setup, DtlsSetup::Active);
        assert!(back.candidates.is_empty());
    }

    #[test]
    fn direction_answer_rules() {
        assert_eq!(
            Direction::answer_for(Direction::SendOnly, Direction::SendRecv),
            Direction::RecvOnly
        );
        assert_eq!(
            Direction::answer_for(Direction::SendRecv, Direction::RecvOnly),
            Direction::RecvOnly
        );
        assert_eq!(
            Direction::answer_for(Direction::RecvOnly, Direction::RecvOnly),
            Direction::Inactive
        );
        assert_eq!(
            Direction::answer_for(Direction::SendRecv, Direction::SendRecv),
            Direction::SendRecv
        );
    }

    #[test]
    fn offer_without_opus_falls_back_to_g711() {
        // G.711 is mandatory-to-implement in WebRTC (RFC 7874 §3), so an
        // offer without Opus is still answerable — this used to be a
        // NoCommonCodec rejection.
        let sdp = CHROME_OFFER.replace("a=rtpmap:111 opus/48000/2\r\n", "");
        let r = parse_remote(&sdp).unwrap();
        let p = local(&[]);
        let (answer, negotiated) = build_answer(&p, &r).unwrap();
        assert_eq!(negotiated.audio, (AudioCodec::PCMU, 0));
        // Single selected codec plus the 8 kHz-clocked telephone-event.
        assert!(
            answer.contains("m=audio 9 UDP/TLS/RTP/SAVPF 0 126\r\n"),
            "{answer}"
        );
        assert!(answer.contains("a=rtpmap:0 PCMU/8000\r\n"));
        assert!(answer.contains("a=rtpmap:126 telephone-event/8000\r\n"));
    }

    #[test]
    fn g711_preference_wins_over_offered_opus() {
        // A bridge matching a G.711 SIP leg prefers PCMA to skip
        // transcoding; the browser offered Opus but our preference decides.
        let r = parse_remote(CHROME_OFFER).unwrap();
        let mut p = local(&[]);
        p.codecs = &[(AudioCodec::PCMA, 8), (AudioCodec::Opus, 111)];
        let (answer, negotiated) = build_answer(&p, &r).unwrap();
        assert_eq!(negotiated.audio, (AudioCodec::PCMA, 8));
        assert!(
            answer.contains("m=audio 9 UDP/TLS/RTP/SAVPF 8 126\r\n"),
            "{answer}"
        );
    }

    #[test]
    fn rejects_offer_with_no_common_codec() {
        // Strip every codec we speak; G722/CN/red remain but none is ours.
        let sdp = CHROME_OFFER
            .replace("a=rtpmap:111 opus/48000/2\r\n", "")
            .replace("a=rtpmap:0 PCMU/8000\r\n", "")
            .replace("a=rtpmap:8 PCMA/8000\r\n", "")
            .replace(
                "m=audio 9 UDP/TLS/RTP/SAVPF 111 63 9 0 8 13 110 126\r\n",
                "m=audio 9 UDP/TLS/RTP/SAVPF 111 63 9 13 110 126\r\n",
            );
        let r = parse_remote(&sdp).unwrap();
        let p = local(&[]);
        assert!(matches!(
            build_answer(&p, &r),
            Err(WebRtcError::SdpError(SdpError::NoCommonCodec))
        ));
    }

    #[test]
    fn static_g711_payload_types_need_no_rtpmap() {
        // RFC 3551 §6: static assignments may be listed with no a=rtpmap.
        // This is the shape a SIP-side gateway's offer often has.
        let sdp = CHROME_OFFER
            .replace("a=rtpmap:0 PCMU/8000\r\n", "")
            .replace("a=rtpmap:8 PCMA/8000\r\n", "");
        let r = parse_remote(&sdp).unwrap();
        let a = r.audio.as_ref().unwrap();
        assert_eq!(a.pt_of(AudioCodec::PCMU), Some(0));
        assert_eq!(a.pt_of(AudioCodec::PCMA), Some(8));
    }

    fn one_cand() -> Vec<IceCandidate> {
        vec![IceCandidate::new_host(
            "1".into(),
            1,
            forge_ice::Protocol::Udp,
            "10.0.0.2".parse().unwrap(),
            40000,
            65535,
        )]
    }

    #[test]
    fn parses_video_section_skipping_mode0_h264_rtx_and_fec() {
        let r = parse_remote(CHROME_VIDEO_OFFER).unwrap();
        let v = r.video.as_ref().unwrap();
        assert_eq!(v.index, 1);
        // VP8 and the packetization-mode=1 H.264 entry only, in m= order.
        assert_eq!(
            v.codecs,
            vec![(VideoCodec::VP8, 96), (VideoCodec::H264, 99)]
        );
        assert_eq!(v.pt_of(VideoCodec::H264), Some(99));
        assert_eq!(v.ssrc, Some(1111));
        assert_eq!(v.direction, Direction::SendRecv);
        assert_eq!(v.feedback_for(96).len(), 5);
        assert_eq!(
            v.fmtp_of(99).as_deref(),
            Some("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f")
        );
    }

    #[test]
    fn answer_accepts_video_by_preference_and_filters_feedback() {
        let r = parse_remote(CHROME_VIDEO_OFFER).unwrap();
        let cands = one_cand();
        let prefs = [(VideoCodec::H264, 96), (VideoCodec::VP8, 97)];
        let mut p = local(&cands);
        p.video = Some(local_video(&prefs));
        let (answer, negotiated) = build_answer(&p, &r).unwrap();
        let v = negotiated.video.expect("video accepted");
        assert_eq!((v.codec, v.payload_type), (VideoCodec::H264, 99));
        assert_eq!(v.remote_ssrc, Some(1111));
        assert!(v.nack && v.pli && v.fir && v.remb);
        for needle in [
            "a=group:BUNDLE 0 1\r\n",
            "m=video 40000 UDP/TLS/RTP/SAVPF 99\r\n",
            "b=AS:1200\r\n",
            "a=mid:1\r\n",
            "a=rtpmap:99 H264/90000\r\n",
            "a=fmtp:99 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n",
            "a=rtcp-fb:99 goog-remb\r\n",
            "a=rtcp-fb:99 ccm fir\r\n",
            "a=rtcp-fb:99 nack\r\n",
            "a=rtcp-fb:99 nack pli\r\n",
            "a=msid:stream video0\r\n",
            "a=ssrc:4242 cname:forge\r\n",
            "a=ssrc:4242 msid:stream video0\r\n",
            "a=msid:stream audio0\r\n",
            "a=ssrc:42 cname:forge\r\n",
        ] {
            assert!(answer.contains(needle), "missing {needle:?} in {answer}");
        }
        assert!(!answer.contains("transport-cc"), "{answer}");
        assert!(!answer.contains("rtx"), "{answer}");
        // The ICE and DTLS attributes are on both sections (RFC 8843 §7.1).
        assert_eq!(answer.matches("a=ice-ufrag:").count(), 2);
        assert_eq!(answer.matches("a=fingerprint:sha-256 ").count(), 2);
        assert_eq!(answer.matches("a=setup:active").count(), 2);
        assert_eq!(answer.matches("a=candidate:").count(), 2);
    }

    #[test]
    fn answer_rejects_video_when_inactive_or_no_common_codec() {
        let r = parse_remote(CHROME_VIDEO_OFFER).unwrap();
        let cands = one_cand();
        let prefs = [(VideoCodec::AV1, 96)];
        let mut p = local(&cands);
        p.video = Some(local_video(&prefs));
        let (answer, negotiated) = build_answer(&p, &r).unwrap();
        assert!(negotiated.video.is_none());
        assert!(
            answer.contains("m=video 0 UDP/TLS/RTP/SAVPF 96 97 98 99 100 101 102\r\n"),
            "{answer}"
        );
        assert!(answer.contains("a=group:BUNDLE 0\r\n"), "{answer}");

        let prefs = [(VideoCodec::VP8, 96)];
        let mut lv = local_video(&prefs);
        lv.direction = Direction::Inactive;
        p.video = Some(lv);
        let (_, negotiated) = build_answer(&p, &r).unwrap();
        assert!(negotiated.video.is_none());
    }

    #[test]
    fn offer_with_video_round_trips_through_parse() {
        let cands = one_cand();
        let prefs = [(VideoCodec::H264, 96), (VideoCodec::VP8, 97)];
        let mut p = local(&cands);
        p.video = Some(local_video(&prefs));
        let offer = build_offer(&p);
        for needle in [
            "a=group:BUNDLE 0 1\r\n",
            "m=audio 40000 UDP/TLS/RTP/SAVPF 111 0 8 101\r\n",
            "m=video 40000 UDP/TLS/RTP/SAVPF 96 97\r\n",
            "b=AS:1200\r\n",
            "a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
            "a=rtcp-fb:96 nack pli\r\n",
            "a=rtcp-fb:97 goog-remb\r\n",
            "a=msid-semantic: WMS stream\r\n",
        ] {
            assert!(offer.contains(needle), "missing {needle:?} in {offer}");
        }
        let r = parse_remote(&offer).unwrap();
        let v = r.video.as_ref().unwrap();
        assert_eq!(
            v.codecs,
            vec![(VideoCodec::H264, 96), (VideoCodec::VP8, 97)]
        );
        assert_eq!(v.ssrc, Some(4242));
        assert_eq!(r.bundle, vec!["0", "1"]);
        assert_eq!(v.feedback_for(96).len(), 4);
    }

    #[test]
    fn video_from_answer_pins_our_payload_type_and_their_feedback() {
        // Offerer prefers H.264; answerer only carries VP8 and does not
        // offer REMB back.
        let cands = one_cand();
        let prefs = [(VideoCodec::H264, 96), (VideoCodec::VP8, 97)];
        let mut offerer = local(&cands);
        offerer.video = Some(local_video(&prefs));
        let offer = build_offer(&offerer);

        let remote_offer = parse_remote(&offer).unwrap();
        let answer_prefs = [(VideoCodec::VP8, 120)];
        let mut answerer = local(&cands);
        answerer.setup = DtlsSetup::Active;
        answerer.video = Some(local_video(&answer_prefs));
        let (answer, negotiated) = build_answer(&answerer, &remote_offer).unwrap();
        assert_eq!(
            negotiated.video.as_ref().map(|v| (v.codec, v.payload_type)),
            Some((VideoCodec::VP8, 97))
        );
        // Strip REMB from the answer as a peer that does not do it would.
        let answer = answer.replace("a=rtcp-fb:97 goog-remb\r\n", "");

        let remote_answer = parse_remote(&answer).unwrap();
        let v = video_from_answer(
            &prefs,
            Direction::SendRecv,
            remote_answer.video.as_ref().unwrap(),
        )
        .expect("video accepted");
        assert_eq!((v.codec, v.payload_type), (VideoCodec::VP8, 97));
        assert!(v.nack && v.pli && v.fir && !v.remb);
        assert_eq!(v.direction, Direction::SendRecv);
        assert_eq!(v.remote_ssrc, Some(4242));

        // An answer that rejected the section pins nothing.
        let rejected = answer.replace("m=video 40000", "m=video 0");
        let remote_answer = parse_remote(&rejected).unwrap();
        assert!(remote_answer.video.is_none());
    }

    #[test]
    fn g722_is_clocked_at_8khz_and_offered_by_static_type() {
        assert_eq!(rtp_clock(AudioCodec::G722), 8_000);
        assert_eq!(rtp_clock(AudioCodec::Opus), 48_000);
        let cands = one_cand();
        let mut p = local(&cands);
        p.codecs = &[(AudioCodec::G722, 9), (AudioCodec::PCMU, 0)];
        let offer = build_offer(&p);
        assert!(
            offer.contains("m=audio 40000 UDP/TLS/RTP/SAVPF 9 0 101\r\n"),
            "{offer}"
        );
        assert!(offer.contains("a=rtpmap:9 G722/8000\r\n"), "{offer}");
        assert!(
            offer.contains("a=rtpmap:101 telephone-event/8000\r\n"),
            "{offer}"
        );
        let r = parse_remote(&offer).unwrap();
        assert_eq!(
            r.audio.unwrap().codecs,
            vec![(AudioCodec::G722, 9), (AudioCodec::PCMU, 0)]
        );
    }
}
