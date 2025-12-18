//! Integration tests for forge-ai-stream
//!
//! These tests verify the full functionality of the AI streaming components.

use forge_ai_stream::{
    AIConnectorConfig, AIConnectorType, AudioConverter, AudioFormat, BargeInConfig,
    BargeInDetector, VadConfig, VadDetector, VadState,
};
use forge_core::SecureString;
use std::time::Duration;

#[test]
fn test_audio_conversion_pipeline() {
    // Test full audio conversion pipeline
    let converter = AudioConverter::new(AudioFormat::Pcm16Mono(16000), AudioFormat::G711Mulaw);

    // Generate test audio
    let input: Vec<i16> = (0..160)
        .map(|i| ((i as f32 * 0.1).sin() * 1000.0) as i16)
        .collect();

    // Convert to G.711
    let encoded = converter.convert(&input).unwrap();
    assert_eq!(encoded.len(), input.len());

    // Convert back
    let decoder = AudioConverter::new(AudioFormat::G711Mulaw, AudioFormat::Pcm16Mono(16000));
    let decoded = decoder.convert(&encoded).unwrap();

    // Verify lossy compression is within tolerance
    for (orig, dec) in input.iter().zip(decoded.iter()) {
        let diff = (orig - dec).abs();
        assert!(diff < 600, "Difference too large: {} vs {}", orig, dec);
    }
}

#[test]
fn test_vad_pipeline() {
    let config = VadConfig {
        sensitivity: 0.5,
        min_speech_duration_ms: 100,
        min_silence_duration_ms: 200,
        sample_rate: 16000,
        frame_size_ms: 20,
        energy_threshold: 100.0,
        zcr_threshold: 0.3,
    };

    let mut detector = VadDetector::new(config);

    // Simulate audio stream with speech and silence
    let speech_frame: Vec<i16> = (0..320)
        .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
        .collect();
    let silence_frame = vec![0i16; 320];

    // Process speech frames
    for _ in 0..10 {
        let (_state, confidence) = detector.process(&speech_frame).unwrap();
        if detector.state() == VadState::Speech {
            assert!(confidence > 0.0);
            break;
        }
    }
    assert_eq!(detector.state(), VadState::Speech);

    // Process silence frames
    for _ in 0..15 {
        detector.process(&silence_frame).unwrap();
    }
    assert_eq!(detector.state(), VadState::Silence);
}

#[test]
fn test_bargein_pipeline() {
    let vad_config = VadConfig {
        sensitivity: 0.5,
        min_speech_duration_ms: 50,
        min_silence_duration_ms: 100,
        sample_rate: 16000,
        frame_size_ms: 20,
        energy_threshold: 100.0,
        zcr_threshold: 0.3,
    };
    let vad = VadDetector::new(vad_config);

    let bargein_config = BargeInConfig {
        enabled: true,
        cooldown_duration: Duration::from_millis(500),
        confidence_threshold: 0.5,
        min_user_speech_duration: Duration::from_millis(100),
    };

    let mut detector = BargeInDetector::new(bargein_config, vad);

    // Simulate AI speaking
    detector.ai_started_speaking();
    assert!(detector.is_ai_speaking());

    // User starts speaking (barge-in)
    let speech_frame: Vec<i16> = (0..320)
        .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
        .collect();

    let mut barge_in_detected = false;
    for _ in 0..20 {
        let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
        if detected {
            barge_in_detected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(barge_in_detected);
    assert_eq!(detector.interrupt_count(), 1);
}

#[test]
fn test_connector_config() {
    let config = AIConnectorConfig {
        connector_type: AIConnectorType::OpenAI,
        api_key: SecureString::new("test-key"),
        endpoint: Some("wss://test.example.com".to_string()),
        model: "gpt-4o-realtime-preview".to_string(),
        voice: Some("alloy".to_string()),
        temperature: Some(0.8),
        max_tokens: Some(4096),
        instructions: Some("You are a helpful assistant".to_string()),
        tools: vec![],
        enable_vad: true,
        enable_barge_in: true,
        connect_timeout: Duration::from_secs(30),
        request_timeout: Duration::from_secs(60),
    };

    assert_eq!(config.connector_type, AIConnectorType::OpenAI);
    assert_eq!(config.model, "gpt-4o-realtime-preview");
    assert!(config.enable_vad);
    assert!(config.enable_barge_in);
}

#[test]
fn test_multi_format_conversion() {
    // Test converting between multiple formats
    let original: Vec<i16> = (0..160).map(|i| (i * 100) as i16).collect();

    // PCM16 Mono 16kHz -> PCM16 Mono 8kHz
    let converter1 =
        AudioConverter::new(AudioFormat::Pcm16Mono(16000), AudioFormat::Pcm16Mono(8000));
    let resampled = converter1.convert(&original).unwrap();
    assert!(resampled.len() < original.len());

    // PCM16 Mono -> PCM16 Stereo
    let converter2 =
        AudioConverter::new(AudioFormat::Pcm16Mono(8000), AudioFormat::Pcm16Stereo(8000));
    let stereo = converter2.convert(&resampled).unwrap();
    assert_eq!(stereo.len(), resampled.len() * 2);

    // PCM16 Stereo -> G.711 A-law (via mono conversion)
    let converter3 =
        AudioConverter::new(AudioFormat::Pcm16Stereo(8000), AudioFormat::Pcm16Mono(8000));
    let mono = converter3.convert(&stereo).unwrap();
    assert_eq!(mono.len(), resampled.len());
}

#[test]
fn test_vad_adaptive_threshold() {
    let config = VadConfig {
        sensitivity: 0.6,
        energy_threshold: 0.0, // Enable adaptive threshold
        ..Default::default()
    };

    let mut detector = VadDetector::new(config);

    // Simulate varying noise levels
    for level in [50, 100, 150, 200, 250].iter().cycle().take(100) {
        let frame: Vec<i16> = vec![*level as i16; 320];
        detector.process(&frame).unwrap();
    }

    // Adaptive threshold should be established
    assert!(detector.noise_level() > 0.0);
    assert!(detector.energy_threshold() > 0.0);

    // High energy should now be detected as speech
    let high_energy: Vec<i16> = (0..320)
        .map(|i| ((i as f32 * 0.1).sin() * 3000.0) as i16)
        .collect();

    for _ in 0..10 {
        detector.process(&high_energy).unwrap();
    }

    // Should detect speech with adaptive threshold
    assert_eq!(detector.state(), VadState::Speech);
}

#[test]
fn test_bargein_cooldown() {
    let vad_config = VadConfig {
        sensitivity: 0.5,
        min_speech_duration_ms: 50,
        min_silence_duration_ms: 100,
        sample_rate: 16000,
        frame_size_ms: 20,
        energy_threshold: 100.0,
        zcr_threshold: 0.3,
    };
    let vad = VadDetector::new(vad_config);

    let bargein_config = BargeInConfig {
        enabled: true,
        cooldown_duration: Duration::from_millis(200),
        confidence_threshold: 0.5,
        min_user_speech_duration: Duration::from_millis(50),
    };

    let mut detector = BargeInDetector::new(bargein_config, vad);
    detector.ai_started_speaking();

    let speech_frame: Vec<i16> = (0..320)
        .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
        .collect();

    // First barge-in
    for _ in 0..10 {
        let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
        if detected {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(detector.interrupt_count(), 1);

    // Immediate second attempt should be blocked by cooldown
    let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
    assert!(!detected);

    // Process silence to reset VAD state
    let silence_frame = vec![0i16; 320];
    for _ in 0..10 {
        detector.process_audio(&silence_frame).unwrap();
    }

    // Wait for cooldown to expire
    std::thread::sleep(Duration::from_millis(250));

    // AI starts speaking again
    detector.ai_started_speaking();

    // Now user speaks again (should detect second barge-in)
    let mut detected_second = false;
    for _ in 0..20 {
        let (detected, _, _) = detector.process_audio(&speech_frame).unwrap();
        if detected {
            detected_second = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        detected_second,
        "Second barge-in should be detected after cooldown"
    );
    assert_eq!(detector.interrupt_count(), 2);
}
