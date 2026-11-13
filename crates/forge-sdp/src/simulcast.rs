//! Simulcast in SDP: `a=rid` restriction identifiers (RFC 8851),
//! `a=simulcast` (RFC 8853) and the `a=extmap` header extensions that name
//! each RTP stream by its rid (RFC 8852, RFC 8285).
//!
//! A sender offering simulcast lists one `a=rid:<id> send` per encoding and
//! an `a=simulcast:send <id>;<id>;…`. Each `;`-separated entry is one
//! stream; a `,` inside an entry lists alternatives, and `~` marks a stream
//! paused. The answer reverses the direction (`recv`) and may drop
//! streams. The packets of each stream carry the rid in the
//! `sdes:rtp-stream-id` header extension until the receiver knows its SSRC.

use crate::{Attribute, MediaDescription, SessionDescription};
use smol_str::SmolStr;

/// RFC 8852's RTP stream id header extension: the rid of the stream a
/// packet belongs to.
pub const RTP_STREAM_ID_URI: &str = "urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id";

/// RFC 8852's repaired RTP stream id: the rid an RTX stream repairs.
pub const REPAIRED_RTP_STREAM_ID_URI: &str =
    "urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id";

/// The direction of an `a=rid` or an `a=simulcast` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RidDirection {
    Send,
    Recv,
}

impl RidDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Recv => "recv",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "send" => Some(Self::Send),
            "recv" => Some(Self::Recv),
            _ => None,
        }
    }

    /// What the other party writes for the same streams.
    pub fn reversed(self) -> Self {
        match self {
            Self::Send => Self::Recv,
            Self::Recv => Self::Send,
        }
    }
}

/// One `a=rid` line (RFC 8851 §10): `a=rid:<id> <direction> [<restrictions>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rid {
    pub id: SmolStr,
    pub direction: RidDirection,
    /// The restrictions as written (`pt=96;max-width=1280`), `""` when none.
    pub restrictions: SmolStr,
}

impl Rid {
    /// A restriction's value (`max-height`, `max-fps`, …).
    pub fn restriction(&self, name: &str) -> Option<&str> {
        self.restrictions
            .split(';')
            .filter_map(|r| r.split_once('='))
            .find(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim())
    }

    /// The `max-height` restriction, when the sender gave one.
    pub fn max_height(&self) -> Option<u32> {
        self.restriction("max-height")?.parse().ok()
    }

    /// The `max-width` restriction, when the sender gave one.
    pub fn max_width(&self) -> Option<u32> {
        self.restriction("max-width")?.parse().ok()
    }
}

/// A rid token is 1–255 alphanumerics, `-` or `_` (RFC 8851 §10).
fn is_rid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Every well-formed `a=rid` on a section, in order.
pub fn rids(media: &MediaDescription) -> Vec<Rid> {
    media
        .attributes
        .iter()
        .filter_map(|a| match a {
            Attribute::Value { name, value } if name == "rid" => parse_rid(value),
            _ => None,
        })
        .collect()
}

fn parse_rid(value: &str) -> Option<Rid> {
    let mut parts = value.trim().splitn(3, ' ');
    let id = parts.next()?;
    let direction = RidDirection::parse(parts.next()?)?;
    if !is_rid_token(id) {
        return None;
    }
    Some(Rid {
        id: SmolStr::new(id),
        direction,
        restrictions: SmolStr::new(parts.next().unwrap_or("").trim()),
    })
}

/// One stream of an `a=simulcast` list: a rid, and whether it starts paused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulcastStream {
    pub rid: SmolStr,
    pub paused: bool,
}

/// A section's `a=simulcast` (RFC 8853 §5.1): its send and receive
/// lists, each a list of streams, each a list of alternatives in order
/// of preference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Simulcast {
    pub send: Vec<Vec<SimulcastStream>>,
    pub recv: Vec<Vec<SimulcastStream>>,
}

impl Simulcast {
    pub fn list(&self, direction: RidDirection) -> &[Vec<SimulcastStream>] {
        match direction {
            RidDirection::Send => &self.send,
            RidDirection::Recv => &self.recv,
        }
    }
}

/// The section's `a=simulcast`, when it has a well-formed one.
pub fn simulcast(media: &MediaDescription) -> Option<Simulcast> {
    let value = media.attributes.iter().find_map(|a| match a {
        Attribute::Value { name, value } if name == "simulcast" => Some(value.as_str()),
        _ => None,
    })?;
    parse_simulcast(value)
}

fn parse_simulcast(value: &str) -> Option<Simulcast> {
    let mut out = Simulcast::default();
    let mut words = value.split_whitespace();
    let mut any = false;
    while let Some(dir) = words.next() {
        let direction = RidDirection::parse(dir)?;
        let list = words.next()?;
        let mut streams = Vec::new();
        for entry in list.split(';') {
            let mut alternatives = Vec::new();
            for alt in entry.split(',') {
                let (paused, rid) = match alt.strip_prefix('~') {
                    Some(rest) => (true, rest),
                    None => (false, alt),
                };
                if !is_rid_token(rid) {
                    return None;
                }
                alternatives.push(SimulcastStream {
                    rid: SmolStr::new(rid),
                    paused,
                });
            }
            streams.push(alternatives);
        }
        match direction {
            RidDirection::Send if out.send.is_empty() => out.send = streams,
            RidDirection::Recv if out.recv.is_empty() => out.recv = streams,
            // The same direction twice is malformed.
            _ => return None,
        }
        any = true;
    }
    any.then_some(out)
}

/// The id an `a=extmap` list assigns to `uri`
/// (`a=extmap:<id>[/<direction>] <uri> [<attributes>]`).
pub fn extmap_id(attrs: &[Attribute], uri: &str) -> Option<u8> {
    attrs.iter().find_map(|a| match a {
        Attribute::Value { name, value } if name == "extmap" => {
            let mut parts = value.split_whitespace();
            let id = parts.next()?.split('/').next()?.parse::<u8>().ok()?;
            (parts.next()? == uri && id >= 1).then_some(id)
        }
        _ => None,
    })
}

/// The id of `uri` on a section, or at session level when the section has
/// none (RFC 8285 §5 lets either carry it).
pub fn media_extmap_id(
    session: &SessionDescription,
    media: &MediaDescription,
    uri: &str,
) -> Option<u8> {
    extmap_id(&media.attributes, uri).or_else(|| extmap_id(&session.attributes, uri))
}

/// Add `a=extmap:<id> <uri>` to a section.
pub fn push_extmap(media: &mut MediaDescription, id: u8, uri: &str) {
    media.attributes.push(Attribute::Value {
        name: SmolStr::new("extmap"),
        value: SmolStr::new(format!("{id} {uri}")),
    });
}

/// The streams one side of a simulcast section names, as negotiated:
/// one rid per stream (the first alternative described by an `a=rid` in
/// the matching direction), in the list's order, with the id of the
/// rid header extension when one was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulcastLayers {
    /// The rids, in the order of the `a=simulcast` list.
    pub rids: Vec<Rid>,
    /// Which streams start paused (`~`), parallel to `rids`.
    pub paused: Vec<bool>,
    /// The `sdes:rtp-stream-id` extension's id, when negotiated.
    pub rid_extension: Option<u8>,
}

/// The simulcast streams a section says it will send (`direction` =
/// `Send`) or receive (`Recv`): only streams whose rid has an `a=rid` in
/// that direction count, and at most `max` of them, in the list's order.
/// `None` when the section has no simulcast in that direction or fewer
/// than two usable streams (one stream is not simulcast).
pub fn simulcast_layers(
    session: &SessionDescription,
    media: &MediaDescription,
    direction: RidDirection,
    max: usize,
) -> Option<SimulcastLayers> {
    let list = simulcast(media)?;
    let described: Vec<Rid> = rids(media)
        .into_iter()
        .filter(|r| r.direction == direction)
        .collect();
    let mut chosen = Vec::new();
    let mut paused = Vec::new();
    for alternatives in list.list(direction) {
        if chosen.len() >= max {
            break;
        }
        if let Some((alt, rid)) = alternatives.iter().find_map(|alt| {
            described
                .iter()
                .find(|r| r.id == alt.rid)
                .map(|rid| (alt, rid))
        }) {
            if !chosen.iter().any(|r: &Rid| r.id == rid.id) {
                chosen.push(rid.clone());
                paused.push(alt.paused);
            }
        }
    }
    if chosen.len() < 2 {
        return None;
    }
    Some(SimulcastLayers {
        rids: chosen,
        paused,
        rid_extension: media_extmap_id(session, media, RTP_STREAM_ID_URI),
    })
}

/// Write simulcast onto a section: an `a=rid:<id> <direction>` per layer
/// (restrictions kept), `a=simulcast:<direction> <id>;<id>;…` and, when
/// `layers.rid_extension` is set, the rid header extension under that id.
/// Any simulcast, rid or rid-extension attribute already there is
/// replaced. An answer passes the offer's layers it accepts with the
/// direction reversed.
pub fn write_simulcast(
    media: &mut MediaDescription,
    direction: RidDirection,
    layers: &SimulcastLayers,
) {
    media.attributes.retain(|a| match a {
        Attribute::Value { name, value } => {
            !(name == "rid"
                || name == "simulcast"
                || (name == "extmap" && value.split_whitespace().nth(1) == Some(RTP_STREAM_ID_URI)))
        }
        Attribute::Property(_) => true,
    });
    if let Some(id) = layers.rid_extension {
        push_extmap(media, id, RTP_STREAM_ID_URI);
    }
    for rid in &layers.rids {
        let value = if rid.restrictions.is_empty() {
            format!("{} {}", rid.id, direction.as_str())
        } else {
            format!("{} {} {}", rid.id, direction.as_str(), rid.restrictions)
        };
        media.attributes.push(Attribute::Value {
            name: SmolStr::new("rid"),
            value: SmolStr::new(value),
        });
    }
    let list = layers
        .rids
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if layers.paused.get(i).copied().unwrap_or(false) {
                format!("~{}", r.id)
            } else {
                r.id.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(";");
    media.attributes.push(Attribute::Value {
        name: SmolStr::new("simulcast"),
        value: SmolStr::new(format!("{} {list}", direction.as_str())),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionDescription, SessionDescriptionExt};

    fn sdp(video: &str) -> SessionDescription {
        let text = format!(
            "v=0\r\no=- 1 1 IN IP4 192.0.2.1\r\ns=-\r\nc=IN IP4 192.0.2.1\r\nt=0 0\r\n\
             m=video 4000 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n{video}"
        );
        SessionDescription::from_str(&text).expect("parses")
    }

    #[test]
    fn rids_are_read_with_their_restrictions() {
        let s = sdp("a=rid:h send max-height=720;max-fps=30\r\na=rid:l send\r\n\
             a=rid:bad! send\r\na=rid:x sideways\r\n");
        let r = rids(&s.media[0]);
        assert_eq!(r.len(), 2, "malformed rids are skipped: {r:?}");
        assert_eq!(r[0].id, "h");
        assert_eq!(r[0].direction, RidDirection::Send);
        assert_eq!(r[0].max_height(), Some(720));
        assert_eq!(r[0].restriction("max-fps"), Some("30"));
        assert_eq!(r[1].restrictions, "");
        assert_eq!(r[1].max_height(), None);
    }

    #[test]
    fn a_simulcast_list_has_streams_alternatives_and_pauses() {
        let s = sdp("a=simulcast:send h;m,m2;~l recv r\r\n");
        let sc = simulcast(&s.media[0]).expect("simulcast");
        assert_eq!(sc.send.len(), 3);
        assert_eq!(sc.send[1].len(), 2, "two alternatives");
        assert!(sc.send[2][0].paused);
        assert_eq!(sc.send[2][0].rid, "l");
        assert_eq!(sc.recv.len(), 1);
        assert!(parse_simulcast("send h recv").is_none());
        assert!(parse_simulcast("send h send l").is_none());
        assert!(parse_simulcast("sideways h").is_none());
    }

    #[test]
    fn the_layers_offered_are_those_with_a_rid_in_that_direction() {
        let s = sdp(
            "a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n\
             a=rid:h send\r\na=rid:m send\r\na=rid:l send\r\na=rid:q recv\r\n\
             a=simulcast:send h;m;~l;q\r\n",
        );
        let layers =
            simulcast_layers(&s, &s.media[0], RidDirection::Send, 8).expect("simulcast offered");
        let ids: Vec<&str> = layers.rids.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["h", "m", "l"], "q has no send rid");
        assert_eq!(layers.paused, [false, false, true]);
        assert_eq!(layers.rid_extension, Some(4));

        let capped = simulcast_layers(&s, &s.media[0], RidDirection::Send, 2).unwrap();
        assert_eq!(capped.rids.len(), 2);
        assert!(simulcast_layers(&s, &s.media[0], RidDirection::Recv, 8).is_none());
    }

    #[test]
    fn one_stream_is_not_simulcast() {
        let s = sdp("a=rid:h send\r\na=simulcast:send h\r\n");
        assert!(simulcast_layers(&s, &s.media[0], RidDirection::Send, 3).is_none());
    }

    #[test]
    fn the_extension_may_be_at_session_level() {
        let text = "v=0\r\no=- 1 1 IN IP4 192.0.2.1\r\ns=-\r\nc=IN IP4 192.0.2.1\r\nt=0 0\r\n\
                    a=extmap:7 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n\
                    m=video 4000 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n\
                    a=rid:a send\r\na=rid:b send\r\na=simulcast:send a;b\r\n";
        let s = SessionDescription::from_str(text).unwrap();
        let layers = simulcast_layers(&s, &s.media[0], RidDirection::Send, 3).unwrap();
        assert_eq!(layers.rid_extension, Some(7));
    }

    #[test]
    fn an_answer_reverses_the_layers_it_takes() {
        let offer = sdp(
            "a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n\
             a=rid:h send max-height=720\r\na=rid:l send\r\na=simulcast:send h;l\r\n",
        );
        let layers = simulcast_layers(&offer, &offer.media[0], RidDirection::Send, 3).unwrap();
        let mut answer =
            sdp("a=rid:old recv\r\na=extmap:9 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n");
        write_simulcast(&mut answer.media[0], RidDirection::Recv, &layers);
        let text = crate::serialize::serialize_sdp(&answer);
        assert!(
            text.contains("a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n"),
            "{text}"
        );
        assert!(
            !text.contains("a=extmap:9"),
            "the old mapping is replaced: {text}"
        );
        assert!(!text.contains("a=rid:old"), "{text}");
        assert!(text.contains("a=rid:h recv max-height=720\r\n"), "{text}");
        assert!(text.contains("a=rid:l recv\r\n"), "{text}");
        assert!(text.contains("a=simulcast:recv h;l\r\n"), "{text}");

        let back = SessionDescription::from_str(&text).unwrap();
        let read = simulcast_layers(&back, &back.media[0], RidDirection::Recv, 3).unwrap();
        assert_eq!(read.rids.len(), 2);
        assert_eq!(read.rid_extension, Some(4));
    }
}
