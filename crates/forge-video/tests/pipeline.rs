//! The whole video path with no native codec: two participants' frames
//! are "encoded" by the raw codec, packetized into RTP, reassembled by
//! the frame assembler (with loss on the way), decoded, composited into
//! a grid, and the composite encoded for a subscriber and decoded again.
//! What phase 1 (forge-rtp) and phase 2 (forge-video) promise, checked
//! together.

use bytes::Bytes;
use forge_core::VideoCodec;
use forge_rtp::video::payload::packetize;
use forge_rtp::{AssemblerEvent, FrameAssembler, RtpPacket};
use forge_video::codec::{EncoderSettings, VideoDecoder, VideoEncoder};
use forge_video::compose::{Compositor, HostCompositor, Theme, TileSource};
use forge_video::frame::{HostFrame, Resolution, VideoFrame};
use forge_video::layout::Layout;
use forge_video::metrics::psnr_luma;
use forge_video::raw::raw_registry;
use forge_video::{Flavor, FlavorTable, MediaDevice};

/// A participant's sending side: raw encoder + packetizer + sequence.
struct Sender {
    enc: Box<dyn VideoEncoder>,
    seq: u16,
    ssrc: u32,
}

impl Sender {
    fn new(ssrc: u32, w: u32, h: u32) -> Self {
        let r = raw_registry();
        let settings = EncoderSettings {
            codec: VideoCodec::VP8,
            resolution: Resolution::new(w, h),
            fps: 15,
            bitrate_kbps: 500,
            keyframe_interval: 30,
            profile: String::new(),
        };
        Self {
            enc: r.encoder(&settings, &MediaDevice::Host).unwrap(),
            seq: 100,
            ssrc,
        }
    }

    /// Encode a frame and return its RTP packets.
    fn send(&mut self, frame: HostFrame) -> Vec<RtpPacket> {
        let coded = self.enc.encode(&VideoFrame::Host(frame), false).unwrap();
        let mut packets = Vec::new();
        for c in coded {
            let payloads = packetize(VideoCodec::VP8, &c, 1200).unwrap();
            let n = payloads.len();
            for (i, p) in payloads.into_iter().enumerate() {
                packets.push(RtpPacket::build(
                    97,
                    self.seq,
                    c.timestamp,
                    self.ssrc,
                    p,
                    i == n - 1,
                ));
                self.seq = self.seq.wrapping_add(1);
            }
        }
        packets
    }
}

/// A participant's receiving side in the mixer: assembler + decoder + the
/// latest decoded frame (the "frame slot").
struct Receiver {
    asm: FrameAssembler,
    dec: Box<dyn VideoDecoder>,
    slot: Option<VideoFrame>,
    lost: usize,
}

impl Receiver {
    fn new() -> Self {
        Self {
            asm: FrameAssembler::new(VideoCodec::VP8),
            dec: raw_registry()
                .decoder(VideoCodec::VP8, &MediaDevice::Host)
                .unwrap(),
            slot: None,
            lost: 0,
        }
    }

    fn receive(&mut self, packet: RtpPacket) {
        for ev in self.asm.push(packet) {
            match ev {
                AssemblerEvent::Frame(f) => {
                    if let Some(v) = self.dec.decode(&f).unwrap() {
                        self.slot = Some(v);
                    }
                }
                AssemblerEvent::Lost { .. } => self.lost += 1,
                AssemblerEvent::Invalid { .. } => panic!("raw frames are never invalid"),
            }
        }
    }
}

#[test]
fn two_participants_are_mixed_into_a_grid_and_delivered_to_a_subscriber() {
    let (w, h) = (128u32, 72u32);
    let mut alice = Sender::new(0xA11CE, w, h);
    let mut bob = Sender::new(0xB0B, w, h);
    let mut rx_alice = Receiver::new();
    let mut rx_bob = Receiver::new();

    // Bob's fourth frame loses a packet on the way: the assembler must
    // drop that frame, keep going, and Bob's slot must hold the last good
    // one until the next frame lands.
    for i in 0..6u32 {
        let a = HostFrame::solid(w, h, 200, 128, 128).with_pts(i * 6000);
        let b = HostFrame::solid(w, h, 60 + (i as u8) * 10, 128, 128).with_pts(i * 6000);
        for p in alice.send(a) {
            rx_alice.receive(p);
        }
        let mut packets = bob.send(b);
        if i == 3 && packets.len() > 1 {
            packets.remove(1);
        }
        for p in packets {
            rx_bob.receive(p);
        }
    }
    assert_eq!(rx_alice.lost, 0);
    let bob_slot = rx_bob.slot.clone().expect("bob has a frame");
    assert_eq!(
        bob_slot.as_host().unwrap().luma(10, 10),
        60 + 50,
        "bob's last frame arrived after the loss"
    );

    // Compose the room: a 2×1 grid, Alice speaking.
    let mut comp = HostCompositor::new(256, 72, Layout::Grid).with_theme(Theme {
        border_px: 0,
        gap_px: 0,
        ..Theme::default()
    });
    let alice_slot = rx_alice.slot.clone().unwrap();
    comp.render(
        &[
            TileSource {
                id: "alice",
                name: "Alice",
                frame: Some(&alice_slot),
                speaking: true,
                muted: false,
            },
            TileSource {
                id: "bob",
                name: "Bob",
                frame: Some(&bob_slot),
                speaking: false,
                muted: true,
            },
        ],
        30000,
    )
    .unwrap();
    let canvas = comp.host_canvas().clone();
    assert_eq!(canvas.luma(64, 30), 200, "alice's tile");
    assert_eq!(canvas.luma(192, 30), 110, "bob's tile");

    // A subscriber wants the composite at 128×36; its flavor is shared by
    // a second identical subscriber, so one encoder serves both.
    let mut table = FlavorTable::new();
    let flavor = Flavor::new(VideoCodec::VP8, "", Resolution::new(128, 36), 15, 400);
    assert!(table.subscribe("phone-1", flavor.clone()));
    assert!(!table.subscribe("phone-2", flavor.clone()));
    assert_eq!(table.encoder_count(), 1);
    let settings = EncoderSettings::for_flavor(&flavor, 30);
    let r = raw_registry();
    let mut out_enc = r.encoder(&settings, &MediaDevice::Host).unwrap();
    let coded = out_enc
        .encode(&VideoFrame::Host(canvas.clone()), true)
        .unwrap();
    assert_eq!(coded.len(), 1);

    // ... and travels as RTP to the phone, which decodes the composite.
    let payloads = packetize(VideoCodec::VP8, &coded[0], 600).unwrap();
    assert!(payloads.len() > 1, "a composite spans several packets");
    let mut phone = Receiver::new();
    let n = payloads.len();
    for (i, p) in payloads.into_iter().enumerate() {
        phone.receive(RtpPacket::build(
            97,
            7000 + i as u16,
            coded[0].timestamp,
            0xF00D,
            p,
            i == n - 1,
        ));
    }
    let got = phone
        .slot
        .expect("phone decoded the composite")
        .into_host()
        .unwrap();
    assert_eq!(got.resolution(), Resolution::new(128, 36));
    assert_eq!(got.pts, 30000);
    // Scaled by half: Alice's tile centre is still bright, Bob's still dim.
    assert!(got.luma(32, 15) > 180, "{}", got.luma(32, 15));
    assert!(got.luma(96, 15) < 130, "{}", got.luma(96, 15));
    // And it is a faithful half-size picture of the canvas.
    let reference = forge_video::scale::resize(&canvas, 128, 36);
    let p = psnr_luma(&got, &reference).unwrap();
    assert!(p > 40.0, "psnr {p}");

    // Bytes on the wire are what the receiver saw: the raw payload is the
    // frame itself, so a mid-frame packet is never a frame start.
    let mid = Bytes::from_static(&[0x00, 0x11, 0x22]);
    assert!(!forge_rtp::video::inspect(VideoCodec::VP8, &mid).frame_start);
}
