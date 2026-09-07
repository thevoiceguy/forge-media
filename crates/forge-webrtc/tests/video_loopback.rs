//! Two peer connections in one process with a video section: offer/answer
//! with BUNDLE'd audio and video, video packets both ways under the
//! transport's own SSRC, feedback across the shared RTCP path, periodic
//! reports, video added by re-offer, and video refused by a peer without it.

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::*;
use forge_rtp::{PayloadFeedback, PsFeedback, RtcpPacket, RtpFeedback, RtpPacket};
use forge_webrtc::{Direction, PeerConfig, PeerConnection, VideoCodec, VideoConfig};

fn video_cfg(codecs: &[(VideoCodec, u8)]) -> PeerConfig {
    PeerConfig {
        video: Some(VideoConfig {
            codecs: codecs.to_vec(),
            ..VideoConfig::default()
        }),
        ..PeerConfig::default()
    }
}

fn video_packet(seq: u16, ts: u32, marker: bool) -> Bytes {
    RtpPacket::build(
        96,
        seq,
        ts,
        0xDEAD_BEEF,
        Bytes::from(vec![seq as u8; 100]),
        marker,
    )
    .to_bytes()
    .freeze()
}

#[tokio::test]
async fn video_both_ways_with_feedback_and_reports() {
    init_tracing();
    let vp8 = [(VideoCodec::VP8, 96)];
    let (caller, callee, mut caller_ev, mut callee_ev) =
        connect_pair_with(video_cfg(&vp8), video_cfg(&vp8)).await;
    for peer in [&caller, &callee] {
        let v = peer.negotiated_video().expect("video negotiated");
        assert_eq!((v.codec, v.payload_type), (VideoCodec::VP8, 96));
        assert!(v.nack && v.pli && v.fir && v.remb, "{v:?}");
        assert_eq!(v.direction, Direction::SendRecv);
    }
    assert_eq!(
        callee.negotiated_video().unwrap().remote_ssrc,
        Some(caller.video_ssrc())
    );
    assert_ne!(caller.video_ssrc(), caller.ssrc());

    // Caller → callee: five packets with the producer's numbering.
    let vs = caller.video_sender().unwrap();
    assert_eq!(vs.ssrc(), caller.video_ssrc());
    assert_eq!(vs.payload_type(), 96);
    for i in 0..5u16 {
        vs.send_packet(video_packet(1000 + i, 3000 * i as u32, i == 4))
            .await
            .unwrap();
    }
    let got = expect_video(&mut callee_ev, 5).await;
    for (i, pkt) in got.iter().enumerate() {
        assert_eq!({ pkt.header.ssrc }, caller.video_ssrc());
        assert_eq!({ pkt.header.sequence_number }, 1000 + i as u16);
        assert_eq!({ pkt.header.timestamp }, 3000 * i as u32);
        assert_eq!(pkt.header.payload_type(), 96);
        assert_eq!(pkt.header.marker(), i == 4);
        assert_eq!(pkt.payload.len(), 100);
    }

    // Callee → caller: PLI and a NACK about the caller's video, which
    // arrive parsed and behind a report.
    callee
        .send_rtcp(&[
            RtcpPacket::PayloadFeedback(PsFeedback::pli(callee.video_ssrc(), caller.video_ssrc())),
            RtcpPacket::TransportFeedback(RtpFeedback::nack(
                callee.video_ssrc(),
                caller.video_ssrc(),
                &[1002],
            )),
        ])
        .await
        .unwrap();
    let compound = expect_rtcp(&mut caller_ev, Duration::from_secs(5), |p| {
        matches!(
            p,
            RtcpPacket::PayloadFeedback(PsFeedback {
                kind: PayloadFeedback::Pli,
                ..
            })
        )
    })
    .await;
    assert!(
        matches!(
            compound[0],
            RtcpPacket::SenderReport(_) | RtcpPacket::ReceiverReport(_)
        ),
        "compound must start with a report: {compound:?}"
    );
    assert!(compound
        .iter()
        .any(|p| matches!(p, RtcpPacket::SourceDescription(_))));
    let nack = compound
        .iter()
        .find_map(|p| match p {
            RtcpPacket::TransportFeedback(fb) => Some(fb),
            _ => None,
        })
        .expect("NACK in the same compound");
    assert_eq!(nack.media_ssrc, caller.video_ssrc());

    // Audio still flows, and lands on the audio event.
    let audio = caller.sender().unwrap();
    audio
        .send_audio(Bytes::from_static(&[0xf8, 0xff, 0xfe]), 960)
        .await
        .unwrap();
    let pkt = expect_rtp(&mut callee_ev, &[0xf8, 0xff, 0xfe]).await;
    assert_eq!({ pkt.header.ssrc }, caller.ssrc());

    // Periodic reports: the callee hears an SR for the caller's video
    // stream, and the caller hears a report block about it with the
    // packets counted.
    let sr = expect_rtcp(
        &mut callee_ev,
        Duration::from_secs(4),
        |p| matches!(p, RtcpPacket::SenderReport(sr) if sr.ssrc == caller.video_ssrc()),
    )
    .await;
    let sr = sr
        .iter()
        .find_map(|p| match p {
            RtcpPacket::SenderReport(sr) if sr.ssrc == caller.video_ssrc() => Some(sr),
            _ => None,
        })
        .unwrap();
    assert_eq!(sr.sender_packet_count, 5);
    assert_eq!(sr.sender_octet_count, 500);
    assert_ne!(sr.ntp_timestamp_msw, 0);

    let block_about_video = |p: &RtcpPacket| {
        let blocks = match p {
            RtcpPacket::SenderReport(sr) => &sr.report_blocks,
            RtcpPacket::ReceiverReport(rr) => &rr.report_blocks,
            _ => return false,
        };
        blocks
            .iter()
            .any(|b| b.ssrc == caller.video_ssrc() && b.extended_highest_seq == 1004)
    };
    expect_rtcp(&mut caller_ev, Duration::from_secs(4), block_about_video).await;
    let sources = callee.sources();
    let src = sources
        .iter()
        .find(|s| s.ssrc() == caller.video_ssrc())
        .expect("callee tracks the caller's video");
    assert_eq!(src.packets_received(), 5);
    assert_eq!(src.packets_lost(), 0);

    // Callee → caller video works the same way.
    let vs2 = callee.video_sender().unwrap();
    vs2.send_packet(video_packet(7, 0, true)).await.unwrap();
    let got = expect_video(&mut caller_ev, 1).await;
    assert_eq!({ got[0].header.ssrc }, callee.video_ssrc());
    assert_eq!({ got[0].header.sequence_number }, 7);
}

#[tokio::test]
async fn peer_without_video_rejects_it_and_the_offerer_pins_nothing() {
    init_tracing();
    let vp8 = [(VideoCodec::VP8, 96)];
    let (caller, callee, _caller_ev, mut callee_ev) =
        connect_pair_with(video_cfg(&vp8), PeerConfig::default()).await;
    assert!(caller.negotiated_video().is_none());
    assert!(callee.negotiated_video().is_none());
    assert!(caller.video_sender().is_err());
    assert!(callee.remote_sdp().unwrap().contains("m=video"));
    assert!(caller
        .remote_sdp()
        .unwrap()
        .contains("m=video 0 UDP/TLS/RTP/SAVPF 96"));
    // Audio is unaffected.
    let audio = caller.sender().unwrap();
    audio
        .send_audio(Bytes::from_static(&[1, 2, 3]), 960)
        .await
        .unwrap();
    expect_rtp(&mut callee_ev, &[1, 2, 3]).await;
}

#[tokio::test]
async fn video_added_by_reoffer_on_the_same_transport() {
    init_tracing();
    let h264_then_vp8 = [(VideoCodec::H264, 96), (VideoCodec::VP8, 97)];
    let vp8_only = [(VideoCodec::VP8, 120)];
    let (mut caller, mut callee, mut caller_ev, mut callee_ev) =
        connect_pair_with(PeerConfig::default(), video_cfg(&vp8_only)).await;
    assert!(caller.negotiated_video().is_none());

    caller.set_video(Some(VideoConfig {
        codecs: h264_then_vp8.to_vec(),
        max_kbps: Some(800),
        ..VideoConfig::default()
    }));
    let reoffer = caller.create_offer().await.unwrap();
    assert!(reoffer.contains("a=group:BUNDLE 0 1\r\n"), "{reoffer}");
    assert!(reoffer.contains("m=video "), "{reoffer}");
    callee.set_remote_offer(&reoffer).await.unwrap();
    let answer = callee.create_answer().await.unwrap();
    caller.set_remote_answer(&answer).await.unwrap();

    // The answerer only carries VP8, so that is what both pinned, at the
    // offerer's payload type.
    for peer in [&caller, &callee] {
        let v = peer.negotiated_video().expect("video negotiated");
        assert_eq!((v.codec, v.payload_type), (VideoCodec::VP8, 97));
    }
    let vs = caller.video_sender().unwrap();
    let pkt = RtpPacket::build(97, 1, 0, 1, Bytes::from_static(&[9; 50]), true)
        .to_bytes()
        .freeze();
    vs.send_packet(pkt).await.unwrap();
    let got = expect_video(&mut callee_ev, 1).await;
    assert_eq!(got[0].header.payload_type(), 97);
    assert_eq!({ got[0].header.ssrc }, caller.video_ssrc());

    // And the other way, without renegotiating again.
    let vs2 = callee.video_sender().unwrap();
    vs2.send_packet(
        RtpPacket::build(97, 2, 0, 1, Bytes::from_static(&[8; 50]), true)
            .to_bytes()
            .freeze(),
    )
    .await
    .unwrap();
    let got = expect_video(&mut caller_ev, 1).await;
    assert_eq!({ got[0].header.ssrc }, callee.video_ssrc());

    // Dropping video again by re-offer rejects the section.
    caller.set_video(None);
    let reoffer = caller.create_offer().await.unwrap();
    assert!(!reoffer.contains("m=video"), "{reoffer}");
    callee.set_remote_offer(&reoffer).await.unwrap();
    let answer = callee.create_answer().await.unwrap();
    caller.set_remote_answer(&answer).await.unwrap();
    assert!(caller.negotiated_video().is_none());
    assert!(callee.negotiated_video().is_none());
    let _ = PeerConnection::new(vec![]).await.unwrap();
}
