//! The publisher against the stand-in ingest: a stream published and
//! every tag arriving as sent, a key refused, a connection cut under it,
//! and the same over RTMPS.

use forge_rtmp::client::Options;
use forge_rtmp::flv::{aac_frame, aac_sequence_header, Metadata};
use forge_rtmp::server::StandInServer;
use forge_rtmp::{AvcTagger, Publisher, RtmpError, RtmpUrl, TagKind};
use std::sync::Arc;
use std::time::Duration;

const SPS: &[u8] = &[0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x01, 0x40];
const PPS: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

fn picture(keyframe: bool, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if keyframe {
        for nal in [SPS, PPS] {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
    }
    out.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }]);
    out.extend((0..size).map(|i| (i % 251) as u8 | 1));
    out
}

async fn publish_a_second(p: &Publisher) {
    p.send_metadata(&Metadata {
        width: Some(1280),
        height: Some(720),
        framerate: Some(15.0),
        audio: Some((48_000, 1)),
        encoder: "test".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    p.send_audio(0, aac_sequence_header(&[0x11, 0x88]))
        .await
        .unwrap();
    let mut tagger = AvcTagger::new();
    for i in 0..15u32 {
        // A large keyframe to cross many chunks, small inter frames.
        let pic = picture(i == 0, if i == 0 { 20_000 } else { 900 });
        for body in tagger.bodies(i == 0, &pic).unwrap() {
            p.send_video(i * 66, body).await.unwrap();
        }
    }
    for i in 0..47u32 {
        p.send_audio(i * 21, aac_frame(&[0x21, i as u8]))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_stream_is_published_and_arrives_as_it_was_sent() {
    let server = StandInServer::start().await.unwrap();
    let url = RtmpUrl::parse(&server.url("live2")).unwrap();
    let p = Publisher::connect(&url, "abcd-efgh").await.unwrap();
    publish_a_second(&p).await;
    let got = server
        .wait_for(Duration::from_secs(5), |all| {
            all.first()
                .is_some_and(|p| p.tags.len() == 1 + 16 + 47 && p.pongs == 1)
        })
        .await
        .unwrap_or_else(|all| panic!("not all arrived: {all:#?}"));
    let one = &got[0];
    assert_eq!((one.app.as_str(), one.key.as_str()), ("live2", "abcd-efgh"));
    assert_eq!(one.tc_url, url.tc_url);
    let meta = one.metadata.as_ref().expect("onMetaData");
    assert_eq!(meta[0].as_str(), Some("onMetaData"));
    assert_eq!(
        meta[1].get("width").and_then(|v| v.as_number()),
        Some(1280.0)
    );
    assert_eq!(
        meta[1].get("audiocodecid").and_then(|v| v.as_number()),
        Some(10.0)
    );

    let video: Vec<_> = one.video().collect();
    assert_eq!(video.len(), 16);
    assert_eq!(&video[0].body[..2], &[0x17, 0], "the sequence header first");
    assert_eq!(&video[1].body[..2], &[0x17, 1], "then the keyframe");
    assert!(video[1].body.len() > 20_000, "whole, across chunks");
    assert_eq!(video[15].timestamp, 14 * 66);
    let audio: Vec<_> = one.audio().collect();
    assert_eq!(audio[0].body, vec![0xAF, 0, 0x11, 0x88]);
    assert_eq!(audio[47].timestamp, 46 * 21);
    assert!(p.bytes_sent() > 20_000);

    p.close().await;
    server
        .wait_for(Duration::from_secs(5), |all| {
            all[0].unpublished && all[0].closed
        })
        .await
        .expect("unpublished and closed");
}

#[tokio::test]
async fn a_refused_key_is_an_error_naming_why() {
    let server = StandInServer::start().await.unwrap();
    server.refuse_key("wrong");
    let url = RtmpUrl::parse(&server.url("live")).unwrap();
    let e = Publisher::connect(&url, "wrong").await.unwrap_err();
    assert!(
        matches!(&e, RtmpError::Refused { code, .. } if code == "NetStream.Publish.BadName"),
        "{e:?}"
    );
    assert!(server.publishes().is_empty());
}

#[tokio::test]
async fn a_cut_connection_fails_the_next_send() {
    let server = StandInServer::start().await.unwrap();
    let url = RtmpUrl::parse(&server.url("live")).unwrap();
    let p = Publisher::connect(&url, "k").await.unwrap();
    server
        .wait_for(Duration::from_secs(5), |all| all.len() == 1)
        .await
        .unwrap();
    server.cut();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if p.error().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cut was never seen"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(p.error(), Some(RtmpError::Closed));
    assert!(p.send_audio(0, aac_frame(&[1])).await.is_err());
}

#[tokio::test]
async fn nothing_listening_is_an_error_within_the_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live")).unwrap();
    assert!(matches!(
        Publisher::connect(&url, "k").await,
        Err(RtmpError::Io(_))
    ));

    // Something that accepts and says nothing: the timeout.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _held = silent.accept().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live")).unwrap();
    let options = Options {
        timeout: Duration::from_millis(300),
        ..Default::default()
    };
    assert!(matches!(
        Publisher::connect_with(&url, "k", &options).await,
        Err(RtmpError::Timeout(_))
    ));
}

#[tokio::test]
async fn rtmps_publishes_over_tls() {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    let server_config = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key)
        .unwrap();
    let server = StandInServer::start_tls(Arc::new(server_config))
        .await
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).unwrap();
    let client_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let url = RtmpUrl::parse(&server.url("live")).unwrap();
    assert!(url.tls);
    let options = Options {
        tls: Some(Arc::new(client_config)),
        ..Default::default()
    };
    let p = Publisher::connect_with(&url, "secure", &options)
        .await
        .unwrap();
    publish_a_second(&p).await;
    let got = server
        .wait_for(Duration::from_secs(5), |all| {
            all.first().is_some_and(|p| p.tags.len() == 64)
        })
        .await
        .unwrap_or_else(|all| panic!("{all:#?}"));
    assert_eq!(got[0].key, "secure");
    assert!(got[0].tags.iter().any(|t| t.kind == TagKind::Video));

    // A certificate nobody trusts: refused.
    let e = Publisher::connect(&url, "secure").await.unwrap_err();
    assert!(matches!(e, RtmpError::Tls(_)), "{e:?}");
}
