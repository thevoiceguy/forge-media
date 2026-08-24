//! SDP construction and inspection for the endpoint-shaped peer connection.
//!
//! The peer connection negotiates one audio section (Opus and/or G.711,
//! optional telephone-event), BUNDLE + rtcp-mux, DTLS-SRTP and trickle ICE —
//! the shape every browser produces and the shape the DSIP WebRTC Media
//! Binding pins. Offers are built from scratch; answers mirror the remote
//! offer (payload types, `a=mid`, protocol) and reject every non-audio
//! section with port 0 so the answer stays a valid RFC 3264 answer to a
//! browser offer that carries video or data sections.
//!
//! Codec selection: the local preference list ([`LocalParams::codecs`])
//! decides. An answer accepts exactly one codec — the first local preference
//! the remote offered — so the negotiated codec is pinned deterministically
//! rather than left to the sender. G.711 (PCMU/PCMA) is
//! mandatory-to-implement in WebRTC (RFC 7874 §3), so against a browser it
//! can always be preferred to skip transcoding toward a G.711 SIP leg.

use forge_core::AudioCodec;
use forge_ice::IceCandidate;
use forge_sdp::{
    Attribute, Connection, DtlsAttributesExt, DtlsSetup, IceAttributesExt, MediaDescription,
    MediaDtlsAttributesExt, MediaIceAttributesExt, MediaType, Protocol, SdpError,
    SessionDescription, SessionDescriptionExt,
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
    /// Our sending SSRC.
    pub ssrc: u32,
    /// RTCP CNAME.
    pub cname: &'a str,
    /// Desired direction.
    pub direction: Direction,
    /// Codecs we offer (or are willing to answer with), in preference
    /// order, each with the payload type we use when offering it. Only
    /// [`AudioCodec::Opus`], [`AudioCodec::PCMU`] and [`AudioCodec::PCMA`]
    /// are supported here.
    pub codecs: &'a [(AudioCodec, u8)],
    /// telephone-event payload type we offer, if any.
    pub dtmf_pt: Option<u8>,
    /// `a=mid` for our audio section in an offer.
    pub mid: &'a str,
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
    /// Codecs the remote listed that we can speak (Opus, PCMU, PCMA), with
    /// the remote's payload type for each, in the remote's `m=` format
    /// order. Static payload types 0/8 are recognised without an `a=rtpmap`
    /// line (RFC 3551 §6).
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
    /// Inline candidates of the audio section.
    pub candidates: Vec<IceCandidate>,
    /// `a=end-of-candidates` present.
    pub end_of_candidates: bool,
    /// `a=ice-options:trickle` present at either level.
    pub trickle: bool,
    /// `a=ice-lite` present.
    pub ice_lite: bool,
    /// The accepted audio section, if the remote offered/answered one.
    pub audio: Option<RemoteAudio>,
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
        let clock = codec.sample_rate();
        self.dtmf_pts
            .iter()
            .find(|(_, c)| *c == clock)
            .or_else(|| self.dtmf_pts.first())
            .copied()
    }
}

/// The first local preference the remote listed, with the **remote's**
/// payload type (an answer must mirror the offer's payload types, RFC 3264
/// §6.1). `None` means no codec in common.
pub fn select_codec(prefs: &[(AudioCodec, u8)], remote: &RemoteAudio) -> Option<(AudioCodec, u8)> {
    prefs
        .iter()
        .find_map(|&(c, _)| remote.pt_of(c).map(|pt| (c, pt)))
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

/// Parse a remote offer or answer.
pub fn parse_remote(sdp: &str) -> Result<RemoteDescription> {
    let desc = SessionDescription::from_str(sdp)?;

    let audio_index = desc
        .media
        .iter()
        .position(|m| m.media_type == MediaType::Audio && m.port != 0);
    let audio_media = audio_index.map(|i| &desc.media[i]);

    // Credentials, fingerprint and setup: media level first (Chrome), then
    // session level (Firefox, SIP-style endpoints).
    let (ufrag, pwd) = audio_media
        .and_then(|m| m.get_media_ice_credentials())
        .or_else(|| desc.get_ice_credentials())
        .ok_or_else(|| missing("ICE credentials"))?;
    let (fingerprint_alg, fingerprint) = audio_media
        .and_then(|m| m.get_media_dtls_fingerprint())
        .or_else(|| desc.get_dtls_fingerprint())
        .ok_or_else(|| missing("DTLS fingerprint"))?;
    if !fingerprint_alg.eq_ignore_ascii_case("sha-256") {
        return Err(WebRtcError::SdpError(SdpError::Internal(format!(
            "unsupported fingerprint algorithm {fingerprint_alg}"
        ))));
    }
    let setup = audio_media
        .and_then(|m| m.get_media_dtls_setup())
        .or_else(|| desc.get_dtls_setup())
        .ok_or_else(|| missing("DTLS setup"))?;

    let candidates = audio_media
        .map(|m| {
            MediaIceAttributesExt::get_ice_candidates(m)
                .iter()
                .filter_map(|s| IceCandidate::from_sdp_attribute(s).ok())
                .collect()
        })
        .unwrap_or_default();

    let trickle = desc.has_trickle_ice()
        || audio_media
            .map(|m| {
                m.attributes.iter().any(|a| {
                    matches!(a, Attribute::Value { name, value } if name == "ice-options" && value.split(' ').any(|v| v == "trickle"))
                })
            })
            .unwrap_or(false);
    let end_of_candidates = audio_media
        .map(|m| has_property(&m.attributes, "end-of-candidates"))
        .unwrap_or(false)
        || has_property(&desc.attributes, "end-of-candidates");
    let ice_lite = has_property(&desc.attributes, "ice-lite");

    let audio = audio_index.map(|index| {
        let m = &desc.media[index];
        // Walk the m= format list in order (the remote's preference order,
        // RFC 3264 §5.1), resolving each payload type through its rtpmap —
        // or, absent one, through the RFC 3551 static assignments for
        // G.711.
        let mut codecs = Vec::new();
        let mut dtmf_pts = Vec::new();
        for f in &m.formats {
            let Ok(pt) = f.parse::<u8>() else { continue };
            match m.rtpmaps.get(&pt) {
                Some(r) if r.encoding_name.eq_ignore_ascii_case("opus") => {
                    codecs.push((AudioCodec::Opus, pt));
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
                    _ => {}
                },
            }
        }
        let ssrc = value_attr(&m.attributes, "ssrc")
            .and_then(|v| v.split(' ').next())
            .and_then(|v| v.parse().ok());
        RemoteAudio {
            index,
            mid: value_attr(&m.attributes, "mid").map(str::to_string),
            codecs,
            dtmf_pts,
            direction: direction_of(&m.attributes)
                .or_else(|| direction_of(&desc.attributes))
                .unwrap_or_default(),
            ssrc,
            protocol: m.protocol.clone(),
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

/// The `a=rtpmap` encoding for a supported codec.
fn rtpmap_of(codec: AudioCodec) -> (&'static str, u32, Option<&'static str>) {
    match codec {
        AudioCodec::Opus => ("opus", 48_000, Some("2")),
        AudioCodec::PCMU => ("PCMU", 8_000, None),
        AudioCodec::PCMA => ("PCMA", 8_000, None),
        // LocalParams::codecs documents the supported set; peer.rs
        // constructs it from the same enum, so this is unreachable
        // without a code change here.
        other => unreachable!("unsupported WebRTC codec {other:?}"),
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
    if port == 9 {
        push_value(a, "rtcp", "9 IN IP4 0.0.0.0".into());
    }
    push_value(a, "ice-ufrag", p.ufrag.into());
    push_value(a, "ice-pwd", p.pwd.into());
    push_value(a, "ice-options", "trickle".into());
    push_value(a, "fingerprint", format!("sha-256 {}", p.fingerprint));
    push_value(a, "setup", p.setup.as_str().into());
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
    if direction.sends() {
        push_value(a, "ssrc", format!("{} cname:{}", p.ssrc, p.cname));
    }
    for c in p.candidates {
        media.add_ice_candidate_from_forge(c);
    }
    if p.end_of_candidates {
        push_prop(&mut media.attributes, "end-of-candidates");
    }
    media
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
    push_value(&mut sdp.attributes, "msid-semantic", " WMS *".into());
    sdp.media = media;
    forge_sdp::serialize::serialize_sdp(&sdp)
}

/// Build an SDP offer carrying every codec in [`LocalParams::codecs`], in
/// preference order. telephone-event, when offered, is clocked at the first
/// (preferred) codec's rate (RFC 4733 §2.1).
pub fn build_offer(p: &LocalParams<'_>) -> String {
    let (_, port) = advertised(p.candidates);
    let dtmf = p.dtmf_pt.map(|pt| {
        let clock = p
            .codecs
            .first()
            .map(|&(c, _)| c.sample_rate())
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
    session(p, &[p.mid], vec![audio])
}

/// Build an SDP answer to `remote`: accept the audio section with exactly
/// one codec — the first local preference the remote offered, at the
/// remote's payload type — and reject everything else. Returns the answer
/// and the selected codec.
pub fn build_answer(
    p: &LocalParams<'_>,
    remote: &RemoteDescription,
) -> Result<(String, (AudioCodec, u8))> {
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
    let mid = audio.mid.clone().unwrap_or_else(|| "0".to_string());
    let (_, port) = advertised(p.candidates);
    let direction = Direction::answer_for(audio.direction, p.direction);
    let accepted = audio_section(
        p,
        port,
        audio.protocol.clone(),
        &mid,
        direction,
        &[selected],
        dtmf,
    );

    let mut media = Vec::with_capacity(remote.media.len());
    for (i, m) in remote.media.iter().enumerate() {
        if i == audio.index {
            media.push(accepted.clone());
            continue;
        }
        // Rejected: port 0, same protocol and formats, mid mirrored (RFC 3264
        // §6, RFC 8843 §7.3).
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
        media.push(rejected);
    }
    Ok((session(p, &[mid.as_str()], media), selected))
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
            direction: Direction::SendRecv,
            codecs: &[
                (AudioCodec::Opus, 111),
                (AudioCodec::PCMU, 0),
                (AudioCodec::PCMA, 8),
            ],
            dtmf_pt: Some(101),
            mid: "0",
            session_id: 1,
            session_version: 1,
        }
    }

    #[test]
    fn parses_chrome_offer_media_level_attributes() {
        let r = parse_remote(CHROME_OFFER).unwrap();
        assert_eq!(r.ufrag, "abcd");
        assert_eq!(r.pwd, "efghijklmnopqrstuvwxyz0123");
        assert_eq!(r.setup, DtlsSetup::Actpass);
        assert!(r.trickle);
        assert_eq!(r.candidates.len(), 1);
        let a = r.audio.as_ref().unwrap();
        // Recognised codecs in the offer's m= order: opus 111, PCMU 0,
        // PCMA 8 (G722 and CN are skipped; red is an unknown encoding).
        assert_eq!(
            a.codecs,
            vec![
                (AudioCodec::Opus, 111),
                (AudioCodec::PCMU, 0),
                (AudioCodec::PCMA, 8),
            ]
        );
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
        let (answer, selected) = build_answer(&p, &r).unwrap();
        // Opus is our first preference and the offer has it; the answer's
        // telephone-event mirrors the 48 kHz-clocked one (110, not 126).
        assert_eq!(selected, (AudioCodec::Opus, 111));
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
        let (answer, selected) = build_answer(&p, &r).unwrap();
        assert_eq!(selected, (AudioCodec::PCMU, 0));
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
        let (answer, selected) = build_answer(&p, &r).unwrap();
        assert_eq!(selected, (AudioCodec::PCMA, 8));
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
}
