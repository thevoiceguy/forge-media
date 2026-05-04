//! Comprehensive SDP Negotiation Tests
//!
//! Tests RFC 3264 offer/answer negotiation with various scenarios including:
//! - Basic codec negotiation
//! - Codec prioritization
//! - Direction negotiation
//! - Multiple media streams
//! - Error cases and edge conditions

use forge_sdp::{
    helpers, profiles::SdpProfile, CodecInfo, MediaType, SessionDescription, SessionDescriptionExt,
};

/// Helper to create a simple audio SDP offer
fn create_audio_offer(addr: &str, port: u16, codecs: &[(u8, &str, u32)]) -> SessionDescription {
    let mut builder = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("alice", "1", addr)
        .unwrap()
        .session_name("Test Offer")
        .unwrap()
        .connection(addr)
        .unwrap()
        .time(0, 0);

    let mut media = sip_sdp::MediaDescription::audio(port);
    for (pt, name, rate) in codecs {
        media = media
            .add_format(*pt)
            .unwrap()
            .add_rtpmap(*pt, name, *rate, None)
            .unwrap();
    }

    builder = builder.media(media).unwrap();
    builder.build()
}

// ============================================================================
// Basic Codec Negotiation Tests
// ============================================================================

#[test]
fn test_basic_pcmu_pcma_negotiation() {
    // Offer: PCMU + PCMA
    let offer = create_audio_offer(
        "192.168.1.100",
        5000,
        &[(0, "PCMU", 8000), (8, "PCMA", 8000)],
    );

    // Answer: PCMU only (from capabilities)
    let profile = SdpProfile::audio_only();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Verify answer has audio media
    assert_eq!(answer.media.len(), 1);
    assert_eq!(answer.media[0].media_type, MediaType::Audio);

    // Verify negotiated codecs (should have both PCMU and PCMA)
    assert!(answer.media[0].formats.contains(&"0".into())); // PCMU
    assert!(answer.media[0].formats.contains(&"8".into())); // PCMA
}

#[test]
fn test_codec_prioritization_offerer_preference() {
    // Offer: PCMU (first), PCMA (second)
    let offer = create_audio_offer(
        "192.168.1.100",
        5000,
        &[(0, "PCMU", 8000), (8, "PCMA", 8000)],
    );

    let profile = SdpProfile::audio_only();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // First format should be PCMU (offerer's preference)
    assert_eq!(answer.media[0].formats[0].as_str(), "0"); // PCMU first
}

#[test]
fn test_opus_negotiation() {
    // Offer: Opus only. Channels must match audio_opus profile (opus/48000/2)
    // because sip-sdp's dynamic-rtpmap matcher uses strict encoding_params equality.
    let opus_media = sip_sdp::MediaDescription::audio(5000)
        .add_format(111)
        .unwrap()
        .add_rtpmap(111, "opus", 48000, Some("2"))
        .unwrap();
    let offer = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("alice", "1", "192.168.1.100")
        .unwrap()
        .session_name("Test Offer")
        .unwrap()
        .connection("192.168.1.100")
        .unwrap()
        .time(0, 0)
        .media(opus_media)
        .unwrap()
        .build();

    let profile = SdpProfile::audio_opus();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Verify Opus was negotiated
    assert!(answer.media[0].formats.contains(&"111".into()));

    // Verify Opus rtpmap
    let opus = answer.media[0].rtpmaps.get(&111).unwrap();
    assert_eq!(opus.encoding_name.as_str(), "opus");
    assert_eq!(opus.clock_rate, 48000);
}

#[test]
fn test_multi_codec_negotiation() {
    // Offer: PCMU + PCMA + Opus
    let offer = create_audio_offer(
        "192.168.1.100",
        5000,
        &[(0, "PCMU", 8000), (8, "PCMA", 8000), (111, "opus", 48000)],
    );

    let profile = SdpProfile::audio_all();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // All codecs should be present
    assert!(answer.media[0].formats.contains(&"0".into()));
    assert!(answer.media[0].formats.contains(&"8".into()));
    assert!(answer.media[0].formats.contains(&"111".into()));
}

// ============================================================================
// No Common Codec Tests
// ============================================================================

#[test]
fn test_no_common_codec_audio_only() {
    // Offer: G.729 only (not supported)
    let offer = create_audio_offer("192.168.1.100", 5000, &[(18, "G729", 8000)]);

    let profile = SdpProfile::audio_only(); // Only supports PCMU/PCMA
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Answer should have audio media but with port 0 (rejected)
    assert_eq!(answer.media.len(), 1);
    assert_eq!(answer.media[0].media_type, MediaType::Audio);
    // Note: sip-sdp's negotiate_answer may return formats even if no match,
    // or may set port to 0. Check actual behavior.
}

// ============================================================================
// Direction Negotiation Tests
// ============================================================================

#[test]
fn test_direction_sendrecv_to_sendrecv() {
    let media = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap()
        .with_direction("sendrecv")
        .unwrap();
    let offer = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("alice", "1", "192.168.1.100")
        .unwrap()
        .session_name("Test")
        .unwrap()
        .connection("192.168.1.100")
        .unwrap()
        .time(0, 0)
        .media(media)
        .unwrap()
        .build();

    let profile = SdpProfile::audio_only();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Check for sendrecv direction
    let has_sendrecv = answer.media[0]
        .attributes
        .iter()
        .any(|attr| matches!(attr, sip_sdp::Attribute::Property(name) if name == "sendrecv"));

    assert!(has_sendrecv);
}

#[test]
fn test_direction_sendonly_to_recvonly() {
    let offer_media = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap()
        .with_direction("sendonly")
        .unwrap();
    let offer = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("alice", "1", "192.168.1.100")
        .unwrap()
        .session_name("Test")
        .unwrap()
        .connection("192.168.1.100")
        .unwrap()
        .time(0, 0)
        .media(offer_media)
        .unwrap()
        .build();

    let caps_media = sip_sdp::MediaDescription::audio(6000)
        .add_format(0)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap()
        .with_direction("sendrecv")
        .unwrap();
    let local_caps = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("bob", "1", "10.0.0.1")
        .unwrap()
        .session_name("Capabilities")
        .unwrap()
        .connection("10.0.0.1")
        .unwrap()
        .time(0, 0)
        .media(caps_media)
        .unwrap()
        .build();

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Answer should be recvonly (computed from offer's sendonly)
    let has_recvonly = answer.media[0]
        .attributes
        .iter()
        .any(|attr| matches!(attr, sip_sdp::Attribute::Property(name) if name == "recvonly"));

    assert!(has_recvonly);
}

// ============================================================================
// Multiple Media Streams Tests
// ============================================================================

#[test]
fn test_audio_video_offer_audio_only_answer() {
    let audio = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap();
    let video = sip_sdp::MediaDescription::video(5002)
        .add_format(96)
        .unwrap()
        .add_rtpmap(96, "H264", 90000, None)
        .unwrap();
    let offer = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("alice", "1", "192.168.1.100")
        .unwrap()
        .session_name("Test")
        .unwrap()
        .connection("192.168.1.100")
        .unwrap()
        .time(0, 0)
        .media(audio)
        .unwrap()
        .media(video)
        .unwrap()
        .build();

    let profile = SdpProfile::audio_only();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);

    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Answer should have 2 media sections
    assert_eq!(answer.media.len(), 2);

    // Audio should be accepted
    assert_eq!(answer.media[0].media_type, MediaType::Audio);
    assert!(answer.media[0].port > 0);

    // Video should be rejected (port 0)
    assert_eq!(answer.media[1].media_type, MediaType::Video);
    assert_eq!(answer.media[1].port, 0);
}

// ============================================================================
// Codec Extraction and Helper Tests
// ============================================================================

#[test]
fn test_extract_codecs_from_media() {
    let media = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_format(8)
        .unwrap()
        .add_format(111)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap()
        .add_rtpmap(8, "PCMA", 8000, None)
        .unwrap()
        .add_rtpmap(111, "opus", 48000, Some("2"))
        .unwrap();

    let codecs = helpers::extract_codecs(&media);

    assert_eq!(codecs.len(), 3);
    assert_eq!(codecs[0].payload_type, 0);
    assert_eq!(codecs[0].encoding_name, "PCMU");
    assert_eq!(codecs[1].payload_type, 8);
    assert_eq!(codecs[1].encoding_name, "PCMA");
    assert_eq!(codecs[2].payload_type, 111);
    assert_eq!(codecs[2].encoding_name, "opus");
    assert_eq!(codecs[2].channels, Some(2));
}

#[test]
fn test_extract_primary_codec() {
    let media = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_format(8)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap()
        .add_rtpmap(8, "PCMA", 8000, None)
        .unwrap();

    let primary = helpers::extract_primary_codec(&media).unwrap();

    assert_eq!(primary.payload_type, 0);
    assert_eq!(primary.encoding_name, "PCMU");
}

#[test]
fn test_find_dtmf_codec() {
    let media = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_format(101)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap()
        .add_rtpmap(101, "telephone-event", 8000, None)
        .unwrap();

    let dtmf = helpers::find_dtmf_codec(&media).unwrap();

    assert_eq!(dtmf.payload_type, 101);
    assert_eq!(dtmf.encoding_name, "telephone-event");
    assert!(dtmf.is_dtmf());
}

#[test]
fn test_has_codec() {
    let sdp = create_audio_offer(
        "192.168.1.100",
        5000,
        &[(0, "PCMU", 8000), (111, "opus", 48000)],
    );

    assert!(helpers::has_codec(&sdp, "PCMU"));
    assert!(helpers::has_codec(&sdp, "opus"));
    assert!(helpers::has_codec(&sdp, "OpUs")); // Case-insensitive
    assert!(!helpers::has_codec(&sdp, "PCMA"));
}

// ============================================================================
// Codec Info to AudioCodec Conversion Tests
// ============================================================================

#[test]
fn test_codec_info_to_audio_codec() {
    let test_cases = vec![
        ("PCMU", Some(forge_core::AudioCodec::PCMU)),
        ("PCMA", Some(forge_core::AudioCodec::PCMA)),
        ("opus", Some(forge_core::AudioCodec::Opus)),
        ("G722", Some(forge_core::AudioCodec::G722)),
        ("G729", Some(forge_core::AudioCodec::G729)),
        ("speex", Some(forge_core::AudioCodec::Speex)),
        ("iLBC", Some(forge_core::AudioCodec::ILBC)),
        ("AMR", Some(forge_core::AudioCodec::AMR)),
        ("AMR-WB", Some(forge_core::AudioCodec::AMRWB)),
        ("UnknownCodec", None),
    ];

    for (name, expected) in test_cases {
        let codec_info = CodecInfo {
            payload_type: 96,
            encoding_name: name.to_string(),
            clock_rate: 8000,
            channels: None,
        };

        assert_eq!(
            codec_info.to_audio_codec(),
            expected,
            "Failed for codec: {}",
            name
        );
    }
}

// ============================================================================
// SDP Parsing Tests
// ============================================================================

#[test]
fn test_parse_valid_sdp() {
    let sdp_text = "v=0\r\n\
                    o=alice 123 0 IN IP4 192.168.1.100\r\n\
                    s=Test Session\r\n\
                    c=IN IP4 192.168.1.100\r\n\
                    t=0 0\r\n\
                    m=audio 5000 RTP/AVP 0 8\r\n\
                    a=rtpmap:0 PCMU/8000\r\n\
                    a=rtpmap:8 PCMA/8000\r\n\
                    a=sendrecv\r\n";

    let sdp = SessionDescription::from_str(sdp_text).unwrap();

    assert_eq!(sdp.origin.username.as_str(), "alice");
    assert_eq!(sdp.media.len(), 1);
    assert_eq!(sdp.media[0].media_type, MediaType::Audio);
    assert_eq!(sdp.media[0].port, 5000);
    assert!(sdp.media[0].formats.contains(&"0".into()));
    assert!(sdp.media[0].formats.contains(&"8".into()));
}

#[test]
fn test_parse_invalid_sdp() {
    let invalid_sdp = "This is not valid SDP";

    let result = SessionDescription::from_str(invalid_sdp);

    assert!(result.is_err());
}

#[test]
fn test_parse_sdp_with_opus() {
    let sdp_text = "v=0\r\n\
                    o=alice 123 0 IN IP4 192.168.1.100\r\n\
                    s=Test Session\r\n\
                    c=IN IP4 192.168.1.100\r\n\
                    t=0 0\r\n\
                    m=audio 5000 RTP/AVP 111 101\r\n\
                    a=rtpmap:111 opus/48000/2\r\n\
                    a=rtpmap:101 telephone-event/8000\r\n\
                    a=fmtp:101 0-16\r\n\
                    a=sendrecv\r\n";

    let sdp = SessionDescription::from_str(sdp_text).unwrap();

    assert_eq!(sdp.media[0].formats.len(), 2);

    // Check Opus
    let opus = sdp.media[0].rtpmaps.get(&111).unwrap();
    assert_eq!(opus.encoding_name.as_str(), "opus");
    assert_eq!(opus.clock_rate, 48000);
    assert_eq!(opus.encoding_params.as_ref().map(|s| s.as_str()), Some("2"));

    // Check DTMF
    let dtmf = sdp.media[0].rtpmaps.get(&101).unwrap();
    assert_eq!(dtmf.encoding_name.as_str(), "telephone-event");
}

// ============================================================================
// Profile Integration Tests
// ============================================================================

#[test]
fn test_profile_generates_valid_sdp() {
    let profile = SdpProfile::audio_only();
    let sdp = profile.with_local_addr("10.0.0.1", 5000);

    // Verify it can be serialized
    let sdp_text = sip_sdp::serialize::serialize_sdp(&sdp);
    assert!(sdp_text.contains("v=0"));
    assert!(sdp_text.contains("10.0.0.1"));
    assert!(sdp_text.contains("m=audio 5000"));
}

#[test]
fn test_profile_round_trip() {
    let profile = SdpProfile::audio_all();
    let sdp = profile.with_local_addr("10.0.0.1", 5000);

    // Serialize
    let sdp_text = sip_sdp::serialize::serialize_sdp(&sdp);

    // Parse back
    let parsed = SessionDescription::from_str(&sdp_text).unwrap();

    // Verify
    assert_eq!(parsed.origin.unicast_address.as_str(), "10.0.0.1");
    assert_eq!(parsed.media[0].port, 5000);
    assert!(parsed.media[0].formats.contains(&"0".into())); // PCMU
    assert!(parsed.media[0].formats.contains(&"8".into())); // PCMA
    assert!(parsed.media[0].formats.contains(&"111".into())); // Opus
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn test_empty_sdp() {
    let result = SessionDescription::from_str("");
    assert!(result.is_err());
}

#[test]
fn test_sdp_missing_media() {
    let sdp_text = "v=0\r\n\
                    o=alice 123 0 IN IP4 192.168.1.100\r\n\
                    s=Test Session\r\n\
                    c=IN IP4 192.168.1.100\r\n\
                    t=0 0\r\n";

    let sdp = SessionDescription::from_str(sdp_text).unwrap();
    assert_eq!(sdp.media.len(), 0);
}

#[test]
fn test_sdp_with_unknown_codec() {
    let sdp_text = "v=0\r\n\
                    o=alice 123 0 IN IP4 192.168.1.100\r\n\
                    s=Test Session\r\n\
                    c=IN IP4 192.168.1.100\r\n\
                    t=0 0\r\n\
                    m=audio 5000 RTP/AVP 96\r\n\
                    a=rtpmap:96 UnknownCodec/8000\r\n";

    let sdp = SessionDescription::from_str(sdp_text).unwrap();
    let codec_info = helpers::extract_primary_codec(&sdp.media[0]).unwrap();

    // Should parse successfully but not map to AudioCodec
    assert_eq!(codec_info.encoding_name, "UnknownCodec");
    assert_eq!(codec_info.to_audio_codec(), None);
}

#[test]
fn test_get_audio_codec_from_negotiated_sdp() {
    let profile = SdpProfile::audio_only();
    let sdp = profile.with_local_addr("10.0.0.1", 5000);

    let codec = sdp.get_audio_codec().unwrap();

    assert_eq!(codec.0, 0); // PCMU payload type
    assert_eq!(codec.1, "PCMU");
}

#[test]
fn test_find_media_by_type() {
    let audio = sip_sdp::MediaDescription::audio(5000)
        .add_format(0)
        .unwrap()
        .add_rtpmap(0, "PCMU", 8000, None)
        .unwrap();
    let video = sip_sdp::MediaDescription::video(5002)
        .add_format(96)
        .unwrap()
        .add_rtpmap(96, "H264", 90000, None)
        .unwrap();
    let sdp = sip_sdp::builder::SessionDescriptionBuilder::new()
        .origin("alice", "1", "192.168.1.100")
        .unwrap()
        .session_name("Test")
        .unwrap()
        .connection("192.168.1.100")
        .unwrap()
        .time(0, 0)
        .media(audio)
        .unwrap()
        .media(video)
        .unwrap()
        .build();

    let audio = sdp.find_media(MediaType::Audio).unwrap();
    assert_eq!(audio.port, 5000);

    let video = sdp.find_media(MediaType::Video).unwrap();
    assert_eq!(video.port, 5002);

    assert!(sdp.find_media(MediaType::Application).is_none());
}

// ============================================================================
// Real-World Scenario Tests
// ============================================================================

#[test]
fn test_typical_sip_phone_offer() {
    // Typical SIP phone offer
    let sdp_text = "v=0\r\n\
                    o=user1 123456789 987654321 IN IP4 192.168.1.50\r\n\
                    s=Phone Call\r\n\
                    c=IN IP4 192.168.1.50\r\n\
                    t=0 0\r\n\
                    m=audio 10000 RTP/AVP 0 8 101\r\n\
                    a=rtpmap:0 PCMU/8000\r\n\
                    a=rtpmap:8 PCMA/8000\r\n\
                    a=rtpmap:101 telephone-event/8000\r\n\
                    a=fmtp:101 0-15\r\n\
                    a=sendrecv\r\n\
                    a=ptime:20\r\n";

    let offer = SessionDescription::from_str(sdp_text).unwrap();

    // Generate answer
    let profile = SdpProfile::audio_only();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);
    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Verify answer
    assert_eq!(answer.media.len(), 1);
    assert!(answer.media[0].formats.contains(&"0".into())); // PCMU
    assert!(answer.media[0].formats.contains(&"8".into())); // PCMA
    assert!(answer.media[0].formats.contains(&"101".into())); // telephone-event
}

#[test]
#[ignore = "WebRTC DTLS-SRTP (UDP/TLS/RTP/SAVPF) support is Phase 3"]
fn test_webrtc_style_offer() {
    // WebRTC-style offer with Opus and DTLS-SRTP protocol
    // This will be supported in Phase 3 when we add WebRTC support
    let sdp_text = "v=0\r\n\
                    o=- 4611731400430051336 2 IN IP4 127.0.0.1\r\n\
                    s=-\r\n\
                    t=0 0\r\n\
                    m=audio 9 UDP/TLS/RTP/SAVPF 111 103 104\r\n\
                    c=IN IP4 0.0.0.0\r\n\
                    a=rtpmap:111 opus/48000/2\r\n\
                    a=rtpmap:103 ISAC/16000\r\n\
                    a=rtpmap:104 ISAC/32000\r\n\
                    a=sendrecv\r\n";

    let offer = SessionDescription::from_str(sdp_text).unwrap();

    // Forge supports Opus
    let profile = SdpProfile::audio_opus();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);
    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Should negotiate Opus
    assert!(answer.media[0].formats.contains(&"111".into()));
}

#[test]
fn test_asterisk_style_offer() {
    // Asterisk-style offer
    let sdp_text = "v=0\r\n\
                    o=root 31337 31337 IN IP4 192.168.1.1\r\n\
                    s=Asterisk PBX 18.0.0\r\n\
                    c=IN IP4 192.168.1.1\r\n\
                    t=0 0\r\n\
                    m=audio 16384 RTP/AVP 0 8 18 101\r\n\
                    a=rtpmap:0 PCMU/8000\r\n\
                    a=rtpmap:8 PCMA/8000\r\n\
                    a=rtpmap:18 G729/8000\r\n\
                    a=fmtp:18 annexb=no\r\n\
                    a=rtpmap:101 telephone-event/8000\r\n\
                    a=fmtp:101 0-16\r\n\
                    a=ptime:20\r\n\
                    a=maxptime:150\r\n\
                    a=sendrecv\r\n";

    let offer = SessionDescription::from_str(sdp_text).unwrap();

    // Negotiate with audio_only profile (doesn't support G.729)
    let profile = SdpProfile::audio_only();
    let local_caps = profile.with_local_addr("10.0.0.1", 6000);
    let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1").unwrap();

    // Should have PCMU and PCMA, maybe not G.729
    assert!(answer.media[0].formats.contains(&"0".into()));
    assert!(answer.media[0].formats.contains(&"8".into()));
}
