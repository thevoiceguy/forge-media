//! Function Calling Example
//!
//! Demonstrates how to use function/tool calling with the AI.
//!
//! Usage:
//!   cargo run --example function_calling
//!
//! Set environment variable:
//!   export OPENAI_API_KEY=sk-...

use forge_ai_stream::{
    AIConnector, AIConnectorConfig, AIConnectorType, AIEvent, OpenAIConnector,
};
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::time::Duration;

/// Simulated weather database
fn get_weather(location: &str) -> String {
    let weather_data: HashMap<&str, &str> = [
        ("san francisco", "Foggy, 62°F"),
        ("new york", "Sunny, 75°F"),
        ("london", "Rainy, 55°F"),
        ("tokyo", "Clear, 68°F"),
    ]
    .iter()
    .cloned()
    .collect();

    weather_data
        .get(location.to_lowercase().as_str())
        .unwrap_or(&"Weather data not available")
        .to_string()
}

/// Simulated calendar database
fn get_calendar_events(date: &str) -> String {
    format!("Events for {}: Meeting at 10am, Lunch at 12pm, Review at 3pm", date)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let api_key = env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY environment variable not set");

    // Define tools/functions the AI can call
    let tools = vec![
        forge_ai_stream::events::ToolDefinition {
            tool_type: "function".to_string(),
            name: "get_weather".to_string(),
            description: "Get current weather for a location".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "City name, e.g. San Francisco"
                    }
                },
                "required": ["location"]
            }),
        },
        forge_ai_stream::events::ToolDefinition {
            tool_type: "function".to_string(),
            name: "get_calendar_events".to_string(),
            description: "Get calendar events for a specific date".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "date": {
                        "type": "string",
                        "description": "Date in YYYY-MM-DD format"
                    }
                },
                "required": ["date"]
            }),
        },
    ];

    let config = AIConnectorConfig {
        connector_type: AIConnectorType::OpenAI,
        api_key,
        endpoint: None,
        model: "gpt-4o-realtime-preview".to_string(),
        voice: Some("alloy".to_string()),
        temperature: Some(0.8),
        max_tokens: Some(4096),
        instructions: Some(
            "You are a helpful assistant. Use the available tools to answer questions."
                .to_string(),
        ),
        tools,
        enable_vad: true,
        enable_barge_in: true,
        connect_timeout: Duration::from_secs(30),
        request_timeout: Duration::from_secs(60),
    };

    println!("Connecting with function calling enabled...");
    let mut connector = OpenAIConnector::new(config).await?;
    let session_id = connector.connect().await?;
    println!("Connected! Session ID: {}", session_id);

    // In a real application, you would:
    // 1. Send user audio asking something like "What's the weather in San Francisco?"
    // 2. The AI would call get_weather function
    // 3. You'd return the result
    // 4. The AI would synthesize a response with the weather data

    println!("\nListening for function calls and events...");
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);

    while start.elapsed() < timeout {
        match tokio::time::timeout(Duration::from_millis(100), connector.next_event()).await {
            Ok(Ok(Some(event))) => match event {
                AIEvent::FunctionCall { call } => {
                    println!("\n📞 Function Call Received!");
                    println!("  Call ID: {}", call.call_id);
                    println!("  Function: {}", call.name);
                    println!("  Arguments: {}", call.arguments);

                    // Execute the function
                    let result = match call.name.as_str() {
                        "get_weather" => {
                            let location = call.arguments["location"]
                                .as_str()
                                .unwrap_or("unknown");
                            println!("  → Getting weather for: {}", location);
                            get_weather(location)
                        }
                        "get_calendar_events" => {
                            let date = call.arguments["date"].as_str().unwrap_or("unknown");
                            println!("  → Getting calendar for: {}", date);
                            get_calendar_events(date)
                        }
                        _ => format!("Unknown function: {}", call.name),
                    };

                    println!("  ✓ Result: {}", result);

                    // Send the result back to the AI
                    connector
                        .send_function_response(&call.call_id, result)
                        .await?;
                    println!("  ✓ Response sent to AI");
                }
                AIEvent::Transcript { segment } => {
                    println!("💬 {:?}: {}", segment.role, segment.text);
                }
                AIEvent::AudioResponse { audio_data, .. } => {
                    println!("♪ Audio response: {} samples", audio_data.len());
                }
                AIEvent::Error { message, .. } => {
                    eprintln!("❌ Error: {}", message);
                }
                _ => {}
            },
            Ok(Ok(None)) => {}
            Ok(Err(e)) => {
                eprintln!("Error: {}", e);
                break;
            }
            Err(_) => {}
        }
    }

    connector.disconnect().await?;

    let stats = connector.stats();
    println!("\n📊 Statistics:");
    println!("  Function calls: {}", stats.function_calls);
    println!("  Events sent: {}", stats.events_sent);

    Ok(())
}
