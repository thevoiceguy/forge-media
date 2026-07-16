# Forge Media Engine

## Project Identity

**Name:** Forge  
**Full Name:** forge-media  
**Part of:** Ferrous Communications Platform (FCP)  
**Companion:** Siphon (SIP stack)  
**Language:** Rust  

## What is Forge?

Forge is a high-performance RTP and WebRTC media engine for real-time communications. It handles all media processing for the Ferrous Communications Platform, working alongside the Siphon SIP stack.  

## Core Capabilities

- **RTP/SRTP**: Packet handling, encryption, jitter buffering
- **WebRTC**: Full ICE/DTLS/SRTP stack with signaling server
- **Transcoding**: G.711, G.722, Opus, G.729 codec conversion
- **Conferencing**: Audio mixing, VAD, AGC, dominant speaker
- **Recording**: Multi-format, multi-storage backend, audio and video recording
- **SIPREC**: RFC 7865/7866 SRC and SRS roles
- **DTMF**: RFC 2833 and in-band detection/generation
- **Voice Activity Detection**: energy+ZCR (default) or neural Silero backend (`neural-vad` feature, local tract-onnx inference) driving `SpeechStarted`/`SpeechStopped` — see docs/NEURAL_VAD.md
- **Transcription**: Real-time STT (Deepgram, Google, AWS, Azure)
- **AI Streaming**: OpenAI Realtime, Dialogflow CX, Amazon Lex, Azure
- **Audio Injection**: File playback, TTS, tone generation
- **Kernel Offload**: netfilter/eBPF for low-latency forwarding
- **SBC/Carrier-Grade**: Media proxy, CAC, DoS protection
- **High Availability**: Session replication, VIP failover, clustering
- **Video Support**: Video Codecs, Video Transcoding, Video Conferenceing, Video         Recording
- **Advanced RTCP Feedback & Congestion Control**: 
- **Quality of Experience (QoE) Monitoring**: Metrics, MOS scores, packet loss, jitter, latency 
- **Echo Cancellation**: Echo cancellation, noise reduction, 
- **Observability & Metrics**:
- **Security & Compliance**: Lawful intercept, Stir/Shaken



## Project Structure

```
forge-media/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── forge-core/         # Common types, traits, config
│   ├── forge-rtp/          # RTP/RTCP/SRTP
│   ├── forge-engine/       # Core engine, sessions, sockets
│   ├── forge-transcoding/  # Codec implementations
│   ├── forge-kernel/       # Kernel offload (netfilter/eBPF)
│   ├── forge-conference/   # Audio conferencing
│   ├── forge-recording/    # Recording system
│   ├── forge-dtmf/         # DTMF detection/generation
│   ├── forge-transcription/# Real-time transcription
│   ├── forge-injection/    # Audio injection/TTS
│   ├── forge-webrtc/       # WebRTC stack
│   ├── forge-sdp/          # SDP parsing/generation
│   ├── forge-sbc/          # SBC features (proxy, CAC, DoS)
│   ├── forge-siprec/       # SIPREC SRC & SRS (RFC 7865/7866)
│   ├── forge-ai-stream/    # Real-time AI streaming
│   ├── forge-ha/           # High Availability & failover
│   └── forge-api/          # HTTP/WebSocket API
├── src/main.rs             # Binary entry point
└── config/forge.toml       # Default configuration
```

## Key Design Decisions

### Architecture
- **Three-layer design**: Control plane (API) → Media plane (processing) → Kernel (optional offload)
- **Async runtime**: Tokio for all I/O operations
- **Zero-copy where possible**: Parse RTP headers in-place
- **Workspace with crates**: Each major feature is a separate crate

### Concurrency
- `parking_lot` for synchronous locks (faster than std)
- `DashMap` for concurrent hash maps
- `tokio::sync` for async primitives
- Per-stream tasks for packet forwarding

### Memory
- Reuse buffers where possible
- Pre-allocated port pools
- Bounded channels to prevent unbounded growth

### Network
- `socket2` for fine-grained socket control
- TOS/DSCP setting for QoS
- IPv4/IPv6 dual-stack and bridging
- Advertised addresses for NAT traversal

## Naming Conventions

| Concept | Naming |
|---------|--------|
| Crate | `forge-{feature}` |
| Main struct | `{Feature}Manager` or `{Feature}` |
| Config | `{Feature}Config` |
| Error | `{Feature}Error` |
| Event | `{Feature}Event` |
| ID types | `{Thing}Id` (e.g., `CallId`, `RoomId`) |

## API Style

### REST Endpoints
```
/v1/{resource}              # Collection
/v1/{resource}/:id          # Item
/v1/{resource}/:id/{action} # Action
```

### WebSocket
- JSON messages with `type` field for message type
- Events broadcast to subscribers
- Room-based for WebRTC signaling

### Response Format
```json
{
  "status": "ok|error",
  "data": { ... },
  "error": { "code": "...", "message": "..." }
}
```

## Common Patterns

### Manager Pattern
```rust
pub struct RecordingManager {
    config: Arc<RecordingManagerConfig>,
    recordings: RwLock<HashMap<RecordingId, Arc<Recording>>>,
    event_tx: broadcast::Sender<RecordingEvent>,
}

impl RecordingManager {
    pub fn new(config: RecordingManagerConfig) -> Self { ... }
    pub async fn start(&self, request: StartRequest) -> Result<Info> { ... }
    pub async fn stop(&self, id: &RecordingId) -> Result<Summary> { ... }
    pub fn subscribe(&self) -> broadcast::Receiver<RecordingEvent> { ... }
}
```

### ID Types
```rust
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallId(pub String);

impl CallId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}
```

### Error Handling
```rust
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("Session not found: {0}")]
    SessionNotFound(CallId),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Codec error: {0}")]
    Codec(String),
}
```

### Event Broadcasting
```rust
let (event_tx, _) = broadcast::channel(4096);

// Send event
event_tx.send(Event::Started { id }).ok();

// Subscribe
let mut rx = manager.subscribe();
while let Ok(event) = rx.recv().await {
    // Handle event
}
```

## Configuration

Environment variables override config file values:
- `FORGE_PORT_RANGE_START`
- `FORGE_PORT_RANGE_END`
- `DEEPGRAM_API_KEY`
- `GOOGLE_TTS_API_KEY`
- etc.

Config file: `/etc/forge/config.toml` or `./config/forge.toml`

## Testing Strategy

- Unit tests in each module (`#[cfg(test)]`)
- Integration tests in `tests/integration/`
- Benchmarks in `tests/benchmarks/`
- Use `tokio::test` for async tests

## Dependencies to Know

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime |
| `axum` | HTTP framework |
| `socket2` | Low-level socket control |
| `parking_lot` | Fast synchronous locks |
| `dashmap` | Concurrent HashMap |
| `symphonia` | Audio file decoding |
| `opus` | Opus codec |
| `openssl` | DTLS/TLS |
| `libbpf-rs` | eBPF programs |
| `tokio-tungstenite` | WebSocket (AI streaming) |
| `tonic` | gRPC (Dialogflow, etc.) |
| `quick-xml` | XML generation (SIPREC metadata) |
| `base64` | Audio encoding for AI APIs |

## Building

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# With all features
cargo build --release --features full

# Run tests
cargo test

# Run with logging
RUST_LOG=forge=debug cargo run
```

## Quick Reference

### Create a new session
```rust
let session = engine.create_session(CreateSessionRequest {
    call_id: CallId::generate(),
    ..Default::default()
}).await?;
```

### Start recording
```rust
let recording = engine.recording_manager().start_recording(StartRecordingRequest {
    target: RecordingTarget::SessionLeg { call_id, leg: LegIdentifier::LegA },
    format: AudioFormat::Opus,
    ..Default::default()
}).await?;
```

### Play audio
```rust
engine.audio_injector().inject(InjectRequest {
    target: InjectionTarget::SessionLeg { call_id, leg: LegIdentifier::LegA },
    source: AudioSource::File { path: "/path/to/audio.wav".into() },
    config: InjectionConfig::default(),
    ..Default::default()
}).await?;
```

### Create WebRTC connection
```rust
let pc = engine.webrtc_manager().create_connection(WebRtcConfig::default()).await?;
let offer = pc.create_offer(None).await?;
pc.set_local_description(offer.clone()).await?;
// Exchange SDP with remote peer...
pc.set_remote_description(answer).await?;
```

### Create SBC proxy session
```rust
// Create session
let session = engine.media_proxy().create_session(CreateProxySessionRequest {
    call_id: call_id.clone(),
    ..Default::default()
}).await?;

// Add legs
let leg_a = engine.media_proxy().add_leg(&call_id, SbcLeg {
    id: LegId::new("leg-a"),
    tag: from_tag.clone(),
    media_mode: MediaMode::Proxy,
    rtp_flags: RtpFlags { symmetric: true, ..Default::default() },
    codecs: vec![CodecConfig::pcmu()],
    ..Default::default()
}).await?;

let leg_b = engine.media_proxy().add_leg(&call_id, SbcLeg {
    id: LegId::new("leg-b"),
    tag: to_tag.clone(),
    media_mode: MediaMode::Proxy,
    ..Default::default()
}).await?;

// Connect legs (bidirectional)
engine.media_proxy().connect_legs(&call_id, &leg_a.leg_id, &leg_b.leg_id, ConnectionMode::Forward).await?;
engine.media_proxy().connect_legs(&call_id, &leg_b.leg_id, &leg_a.leg_id, ConnectionMode::Forward).await?;
```

### CAC check
```rust
engine.cac().check_admission(&AdmissionRequest {
    call_id: call_id.clone(),
    source_ip: remote_ip,
    tenant: Some("customer-123".into()),
    priority: 5,
    required_bandwidth: 64000,
    num_legs: 2,
})?;
```

### Start SIPREC recording
```rust
let siprec = engine.siprec_client().start_recording(StartSiprecRequest {
    communication_session: CommunicationSession {
        session_id: call_id.to_string(),
        start_time: Utc::now(),
        participants: vec![
            Participant {
                id: "p1".into(),
                aor: "sip:alice@example.com".into(),
                name: Some("Alice".into()),
                role: ParticipantRole::Caller,
                streams: vec!["s1".into()],
            },
            Participant {
                id: "p2".into(),
                aor: "sip:bob@example.com".into(),
                name: Some("Bob".into()),
                role: ParticipantRole::Callee,
                streams: vec!["s2".into()],
            },
        ],
        streams: vec![
            MediaStreamInfo { id: "s1".into(), media_type: MediaType::Audio, .. },
            MediaStreamInfo { id: "s2".into(), media_type: MediaType::Audio, .. },
        ],
        ..Default::default()
    },
    mode: RecordingMode::Full,
    forward_srtp_keys: true,
    ..Default::default()
}).await?;
```

### Start AI streaming session
```rust
let ai_session = engine.ai_stream_manager().start_session(StartAiSessionRequest {
    call_id: call_id.clone(),
    leg: LegIdentifier::LegA,
    config: AiConnectorConfig {
        provider: AiProvider::OpenAi,
        mode: AiStreamMode::Conversational,
        model: Some("gpt-4o-realtime-preview-2024-12-17".into()),
        voice: Some("alloy".into()),
        system_prompt: Some("You are a helpful customer service agent.".into()),
        tools: vec![
            AiTool {
                name: "lookup_order".into(),
                description: "Look up an order by number".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "order_number": { "type": "string" }
                    },
                    "required": ["order_number"]
                }),
            },
        ],
        vad_config: VadConfig { server_vad: true, ..Default::default() },
        barge_in: BargeInConfig { enabled: true, action: BargeInAction::StopImmediately, ..Default::default() },
        ..Default::default()
    },
    ..Default::default()
}).await?;

// Handle AI events
let mut rx = ai_session.subscribe();
while let Ok(event) = rx.recv().await {
    match event {
        AiEvent::ToolCall { call_id, name, arguments, .. } => {
            // Execute tool and return result
            let result = execute_tool(&name, &arguments).await?;
            ai_session.execute_tool(&call_id, result).await?;
        }
        AiEvent::TranscriptDelta { text, is_final, .. } => {
            println!("User said: {} (final: {})", text, is_final);
        }
        _ => {}
    }
}
```

### Stream to Dialogflow
```rust
let ai_session = engine.ai_stream_manager().start_session(StartAiSessionRequest {
    call_id: call_id.clone(),
    leg: LegIdentifier::LegA,
    config: AiConnectorConfig {
        provider: AiProvider::Google,
        mode: AiStreamMode::IntentDetection,
        language: "en-US".into(),
        credentials: AiCredentials {
            project_id: Some("my-project".into()),
            region: Some("us-central1".into()),
            ..Default::default()
        },
        provider_options: [
            ("agent_id".into(), serde_json::json!("my-agent-id")),
        ].into(),
        ..Default::default()
    },
    ..Default::default()
}).await?;
```

## SIPREC Quick Reference

### Start Recording (SRC Role)
```rust
// Fork call media to external SRS
let recording = engine.siprec_client().start_recording(StartSiprecRequest {
    call_id: call_id.clone(),
    participants: vec![
        Participant { aor: "sip:alice@example.com".into(), role: ParticipantRole::Caller, .. },
        Participant { aor: "sip:bob@example.com".into(), role: ParticipantRole::Callee, .. },
    ],
    srs_uri: Some("sip:srs.example.com:5060".into()),
    mode: RecordingMode::Full,
}).await?;
```

### Handle SRS Incoming (SRS Role)
```rust
// When configured as SRS, handle incoming SIPREC INVITEs
fn handle_siprec_invite(invite: &SipRequest) -> SipResponse {
    let (sdp, metadata) = parse_siprec_body(invite)?;
    let session = srs.create_recording_session(sdp, metadata).await?;
    SipResponse::ok().body(session.answer_sdp)
}
```

## High Availability Quick Reference

### Configure HA
```toml
[ha]
enabled = true
node_id = "forge-1"
mode = "active-standby"

[ha.storage]
type = "redis"
url = "redis://redis:6379"

[ha.vip]
address = "192.168.1.200"
interface = "eth0"
```

### Check Cluster Status
```bash
curl http://localhost:8080/v1/ha/status
```

### Force Failover
```bash
curl -X POST http://localhost:8080/v1/ha/failover \
  -H "Content-Type: application/json" \
  -d '{"reason": "maintenance"}'
```

### Drain Node for Maintenance
```bash
curl -X POST http://localhost:8080/v1/ha/drain \
  -H "Content-Type: application/json" \
  -d '{"target_node": "forge-2", "timeout_secs": 300}'
```

## Architecture Document

See `ARCHITECTURE.md` for complete technical design including:
- Detailed module designs
- API specifications
- Data structures
- Protocol details
- Configuration options

## Related Projects

- **Siphon** (`siphon-rs`): SIP stack for signaling
- **FCP Server**: Platform integration layer

---

*Forge - Forging connections, one stream at a time.*
