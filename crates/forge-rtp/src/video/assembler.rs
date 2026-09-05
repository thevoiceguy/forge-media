//! Frame assembly for one video source.
//!
//! Packets arrive with gaps and out of order; frames are what the decoder
//! wants. The assembler keeps a source's packets in sequence order,
//! groups them into frames (a frame ends with the marker bit, or when the
//! timestamp changes), rebuilds each frame with [`depacketize`], and
//! reports loss: when a gap is not filled within the reorder window the
//! frame spanning it is dropped, everything up to the next frame start is
//! dropped with it, and the source needs a keyframe before its picture is
//! valid again ([`needs_keyframe`](FrameAssembler::needs_keyframe)). The
//! caller turns that into NACKs ([`missing`](FrameAssembler::missing))
//! and a PLI.

use super::payload::{depacketize, CodedFrame, PayloadError};
use super::{inspect, PayloadInfo};
use crate::rtp::RtpPacket;
use forge_core::VideoCodec;
use std::collections::BTreeMap;

/// What [`FrameAssembler::push`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblerEvent {
    /// A complete frame.
    Frame(CodedFrame),
    /// Packets `from_seq..=to_seq` were given up on. The frame they
    /// belonged to is gone and a keyframe is needed.
    Lost { from_seq: u16, to_seq: u16 },
    /// A frame arrived whole but could not be rebuilt (bad
    /// packetization); a keyframe is needed.
    Invalid { timestamp: u32, error: PayloadError },
}

/// Groups one source's RTP packets into coded frames.
#[derive(Debug)]
pub struct FrameAssembler {
    codec: VideoCodec,
    /// Out-of-order packets tolerated before a gap counts as loss.
    max_reorder: u64,
    /// A frame larger than this is dropped (a runaway or hostile source).
    max_frame_bytes: usize,
    /// Extended sequence number expected next; `None` before the first packet.
    next: Option<u64>,
    /// Packets ahead of `next`, by extended sequence number.
    pending: BTreeMap<u64, RtpPacket>,
    /// The frame in progress, in order.
    current: Vec<RtpPacket>,
    current_bytes: usize,
    current_keyframe: bool,
    /// After a loss: skip until a packet that starts a frame.
    skipping: bool,
    needs_keyframe: bool,
}

impl FrameAssembler {
    /// Defaults: a reorder window of 16 packets, frames up to 4 MiB.
    pub fn new(codec: VideoCodec) -> Self {
        Self::with_limits(codec, 16, 4 * 1024 * 1024)
    }

    pub fn with_limits(codec: VideoCodec, max_reorder: usize, max_frame_bytes: usize) -> Self {
        Self {
            codec,
            max_reorder: max_reorder.max(1) as u64,
            max_frame_bytes,
            next: None,
            pending: BTreeMap::new(),
            current: Vec::new(),
            current_bytes: 0,
            current_keyframe: false,
            skipping: true,
            needs_keyframe: true,
        }
    }

    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// The source's picture is not decodable until a keyframe arrives:
    /// at start, after loss, after a bad frame.
    pub fn needs_keyframe(&self) -> bool {
        self.needs_keyframe
    }

    /// Sequence numbers between the last packet consumed and the packets
    /// waiting out of order: what a NACK should ask for right now.
    pub fn missing(&self) -> Vec<u16> {
        let (Some(next), Some((&last, _))) = (self.next, self.pending.last_key_value()) else {
            return Vec::new();
        };
        (next..last)
            .filter(|s| !self.pending.contains_key(s))
            .map(|s| s as u16)
            .collect()
    }

    /// Forget everything (the source restarted).
    pub fn reset(&mut self) {
        self.next = None;
        self.pending.clear();
        self.drop_current();
        self.skipping = true;
        self.needs_keyframe = true;
    }

    /// Offer one packet from the source. Packets already consumed are
    /// ignored; packets ahead are held until the gap fills or is given up.
    pub fn push(&mut self, packet: RtpPacket) -> Vec<AssemblerEvent> {
        let mut events = Vec::new();
        let seq = packet.header.sequence_number;
        let next = match self.next {
            Some(n) => n,
            None => {
                let start = 0x1_0000u64 + seq as u64;
                self.next = Some(start);
                start
            }
        };
        let ext = extend(seq, next);
        if ext < next {
            return events; // late or duplicate
        }
        if ext > next {
            self.pending.insert(ext, packet);
            if ext - next > self.max_reorder || self.pending.len() as u64 > self.max_reorder {
                let (&first, _) = self.pending.first_key_value().expect("just inserted");
                events.push(AssemblerEvent::Lost {
                    from_seq: next as u16,
                    to_seq: (first - 1) as u16,
                });
                self.give_up();
                self.next = Some(first);
            } else {
                return events;
            }
        } else {
            self.consume(packet, &mut events);
            self.next = Some(next + 1);
        }
        // Drain whatever became contiguous.
        while let Some(n) = self.next {
            match self.pending.remove(&n) {
                Some(p) => {
                    self.consume(p, &mut events);
                    self.next = Some(n + 1);
                }
                None => break,
            }
        }
        events
    }

    /// Loss: drop the frame in progress and skip to the next frame start.
    fn give_up(&mut self) {
        self.drop_current();
        self.skipping = true;
        self.needs_keyframe = true;
    }

    fn drop_current(&mut self) {
        self.current.clear();
        self.current_bytes = 0;
        self.current_keyframe = false;
    }

    /// Add the next in-order packet to the frame in progress, emitting
    /// the frame when it completes.
    fn consume(&mut self, packet: RtpPacket, events: &mut Vec<AssemblerEvent>) {
        let info: PayloadInfo = inspect(self.codec, &packet.payload);
        let ts = packet.header.timestamp;
        let marker = packet.header.marker();

        // A new timestamp ends the previous frame even without a marker.
        if let Some(first) = self.current.first() {
            if first.header.timestamp != ts {
                self.finish(events);
            }
        }
        if self.skipping {
            if !info.frame_start {
                return;
            }
            self.skipping = false;
        }
        if self.current.is_empty() && !info.frame_start && self.codec_needs_frame_start() {
            // Mid-frame packet with nothing before it: the start was lost
            // earlier than we noticed.
            self.skipping = true;
            self.needs_keyframe = true;
            return;
        }
        self.current_keyframe |= info.keyframe;
        self.current_bytes += packet.payload.len();
        self.current.push(packet);
        if self.current_bytes > self.max_frame_bytes {
            self.give_up();
            events.push(AssemblerEvent::Invalid {
                timestamp: ts,
                error: PayloadError::Malformed("frame exceeds size limit"),
            });
            return;
        }
        if marker {
            self.finish(events);
        }
    }

    /// Whether the codec's payloads say where frames start. All of ours
    /// do; kept as one place to relax if a codec without it is added.
    fn codec_needs_frame_start(&self) -> bool {
        true
    }

    /// Rebuild the frame in progress and emit it.
    fn finish(&mut self, events: &mut Vec<AssemblerEvent>) {
        if self.current.is_empty() {
            return;
        }
        let timestamp = self.current[0].header.timestamp;
        let keyframe = self.current_keyframe;
        let payloads: Vec<&[u8]> = self.current.iter().map(|p| p.payload.as_ref()).collect();
        match depacketize(self.codec, &payloads) {
            Ok(data) => {
                if keyframe {
                    self.needs_keyframe = false;
                }
                events.push(AssemblerEvent::Frame(CodedFrame {
                    timestamp,
                    keyframe,
                    data,
                }));
            }
            Err(error) => {
                self.needs_keyframe = true;
                events.push(AssemblerEvent::Invalid { timestamp, error });
            }
        }
        self.drop_current();
    }
}

/// Extend a 16-bit sequence number to the 64-bit counter nearest `next`.
fn extend(seq: u16, next: u64) -> u64 {
    let delta = seq.wrapping_sub(next as u16) as i16 as i64;
    (next as i64 + delta).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::payload::packetize;
    use bytes::Bytes;

    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00];
    const P: &[u8] = &[0x41, 0x9A, 0x01, 0x02];

    fn pkt(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> RtpPacket {
        RtpPacket::build(96, seq, ts, 0xABCD, Bytes::copy_from_slice(payload), marker)
    }

    /// One H.264 frame as packets, starting at `seq`.
    fn h264_frame(seq: u16, ts: u32, nal: &[u8], mtu: usize) -> Vec<RtpPacket> {
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend_from_slice(nal);
        let f = CodedFrame {
            timestamp: ts,
            keyframe: false,
            data: Bytes::from(annexb),
        };
        let payloads = packetize(VideoCodec::H264, &f, mtu).unwrap();
        let n = payloads.len();
        payloads
            .into_iter()
            .enumerate()
            .map(|(i, p)| RtpPacket::build(96, seq.wrapping_add(i as u16), ts, 1, p, i == n - 1))
            .collect()
    }

    fn frames(events: &[AssemblerEvent]) -> Vec<(u32, bool, usize)> {
        events
            .iter()
            .filter_map(|e| match e {
                AssemblerEvent::Frame(f) => Some((f.timestamp, f.keyframe, f.data.len())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn in_order_packets_become_frames_and_the_first_keyframe_clears_the_need() {
        let mut a = FrameAssembler::new(VideoCodec::H264);
        assert!(a.needs_keyframe());
        let mut ev = Vec::new();
        for p in h264_frame(100, 3000, IDR, 1200) {
            ev.extend(a.push(p));
        }
        assert_eq!(frames(&ev), vec![(3000, true, 8)]);
        assert!(!a.needs_keyframe());
        let ev: Vec<_> = h264_frame(101, 6000, P, 1200)
            .into_iter()
            .flat_map(|p| a.push(p))
            .collect();
        assert_eq!(frames(&ev), vec![(6000, false, 8)]);
        assert!(a.missing().is_empty());
    }

    #[test]
    fn a_fragmented_frame_reorders_within_the_window() {
        let mut a = FrameAssembler::new(VideoCodec::H264);
        let big: Vec<u8> = std::iter::once(0x65)
            .chain((0..2500u32).map(|i| i as u8))
            .collect();
        let packets = h264_frame(10, 9000, &big, 1000);
        assert_eq!(packets.len(), 3);
        // Deliver 10, 12, 11.
        assert!(a.push(packets[0].clone()).is_empty());
        assert!(a.push(packets[2].clone()).is_empty());
        assert_eq!(a.missing(), vec![11]);
        let ev = a.push(packets[1].clone());
        assert_eq!(frames(&ev), vec![(9000, true, 2505)]);
        assert!(a.missing().is_empty());
    }

    #[test]
    fn a_gap_beyond_the_window_is_reported_and_skips_to_the_next_frame_start() {
        let mut a = FrameAssembler::with_limits(VideoCodec::H264, 4, 1 << 20);
        for p in h264_frame(0, 0, IDR, 1200) {
            a.push(p);
        }
        // Frame at ts 3000 spans seq 1..=3; packet 2 is lost. Then more.
        let big: Vec<u8> = std::iter::once(0x41)
            .chain((0..2500u32).map(|i| i as u8))
            .collect();
        let lost_frame = h264_frame(1, 3000, &big, 1000);
        assert!(a.push(lost_frame[0].clone()).is_empty());
        assert!(a.push(lost_frame[2].clone()).is_empty());
        // Packets 4.. keep arriving: frames at 6000 (seq 4) and 9000 (seq 5).
        let mut ev = Vec::new();
        ev.extend(a.push(pkt(4, 6000, true, P)));
        ev.extend(a.push(pkt(5, 9000, true, P)));
        ev.extend(a.push(pkt(6, 12000, true, P)));
        ev.extend(a.push(pkt(7, 15000, true, P)));
        // 7 - 1 > 4: give up on seq 2, drop the frame at 3000, and the
        // rest are P frames the decoder cannot use until a keyframe.
        assert!(
            ev.contains(&AssemblerEvent::Lost {
                from_seq: 2,
                to_seq: 2
            }),
            "{ev:?}"
        );
        assert!(a.needs_keyframe());
        let decoded = frames(&ev);
        // The P frames after the loss are still assembled (they start
        // frames); the caller decides whether to decode them.
        assert!(decoded.iter().all(|(ts, _, _)| *ts >= 6000), "{decoded:?}");
        assert!(!decoded.iter().any(|(ts, _, _)| *ts == 3000));
        // A keyframe clears the need.
        let ev = a.push(pkt(8, 18000, true, IDR));
        assert_eq!(frames(&ev), vec![(18000, true, 8)]);
        assert!(!a.needs_keyframe());
    }

    #[test]
    fn late_and_duplicate_packets_are_ignored_and_sequence_wraps() {
        let mut a = FrameAssembler::new(VideoCodec::H264);
        let ev = a.push(pkt(65534, 0, true, IDR));
        assert_eq!(frames(&ev).len(), 1);
        assert!(a.push(pkt(65534, 0, true, IDR)).is_empty(), "duplicate");
        assert!(a.push(pkt(65000, 0, true, IDR)).is_empty(), "late");
        let ev = a.push(pkt(65535, 3000, true, P));
        assert_eq!(frames(&ev), vec![(3000, false, 8)]);
        let ev = a.push(pkt(0, 6000, true, P));
        assert_eq!(frames(&ev), vec![(6000, false, 8)]);
        let ev = a.push(pkt(1, 9000, true, P));
        assert_eq!(frames(&ev), vec![(9000, false, 8)]);
    }

    #[test]
    fn a_missing_marker_is_closed_by_the_next_timestamp() {
        let mut a = FrameAssembler::new(VideoCodec::H264);
        assert!(a.push(pkt(1, 0, false, IDR)).is_empty());
        let ev = a.push(pkt(2, 3000, true, P));
        assert_eq!(frames(&ev), vec![(0, true, 8), (3000, false, 8)]);
    }

    #[test]
    fn starting_mid_frame_waits_for_a_frame_start() {
        let mut a = FrameAssembler::new(VideoCodec::H264);
        // FU-A middle fragment: not a frame start.
        assert!(a.push(pkt(5, 0, false, &[0x7C, 0x05, 1, 2])).is_empty());
        assert!(a.push(pkt(6, 0, true, &[0x7C, 0x45, 3, 4])).is_empty());
        assert!(a.needs_keyframe());
        let ev = a.push(pkt(7, 3000, true, IDR));
        assert_eq!(frames(&ev), vec![(3000, true, 8)]);
    }

    #[test]
    fn oversized_and_malformed_frames_are_invalid_not_fatal() {
        let mut a = FrameAssembler::with_limits(VideoCodec::H264, 16, 10);
        let ev = a.push(pkt(1, 0, true, &[0x65, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]));
        assert!(
            matches!(ev[0], AssemblerEvent::Invalid { timestamp: 0, .. }),
            "{ev:?}"
        );
        assert!(a.needs_keyframe());
        // A frame that starts (FU-A start) but whose end fragment is a
        // start again: depacketize rejects it.
        let mut b = FrameAssembler::new(VideoCodec::H264);
        b.push(pkt(1, 0, false, &[0x7C, 0x85, 1]));
        let ev = b.push(pkt(2, 0, true, &[0x7C, 0x85, 2]));
        assert!(matches!(ev[0], AssemblerEvent::Invalid { .. }), "{ev:?}");
        let ev = b.push(pkt(3, 3000, true, IDR));
        assert_eq!(frames(&ev), vec![(3000, true, 8)]);
    }

    #[test]
    fn vp8_frames_assemble_from_descriptors() {
        let mut a = FrameAssembler::new(VideoCodec::VP8);
        let data: Vec<u8> = std::iter::once(0x00)
            .chain((0..1500u32).map(|i| i as u8))
            .collect();
        let f = CodedFrame {
            timestamp: 100,
            keyframe: true,
            data: Bytes::from(data.clone()),
        };
        let payloads = packetize(VideoCodec::VP8, &f, 700).unwrap();
        let n = payloads.len();
        let mut ev = Vec::new();
        for (i, p) in payloads.into_iter().enumerate() {
            ev.extend(a.push(RtpPacket::build(97, i as u16, 100, 1, p, i == n - 1)));
        }
        assert_eq!(frames(&ev), vec![(100, true, data.len())]);
        assert!(!a.needs_keyframe());
    }

    #[test]
    fn reset_forgets_state() {
        let mut a = FrameAssembler::new(VideoCodec::H264);
        a.push(pkt(1, 0, true, IDR));
        assert!(!a.needs_keyframe());
        a.reset();
        assert!(a.needs_keyframe());
        assert!(a.missing().is_empty());
        // A fresh sequence space is accepted.
        let ev = a.push(pkt(40000, 0, true, IDR));
        assert_eq!(frames(&ev).len(), 1);
    }
}
