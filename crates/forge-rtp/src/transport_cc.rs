//! Transport-wide congestion control (draft-holmer-rmcat-transport-wide-cc-extensions-01).
//!
//! The sender numbers every RTP packet it sends on a transport with one
//! sequence, carried in a header extension ([`TRANSPORT_CC_URI`]). The
//! receiver tells it when each numbered packet arrived, or that it did not,
//! in RTPFB FMT 15 ([`TransportCcFeedback`]). The sender pairs those
//! arrivals with its own send times to estimate the path.

use crate::rtp::{RtpExtension, RtpPacket, ONE_BYTE_EXTENSION_PROFILE, TWO_BYTE_EXTENSION_PROFILE};
use bytes::{BufMut, BytesMut};
use forge_core::ForgeError;

/// The header extension's URI, as SDP's `a=extmap` names it.
pub const TRANSPORT_CC_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";

/// The feedback message's format number in an RTPFB packet.
pub const TRANSPORT_CC_FMT: u8 = 15;

/// One receive delta tick: 250 µs.
pub const DELTA_TICK_US: i64 = 250;
/// One reference-time tick: 64 ms.
pub const REFERENCE_TICK_US: i64 = 64_000;

/// The transport-wide sequence number `packet` carries under extension
/// `id`, if any.
pub fn transport_sequence(packet: &RtpPacket, id: u8) -> Option<u16> {
    let data = packet.extension.as_ref()?.element(id)?;
    (data.len() == 2).then(|| u16::from_be_bytes([data[0], data[1]]))
}

/// Write `seq` into `packet` as extension element `id`, replacing an
/// element already under that id and keeping every other one. The packet
/// keeps the extension form it had (one-byte unless it was two-byte).
pub fn stamp(packet: &mut RtpPacket, id: u8, seq: u16) {
    let value = seq.to_be_bytes();
    let (two_byte, mut elements): (bool, Vec<(u8, Vec<u8>)>) = match &packet.extension {
        Some(ext) if ext.profile & 0xFFF0 == TWO_BYTE_EXTENSION_PROFILE => (
            true,
            ext.elements()
                .into_iter()
                .map(|(i, d)| (i, d.to_vec()))
                .collect(),
        ),
        Some(ext) if ext.profile == ONE_BYTE_EXTENSION_PROFILE => (
            false,
            ext.elements()
                .into_iter()
                .map(|(i, d)| (i, d.to_vec()))
                .collect(),
        ),
        // Some other profile (not RFC 8285): it cannot hold an element,
        // so it gives way.
        _ => (false, Vec::new()),
    };
    elements.retain(|(i, _)| *i != id);
    elements.push((id, value.to_vec()));
    let refs: Vec<(u8, &[u8])> = elements.iter().map(|(i, d)| (*i, d.as_slice())).collect();
    packet.extension = Some(if two_byte || id > 14 {
        RtpExtension::two_byte(&refs)
    } else {
        RtpExtension::one_byte(&refs)
    });
}

/// What a feedback message says of one numbered packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketStatus {
    NotReceived,
    /// Arrived this many 250 µs ticks after the previous packet reported
    /// as arrived (the first: after the reference time). A small delta is
    /// 0..=255 ticks; anything else is large (two bytes, signed).
    Received(i16),
}

impl PacketStatus {
    fn symbol(self) -> u8 {
        match self {
            PacketStatus::NotReceived => 0,
            PacketStatus::Received(d) if (0..=255).contains(&d) => 1,
            PacketStatus::Received(_) => 2,
        }
    }
}

/// A transport-cc feedback message's FCI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportCcFeedback {
    /// The first packet reported.
    pub base_sequence: u16,
    /// A 24-bit signed time in 64 ms ticks, on the receiver's own clock.
    pub reference_time: i32,
    /// The receiver's count of feedback messages sent, wrapping: a gap
    /// says one was lost.
    pub feedback_count: u8,
    /// One per packet from `base_sequence` on.
    pub packets: Vec<PacketStatus>,
}

/// The most packets one message reports: past this, a receiver sends
/// another. Small enough that the message fits well inside an MTU.
pub const MAX_PACKETS_PER_FEEDBACK: usize = 500;

impl TransportCcFeedback {
    /// Every packet reported, with its arrival in µs on the receiver's
    /// clock (`reference_time` × 64 ms plus the deltas so far), or `None`
    /// for one that did not arrive.
    pub fn arrivals(&self) -> Vec<(u16, Option<i64>)> {
        let mut at = i64::from(self.reference_time) * REFERENCE_TICK_US;
        self.packets
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let seq = self.base_sequence.wrapping_add(i as u16);
                match s {
                    PacketStatus::NotReceived => (seq, None),
                    PacketStatus::Received(d) => {
                        at += i64::from(*d) * DELTA_TICK_US;
                        (seq, Some(at))
                    }
                }
            })
            .collect()
    }

    /// Read the FCI. Bytes after the last receive delta (padding) are
    /// ignored.
    pub fn parse(fci: &[u8]) -> Result<Self, ForgeError> {
        let err = |m: &str| ForgeError::Rtcp(format!("transport-cc: {m}"));
        if fci.len() < 8 {
            return Err(err("FCI shorter than its header"));
        }
        let base_sequence = u16::from_be_bytes([fci[0], fci[1]]);
        let count = u16::from_be_bytes([fci[2], fci[3]]) as usize;
        let raw = u32::from_be_bytes([0, fci[4], fci[5], fci[6]]);
        // Sign-extend 24 bits.
        let reference_time = ((raw << 8) as i32) >> 8;
        let feedback_count = fci[7];
        let mut pos = 8;
        let mut symbols: Vec<u8> = Vec::with_capacity(count);
        while symbols.len() < count {
            if pos + 2 > fci.len() {
                return Err(err("packet chunks end early"));
            }
            let chunk = u16::from_be_bytes([fci[pos], fci[pos + 1]]);
            pos += 2;
            let left = count - symbols.len();
            if chunk & 0x8000 == 0 {
                // Run length: T=0, S (2), run (13).
                let symbol = ((chunk >> 13) & 0x3) as u8;
                let run = (chunk & 0x1FFF) as usize;
                if run == 0 {
                    return Err(err("a run of nothing"));
                }
                symbols.extend(std::iter::repeat(symbol).take(run.min(left)));
            } else if chunk & 0x4000 == 0 {
                // Status vector of 14 one-bit symbols.
                for k in 0..14.min(left) {
                    symbols.push(((chunk >> (13 - k)) & 0x1) as u8);
                }
            } else {
                // Status vector of 7 two-bit symbols.
                for k in 0..7.min(left) {
                    symbols.push(((chunk >> (12 - 2 * k)) & 0x3) as u8);
                }
            }
        }
        let mut packets = Vec::with_capacity(count);
        for s in symbols {
            packets.push(match s {
                0 => PacketStatus::NotReceived,
                1 => {
                    let d = *fci.get(pos).ok_or_else(|| err("deltas end early"))?;
                    pos += 1;
                    PacketStatus::Received(i16::from(d))
                }
                2 => {
                    if pos + 2 > fci.len() {
                        return Err(err("deltas end early"));
                    }
                    let d = i16::from_be_bytes([fci[pos], fci[pos + 1]]);
                    pos += 2;
                    PacketStatus::Received(d)
                }
                _ => return Err(err("a reserved status")),
            });
        }
        Ok(Self {
            base_sequence,
            reference_time,
            feedback_count,
            packets,
        })
    }

    /// Write the FCI, unpadded: [`crate::rtcp::RtcpPacket::to_bytes`] pads
    /// the packet to a word as RFC 3550 says.
    pub fn to_fci(&self) -> Vec<u8> {
        let mut buf = BytesMut::with_capacity(16 + self.packets.len() * 2);
        buf.put_u16(self.base_sequence);
        buf.put_u16(self.packets.len() as u16);
        let rt = (self.reference_time as u32) & 0x00FF_FFFF;
        buf.put_u8((rt >> 16) as u8);
        buf.put_u8((rt >> 8) as u8);
        buf.put_u8(rt as u8);
        buf.put_u8(self.feedback_count);
        let symbols: Vec<u8> = self.packets.iter().map(|p| p.symbol()).collect();
        let mut i = 0;
        while i < symbols.len() {
            let run = symbols[i..]
                .iter()
                .take_while(|s| **s == symbols[i])
                .count();
            if run >= 14 {
                let run = run.min(0x1FFF);
                buf.put_u16((u16::from(symbols[i]) << 13) | run as u16);
                i += run;
            } else if symbols[i..].iter().take(14).all(|s| *s <= 1) {
                let mut chunk: u16 = 0x8000;
                for (k, s) in symbols[i..].iter().take(14).enumerate() {
                    chunk |= u16::from(*s) << (13 - k);
                }
                buf.put_u16(chunk);
                i += 14;
            } else {
                let mut chunk: u16 = 0xC000;
                for (k, s) in symbols[i..].iter().take(7).enumerate() {
                    chunk |= u16::from(*s) << (12 - 2 * k);
                }
                buf.put_u16(chunk);
                i += 7;
            }
        }
        for p in &self.packets {
            if let PacketStatus::Received(d) = p {
                if (0..=255).contains(d) {
                    buf.put_u8(*d as u8);
                } else {
                    buf.put_i16(*d);
                }
            }
        }
        buf.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn a_sequence_is_stamped_beside_what_a_packet_carried_and_read_back() {
        let mut p = RtpPacket::build(96, 7, 0, 1, Bytes::from_static(&[1, 2, 3]), true);
        p.extension = Some(RtpExtension::one_byte(&[(1, b"0"), (3, &[9; 2])]));
        stamp(&mut p, 3, 0xABCD);
        assert_eq!(transport_sequence(&p, 3), Some(0xABCD));
        assert_eq!(p.extension.as_ref().unwrap().element(1), Some(&b"0"[..]));
        let back = RtpPacket::parse(p.to_bytes().freeze()).unwrap();
        assert_eq!(transport_sequence(&back, 3), Some(0xABCD));
        assert_eq!(back.payload.as_ref(), &[1, 2, 3]);
        // A packet without an extension gets one.
        let mut bare = RtpPacket::build(96, 8, 0, 1, Bytes::from_static(&[4]), false);
        stamp(&mut bare, 5, 1);
        let back = RtpPacket::parse(bare.to_bytes().freeze()).unwrap();
        assert_eq!(transport_sequence(&back, 5), Some(1));
        assert_eq!(transport_sequence(&back, 3), None);
    }

    #[test]
    fn feedback_reads_back_through_every_kind_of_chunk() {
        let mut packets = Vec::new();
        // A long run of small deltas (a run-length chunk), a mixed stretch
        // with losses (one-bit vectors), a large and a negative delta
        // (two-bit vectors), then a long run of losses.
        packets.extend(std::iter::repeat(PacketStatus::Received(4)).take(40));
        for i in 0..20 {
            packets.push(if i % 3 == 0 {
                PacketStatus::NotReceived
            } else {
                PacketStatus::Received(i)
            });
        }
        packets.push(PacketStatus::Received(1000));
        packets.push(PacketStatus::Received(-8));
        packets.push(PacketStatus::Received(2));
        packets.extend(std::iter::repeat(PacketStatus::NotReceived).take(30));
        packets.push(PacketStatus::Received(255));
        let fb = TransportCcFeedback {
            base_sequence: 65_530,
            reference_time: -3,
            feedback_count: 9,
            packets,
        };
        let back = TransportCcFeedback::parse(&fb.to_fci()).unwrap();
        assert_eq!(back, fb);
        let arrivals = back.arrivals();
        assert_eq!(arrivals[0], (65_530, Some(-3 * 64_000 + 1_000)));
        assert_eq!(arrivals[6].0, 0, "the sequence wraps");
        assert_eq!(arrivals[40].1, None);
    }

    #[test]
    fn a_message_libwebrtc_wrote_reads() {
        // base 1, 4 packets, reference 0x000100 (256 × 64 ms), count 0;
        // a two-bit vector: received small, not received, received large,
        // received small; deltas 0x10, 0x01 0x00, 0x04.
        let fci = [
            0x00, 0x01, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00, 0xD2, 0x40, 0x10, 0x01, 0x00, 0x04,
            0x00, 0x00,
        ];
        let fb = TransportCcFeedback::parse(&fci).unwrap();
        assert_eq!(
            fb.packets,
            vec![
                PacketStatus::Received(0x10),
                PacketStatus::NotReceived,
                PacketStatus::Received(0x100),
                PacketStatus::Received(4),
            ]
        );
        assert_eq!(fb.reference_time, 256);
    }

    #[test]
    fn malformed_feedback_is_refused() {
        assert!(TransportCcFeedback::parse(&[0; 7]).is_err());
        // Says one packet, gives no chunk.
        assert!(TransportCcFeedback::parse(&[0, 0, 0, 1, 0, 0, 0, 0]).is_err());
        // A received packet with no delta byte.
        assert!(TransportCcFeedback::parse(&[0, 0, 0, 1, 0, 0, 0, 0, 0x20, 0x01]).is_err());
    }
}
