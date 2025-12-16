//! Basic Audio Streaming Example
//!
//! Demonstrates basic audio streaming with the OpenAI Realtime API.
//!
//! Usage:
//!   cargo run --example basic_streaming
//!
//! Set environment variable:
//!   export OPENAI_API_KEY=sk-...

use forge_ai_stream::{
    AIConnector, AIConnectorConfig, AIConnectorType, AIEvent, OpenAIConnector,
};
use forge_core::SecureString;
use std::env;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Get API key from environment
    let api_key = env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY environment variable not set");

    // Configure the AI connector
    let config = AIConnectorConfig {
        connector_type: AIConnectorType::OpenAI,
        api_key: SecureString::new(api_key),
        endpoint: None, // Use default OpenAI endpoint
        model: "gpt-4o-realtime-preview".to_string(),
        voice: Some("alloy".to_string()),
        temperature: Some(0.8),
        max_tokens: Some(4096),
        instructions: Some("You are a helpful voice assistant. Be concise.".to_string()),
        tools: vec![],
        enable_vad: true,
        enable_barge_in: true,
        connect_timeout: Duration::from_secs(30),
        request_timeout: Duration::from_secs(60),
    };

    println!("Connecting to OpenAI Realtime API...");
    let mut connector = OpenAIConnector::new(config).await?;

    // Connect to the API
    let session_id = connector.connect().await?;
    println!("Connected! Session ID: {}", session_id);

    // Simulate sending audio (in a real app, this would come from microphone)
    // Generate 1 second of audio at 24kHz (OpenAI's native rate)
    let audio_samples: Vec<i16> = (0..24000)
        .map(|i| ((i as f32 * 0.01).sin() * 1000.0) as i16)
        .collect();

    println!("Sending audio samples...");
    connector.send_audio(&audio_samples, 24000).await?;

    // Receive events for 10 seconds
    println!("Listening for AI responses...");
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(10);

    while start.elapsed() < timeout {
        // Check for events with a timeout
        match tokio::time::timeout(Duration::from_millis(100), connector.next_event()).await {
            Ok(Ok(Some(event))) => {
                match event {
                    AIEvent::Connected { session_id } => {
                        println!("✓ Session created: {}", session_id);
                    }
                    AIEvent::AudioResponse { audio_data, sample_rate, .. } => {
                        println!("♪ Received audio: {} samples at {}Hz",
                            audio_data.len(), sample_rate);
                        // In a real app, play this audio through speakers
                    }
                    AIEvent::Transcript { segment } => {
                        println!("💬 Transcript ({}): {}",
                            if segment.is_final { "final" } else { "partial" },
                            segment.text);
                    }
                    AIEvent::VadStateChange { is_speech, confidence } => {
                        println!("🎤 VAD: {} (confidence: {:.2})",
                            if is_speech { "SPEECH" } else { "SILENCE" },
                            confidence);
                    }
                    AIEvent::Error { message, code } => {
                        eprintln!("❌ Error: {} (code: {:?})", message, code);
                    }
                    _ => {
                        println!("📨 Event: {:?}", event);
                    }
                }
            }
            Ok(Ok(None)) => {
                // No event available
            }
            Ok(Err(e)) => {
                eprintln!("Error receiving event: {}", e);
                break;
            }
            Err(_) => {
                // Timeout - continue waiting
            }
        }
    }

    // Disconnect
    println!("\nDisconnecting...");
    connector.disconnect().await?;

    // Print statistics
    let stats = connector.stats();
    println!("\nSession Statistics:");
    println!("  Samples sent: {}", stats.samples_sent);
    println!("  Samples received: {}", stats.samples_received);
    println!("  Events sent: {}", stats.events_sent);

    Ok(())
}
