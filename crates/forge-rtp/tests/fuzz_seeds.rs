//! Seed corpora for the fuzz targets in `fuzz/` (see `fuzz/README.md`).
//!
//! A fuzzer that starts from random bytes spends its budget rediscovering
//! that an RTP packet begins with a version field. Starting it from real
//! output of the packetizers and encoders puts it past the front door, in
//! the part of the parser worth exercising — which is why the corpora come
//! from the round-trip paths rather than from a checked-in blob that would
//! rot the moment a payload format changed.
//!
//! Nothing is written unless `FORGE_FUZZ_SEED_DIR` names a directory, so a
//! normal `cargo test` run only checks that the corpora can still be built.
//! The weekly fuzzing job sets it. Files are laid out as
//! `$FORGE_FUZZ_SEED_DIR/<target>/<name>`, which is what `cargo fuzz run`
//! takes as its corpus directory.
//!
//! The multi-packet targets read their input as `u16`-length-prefixed
//! records; `framed` below writes exactly what `forge_media_fuzz::frames`
//! reads, and the two have to be changed together.

use bytes::Bytes;
use forge_core::VideoCodec;
use forge_rtp::rtcp::{Bye, ReceiverReport, RtcpPacket, SenderReport};
use forge_rtp::rtp::{RtpHeader, RtpPacket};
use forge_rtp::video::payload::{packetize, CodedFrame};
use std::path::PathBuf;

/// Length-prefix a sequence of payloads for the multi-packet targets.
fn framed<'a>(packets: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let packets: Vec<&[u8]> = packets.into_iter().collect();
    let mut out = Vec::new();
    for p in &packets {
        let len = u16::try_from(p.len()).expect("seed payload fits in u16");
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(p);
    }
    // This and `forge_media_fuzz::frames` are the two halves of one
    // contract, and they live in crates that cannot be compiled by the same
    // toolchain. Reading the bytes back with the same rule is the only place
    // the two can be held together.
    assert_eq!(unframe(&out), packets, "seed framing does not round-trip");
    out
}

/// The inverse of [`framed`] — the rule `forge_media_fuzz::frames` applies.
fn unframe(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while rest.len() >= 2 {
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        rest = &rest[2..];
        let take = len.min(rest.len());
        out.push(&rest[..take]);
        rest = &rest[take..];
    }
    out
}

fn seed_dir() -> Option<PathBuf> {
    std::env::var_os("FORGE_FUZZ_SEED_DIR").map(PathBuf::from)
}

fn write(target: &str, name: &str, bytes: &[u8]) {
    let Some(root) = seed_dir() else { return };
    let dir = root.join(target);
    std::fs::create_dir_all(&dir).expect("create seed directory");
    std::fs::write(dir.join(name), bytes).expect("write seed");
}

/// An Annex B stream of `nal_count` NAL units, the last one long enough to
/// fragment at any sane MTU.
fn annexb(nal_types: &[u8], long_tail: usize) -> Vec<u8> {
    let mut data = Vec::new();
    for (i, ty) in nal_types.iter().enumerate() {
        data.extend_from_slice(&[0, 0, 0, 1, *ty]);
        let body = if i + 1 == nal_types.len() {
            long_tail
        } else {
            6
        };
        data.extend((0..body as u32).map(|n| (n % 251) as u8 + 1));
    }
    data
}

/// A temporal unit: temporal delimiter, sequence header, then one frame OBU
/// of `frame_len` bytes — each with `obu_has_size_field` set, which is the
/// form this workspace's AV1 packetizer requires.
fn av1_temporal_unit(frame_len: usize) -> Vec<u8> {
    let mut tu = vec![0x12, 0x00]; // temporal delimiter, size 0
    tu.extend_from_slice(&[0x0A, 0x03, 0xAA, 0xBB, 0xCC]); // sequence header, size 3
    tu.push(0x32); // frame OBU, has_size
    leb128(&mut tu, frame_len);
    tu.extend((0..frame_len as u32).map(|i| (i % 253) as u8));
    tu
}

/// LEB128, as the OBU size field is encoded.
fn leb128(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn frame(keyframe: bool, data: Vec<u8>) -> CodedFrame {
    CodedFrame {
        timestamp: 9000,
        keyframe,
        data: Bytes::from(data),
    }
}

/// One representative keyframe and one delta frame per codec. The keyframe
/// is large enough to fragment, so the corpus covers the fragmentation path
/// as well as the single-payload one.
fn frames_for(codec: VideoCodec) -> Vec<(&'static str, CodedFrame)> {
    match codec {
        // SPS, PPS, then a long IDR that fragments.
        VideoCodec::H264 => vec![
            ("keyframe", frame(true, annexb(&[0x67, 0x68, 0x65], 3000))),
            ("delta", frame(false, annexb(&[0x41], 200))),
        ],
        // Two-byte NAL headers: VPS, SPS, PPS, then a long IRAP.
        VideoCodec::H265 => vec![
            (
                "keyframe",
                frame(true, annexb(&[0x40, 0x42, 0x44, 0x26], 3000)),
            ),
            ("delta", frame(false, annexb(&[0x02], 200))),
        ],
        // VP8 and VP9 payloads are the raw frame; only the length matters
        // for whether the packetizer fragments.
        VideoCodec::VP8 | VideoCodec::VP9 => vec![
            (
                "keyframe",
                frame(true, (0..3000u32).map(|i| (i % 251) as u8).collect()),
            ),
            (
                "delta",
                frame(false, (0..200u32).map(|i| (i % 251) as u8).collect()),
            ),
        ],
        // AV1 is a temporal unit of OBUs, each carrying its size field, so
        // the packetizer rejects anything that is not really one.
        VideoCodec::AV1 => vec![
            ("keyframe", frame(true, av1_temporal_unit(2000))),
            ("delta", frame(false, av1_temporal_unit(200))),
        ],
    }
}

/// The codec's short name, for seed filenames.
fn name_of(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
        VideoCodec::VP8 => "vp8",
        VideoCodec::VP9 => "vp9",
        VideoCodec::AV1 => "av1",
    }
}

fn target_of(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "depacketize_h264",
        VideoCodec::H265 => "depacketize_h265",
        VideoCodec::VP8 => "depacketize_vp8",
        VideoCodec::VP9 => "depacketize_vp9",
        VideoCodec::AV1 => "depacketize_av1",
    }
}

const CODECS: [VideoCodec; 5] = [
    VideoCodec::H264,
    VideoCodec::H265,
    VideoCodec::VP8,
    VideoCodec::VP9,
    VideoCodec::AV1,
];

#[test]
fn depacketizer_seeds() {
    for codec in CODECS {
        for (name, f) in frames_for(codec) {
            for mtu in [200usize, 1200] {
                let packets = packetize(codec, &f, mtu).expect("packetize seed frame");
                let refs: Vec<&[u8]> = packets.iter().map(|p| p.as_ref()).collect();
                // The corpus is only worth seeding with input the parser
                // accepts; a packetizer that stopped round-tripping should
                // fail here rather than quietly seed garbage.
                forge_rtp::video::payload::depacketize(codec, &refs)
                    .expect("seed frame round-trips");
                write(
                    target_of(codec),
                    &format!("{name}_mtu{mtu}"),
                    &framed(refs.iter().copied()),
                );
            }
        }
    }
}

#[test]
fn frame_assembler_seeds() {
    for (i, codec) in CODECS.into_iter().enumerate() {
        for (name, f) in frames_for(codec) {
            let payloads = packetize(codec, &f, 1200).expect("packetize seed frame");
            let last = payloads.len() - 1;
            let wire: Vec<Vec<u8>> = payloads
                .iter()
                .enumerate()
                .map(|(n, payload)| {
                    let packet = RtpPacket {
                        header: RtpHeader {
                            version_flags: 0x80,
                            // Marker on the last packet of a frame.
                            marker_payload_type: if n == last { 0xE0 } else { 0x60 },
                            sequence_number: 1000 + n as u16,
                            timestamp: f.timestamp,
                            ssrc: 0x1234_5678,
                        },
                        csrc_list: Vec::new(),
                        extension: None,
                        payload: payload.clone(),
                        padding_len: 0,
                    };
                    packet.to_bytes().to_vec()
                })
                .collect();
            let mut seed = vec![i as u8];
            seed.extend(framed(wire.iter().map(|p| p.as_slice())));
            write(
                "frame_assembler",
                &format!("{}_{name}", name_of(codec)),
                &seed,
            );
        }
    }
}

#[test]
fn rtp_extension_seeds() {
    use forge_rtp::rtp::RtpExtension;
    for (name, ext) in [
        (
            "one_byte_mid",
            RtpExtension::one_byte(&[(1, b"1"), (4, b"2")]),
        ),
        (
            "two_byte_mid",
            RtpExtension::two_byte(&[(1, b"video0"), (200, b"content")]),
        ),
    ] {
        let mut packet = RtpPacket {
            header: RtpHeader {
                version_flags: 0x90,
                marker_payload_type: 0xE0,
                sequence_number: 1000,
                timestamp: 90_000,
                ssrc: 0x1234_5678,
            },
            csrc_list: Vec::new(),
            extension: Some(ext),
            payload: Bytes::from_static(&[0x10, 0x00, 0x9d, 0x01, 0x2a]),
            padding_len: 0,
        };
        // The builder sets the extension bit; the literal above must too.
        packet.header.version_flags |= 0x10;
        write("rtp_extension", name, &packet.to_bytes());
    }
}

#[test]
fn rtcp_seeds() {
    let mut sr = SenderReport::new(0x1234_5678);
    sr.ntp_timestamp_msw = 0xE1B2_0000;
    sr.ntp_timestamp_lsw = 0x8000_0000;
    sr.rtp_timestamp = 9000;
    sr.sender_packet_count = 42;
    sr.sender_octet_count = 4200;

    // Through the enum, which is the serialiser paired with the parser the
    // target calls; the inner types write their bodies without the common
    // header a whole packet needs.
    let packets = [
        ("sender_report", RtcpPacket::SenderReport(sr)),
        (
            "receiver_report",
            RtcpPacket::ReceiverReport(ReceiverReport::new(0x1234_5678)),
        ),
        (
            "bye",
            RtcpPacket::Bye(Bye::new(vec![0x1234_5678, 0x8765_4321])),
        ),
    ];
    for (name, packet) in packets {
        let bytes = packet.to_bytes();
        // Whatever the corpus holds has to be something the parser accepts,
        // or the fuzzer starts from inputs that die at the first branch.
        RtcpPacket::parse(&bytes).expect("seed RTCP packet parses");
        write("rtcp_parse", name, &bytes);
    }
}
