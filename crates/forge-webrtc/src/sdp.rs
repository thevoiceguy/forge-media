//! SDP construction and inspection for the endpoint-shaped peer connection.
//!
//! The peer connection negotiates one audio section (Opus, optional
//! telephone-event), BUNDLE + rtcp-mux, DTLS-SRTP and trickle ICE — the
//! shape every browser produces and the shape the DSIP WebRTC Media Binding
//! pins. Offers are built from scratch; answers mirror the remote offer
//! (payload types, `a=mid`, protocol) and reject every non-audio section with
//! port 0 so the answer stays a valid RFC 3264 answer to a browser offer that
//! carries video or data sections.

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
    /// Opus payload type we offer.
    pub opus_pt: u8,
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
    /// Payload type the remote uses for Opus.
    pub opus_pt: Option<u8>,
    /// Payload type the remote uses for telephone-event.
    pub dtmf_pt: Option<u8>,
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
        let opus_pt = m
            .rtpmaps
            .values()
            .find(|r| r.encoding_name.eq_ignore_ascii_case("opus"))
            .map(|r| r.payload_type);
        let mut dtmf: Vec<_> = m
            .rtpmaps
            .values()
            .filter(|r| r.encoding_name.eq_ignore_ascii_case("telephone-event"))
            .collect();
        // Prefer the one clocked like Opus (RFC 4733 §2.1).
        dtmf.sort_by_key(|r| if r.clock_rate == 48_000 { 0 } else { 1 });
        let dtmf_pt = dtmf.first().map(|r| r.payload_type);
        let ssrc = value_attr(&m.attributes, "ssrc")
            .and_then(|v| v.split(' ').next())
            .and_then(|v| v.parse().ok());
        RemoteAudio {
            index,
            mid: value_attr(&m.attributes, "mid").map(str::to_string),
            opus_pt,
            dtmf_pt,
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

fn audio_section(
    p: &LocalParams<'_>,
    port: u16,
    protocol: Protocol,
    mid: &str,
    direction: Direction,
    opus_pt: u8,
    dtmf_pt: Option<u8>,
) -> MediaDescription {
    let mut media = MediaDescription::audio(port);
    media.protocol = protocol;
    media.formats.push(SmolStr::new(opus_pt.to_string()));
    media.rtpmaps.insert(
        opus_pt,
        forge_sdp::RtpMap {
            payload_type: opus_pt,
            encoding_name: SmolStr::new("opus"),
            clock_rate: 48_000,
            encoding_params: Some(SmolStr::new("2")),
        },
    );
    if let Some(pt) = dtmf_pt {
        media.formats.push(SmolStr::new(pt.to_string()));
        media.rtpmaps.insert(
            pt,
            forge_sdp::RtpMap {
                payload_type: pt,
                encoding_name: SmolStr::new("telephone-event"),
                clock_rate: 8_000,
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
    push_value(a, "rtpmap", format!("{opus_pt} opus/48000/2"));
    push_value(a, "fmtp", format!("{opus_pt} minptime=10;useinbandfec=1"));
    if let Some(pt) = dtmf_pt {
        push_value(a, "rtpmap", format!("{pt} telephone-event/8000"));
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

/// Build an SDP offer.
pub fn build_offer(p: &LocalParams<'_>) -> String {
    let (_, port) = advertised(p.candidates);
    let audio = audio_section(
        p,
        port,
        Protocol::UdpTlsRtpSavpf,
        p.mid,
        p.direction,
        p.opus_pt,
        p.dtmf_pt,
    );
    session(p, &[p.mid], vec![audio])
}

/// Build an SDP answer to `remote`: accept the audio section (mirroring the
/// remote's payload types, `mid` and protocol), reject everything else.
pub fn build_answer(p: &LocalParams<'_>, remote: &RemoteDescription) -> Result<String> {
    let audio = remote
        .audio
        .as_ref()
        .ok_or_else(|| missing("audio section"))?;
    let opus_pt = audio
        .opus_pt
        .ok_or(WebRtcError::SdpError(SdpError::NoCommonCodec))?;
    let dtmf_pt = match p.dtmf_pt {
        Some(_) => audio.dtmf_pt,
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
        opus_pt,
        dtmf_pt,
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
    Ok(session(p, &[mid.as_str()], media))
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
            opus_pt: 111,
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
        assert_eq!(a.opus_pt, Some(111));
        assert_eq!(a.dtmf_pt, Some(110));
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
        let answer = build_answer(&p, &r).unwrap();
        assert!(answer.contains("a=group:BUNDLE 0\r\n"), "{answer}");
        assert!(
            answer.contains("m=audio 40000 UDP/TLS/RTP/SAVPF 111 110\r\n"),
            "{answer}"
        );
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
        assert_eq!(back.audio.unwrap().opus_pt, Some(111));
        assert_eq!(back.media.len(), 2);
    }

    #[test]
    fn offer_without_candidates_uses_trickle_placeholders() {
        let p = local(&[]);
        let offer = build_offer(&p);
        assert!(
            offer.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111 101\r\n"),
            "{offer}"
        );
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
    fn rejects_offer_without_opus() {
        let sdp = CHROME_OFFER.replace("a=rtpmap:111 opus/48000/2\r\n", "");
        let r = parse_remote(&sdp).unwrap();
        let p = local(&[]);
        assert!(matches!(
            build_answer(&p, &r),
            Err(WebRtcError::SdpError(SdpError::NoCommonCodec))
        ));
    }
}
