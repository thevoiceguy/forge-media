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
- **👥 Conferencing**: Audio mixing, VAD, AGC, dominant speaker detection, host controls, capacity management
- **🎙️ Conference Features**: PIN authentication, wait-for-moderator, audio feedback, per-room configuration
- **📼 Recording**: Multi-format recording with multiple storage backends
- **🤖 AI Integration**: Real-time voice AI with OpenAI Realtime API, bidirectional audio, DTMF support
- **🔐 Enterprise Grade**: SIPREC, CAC, DoS protection, high availability
- **⚡ Performance**: Async Rust, zero-copy parsing, optional kernel offload

---

## 🏗️ Project Status

**Current Phase**: Phase 0 - Foundation ✅

The project structure is established and core types are defined. See [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md) for detailed roadmap.

### What's Working
- ✅ Project structure and workspace
- ✅ Core types and configuration system
- ✅ RTP/RTCP/SRTP packet handling
- ✅ Session management with bidirectional audio
- ✅ WebRTC support (ICE, DTLS, SRTP)
- ✅ **Codec Support**: G.711 (µ-law/A-law), G.722 (wideband), G.729 (with VAD/PLC), Opus
- ✅ Audio conferencing with mixing
- ✅ Conference features (PINs, host controls, capacity management)
- ✅ Audio feedback system with WAV playback
- ✅ Recording system (WAV, Opus)
- ✅ AI integration (OpenAI Realtime API, multiple providers)
- ✅ DTMF detection and handling
- ✅ Prometheus metrics and monitoring

### Coming Soon
- 🔜 Advanced transcoding pipelines
- 🔜 SIPREC recording
- 🔜 High availability features

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

## 🎵 Codec Support

Forge supports a comprehensive range of audio codecs for different use cases:

### Codec Comparison Matrix

| Codec | Bit Rate | Sample Rate | Frame Size | Latency | Use Case | Quality |
|-------|----------|-------------|------------|---------|----------|---------|
| **G.711** (µ-law/A-law) | 64 kbps | 8 kHz | 160 samples (20ms) | ~20ms | Legacy PSTN, high compatibility | Toll quality |
| **G.722** | 48-64 kbps | 16 kHz | 320 samples (20ms) | ~20ms | HD Voice, wideband | Wideband |
| **G.729** | 8 kbps | 8 kHz | 80 samples (10ms) | ~25ms | Low bandwidth, mobile | Near toll quality |
| **Opus** | 6-510 kbps | 8-48 kHz | Variable (2.5-60ms) | 2.5-60ms | Internet, WebRTC | Excellent |

### Codec Features

#### G.711 (µ-law/A-law)
- **Status**: ✅ Fully implemented
- **Format**: PCM-based, log-companded
- **Variants**: µ-law (North America, Japan), A-law (Europe, rest of world)
- **Use**: PSTN interoperability, maximum compatibility
- **Pros**: No licensing, minimal CPU, universal support
- **Cons**: High bandwidth, narrowband only

#### G.722
- **Status**: ✅ Fully implemented with ITU-T compliance
- **Format**: Sub-band ADPCM with QMF (Quadrature Mirror Filter)
- **Features**:
  - 64/56/48 kbps modes
  - Auxiliary bit support for data embedding
  - ITU threshold-based quantization with Gray coding
- **Use**: HD Voice on VoIP, conference systems
- **Pros**: Wideband (7 kHz bandwidth), no licensing, excellent quality
- **Cons**: Higher CPU than G.711, fixed 64 kbps default

#### G.729
- **Status**: ✅ Fully implemented via bcg729 FFI
- **Format**: CS-ACELP (Conjugate-Structure Algebraic-Code-Excited Linear-Prediction)
- **Features**:
  - G.729 Annex A (standard 8 kbps)
  - G.729 Annex B (VAD/DTX for bandwidth savings)
  - Packet Loss Concealment (PLC)
  - Length-prefixed framing for variable-length VAD frames
- **Use**: Mobile networks, satellite links, low-bandwidth scenarios
- **Pros**: Very low bandwidth, good quality, patents expired
- **Cons**: Higher CPU than G.711/G.722, narrowband only
- **Requirements**: `libbcg729-dev` package, enable with `--features g729`
- **See**: [G.729 Integration Guide](docs/CODEC_G729_GUIDE.md) for detailed usage

#### Opus
- **Status**: ✅ Fully implemented
- **Format**: Hybrid SILK + CELT
- **Features**:
  - Adaptive bit rate (6-510 kbps)
  - Multiple bandwidth modes (narrowband to fullband)
  - Built-in FEC (Forward Error Correction)
  - Ultra-low latency option
- **Use**: WebRTC, VoIP, music streaming
- **Pros**: Best quality/bandwidth ratio, flexible, low latency, royalty-free
- **Cons**: Higher complexity than legacy codecs

### Feature Flags

Enable specific codecs in your `Cargo.toml`:

```toml
[dependencies]
forge-codecs = { version = "0.2", features = ["g729", "opus"] }

# Or enable all codecs
forge-codecs = { version = "0.2", features = ["all-codecs"] }
```

### Transcoding

Forge automatically transcodes between codecs when needed:
- Conference mixing (multiple codecs → common format → mix → transcode per participant)
- RTP forwarding (codec negotiation mismatch)
- Recording (input codec → target codec for storage)

See the **Coming Soon** section for advanced transcoding pipeline features.

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
├── forge-siprec                # SIPREC (RFC 7865/7866)
├── forge-ai-stream             # AI streaming integration
├── forge-ha                    # High availability
└── forge-api                   # HTTP/WebSocket API
```

See [FORGE ARCHITECTURE.md](FORGE%20ARCHITECTURE.md) for detailed design.

---

## 🤖 AI Integration

Forge provides seamless integration with real-time AI services like OpenAI's Realtime API for voice agents, IVR systems, and AI-powered call features.

### Quick Example

```bash
# Attach AI to an active call
curl -X POST http://localhost:8080/v1/sessions/call-001/ai \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "gpt-4o-realtime-preview-2024-12-17",
    "voice": "alloy",
    "instructions": "You are a helpful customer service agent."
  }'
```

### Features

- **Bidirectional Audio**: Automatic RTP ↔ AI audio routing with codec conversion
- **DTMF Integration**: Forward DTMF events to AI for IVR scenarios
- **Function Calling**: Let AI trigger actions (transfers, lookups, etc.)
- **Recording**: SIPREC support with AI metadata for compliance
- **Multi-Codec**: G.711, Opus with automatic sample rate conversion

See [AI Integration Guide](docs/AI_INTEGRATION.md) for complete documentation.

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
  "room_id": "room-456"
}

# Configure room
POST /v1/conferences/:room_id/configure
{
  "guest_pin": "1234",
  "host_pin": "9999",
  "max_channels": 100,
  "wait_for_moderator": true,
  "require_guest_pin": true
}

# Get room configuration
GET /v1/conferences/:room_id/config

# Add participant
POST /v1/conferences/:room_id/participants
{
  "participant_id": "user-123",
  "is_host": false
}

# List participants with host status
GET /v1/conferences/:room_id/participants

# List waiting participants
GET /v1/conferences/:room_id/waiting

# Promote participant to host
POST /v1/conferences/:room_id/participants/:id/promote
{
  "host_pin": "9999"
}

# Start recording
POST /v1/conferences/:room_id/recording
{
  "output_path": "conference-123.wav"
}
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

#### AI Integration
```bash
# Attach AI to session
POST /v1/sessions/:call_id/ai
{
  "provider": "openai",
  "model": "gpt-4o-realtime-preview-2024-12-17",
  "voice": "alloy",
  "instructions": "You are a helpful assistant."
}

# Get AI status
GET /v1/sessions/:call_id/ai

# Detach AI
DELETE /v1/sessions/:call_id/ai

# Send function response
POST /v1/sessions/:call_id/ai/function-response
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
- [G.729 Codec Integration Guide](docs/CODEC_G729_GUIDE.md) - G.729 installation and usage
- [AI Integration Guide](docs/AI_INTEGRATION.md) - OpenAI Realtime API integration
- [DTMF Integration](docs/DTMF_INTEGRATION.md) - DTMF detection and handling
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
