# Forge Media Engine

<div align="center">

**High-Performance RTP and WebRTC Media Engine for Real-Time Communications**

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

*Part of the [Ferrous Communications Platform (FCP)](https://github.com/ferrous-comms)*

</div>

---

## 🔨 What is Forge?

Forge is a carrier-grade media server built in Rust that handles all media processing for real-time communications. It works alongside the [Siphon](https://github.com/ferrous-comms/siphon) SIP stack to provide comprehensive VoIP capabilities.

**Forge is both:**
- **📚 A Library**: Use in your Rust projects (FCP, custom applications)
- **🚀 A Binary**: Run as a standalone media server

### Key Features

- **🎵 Audio Processing**: G.711, G.722, G.729, Opus codec support with transcoding
- **📞 RTP/SRTP**: Full RFC-compliant RTP handling with SRTP encryption
- **🌐 WebRTC**: ICE, DTLS, SRTP for browser-based communications
- **👥 Conferencing**: Audio mixing, VAD, AGC, dominant speaker detection
- **📼 Recording**: Multi-format recording with multiple storage backends
- **🤖 AI Integration**: Real-time streaming to OpenAI, Dialogflow, Lex, Azure
- **🔐 Carrier-Grade**: SBC features, SIPREC, CAC, DoS protection, high availability
- **⚡ Performance**: Async Rust, zero-copy parsing, optional kernel offload

---

## 🏗️ Project Status

**Current Phase**: Phase 0 - Foundation ✅

The project structure is established and core types are defined. See [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md) for detailed roadmap.

### What's Working
- ✅ Project structure and workspace
- ✅ Core types and configuration system
- ✅ Basic RTP packet parsing
- 🚧 Session management (in progress)

### Coming Soon
- 🔜 RTP forwarding
- 🔜 Codec transcoding
- 🔜 Audio conferencing
- 🔜 Recording system

---

## 🚀 Quick Start

### Prerequisites

- Rust 1.75 or later
- C compiler (for native dependencies)
- OpenSSL development libraries

```bash
# Ubuntu/Debian
sudo apt-get install build-essential libssl-dev pkg-config

# macOS
brew install openssl pkg-config

# Fedora/RHEL
sudo dnf install gcc openssl-devel pkg-config
```

### Build

```bash
# Clone the repository
git clone https://github.com/ferrous-comms/forge-media
cd forge-media

# Build all crates
cargo build

# Build with all features
cargo build --features full

# Build release version
cargo build --release
```

### Run as Binary

```bash
# Run with default configuration
cargo run

# Run with custom config
cargo run -- --config /path/to/config.toml

# Run with debug logging
RUST_LOG=forge=debug cargo run
```

### Use as Library

Add to your `Cargo.toml`:

```toml
[dependencies]
forge-media = { path = "../forge-media" }
# Or from git:
# forge-media = { git = "https://github.com/ferrous-comms/forge-media" }

# Optional: Choose features
forge-media = { path = "../forge-media", features = ["full"] }
```

Then in your code:

```rust
use forge_media::{ForgeEngine, ForgeConfig, CallId};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create engine with default config
    let config = ForgeConfig::default();
    let engine = ForgeEngine::new(config).await?;

    // Use the engine in your application
    // let session = engine.create_session(...).await?;

    Ok(())
}
```

### Configuration

Copy the example configuration and customize:

```bash
cp config/forge.toml /etc/forge/config.toml
# Edit /etc/forge/config.toml
```

See [config/forge.toml](config/forge.toml) for all configuration options.

---

## 📚 Architecture

Forge follows a modular, layered architecture:

```
┌─────────────────────────────────────────────────────────┐
│                    Control Plane                         │
│  HTTP REST API │ WebSocket Events │ ng Protocol         │
└──────────────────────┬──────────────────────────────────┘
                       │
┌──────────────────────┴──────────────────────────────────┐
│                    Media Plane                           │
│  Sessions │ Conferencing │ Transcoding │ Recording      │
│  RTP/RTCP │ SRTP │ Jitter Buffer │ DTMF                 │
└──────────────────────┬──────────────────────────────────┘
                       │
┌──────────────────────┴──────────────────────────────────┐
│              Kernel Offload (Optional)                   │
│  xt_RTPENGINE │ eBPF/XDP │ Userspace Fallback          │
└─────────────────────────────────────────────────────────┘
```

### Crate Structure

```
forge-media/
├── forge-core                  # Common types, traits, utilities
├── forge-rtp                   # RTP/RTCP/SRTP implementation
├── forge-engine                # Core engine and session management
├── forge-codecs                # Audio codec implementations (G.711, Opus, etc.)
├── forge-resampler             # Audio sample rate conversion
├── forge-transcoder            # Audio transcoding pipeline
├── forge-storage               # Recording storage management
├── forge-recorder              # Audio recording (WAV, Opus)
├── forge-mixer                 # Multi-party audio mixing
├── forge-conference-processor  # Conference bridge management
├── forge-recording             # Recording system
├── forge-dtmf                  # DTMF detection and generation
├── forge-transcription         # Real-time transcription
├── forge-injection             # Audio injection and TTS
├── forge-webrtc                # WebRTC support
├── forge-sdp                   # SDP parsing and generation
├── forge-sbc                   # SBC features
├── forge-siprec                # SIPREC (RFC 7865/7866)
├── forge-ai-stream             # AI streaming integration
├── forge-ha                    # High availability
└── forge-api                   # HTTP/WebSocket API
```

See [FORGE ARCHITECTURE.md](FORGE%20ARCHITECTURE.md) for detailed design.

---

## 🔌 API Reference

### REST API

#### Sessions
```bash
# Create session
POST /v1/sessions
{
  "call_id": "call-123",
  "sdp": "v=0\r\no=- ..."
}

# Get session
GET /v1/sessions/:call_id

# Delete session
DELETE /v1/sessions/:call_id
```

#### Conferences
```bash
# Create conference
POST /v1/conferences
{
  "room_id": "room-456",
  "max_participants": 100
}

# Add participant
POST /v1/conferences/:room_id/participants
```

#### Recording
```bash
# Start recording
POST /v1/recordings
{
  "target": "session_leg",
  "call_id": "call-123",
  "format": "opus"
}
```

See [API Documentation](docs/API.md) for complete reference.

---

## 🧪 Testing

```bash
# Run all tests
cargo test

# Run with logging
RUST_LOG=debug cargo test -- --nocapture

# Run integration tests only
cargo test --test '*'

# Run benchmarks
cargo bench
```

---

## 📈 Performance

Forge is designed for carrier-grade performance:

- **1,000+** concurrent sessions per instance
- **<1ms** packet forwarding latency (p99)
- **<20ms** conference mixing latency
- **100,000+** packets per second

See [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md#performance-targets) for detailed targets.

---

## 🤝 Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Development Setup

```bash
# Install development tools
cargo install cargo-watch cargo-edit cargo-nextest

# Run tests on file change
cargo watch -x test

# Format code
cargo fmt

# Lint code
cargo clippy -- -D warnings
```

---

## 📄 Documentation

- [Development Plan](DEVELOPMENT_PLAN.md) - Phased roadmap and strategy
- [Architecture](FORGE%20ARCHITECTURE.md) - Detailed technical design
- [Feature Specifications](FORGE%20NEW%20FEATURES.MD) - New feature designs
- [Enhancement Recommendations](FORGE%20ENHANCEMENTS.md) - Future improvements
- [API Reference](docs/API.md) - HTTP API documentation
- [Claude Guide](CLAUDE.MD) - Developer quick reference

---

## 🔗 Related Projects

- **[Siphon](https://github.com/ferrous-comms/siphon)** - SIP stack for signaling
- **Ferrous Communications Platform** - Complete UC platform

---

## 📝 License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

---

## 🙏 Acknowledgments

Forge is built with these excellent Rust crates:

- [Tokio](https://tokio.rs/) - Async runtime
- [Axum](https://github.com/tokio-rs/axum) - Web framework
- [Opus](https://opus-codec.org/) - Audio codec
- [RustCrypto](https://github.com/RustCrypto) - Cryptographic algorithms

---

<div align="center">

**🔨 Forging Connections, One Stream at a Time**

[Report Bug](https://github.com/ferrous-comms/forge-media/issues) ·
[Request Feature](https://github.com/ferrous-comms/forge-media/issues) ·
[Documentation](docs/)

</div>
