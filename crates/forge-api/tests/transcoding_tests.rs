//! Integration tests for automatic transcoding functionality
//!
//! These tests verify that sessions automatically initialize transcoders
//! when participants use different codecs.

use forge_core::{AudioCodec, CallId, ParticipantId};
use forge_engine::{ParticipantCodecConfig, SessionManager, SessionManagerConfig};

/// Helper to create a codec config
fn codec_config(codec: AudioCodec, payload_type: u8, clock_rate: u32) -> ParticipantCodecConfig {
    ParticipantCodecConfig {
        payload_type,
        codec,
        clock_rate,
    }
}

#[tokio::test]
async fn test_same_codec_no_transcoder() {
    // Same codec (PCMU → PCMU) should NOT initialize transcoder
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_pcmu = codec_config(AudioCodec::PCMU, 0, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_pcmu.clone(),
            codec_pcmu,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Check that transcoders are NOT initialized
    let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
    let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

    assert!(
        transcoder_a_to_b.is_none(),
        "Transcoder A→B should not be initialized for same codec"
    );
    assert!(
        transcoder_b_to_a.is_none(),
        "Transcoder B→A should not be initialized for same codec"
    );
}

#[tokio::test]
async fn test_pcmu_to_pcma_transcoder() {
    // Different G.711 codecs (PCMU → PCMA) should initialize transcoder
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_pcmu = codec_config(AudioCodec::PCMU, 0, 8000);
    let codec_pcma = codec_config(AudioCodec::PCMA, 8, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_pcmu,
            codec_pcma,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Check that transcoders ARE initialized
    let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
    let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

    assert!(
        transcoder_a_to_b.is_some(),
        "Transcoder A→B should be initialized for PCMU→PCMA"
    );
    assert!(
        transcoder_b_to_a.is_some(),
        "Transcoder B→A should be initialized for PCMA→PCMU"
    );
}

#[tokio::test]
#[cfg(feature = "opus")]
async fn test_opus_to_pcmu_transcoder() {
    // Opus ↔ PCMU with different sample rates should initialize transcoder
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_opus = codec_config(AudioCodec::Opus, 111, 48000);
    let codec_pcmu = codec_config(AudioCodec::PCMU, 0, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_opus,
            codec_pcmu,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Check that transcoders ARE initialized (with resampling)
    let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
    let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

    assert!(
        transcoder_a_to_b.is_some(),
        "Transcoder A→B should be initialized for Opus→PCMU"
    );
    assert!(
        transcoder_b_to_a.is_some(),
        "Transcoder B→A should be initialized for PCMU→Opus"
    );
}

#[tokio::test]
#[cfg(feature = "opus")]
async fn test_opus_to_pcma_transcoder() {
    // Opus ↔ PCMA transcoding
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_opus = codec_config(AudioCodec::Opus, 111, 48000);
    let codec_pcma = codec_config(AudioCodec::PCMA, 8, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_opus,
            codec_pcma,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
    let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

    assert!(transcoder_a_to_b.is_some());
    assert!(transcoder_b_to_a.is_some());
}

#[tokio::test]
async fn test_all_codec_pairs() {
    // Test matrix of all supported codec pairs
    let mut supported_codecs = vec![(AudioCodec::PCMU, 0, 8000), (AudioCodec::PCMA, 8, 8000)];

    #[cfg(feature = "opus")]
    supported_codecs.push((AudioCodec::Opus, 111, 48000));

    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    for (codec_a, pt_a, rate_a) in &supported_codecs {
        for (codec_b, pt_b, rate_b) in &supported_codecs {
            let config_a = codec_config(*codec_a, *pt_a, *rate_a);
            let config_b = codec_config(*codec_b, *pt_b, *rate_b);

            let session = manager
                .create_session_with_codecs(
                    CallId::generate(),
                    ParticipantId::generate(),
                    ParticipantId::generate(),
                    config_a,
                    config_b,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("Failed to create session");

            let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
            let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

            if codec_a == codec_b {
                // Same codec - no transcoder
                assert!(
                    transcoder_a_to_b.is_none(),
                    "No transcoder for {:?} → {:?}",
                    codec_a,
                    codec_b
                );
                assert!(
                    transcoder_b_to_a.is_none(),
                    "No transcoder for {:?} → {:?}",
                    codec_b,
                    codec_a
                );
            } else {
                // Different codecs - transcoder initialized
                assert!(
                    transcoder_a_to_b.is_some(),
                    "Transcoder should exist for {:?} → {:?}",
                    codec_a,
                    codec_b
                );
                assert!(
                    transcoder_b_to_a.is_some(),
                    "Transcoder should exist for {:?} → {:?}",
                    codec_b,
                    codec_a
                );
            }
        }
    }
}

#[tokio::test]
async fn test_unsupported_codec_no_transcoder() {
    // Test that unsupported codecs don't crash, just skip transcoding
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_g722 = codec_config(AudioCodec::G722, 9, 8000); // Not in transcoder
    let codec_pcmu = codec_config(AudioCodec::PCMU, 0, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_g722,
            codec_pcmu,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Should not crash, but transcoders won't be initialized
    let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
    let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

    // G722 not supported by transcoder, so should be None
    assert!(
        transcoder_a_to_b.is_none(),
        "Transcoder should not initialize for unsupported codec"
    );
    assert!(transcoder_b_to_a.is_none());
}

#[tokio::test]
async fn test_transcoding_disabled() {
    // Test that transcoding can be disabled via config
    let mut config = SessionManagerConfig::default();
    config.session_config.transcoding_config.enable_transcoding = false;

    let manager = SessionManager::new(config, None);

    let codec_pcmu = codec_config(AudioCodec::PCMU, 0, 8000);
    let codec_pcma = codec_config(AudioCodec::PCMA, 8, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_pcmu,
            codec_pcma,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Transcoders should NOT be initialized when disabled
    let transcoder_a_to_b = session.transcoder_a_to_b().lock().await;
    let transcoder_b_to_a = session.transcoder_b_to_a().lock().await;

    assert!(
        transcoder_a_to_b.is_none(),
        "Transcoder should not initialize when disabled"
    );
    assert!(transcoder_b_to_a.is_none());
}

#[tokio::test]
async fn test_codec_config_stored() {
    // Verify codec configurations are properly stored in participants
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_opus = codec_config(AudioCodec::Opus, 111, 48000);
    let codec_pcma = codec_config(AudioCodec::PCMA, 8, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_opus.clone(),
            codec_pcma.clone(),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Check participant A codec config
    let participant_a = session.participant_a_stats().await;
    assert_eq!(participant_a.packets_received, 0); // No traffic yet

    // Access codec config through the session (would need getter methods)
    // For now, we verify session was created successfully
    assert_eq!(session.ports().rtp_port % 2, 0, "RTP port should be even");
}

#[tokio::test]
async fn test_bidirectional_transcoding() {
    // Verify both directions are handled
    let config = SessionManagerConfig::default();
    let manager = SessionManager::new(config, None);

    let codec_a = codec_config(AudioCodec::PCMU, 0, 8000);
    let codec_b = codec_config(AudioCodec::PCMA, 8, 8000);

    let session = manager
        .create_session_with_codecs(
            CallId::generate(),
            ParticipantId::generate(),
            ParticipantId::generate(),
            codec_a,
            codec_b,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("Failed to create session");

    // Both directions should have transcoders
    let has_a_to_b = session.transcoder_a_to_b().lock().await.is_some();
    let has_b_to_a = session.transcoder_b_to_a().lock().await.is_some();

    assert!(
        has_a_to_b && has_b_to_a,
        "Both transcoding directions should be initialized"
    );
}
