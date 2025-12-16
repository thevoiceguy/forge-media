# Using Forge as a Library

Forge can be used as a library in your Rust projects, making it easy to integrate carrier-grade media processing into your applications.

## Adding Forge to Your Project

### From Local Path

```toml
[dependencies]
forge-media = { path = "../forge-media" }
```

### From Git (when published)

```toml
[dependencies]
forge-media = { git = "https://github.com/ferrous-comms/forge-media" }
```

### With Specific Features

```toml
[dependencies]
forge-media = { path = "../forge-media", features = ["full"] }
```

Available features:
- `transcoding` - Codec transcoding (default)
- `conference` - Audio conferencing (default)
- `recording` - Call recording (default)
- `dtmf` - DTMF detection/generation (default)
- `transcription` - Real-time transcription
- `injection` - Audio injection and TTS
- `webrtc` - WebRTC support
- `siprec` - SIPREC recording
- `ai-stream` - AI streaming
- `ha` - High availability
- `full` - All features

## Basic Usage

```rust
use forge_media::{ForgeEngine, ForgeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create engine with default configuration
    let config = ForgeConfig::default();
    let engine = ForgeEngine::new(config).await?;

    // Engine is ready to use
    println!("Forge engine initialized!");

    Ok(())
}
```

## Custom Configuration

```rust
use forge_media::{
    ForgeEngine,
    ForgeConfig,
    EngineConfig,
    ApiConfig,
    PortRange,
    InterfaceConfig,
};
use std::net::IpAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ForgeConfig {
        engine: EngineConfig {
            port_range: PortRange {
                start: 40000,
                end: 50000,
            },
            interfaces: vec![
                InterfaceConfig {
                    name: "eth0".to_string(),
                    address: "192.168.1.100".parse::<IpAddr>()?,
                    advertised_address: Some("203.0.113.50".parse()?),
                }
            ],
            tos: 0xB8,
            session_timeout_secs: 600,
            ..Default::default()
        },
        api: ApiConfig {
            http_bind: "0.0.0.0:9090".to_string(),
            enable_cors: true,
            ..Default::default()
        },
    };

    let engine = ForgeEngine::new(config).await?;

    Ok(())
}
```

## Integration with FCP

### In your FCP project

```toml
# Cargo.toml
[dependencies]
forge-media = { path = "../forge-media" }
siphon = { path = "../siphon" }
```

```rust
use forge_media::{ForgeEngine, ForgeConfig, CallId};
use siphon::{SipStack, InviteRequest};

pub struct FcpMediaHandler {
    forge: ForgeEngine,
}

impl FcpMediaHandler {
    pub async fn new(config: ForgeConfig) -> Result<Self> {
        let forge = ForgeEngine::new(config).await?;
        Ok(Self { forge })
    }

    pub async fn handle_sip_invite(&self, invite: InviteRequest) -> Result<SdpAnswer> {
        let call_id = CallId::new(invite.call_id());

        // TODO: Create session in Forge
        // let session = self.forge.create_session(call_id, invite.sdp()).await?;

        // Return SDP answer
        // Ok(session.answer_sdp())
        todo!()
    }
}
```

## Using Specific Components

### RTP Packet Handling

```rust
use forge_media::{RtpPacket, RtpHeader};
use bytes::Bytes;

fn process_rtp_packet(data: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
    let packet = RtpPacket::parse(Bytes::from(data))?;

    println!("RTP Version: {}", packet.header.version());
    println!("Payload Type: {}", packet.header.payload_type());
    println!("Sequence: {}", packet.header.sequence_number);
    println!("Timestamp: {}", packet.header.timestamp);
    println!("SSRC: 0x{:08X}", packet.header.ssrc);
    println!("Payload length: {}", packet.payload.len());

    Ok(())
}
```

### Type Safety

Forge uses newtype patterns for type safety:

```rust
use forge_media::{CallId, RoomId, ParticipantId};

// These are distinct types - won't confuse them!
let call_id = CallId::generate();
let room_id = RoomId::generate();
let participant_id = ParticipantId::generate();

// Prevents bugs:
// fn process_call(id: CallId) { }
// process_call(room_id); // ❌ Compile error!
```

## Error Handling

```rust
use forge_media::{ForgeError, Result};

fn handle_media() -> Result<()> {
    // Forge operations return Result<T, ForgeError>
    match some_forge_operation() {
        Ok(result) => {
            println!("Success: {:?}", result);
            Ok(())
        }
        Err(ForgeError::SessionNotFound(id)) => {
            eprintln!("Session not found: {}", id);
            Err(ForgeError::SessionNotFound(id))
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            Err(e)
        }
    }
}
```

## Async/Await

All Forge operations are async:

```rust
use forge_media::{ForgeEngine, ForgeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = ForgeEngine::new(ForgeConfig::default()).await?;

    // All operations are async
    // let session = engine.create_session(...).await?;
    // let recording = engine.start_recording(...).await?;

    Ok(())
}
```

## Thread Safety

Forge types are designed for concurrent use:

```rust
use forge_media::{ForgeEngine, ForgeConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Arc::new(ForgeEngine::new(ForgeConfig::default()).await?);

    // Clone Arc and use from multiple tasks
    let engine1 = engine.clone();
    tokio::spawn(async move {
        // Use engine1 in this task
    });

    let engine2 = engine.clone();
    tokio::spawn(async move {
        // Use engine2 in this task
    });

    Ok(())
}
```

## Examples (Coming Soon)

More examples will be added as features are implemented:

- Creating and managing sessions
- RTP forwarding
- Audio conferencing
- Recording calls
- Transcoding between codecs
- WebRTC integration
- AI streaming

## API Documentation

Generate and view the API documentation:

```bash
cargo doc --open --no-deps
```

Or with all features:

```bash
cargo doc --open --no-deps --features full
```

## See Also

- [README.md](README.md) - Project overview
- [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md) - Development roadmap
- [CLAUDE.MD](CLAUDE.MD) - Developer quick reference
- [CONTRIBUTING.md](CONTRIBUTING.md) - Contribution guidelines
