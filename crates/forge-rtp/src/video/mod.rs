//! Video RTP handling without decoding.
//!
//! Forge forwards video as opaque RTP. What a forwarder still has to know
//! is where frames begin and which frames are keyframes, because a
//! receiver that starts (or switches to) a stream in the middle of a
//! group of pictures shows garbage until the next keyframe. This module
//! reads just enough of each payload format to answer that:
//!
//! - H.264, RFC 6184: single NAL, STAP-A and FU-A, keyframe = IDR (5) or
//!   an SPS (7) leading the access unit.
//! - H.265, RFC 7798: single NAL, AP and FU, keyframe = IRAP (16..=23).
//! - VP8, RFC 7741: payload descriptor, then the payload header's P bit.
//! - VP9, RFC 9628: payload descriptor P bit.
//! - AV1, AV1 RTP spec: aggregation header N bit (new coded video
//!   sequence).
//!
//! [`StreamRewriter`] then turns several sources into one continuous
//! outgoing stream — one SSRC, monotonic sequence numbers and timestamps
//! — switching sources only at a keyframe, which is what a conference
//! needs to show one participant's video to another and change who that
//! is.
//!
//! The rest of the substrate a video mixer needs lives in the submodules:
//! [`payload`] turns RTP payloads into coded frames and back for every
//! codec, [`assembler`] groups a source's packets into frames and notices
//! loss, and [`cache`] keeps recent outgoing packets for NACK.

pub mod assembler;
pub mod cache;
pub mod payload;

pub use assembler::{AssemblerEvent, FrameAssembler};
pub use cache::{KeyframeRequestGate, RtxCache};
pub use payload::{depacketize, packetize, CodedFrame, PayloadError};

use crate::rtp::RtpPacket;
use forge_core::VideoCodec;
use std::time::Instant;

/// What one RTP payload tells us about the frame it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadInfo {
    /// This packet starts a frame (first packet of the access unit /
    /// picture / temporal unit).
    pub frame_start: bool,
    /// The frame this packet starts is a keyframe (only meaningful when
    /// `frame_start` is set; false otherwise).
    pub keyframe: bool,
}

impl PayloadInfo {
    const NONE: PayloadInfo = PayloadInfo {
        frame_start: false,
        keyframe: false,
    };

    /// A packet that starts a keyframe.
    pub fn is_keyframe_start(&self) -> bool {
        self.frame_start && self.keyframe
    }
}

/// Inspect a payload of `codec`. Unknown or truncated payloads report
/// no frame start.
pub fn inspect(codec: VideoCodec, payload: &[u8]) -> PayloadInfo {
    match codec {
        VideoCodec::H264 => h264::inspect(payload),
        VideoCodec::H265 => h265::inspect(payload),
        VideoCodec::VP8 => vp8::inspect(payload),
        VideoCodec::VP9 => vp9::inspect(payload),
        VideoCodec::AV1 => av1::inspect(payload),
    }
}

pub mod h264 {
    //! RFC 6184 payload inspection.
    use super::PayloadInfo;

    const NAL_IDR: u8 = 5;
    const NAL_SPS: u8 = 7;
    const NAL_PPS: u8 = 8;
    const NAL_AUD: u8 = 9;
    const NAL_STAP_A: u8 = 24;
    const NAL_STAP_B: u8 = 25;
    const NAL_MTAP16: u8 = 26;
    const NAL_MTAP24: u8 = 27;
    const NAL_FU_A: u8 = 28;
    const NAL_FU_B: u8 = 29;

    /// Whether a NAL unit type begins (or is) a keyframe.
    fn nal_is_key(nal_type: u8) -> bool {
        matches!(nal_type, NAL_IDR | NAL_SPS)
    }

    /// Parameter sets and delimiters may precede the first slice of an
    /// access unit; they start a frame without being one.
    fn nal_starts_frame(nal_type: u8) -> bool {
        (1..=5).contains(&nal_type) || matches!(nal_type, NAL_SPS | NAL_PPS | NAL_AUD)
    }

    pub fn inspect(payload: &[u8]) -> PayloadInfo {
        let Some(&first) = payload.first() else {
            return PayloadInfo::NONE;
        };
        let nal_type = first & 0x1F;
        match nal_type {
            NAL_STAP_A => {
                // Aggregation: [hdr][size][NAL][size][NAL]...; the first
                // NAL decides, but any IDR/SPS inside makes it a keyframe.
                let mut off = 1;
                let mut first_nal = None;
                let mut key = false;
                while off + 2 < payload.len() {
                    let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
                    off += 2;
                    if off >= payload.len() {
                        break;
                    }
                    let t = payload[off] & 0x1F;
                    first_nal.get_or_insert(t);
                    key |= nal_is_key(t);
                    off += size.max(1);
                }
                match first_nal {
                    Some(t) => PayloadInfo {
                        frame_start: nal_starts_frame(t) || key,
                        keyframe: key,
                    },
                    None => PayloadInfo::NONE,
                }
            }
            NAL_FU_A | NAL_FU_B => {
                // Fragment: [indicator][FU header: S E R type]
                let Some(&fu) = payload.get(1) else {
                    return PayloadInfo::NONE;
                };
                let start = fu & 0x80 != 0;
                let t = fu & 0x1F;
                PayloadInfo {
                    frame_start: start && nal_starts_frame(t),
                    keyframe: start && nal_is_key(t),
                }
            }
            NAL_STAP_B | NAL_MTAP16 | NAL_MTAP24 => PayloadInfo::NONE,
            t => PayloadInfo {
                frame_start: nal_starts_frame(t),
                keyframe: nal_is_key(t),
            },
        }
    }
}

pub mod h265 {
    //! RFC 7798 payload inspection.
    use super::PayloadInfo;

    const NAL_AP: u8 = 48;
    const NAL_FU: u8 = 49;

    /// IRAP pictures: BLA, IDR, CRA and reserved IRAP types 16..=23.
    fn nal_is_key(t: u8) -> bool {
        (16..=23).contains(&t)
    }

    /// VCL NAL units (0..=31) and the parameter sets / AUD (32..=35).
    fn nal_starts_frame(t: u8) -> bool {
        t <= 35
    }

    pub fn inspect(payload: &[u8]) -> PayloadInfo {
        if payload.len() < 2 {
            return PayloadInfo::NONE;
        }
        let nal_type = (payload[0] >> 1) & 0x3F;
        match nal_type {
            NAL_AP => {
                let mut off = 2;
                let mut first = None;
                let mut key = false;
                while off + 3 < payload.len() {
                    let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
                    off += 2;
                    let t = (payload[off] >> 1) & 0x3F;
                    first.get_or_insert(t);
                    key |= nal_is_key(t);
                    off += size.max(1);
                }
                match first {
                    Some(t) => PayloadInfo {
                        frame_start: nal_starts_frame(t) || key,
                        keyframe: key,
                    },
                    None => PayloadInfo::NONE,
                }
            }
            NAL_FU => {
                // [2-byte PayloadHdr][FU header: S E FuType]
                let Some(&fu) = payload.get(2) else {
                    return PayloadInfo::NONE;
                };
                let start = fu & 0x80 != 0;
                let t = fu & 0x3F;
                PayloadInfo {
                    frame_start: start && nal_starts_frame(t),
                    keyframe: start && nal_is_key(t),
                }
            }
            t => PayloadInfo {
                frame_start: nal_starts_frame(t),
                keyframe: nal_is_key(t),
            },
        }
    }
}

pub mod vp8 {
    //! RFC 7741 payload descriptor and header inspection.
    use super::PayloadInfo;

    /// Length of the payload descriptor, or `None` when truncated.
    pub fn descriptor_len(p: &[u8]) -> Option<usize> {
        let b0 = *p.first()?;
        let mut len = 1;
        if b0 & 0x80 != 0 {
            // X: extension byte
            let x = *p.get(len)?;
            len += 1;
            if x & 0x80 != 0 {
                // I: PictureID, 1 or 2 bytes (M bit)
                let i = *p.get(len)?;
                len += if i & 0x80 != 0 { 2 } else { 1 };
            }
            if x & 0x40 != 0 {
                len += 1; // L: TL0PICIDX
            }
            if x & 0x30 != 0 {
                len += 1; // T/K: TID/KEYIDX
            }
        }
        (len <= p.len()).then_some(len)
    }

    pub fn inspect(payload: &[u8]) -> PayloadInfo {
        let Some(len) = descriptor_len(payload) else {
            return PayloadInfo::NONE;
        };
        let b0 = payload[0];
        let start_of_partition = b0 & 0x10 != 0;
        let partition_index = b0 & 0x07;
        let frame_start = start_of_partition && partition_index == 0;
        // Payload header follows the descriptor; its P bit (LSB of the
        // first byte) is 0 for a keyframe.
        let keyframe = frame_start && payload.get(len).map(|hdr| hdr & 0x01 == 0).unwrap_or(false);
        PayloadInfo {
            frame_start,
            keyframe,
        }
    }
}

pub mod vp9 {
    //! RFC 9628 payload descriptor inspection.
    use super::PayloadInfo;

    pub fn inspect(payload: &[u8]) -> PayloadInfo {
        let Some(&b0) = payload.first() else {
            return PayloadInfo::NONE;
        };
        // I P L F B E V Z
        let inter_picture = b0 & 0x40 != 0; // P
        let begins_frame = b0 & 0x08 != 0; // B
        let layer_indices = b0 & 0x20 != 0; // L
                                            // With layer indices, only spatial layer 0 starts the picture.
        let sid_zero = if layer_indices {
            let mut off = 1;
            if b0 & 0x80 != 0 {
                // I: picture id, 1 or 2 bytes
                off += match payload.get(1) {
                    Some(m) if m & 0x80 != 0 => 2,
                    Some(_) => 1,
                    None => return PayloadInfo::NONE,
                };
            }
            match payload.get(off) {
                Some(l) => (l >> 1) & 0x07 == 0,
                None => return PayloadInfo::NONE,
            }
        } else {
            true
        };
        let frame_start = begins_frame && sid_zero;
        PayloadInfo {
            frame_start,
            keyframe: frame_start && !inter_picture,
        }
    }
}

pub mod av1 {
    //! AV1 RTP aggregation header inspection.
    use super::PayloadInfo;

    pub fn inspect(payload: &[u8]) -> PayloadInfo {
        let Some(&b0) = payload.first() else {
            return PayloadInfo::NONE;
        };
        // Z Y W W N - - -
        let continues_previous = b0 & 0x80 != 0; // Z
        let new_sequence = b0 & 0x08 != 0; // N
        let frame_start = !continues_previous;
        PayloadInfo {
            frame_start,
            keyframe: frame_start && new_sequence,
        }
    }
}

// ─── Stream rewriting ────────────────────────────────────────────────────

/// Turns packets from any number of sources into one outgoing stream.
///
/// The receiver sees a single SSRC with continuous sequence numbers and
/// timestamps whatever source is being forwarded. A source change takes
/// effect at the next keyframe from the new source ([`select`](Self::select)
/// arms it; the caller asks the new source for one with PLI/FIR); until
/// then the old source keeps flowing and the new source's packets are
/// dropped, so the receiver never decodes a frame whose references it
/// has not seen.
#[derive(Debug)]
pub struct StreamRewriter {
    codec: VideoCodec,
    out_ssrc: u32,
    /// Source currently forwarded and its offsets.
    current: Option<Source>,
    /// Source to switch to at its next keyframe.
    pending: Option<u32>,
    /// Last values written on the outgoing stream.
    last_seq: u16,
    last_ts: u32,
    last_sent: Option<Instant>,
    switches: u64,
}

#[derive(Debug, Clone, Copy)]
struct Source {
    ssrc: u32,
    seq_offset: u16,
    ts_offset: u32,
}

impl StreamRewriter {
    /// A rewriter emitting `out_ssrc`; sequence numbers and timestamps
    /// start from the given values.
    pub fn new(codec: VideoCodec, out_ssrc: u32, first_seq: u16, first_ts: u32) -> Self {
        Self {
            codec,
            out_ssrc,
            current: None,
            pending: None,
            last_seq: first_seq.wrapping_sub(1),
            last_ts: first_ts,
            last_sent: None,
            switches: 0,
        }
    }

    pub fn out_ssrc(&self) -> u32 {
        self.out_ssrc
    }

    /// The source currently being forwarded.
    pub fn current_source(&self) -> Option<u32> {
        self.current.map(|s| s.ssrc)
    }

    /// The source a switch is waiting on, if any.
    pub fn pending_source(&self) -> Option<u32> {
        self.pending
    }

    /// How many times the forwarded source has changed.
    pub fn switches(&self) -> u64 {
        self.switches
    }

    /// Forward `ssrc` from its next keyframe on. Selecting the current
    /// source cancels a pending switch. Returns whether a keyframe is now
    /// needed from `ssrc` (the caller should request one).
    pub fn select(&mut self, ssrc: u32) -> bool {
        if self.current.map(|s| s.ssrc) == Some(ssrc) {
            self.pending = None;
            return false;
        }
        self.pending = Some(ssrc);
        true
    }

    /// Stop forwarding anything (the source left).
    pub fn clear(&mut self) {
        self.current = None;
        self.pending = None;
    }

    /// Offer a packet from one of the sources. Returns the packet to send
    /// on the outgoing stream, rewritten, or `None` when it is dropped.
    pub fn feed(&mut self, packet: &RtpPacket) -> Option<RtpPacket> {
        let ssrc = packet.header.ssrc;
        if self.pending == Some(ssrc) && inspect(self.codec, &packet.payload).is_keyframe_start() {
            self.switch_to(ssrc, packet);
        }
        let source = self.current?;
        if source.ssrc != ssrc {
            return None;
        }
        let seq = packet
            .header
            .sequence_number
            .wrapping_add(source.seq_offset);
        let ts = packet.header.timestamp.wrapping_add(source.ts_offset);
        self.last_seq = seq;
        self.last_ts = ts;
        self.last_sent = Some(Instant::now());
        let mut out = packet.clone();
        out.header.ssrc = self.out_ssrc;
        out.header.sequence_number = seq;
        out.header.timestamp = ts;
        Some(out)
    }

    /// Make `ssrc` current, with offsets so that `packet` continues the
    /// outgoing stream: the next sequence number, and a timestamp advanced
    /// by the wall-clock time since the last packet went out (at least
    /// one frame period, so the receiver never sees time stand still).
    fn switch_to(&mut self, ssrc: u32, packet: &RtpPacket) {
        // At least one frame period at 30 fps (3000 ticks at 90 kHz).
        let gap_ticks = self
            .last_sent
            .map(|t| (t.elapsed().as_millis() as u32).saturating_mul(90))
            .unwrap_or(0)
            .max(3000);
        let next_seq = self.last_seq.wrapping_add(1);
        let next_ts = if self.current.is_some() {
            self.last_ts.wrapping_add(gap_ticks)
        } else {
            self.last_ts
        };
        self.current = Some(Source {
            ssrc,
            seq_offset: next_seq.wrapping_sub(packet.header.sequence_number),
            ts_offset: next_ts.wrapping_sub(packet.header.timestamp),
        });
        self.pending = None;
        self.switches += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn pkt(ssrc: u32, seq: u16, ts: u32, payload: &[u8]) -> RtpPacket {
        RtpPacket::build(96, seq, ts, ssrc, Bytes::copy_from_slice(payload), false)
    }

    // The header is packed: copy fields out before comparing.
    fn seq(p: &RtpPacket) -> u16 {
        p.header.sequence_number
    }
    fn ts(p: &RtpPacket) -> u32 {
        p.header.timestamp
    }
    fn ssrc(p: &RtpPacket) -> u32 {
        p.header.ssrc
    }

    #[test]
    fn h264_keyframes_are_idr_or_sps_in_any_packetization() {
        // Single NAL units.
        assert!(inspect(VideoCodec::H264, &[0x65, 0x88]).is_keyframe_start()); // IDR
        assert!(inspect(VideoCodec::H264, &[0x67, 0x42]).is_keyframe_start()); // SPS
        let p = inspect(VideoCodec::H264, &[0x41, 0x9A]); // non-IDR slice
        assert!(p.frame_start && !p.keyframe);
        assert_eq!(inspect(VideoCodec::H264, &[0x06, 0x05]), PayloadInfo::NONE); // SEI
                                                                                 // STAP-A carrying SPS + PPS + IDR.
        let stap = [0x78, 0, 2, 0x67, 0x42, 0, 2, 0x68, 0xCE, 0, 2, 0x65, 0x88];
        assert!(inspect(VideoCodec::H264, &stap).is_keyframe_start());
        // FU-A: start fragment of an IDR is a keyframe start, middle
        // fragments are nothing, start of a P-slice is a frame start only.
        assert!(inspect(VideoCodec::H264, &[0x7C, 0x85, 0xB8]).is_keyframe_start());
        assert_eq!(
            inspect(VideoCodec::H264, &[0x7C, 0x05, 0xB8]),
            PayloadInfo::NONE
        );
        let p = inspect(VideoCodec::H264, &[0x7C, 0x81, 0xB8]);
        assert!(p.frame_start && !p.keyframe);
        assert_eq!(inspect(VideoCodec::H264, &[]), PayloadInfo::NONE);
        assert_eq!(inspect(VideoCodec::H264, &[0x7C]), PayloadInfo::NONE);
    }

    #[test]
    fn h265_keyframes_are_irap_nal_units() {
        // NAL type is bits 1..=6 of the first byte: IDR_W_RADL = 19.
        assert!(inspect(VideoCodec::H265, &[19 << 1, 0x01]).is_keyframe_start());
        let p = inspect(VideoCodec::H265, &[1 << 1, 0x01]); // TRAIL_R
        assert!(p.frame_start && !p.keyframe);
        // FU with start bit, type CRA (21).
        assert!(inspect(VideoCodec::H265, &[49 << 1, 0x01, 0x80 | 21]).is_keyframe_start());
        assert_eq!(
            inspect(VideoCodec::H265, &[49 << 1, 0x01, 21]),
            PayloadInfo::NONE
        );
        // AP with VPS (32) then IDR (20).
        let ap = [48 << 1, 1, 0, 2, 32 << 1, 1, 0, 2, 20 << 1, 1];
        assert!(inspect(VideoCodec::H265, &ap).is_keyframe_start());
    }

    #[test]
    fn vp8_keyframe_is_first_partition_with_p_bit_clear() {
        // Descriptor: S=1, PID=0, no extension; payload header P=0.
        assert!(inspect(VideoCodec::VP8, &[0x10, 0x00, 0x00, 0x9D]).is_keyframe_start());
        // P=1: interframe start.
        let p = inspect(VideoCodec::VP8, &[0x10, 0x01]);
        assert!(p.frame_start && !p.keyframe);
        // Not start of partition.
        assert_eq!(inspect(VideoCodec::VP8, &[0x00, 0x00]), PayloadInfo::NONE);
        // Start of partition 1: not a frame start.
        assert_eq!(inspect(VideoCodec::VP8, &[0x11, 0x00]), PayloadInfo::NONE);
        // Extended descriptor: X=1, I=1 with 15-bit picture id, L=1, T=1.
        let ext = [0x90, 0xF0, 0x80 | 0x12, 0x34, 0x05, 0x20, 0x00];
        assert!(inspect(VideoCodec::VP8, &ext).is_keyframe_start());
        // Same but truncated before the payload header: not a keyframe.
        assert!(!inspect(VideoCodec::VP8, &ext[..6]).keyframe);
    }

    #[test]
    fn vp9_keyframe_is_frame_begin_without_inter_prediction() {
        // B=1, P=0.
        assert!(inspect(VideoCodec::VP9, &[0x08]).is_keyframe_start());
        // B=1, P=1.
        let p = inspect(VideoCodec::VP9, &[0x48]);
        assert!(p.frame_start && !p.keyframe);
        // With layer indices (L=1): SID 0 starts the picture, SID 1 does not.
        assert!(inspect(VideoCodec::VP9, &[0x28, 0x00]).is_keyframe_start());
        assert_eq!(inspect(VideoCodec::VP9, &[0x28, 0x02]), PayloadInfo::NONE);
        // I=1 with 2-byte picture id before the layer byte.
        assert!(inspect(VideoCodec::VP9, &[0xA8, 0x80, 0x01, 0x00]).is_keyframe_start());
    }

    #[test]
    fn av1_keyframe_is_a_new_coded_video_sequence() {
        assert!(inspect(VideoCodec::AV1, &[0x08]).is_keyframe_start());
        let p = inspect(VideoCodec::AV1, &[0x00]);
        assert!(p.frame_start && !p.keyframe);
        // Z=1 continues a previous OBU: not a start.
        assert_eq!(inspect(VideoCodec::AV1, &[0x88]), PayloadInfo::NONE);
    }

    const IDR: &[u8] = &[0x65, 0x88];
    const P_SLICE: &[u8] = &[0x41, 0x9A];

    #[test]
    fn rewriter_forwards_nothing_until_a_source_sends_a_keyframe() {
        let mut rw = StreamRewriter::new(VideoCodec::H264, 0xFEED, 1000, 5000);
        assert!(rw.select(0xA));
        assert!(rw.feed(&pkt(0xA, 10, 900, P_SLICE)).is_none());
        assert!(
            rw.feed(&pkt(0xB, 50, 100, IDR)).is_none(),
            "unselected source"
        );
        let out = rw.feed(&pkt(0xA, 11, 3600, IDR)).unwrap();
        assert_eq!(ssrc(&out), 0xFEED);
        assert_eq!(seq(&out), 1000);
        assert_eq!(ts(&out), 5000);
        assert_eq!(rw.current_source(), Some(0xA));
        assert_eq!(rw.pending_source(), None);
        // Later packets keep the offsets.
        let out = rw.feed(&pkt(0xA, 13, 6600, P_SLICE)).unwrap();
        assert_eq!(seq(&out), 1002);
        assert_eq!(ts(&out), 8000);
        assert_eq!(rw.switches(), 1);
    }

    #[test]
    fn rewriter_switches_only_at_the_new_sources_keyframe_and_keeps_continuity() {
        let mut rw = StreamRewriter::new(VideoCodec::H264, 1, 0, 0);
        rw.select(0xA);
        rw.feed(&pkt(0xA, 100, 0, IDR)).unwrap();
        let last = rw.feed(&pkt(0xA, 101, 3000, P_SLICE)).unwrap();
        assert_eq!(seq(&last), 1);

        assert!(rw.select(0xB));
        assert_eq!(rw.pending_source(), Some(0xB));
        // B's P-frames are dropped, A keeps flowing meanwhile.
        assert!(rw.feed(&pkt(0xB, 7, 77, P_SLICE)).is_none());
        let still_a = rw.feed(&pkt(0xA, 102, 6000, P_SLICE)).unwrap();
        assert_eq!(seq(&still_a), 2);
        assert_eq!(rw.current_source(), Some(0xA));

        // B's keyframe: switch. Sequence continues; timestamp moves forward.
        let first_b = rw.feed(&pkt(0xB, 8, 88, IDR)).unwrap();
        assert_eq!(seq(&first_b), 3);
        assert_eq!(ssrc(&first_b), 1);
        assert!(ts(&first_b).wrapping_sub(ts(&still_a)) >= 3000);
        assert_eq!(rw.current_source(), Some(0xB));
        assert_eq!(rw.switches(), 2);
        // A is now dropped; B's timestamps advance from the switch point.
        assert!(rw.feed(&pkt(0xA, 103, 9000, P_SLICE)).is_none());
        let next_b = rw.feed(&pkt(0xB, 9, 88 + 3000, P_SLICE)).unwrap();
        assert_eq!(seq(&next_b), 4);
        assert_eq!(ts(&next_b).wrapping_sub(ts(&first_b)), 3000);
        // Re-selecting the current source cancels nothing but the pending.
        assert!(!rw.select(0xB));
        assert_eq!(rw.pending_source(), None);
    }

    #[test]
    fn rewriter_handles_sequence_and_timestamp_wrap() {
        let mut rw = StreamRewriter::new(VideoCodec::VP8, 9, 65534, u32::MAX - 1000);
        rw.select(0xA);
        let key = [0x10, 0x00];
        let a = rw.feed(&pkt(0xA, 5, 10, &key)).unwrap();
        assert_eq!(seq(&a), 65534);
        let b = rw.feed(&pkt(0xA, 6, 3010, &[0x10, 0x01])).unwrap();
        assert_eq!(seq(&b), 65535);
        let c = rw.feed(&pkt(0xA, 7, 6010, &[0x10, 0x01])).unwrap();
        assert_eq!(seq(&c), 0);
        assert_eq!(ts(&c).wrapping_sub(ts(&a)), 6000);
        assert!(ts(&c) < ts(&a), "wrapped");
    }

    #[test]
    fn rewriter_clear_stops_everything() {
        let mut rw = StreamRewriter::new(VideoCodec::H264, 1, 0, 0);
        rw.select(0xA);
        rw.feed(&pkt(0xA, 1, 0, IDR)).unwrap();
        rw.clear();
        assert!(rw.feed(&pkt(0xA, 2, 3000, P_SLICE)).is_none());
        assert_eq!(rw.current_source(), None);
    }
}
