# AI Integration Guide

Forge Media provides seamless integration with real-time AI services like OpenAI's Realtime API, enabling voice agents, IVR systems, and AI-powered call features.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Quick Start](#quick-start)
- [API Reference](#api-reference)
- [Configuration](#configuration)
- [Audio Routing](#audio-routing)
- [DTMF Integration](#dtmf-integration)
- [SIPREC Recording](#siprec-recording)
- [Examples](#examples)
- [Troubleshooting](#troubleshooting)

---

## Overview

The AI integration enables:

- **Bidirectional Audio**: RTP → AI and AI → RTP with automatic codec conversion
- **DTMF Detection**: Sends DTMF events to AI for IVR scenarios
- **Session Management**: Full lifecycle control via REST API
- **Recording**: SIPREC support with AI metadata for compliance
- **Multiple Codecs**: G.711 µ-law/A-law, Opus with automatic transcoding
- **Sample Rate Conversion**: Automatic resampling between 8kHz/16kHz/24kHz

### Supported AI Providers

- **OpenAI Realtime API** - WebSocket-based voice conversations
- Extensible architecture for additional providers

---

## Architecture

```
┌──────────────┐          ┌──────────────┐          ┌──────────────┐
│   WebRTC/    │  Audio   │    Forge     │  Audio   │   OpenAI     │
│   SIP Client │ ◄─────► │    Media     │ ◄─────► │  Realtime    │
│              │   RTP    │    Engine    │   WS    │     API      │
└──────┬───────┘          └──────┬───────┘          └──────────────┘
       │                         │
       │ DTMF                    │ Events
       └────────────────────────►│
```

### Components

- **`forge-ai-stream`** - AI provider connectors (OpenAI, extensible)
- **`forge-engine/ai_integration.rs`** - Session management and audio routing
- **`forge-api/routes/ai.rs`** - REST API endpoints
- **`forge-siprec/metadata.rs`** - AI recording metadata

---

## Quick Start

### 1. Prerequisites

```bash
# Set OpenAI API key
export OPENAI_API_KEY="your-api-key-here"
```

### 2. Start Forge Media Server

```bash
cargo run --release
```

### 3. Create a Media Session

```bash
curl -X POST http://localhost:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "call-001",
    "sdp": "v=0\r\no=- 0 0 IN IP4 192.168.1.100\r\n..."
  }'
```

### 4. Attach AI to Session

```bash
curl -X POST http://localhost:8080/v1/sessions/call-001/ai \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "gpt-4o-realtime-preview-2024-12-17",
    "voice": "alloy",
    "instructions": "You are a helpful customer service agent.",
    "temperature": 0.8,
    "turn_detection": {
      "type": "server_vad",
      "threshold": 0.5,
      "prefix_padding_ms": 300,
      "silence_duration_ms": 500
    }
  }'
```

### 5. Check AI Status

```bash
curl http://localhost:8080/v1/sessions/call-001/ai
```

### 6. Detach AI

```bash
curl -X DELETE http://localhost:8080/v1/sessions/call-001/ai
```

---

## API Reference

### Attach AI to Session

**Endpoint**: `POST /v1/sessions/:call_id/ai`

**Request Body**:
```json
{
  "provider": "openai",
  "model": "gpt-4o-realtime-preview-2024-12-17",
  "voice": "alloy",
  "instructions": "You are a helpful assistant.",
  "temperature": 0.8,
  "max_response_output_tokens": 4096,
  "modalities": ["text", "audio"],
  "turn_detection": {
    "type": "server_vad",
    "threshold": 0.5,
    "prefix_padding_ms": 300,
    "silence_duration_ms": 500
  },
  "tools": [
    {
      "type": "function",
      "name": "get_weather",
      "description": "Get current weather for a location",
      "parameters": {
        "type": "object",
        "properties": {
          "location": {
            "type": "string",
            "description": "City name"
          }
        },
        "required": ["location"]
      }
    }
  ],
  "input_audio_format": "pcm16",
  "output_audio_format": "pcm16",
  "input_audio_transcription": {
    "model": "whisper-1"
  }
}
```

**Response**: `201 Created`
```json
{
  "call_id": "call-001",
  "state": "connected",
  "config": { /* configuration */ },
  "stats": {
    "audio_samples_sent": 0,
    "audio_chunks_received": 0,
    "events_sent": 0,
    "events_received": 0,
    "started_at": "2025-12-15T10:30:00Z"
  }
}
```

### Get AI Status

**Endpoint**: `GET /v1/sessions/:call_id/ai`

**Response**: `200 OK`
```json
{
  "call_id": "call-001",
  "state": "connected",
  "config": { /* configuration */ },
  "stats": {
    "audio_samples_sent": 48000,
    "audio_chunks_received": 24,
    "events_sent": 15,
    "events_received": 12,
    "started_at": "2025-12-15T10:30:00Z"
  }
}
```

### Detach AI from Session

**Endpoint**: `DELETE /v1/sessions/:call_id/ai`

**Response**: `204 No Content`

### Send Function Response

**Endpoint**: `POST /v1/sessions/:call_id/ai/function-response`

**Request Body**:
```json
{
  "call_id": "function-call-001",
  "output": "{\"temperature\": 72, \"conditions\": \"sunny\"}"
}
```

**Response**: `200 OK`

---

## Configuration

### OpenAI Session Config

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `provider` | string | `"openai"` | AI provider name |
| `model` | string | **required** | Model name (e.g., `gpt-4o-realtime-preview-2024-12-17`) |
| `voice` | string | `"alloy"` | Voice name (`alloy`, `shimmer`, `echo`) |
| `instructions` | string | `""` | System instructions for the AI |
| `temperature` | number | `0.8` | Creativity (0.0-1.0) |
| `max_response_output_tokens` | number | `4096` | Max tokens per response |
| `modalities` | array | `["text", "audio"]` | Enabled modalities |
| `turn_detection` | object | `null` | Voice activity detection settings |
| `tools` | array | `[]` | Function calling tools |
| `input_audio_format` | string | `"pcm16"` | Input format (`pcm16`, `g711_ulaw`, `g711_alaw`) |
| `output_audio_format` | string | `"pcm16"` | Output format (`pcm16`, `g711_ulaw`, `g711_alaw`) |
| `input_audio_transcription` | object | `null` | Transcription config |

### Turn Detection (VAD)

```json
{
  "type": "server_vad",
  "threshold": 0.5,
  "prefix_padding_ms": 300,
  "silence_duration_ms": 500
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | `"server_vad"` for OpenAI's VAD |
| `threshold` | number | Activation threshold (0.0-1.0) |
| `prefix_padding_ms` | number | Audio to include before speech |
| `silence_duration_ms` | number | Silence duration to detect end of speech |

### Function Calling

```json
{
  "tools": [
    {
      "type": "function",
      "name": "transfer_call",
      "description": "Transfer call to another department",
      "parameters": {
        "type": "object",
        "properties": {
          "department": {
            "type": "string",
            "enum": ["sales", "support", "billing"]
          },
          "reason": {
            "type": "string"
          }
        },
        "required": ["department"]
      }
    }
  ]
}
```

When the AI calls a function, your application receives an event via EventBus. Respond using the function-response endpoint.

---

## Audio Routing

### RTP → AI Audio Flow

1. **Packet Reception**: RTP packets received from participants
2. **Codec Decoding**: G.711/Opus decoded to PCM16
3. **Audio Tap**: PCM samples tapped for AI (non-blocking)
4. **Sample Rate Conversion**: Resampled to AI's expected rate (typically 24kHz)
5. **AI Transmission**: Sent via WebSocket to OpenAI

**Implementation**: `forge-engine/src/forwarding.rs:238-246`

```rust
// Audio tap for AI integration
if let Some(ai_manager) = session.ai_manager().await {
    if ai_manager.has_ai(call_id) {
        if let Err(e) = ai_manager.send_audio(call_id, &pcm_samples).await {
            tracing::debug!("Failed to send audio to AI: {}", e);
        }
    }
}
```

### AI → RTP Audio Flow

1. **AI Response**: AI generates audio (PCM16 at 24kHz)
2. **Sample Rate Conversion**: Resampled to participant's codec rate
3. **Codec Encoding**: Encoded to G.711/Opus based on SDP
4. **RTP Packetization**: Split into RTP packets with proper timing
5. **Packet Transmission**: Sent to both participants

**Implementation**: `forge-engine/src/forwarding.rs:692-855`

**Special AI SSRC**: `0xA1A1A1A1` - Used for all AI-generated RTP packets

### Supported Codecs

| Codec | Sample Rate | Channels | Notes |
|-------|-------------|----------|-------|
| G.711 µ-law | 8 kHz | 1 | PCMU (payload type 0) |
| G.711 A-law | 8 kHz | 1 | PCMA (payload type 8) |
| Opus | 48 kHz | 1-2 | Dynamic payload type |

**Automatic Transcoding**: Forge automatically converts between AI format (typically PCM16 @ 24kHz) and participant codec/rate.

---

## DTMF Integration

DTMF events are automatically forwarded to AI sessions, enabling IVR scenarios.

### Detection Methods

- **RFC 2833** (telephone-event) - Out-of-band, most reliable
- **Inband** (Goertzel algorithm) - Detected from audio stream
- **SIP INFO** - Via SIP signaling

### Event Flow

```
User Presses Key → DTMF Detector → EventBus → AI Session → OpenAI API
                                                              ↓
                                          AI receives: "[DTMF: User pressed '5' via rfc2833]"
```

### Example

User presses `5` on their phone:

**Event Published to EventBus**:
```rust
DtmfEvent {
    call_id: CallId("call-001"),
    participant_id: ParticipantId("participant-a"),
    digit: '5',
    kind: DtmfEventKind::End,
    detection_method: DtmfDetectionMethod::Rfc2833,
    timestamp: Instant::now(),
}
```

**Sent to AI**:
```json
{
  "type": "conversation.item.create",
  "item": {
    "type": "message",
    "role": "user",
    "content": [{
      "type": "input_text",
      "text": "[DTMF: User pressed '5' via rfc2833]"
    }]
  }
}
```

**AI Response** (example):
> "I see you pressed 5. Let me transfer you to the billing department."

**Implementation**: `forge-engine/src/ai_integration.rs:299-334`

---

## SIPREC Recording

Record AI conversations with compliance metadata.

### Adding AI Metadata

```rust
use forge_siprec::SiprecMetadata;

let mut metadata = SiprecMetadata::new("session-001");

// Add AI metadata
metadata.add_ai_metadata(
    "openai",
    "gpt-4o-realtime-preview-2024-12-17",
    Some("alloy")
);

// Add AI as participant
metadata.add_ai_participant("OpenAI Assistant", "openai");
```

### Generated XML

```xml
<recording xmlns="urn:ietf:params:xml:ns:recording:1">
  <session session_id="session-001">
    <extensiondata>
      <extension>
        <name>ai-provider</name>
        <value>openai</value>
      </extension>
      <extension>
        <name>ai-model</name>
        <value>gpt-4o-realtime-preview-2024-12-17</value>
      </extension>
      <extension>
        <name>ai-voice</name>
        <value>alloy</value>
      </extension>
      <extension>
        <name>ai-enabled</name>
        <value>true</value>
      </extension>
    </extensiondata>
    <participant participant_id="ai-participant-2">
      <nameID aor="sip:ai@openai.local">
        <name xml:lang="en">OpenAI Assistant</name>
      </nameID>
    </participant>
  </session>
</recording>
```

---

## Examples

### Example 1: Basic Voice Agent

```bash
#!/bin/bash

# Start a session
curl -X POST http://localhost:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "support-001",
    "sdp": "v=0\r\n..."
  }'

# Attach AI
curl -X POST http://localhost:8080/v1/sessions/support-001/ai \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "gpt-4o-realtime-preview-2024-12-17",
    "voice": "alloy",
    "instructions": "You are a friendly customer support agent. Help users with their questions and escalate to human agents when needed."
  }'
```

### Example 2: IVR with DTMF

```bash
curl -X POST http://localhost:8080/v1/sessions/ivr-001/ai \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "gpt-4o-realtime-preview-2024-12-17",
    "voice": "alloy",
    "instructions": "You are an IVR system. Ask the user to press 1 for sales, 2 for support, or 3 for billing. When you receive a DTMF event, respond accordingly and transfer the call.",
    "temperature": 0.3
  }'
```

### Example 3: Function Calling for Call Transfer

```bash
curl -X POST http://localhost:8080/v1/sessions/call-001/ai \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "gpt-4o-realtime-preview-2024-12-17",
    "voice": "alloy",
    "instructions": "You can transfer calls to different departments using the transfer_call function.",
    "tools": [
      {
        "type": "function",
        "name": "transfer_call",
        "description": "Transfer the call to another department",
        "parameters": {
          "type": "object",
          "properties": {
            "department": {
              "type": "string",
              "enum": ["sales", "support", "billing"]
            }
          },
          "required": ["department"]
        }
      }
    ]
  }'

# When AI calls transfer_call, send response:
curl -X POST http://localhost:8080/v1/sessions/call-001/ai/function-response \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "fc-123",
    "output": "{\"success\": true, \"transferred_to\": \"sales\"}"
  }'
```

### Example 4: Recording AI Session

```bash
# Start recording with AI metadata
curl -X POST http://localhost:8080/v1/recordings \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "call-001",
    "format": "opus",
    "metadata": {
      "ai_enabled": true,
      "ai_provider": "openai",
      "ai_model": "gpt-4o-realtime-preview-2024-12-17",
      "ai_voice": "alloy"
    }
  }'
```

### Example 5: Rust Application Integration

```rust
use forge_engine::ai_integration::{AISessionManager, AISessionConfig};
use forge_ai_stream::openai::OpenAIConfig;
use forge_core::CallId;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create AI session manager
    let ai_manager = AISessionManager::new();

    // Configure OpenAI session
    let config = AISessionConfig {
        provider: "openai".to_string(),
        model: "gpt-4o-realtime-preview-2024-12-17".to_string(),
        voice: Some("alloy".to_string()),
        instructions: Some("You are a helpful assistant.".to_string()),
        temperature: Some(0.8),
        ..Default::default()
    };

    // Attach AI to call
    let call_id = CallId::from("call-001");
    ai_manager.attach_ai(call_id.clone(), config, None).await?;

    // Send audio (PCM16 samples)
    let samples: Vec<i16> = vec![/* audio data */];
    ai_manager.send_audio(&call_id, &samples).await?;

    // Get session info
    if let Some(info) = ai_manager.get_session_info(&call_id).await {
        println!("AI State: {:?}", info.state);
        println!("Stats: {:?}", info.stats);
    }

    // Detach when done
    ai_manager.detach_ai(&call_id).await?;

    Ok(())
}
```

---

## Troubleshooting

### AI Not Responding

**Check Connection Status**:
```bash
curl http://localhost:8080/v1/sessions/call-001/ai
```

Look for `"state": "connected"`. If disconnected, check logs for WebSocket errors.

**Common Issues**:
- Invalid OpenAI API key (`export OPENAI_API_KEY`)
- Network connectivity to OpenAI API
- Invalid model name

### No Audio from AI

**Check Stats**:
```bash
curl http://localhost:8080/v1/sessions/call-001/ai | jq '.stats'
```

Verify:
- `audio_samples_sent > 0` (RTP → AI working)
- `audio_chunks_received > 0` (AI → RTP working)
- `events_received > 0` (WebSocket communication working)

**Common Issues**:
- Codec negotiation failed - check SDP
- Sample rate mismatch - Forge automatically converts
- AI not generating audio - check `modalities` includes `"audio"`

### DTMF Not Detected

**Check DTMF Configuration**:
- Ensure RFC 2833 is enabled in SDP
- Check for `telephone-event` payload type
- Verify DTMF events in logs: `RUST_LOG=forge=debug`

### High Latency

**Optimization Tips**:
- Use G.711 codecs for lowest latency (avoid transcoding)
- Enable OpenAI's server VAD for faster turn detection
- Reduce `silence_duration_ms` in turn detection
- Use lower `temperature` for faster responses

### Function Calling Not Working

**Check Tool Definition**:
- Ensure JSON schema is valid
- Required parameters must be specified
- Descriptions should be clear and specific

**Monitor Events**:
Subscribe to EventBus to see function call events:
```rust
let mut rx = session.event_bus().subscribe();
while let Ok(event) = rx.recv().await {
    println!("Event: {:?}", event);
}
```

---

## Performance Considerations

### Resource Usage

Per AI session:
- **CPU**: ~2-5% (audio processing + WebSocket)
- **Memory**: ~10-20 MB (buffers + state)
- **Network**: ~24-32 Kbps (PCM16 @ 24kHz)

### Scaling

- **Horizontal**: Run multiple Forge instances, load balance via DNS/SIP proxy
- **Vertical**: 1000+ concurrent AI sessions per 32-core server

### Latency Budget

| Component | Latency | Notes |
|-----------|---------|-------|
| RTP Reception | <1ms | Network jitter buffer |
| Codec Decoding | <1ms | G.711 is trivial |
| Audio Tap | <0.1ms | Non-blocking channel send |
| WebSocket Send | 10-50ms | Network RTT to OpenAI |
| AI Processing | 200-500ms | OpenAI response time |
| Audio Generation | 50-200ms | Depends on response length |
| **Total (p50)** | **300-800ms** | Acceptable for voice |

---

## Security

### API Key Management

**Best Practices**:
- Store API keys in environment variables
- Use secrets management (Vault, AWS Secrets Manager)
- Rotate keys regularly
- Never commit keys to version control

### Network Security

- Use HTTPS for API endpoints (configure TLS in `axum-server`)
- Use SRTP for media encryption
- Whitelist OpenAI API endpoints in firewall

### Data Privacy

- AI receives only audio from the call
- DTMF events sent as text markers
- Configure transcription settings based on compliance requirements
- Use SIPREC recording for audit trails

---

## Advanced Topics

### Custom AI Providers

Implement the `AIConnector` trait:

```rust
use forge_ai_stream::{AIConnector, AIEvent, AIStreamError};

pub struct CustomAIConnector {
    // Your implementation
}

#[async_trait::async_trait]
impl AIConnector for CustomAIConnector {
    async fn connect(&mut self) -> Result<(), AIStreamError> {
        // Connect to your AI service
    }

    async fn send_audio(&mut self, samples: &[i16], sample_rate: u32) -> Result<(), AIStreamError> {
        // Send audio to your AI
    }

    async fn receive_event(&mut self) -> Result<AIEvent, AIStreamError> {
        // Receive events from your AI
    }

    // ... implement remaining methods
}
```

### EventBus Integration

Subscribe to AI events in your application:

```rust
use forge_core::{Event, EventBus};

let event_bus = Arc::new(EventBus::new());
let mut rx = event_bus.subscribe();

tokio::spawn(async move {
    while let Ok(event) = rx.recv().await {
        match event {
            Event::AiTranscription { call_id, text, .. } => {
                println!("AI heard: {}", text);
            }
            Event::AiFunctionCall { call_id, name, arguments, .. } => {
                println!("AI called function: {}", name);
                // Handle function call, send response
            }
            _ => {}
        }
    }
});
```

### Audio Quality Tuning

```json
{
  "turn_detection": {
    "threshold": 0.7,           // Higher = less sensitive (fewer false positives)
    "prefix_padding_ms": 500,   // More context before speech
    "silence_duration_ms": 300  // Faster interruption detection
  },
  "temperature": 0.6            // Lower = more consistent responses
}
```

---

## References

- [OpenAI Realtime API Documentation](https://platform.openai.com/docs/guides/realtime)
- [RFC 7865 - SIPREC Architecture](https://tools.ietf.org/html/rfc7865)
- [RFC 2833 - DTMF](https://tools.ietf.org/html/rfc2833)
- [Forge DTMF Integration Guide](./DTMF_INTEGRATION.md)
- [Forge API Reference](./API.md)

---

## Support

- [GitHub Issues](https://github.com/ferrous-comms/forge-media/issues)
- [Documentation](https://github.com/ferrous-comms/forge-media/tree/main/docs)
- [Examples](../examples/)

