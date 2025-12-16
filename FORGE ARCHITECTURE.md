# Forge Media Engine

## Architecture Design Document

**Version:** 1.0  
**Date:** December 2024  
**Part of:** Ferrous Communications Platform (FCP)

---

## Table of Contents

1. [Overview](#overview)
2. [Architecture](#architecture)
3. [Project Structure](#project-structure)
4. [Core Components](#core-components)
5. [RTP Core](#rtp-core)
6. [Media Engine](#media-engine)
7. [Session Management](#session-management)
8. [Transcoding](#transcoding)
9. [Kernel Offload](#kernel-offload)
10. [Audio Conferencing](#audio-conferencing)
11. [Recording System](#recording-system)
12. [DTMF System](#dtmf-system)
13. [Real-Time Transcription](#real-time-transcription)
14. [Audio Injection](#audio-injection)
15. [WebRTC Support](#webrtc-support)
16. [SIPREC](#siprec-rfc-78657866)
17. [Real-Time AI Streaming](#real-time-ai-streaming)
18. [Control API](#control-api)
19. [Configuration](#configuration)
20. [Integration with Siphon](#integration-with-siphon)
21. [Dependencies](#dependencies)

---

## Overview

Forge is a high-performance RTP and WebRTC media engine built in Rust, designed to support the Siphon SIP stack and the broader Ferrous Communications Platform. It provides comprehensive media handling capabilities for real-time communications.

### Core Features

| Category | Features |
|----------|----------|
| **Media Transport** | RTP/RTCP, SRTP/SRTCP, WebRTC, DTLS-SRTP |
| **Codecs** | G.711 (μ-law/A-law), G.722, G.729, Opus, Speex, iLBC, AMR, AMR-WB |
| **Network** | IPv4/IPv6 dual-stack, NAT traversal, ICE/STUN/TURN, TOS/QoS |
| **Conferencing** | Audio mixing, VAD, AGC, dominant speaker detection |
| **Recording** | Per-leg, stereo, conference, multiple formats, cloud storage |
| **SIPREC** | RFC 7865/7866 SRC & SRS roles, metadata, SRTP key forwarding |
| **DTMF** | RFC 2833 and in-band detection/generation |
| **Transcription** | Real-time STT with multiple providers |
| **AI Streaming** | OpenAI Realtime, Dialogflow CX, Amazon Lex, Azure Bot |
| **Audio Injection** | File playback, TTS, tone generation |
| **High Availability** | Active/standby, active/active, session replication, VIP failover |
| **Performance** | Kernel offload (netfilter/eBPF), zero-copy parsing |

---

## Architecture

### Three-Layer Design

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              Control Plane                                       │
│  ┌───────────────────────────────────────────────────────────────────────────┐  │
│  │  HTTP/HTTPS REST API  │  WebSocket Events  │  ng Protocol (rtpengine)    │  │
│  └───────────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────────┘
                                         │
┌────────────────────────────────────────┼────────────────────────────────────────┐
│                               Media Plane                                        │
│                                        │                                         │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────┐  ┌────────────────────────┐ │
│  │  Session    │  │ Conference  │  │  Transcoding │  │  Recording/Injection   │ │
│  │  Registry   │  │   Rooms     │  │   Pipeline   │  │      /Transcription    │ │
│  └─────────────┘  └─────────────┘  └──────────────┘  └────────────────────────┘ │
│                                        │                                         │
│  ┌─────────────────────────────────────▼─────────────────────────────────────┐  │
│  │                        Packet Router / Media Streams                       │  │
│  │   RTP/RTCP  │  SRTP/SRTCP  │  DTLS-SRTP  │  Jitter Buffer  │  DTMF       │  │
│  └─────────────────────────────────────┬─────────────────────────────────────┘  │
└────────────────────────────────────────┼────────────────────────────────────────┘
                                         │
┌────────────────────────────────────────┼────────────────────────────────────────┐
│                         Kernel Module (Optional)                                 │
│  ┌─────────────────────────────────────▼─────────────────────────────────────┐  │
│  │   xt_RTPENGINE (netfilter)  │  eBPF/XDP  │  Userspace Fallback            │  │
│  └───────────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### WebRTC Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              Signaling Layer                                     │
│  ┌────────────────────────────────────────────────────────────────────────────┐ │
│  │  WebSocket Signaling Server  │  SDP Exchange  │  ICE Trickle              │ │
│  └────────────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────────┘
                                         │
┌────────────────────────────────────────┼────────────────────────────────────────┐
│                              WebRTC Layer                                        │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐ │
│  │  ICE Agent   │  │    DTLS      │  │    SCTP      │  │   Peer Connection    │ │
│  │  STUN/TURN   │  │  Transport   │  │ Data Channel │  │      Manager         │ │
│  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────────────┘ │
│                                        │                                         │
│  ┌─────────────────────────────────────▼─────────────────────────────────────┐  │
│  │                     WebRTC ↔ SIP/RTP Bridge                                │  │
│  │   Codec Transcoding  │  SRTP Key Translation  │  DTMF Relay               │  │
│  └───────────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Project Structure

```
forge-media/
├── Cargo.toml                    # Workspace definition
├── crates/
│   ├── forge-core/              # Core types, traits, utilities
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs         # Common types (CallId, RoomId, etc.)
│   │       ├── error.rs         # Error types
│   │       └── config.rs        # Configuration structures
│   │
│   ├── forge-rtp/               # RTP/RTCP/SRTP implementation
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── rtp.rs           # RTP packet parsing/building
│   │       ├── rtcp.rs          # RTCP packet types
│   │       ├── srtp.rs          # SRTP encryption/decryption
│   │       ├── jitter.rs        # Jitter buffer
│   │       └── dtls_srtp.rs     # DTLS-SRTP key derivation
│   │
│   ├── forge-engine/            # Core media engine
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs        # Main engine struct
│   │       ├── session.rs       # Media session management
│   │       ├── stream.rs        # Media stream handling
│   │       ├── ports.rs         # Port pool management
│   │       └── sockets.rs       # Socket management with TOS/QoS
│   │
│   ├── forge-codecs/            # Audio codec implementations
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── mod.rs
│   │       ├── g711.rs          # G.711 μ-law/A-law
│   │       ├── g722.rs          # G.722
│   │       ├── opus.rs          # Opus
│   │       └── g729.rs          # G.729 (feature-gated)
│   │
│   ├── forge-resampler/         # Sample rate conversion
│   │   └── src/
│   │       ├── lib.rs
│   │       └── resampler.rs     # Rubato-based resampler
│   │
│   ├── forge-transcoder/        # Audio transcoding pipeline
│   │   └── src/
│   │       ├── lib.rs
│   │       └── transcoder.rs    # Transcode between codecs/sample rates
│   │
│   ├── forge-storage/           # Recording storage management
│   │   └── src/
│   │       ├── lib.rs
│   │       └── storage.rs       # Storage backend abstraction
│   │
│   ├── forge-recorder/          # Audio recording (WAV, Opus)
│   │   └── src/
│   │       ├── lib.rs
│   │       └── recorder.rs      # Audio recording to files
│   │
│   ├── forge-mixer/             # Multi-party audio mixing
│   │   └── src/
│   │       ├── lib.rs
│   │       └── mixer.rs         # Audio mixing and stream management
│   │
│   ├── forge-conference-processor/  # Conference bridge management
│   │   └── src/
│   │       ├── lib.rs
│   │       └── conference.rs    # Conference rooms and bridges
│   │
│   ├── forge-media-processor/   # Media processing core
│   │   └── src/
│   │       ├── lib.rs
│   │       └── codecs/          # Legacy codec module
│   │
│   ├── forge-kernel/            # Kernel offload
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── rtpengine.rs     # xt_RTPENGINE interface
│   │       └── ebpf.rs          # eBPF/XDP programs
│   │
│   ├── forge-recording/         # Recording system
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manager.rs       # Recording manager
│   │       ├── task.rs          # Recording task
│   │       ├── encoders/
│   │       │   ├── mod.rs
│   │       │   ├── wav.rs       # WAV encoder
│   │       │   ├── opus.rs      # Opus/OGG encoder
│   │       │   ├── mp3.rs       # MP3 encoder (feature-gated)
│   │       │   └── flac.rs      # FLAC encoder (feature-gated)
│   │       ├── storage/
│   │       │   ├── mod.rs
│   │       │   ├── file.rs      # Local filesystem
│   │       │   ├── s3.rs        # AWS S3/compatible
│   │       │   └── streaming.rs # WebSocket streaming
│   │       └── tap.rs           # Media tap for audio capture
│   │
│   ├── forge-dtmf/              # DTMF detection/generation
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── detector.rs      # Combined detector
│   │       ├── rfc2833.rs       # RFC 2833 telephone-event
│   │       ├── goertzel.rs      # In-band Goertzel detection
│   │       ├── generator.rs     # DTMF tone generation
│   │       └── buffer.rs        # Digit collection buffer
│   │
│   ├── forge-transcription/     # Real-time transcription
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manager.rs       # Transcription manager
│   │       ├── session.rs       # Transcription session
│   │       ├── providers/
│   │       │   ├── mod.rs
│   │       │   ├── deepgram.rs  # Deepgram
│   │       │   ├── google.rs    # Google Speech-to-Text
│   │       │   ├── aws.rs       # AWS Transcribe
│   │       │   ├── azure.rs     # Azure Speech Services
│   │       │   └── whisper.rs   # OpenAI Whisper
│   │       └── events.rs        # Transcription events
│   │
│   ├── forge-injection/         # Audio injection
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── injector.rs      # Injection manager
│   │       ├── sources/
│   │       │   ├── mod.rs
│   │       │   ├── file.rs      # File playback
│   │       │   ├── tts.rs       # Text-to-speech
│   │       │   ├── tone.rs      # Tone generation
│   │       │   └── stream.rs    # HTTP/WebSocket streams
│   │       └── mixer.rs         # Injection mixing
│   │
│   ├── forge-webrtc/            # WebRTC support
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── session.rs       # Peer connection
│   │       ├── ice/
│   │       │   ├── mod.rs
│   │       │   ├── agent.rs     # ICE agent
│   │       │   ├── candidate.rs # ICE candidates
│   │       │   └── gathering.rs # Candidate gathering
│   │       ├── dtls/
│   │       │   ├── mod.rs
│   │       │   ├── handshake.rs # DTLS handshake
│   │       │   └── srtp_keying.rs
│   │       ├── stun/
│   │       │   ├── mod.rs
│   │       │   ├── message.rs   # STUN messages
│   │       │   └── client.rs    # STUN client
│   │       ├── turn/
│   │       │   ├── mod.rs
│   │       │   └── client.rs    # TURN client
│   │       ├── sctp/
│   │       │   └── mod.rs       # Data channels
│   │       ├── signaling.rs     # Signaling server
│   │       └── bridge.rs        # WebRTC ↔ SIP bridge
│   │
│   ├── forge-sdp/               # SDP utilities
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── parser.rs        # SDP parsing
│   │       ├── builder.rs       # SDP generation
│   │       └── webrtc.rs        # WebRTC SDP extensions
│   │
│   ├── forge-siprec/            # SIPREC (RFC 7865/7866)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── src/             # Session Recording Client
│   │       │   ├── mod.rs
│   │       │   ├── client.rs    # SRC implementation
│   │       │   └── forwarder.rs # Media forking to SRS
│   │       ├── srs/             # Session Recording Server
│   │       │   ├── mod.rs
│   │       │   ├── server.rs    # SRS implementation
│   │       │   └── receiver.rs  # Media reception
│   │       ├── metadata.rs      # XML metadata (RFC 7865)
│   │       ├── session.rs       # Recording session
│   │       └── srtp_passthrough.rs # SRTP key forwarding
│   │
│   ├── forge-ai-stream/         # Real-time AI streaming
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manager.rs       # AI connector manager
│   │       ├── session.rs       # Streaming session
│   │       ├── connectors/
│   │       │   ├── mod.rs
│   │       │   ├── openai.rs    # OpenAI Realtime API
│   │       │   ├── google.rs    # Google Dialogflow CX
│   │       │   ├── amazon.rs    # Amazon Lex
│   │       │   ├── azure.rs     # Azure Speech/Bot
│   │       │   ├── deepgram.rs  # Deepgram
│   │       │   ├── websocket.rs # Generic WebSocket
│   │       │   └── grpc.rs      # Generic gRPC
│   │       ├── audio_pipeline.rs # Audio format conversion
│   │       ├── vad.rs           # Voice activity detection
│   │       ├── barge_in.rs      # Barge-in handling
│   │       └── events.rs        # AI events
│   │
│   ├── forge-ha/                # High Availability & Failover
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── cluster.rs       # Cluster membership
│   │       ├── state_sync.rs    # Session state replication
│   │       ├── failover.rs      # Failover coordination
│   │       ├── vip.rs           # Virtual IP management
│   │       ├── health.rs        # Health monitoring
│   │       └── storage/
│   │           ├── mod.rs
│   │           ├── redis.rs     # Redis state backend
│   │           └── etcd.rs      # etcd state backend
│   │
│   └── forge-api/               # Control API
│       └── src/
│           ├── lib.rs
│           ├── server.rs        # Axum HTTP server
│           ├── routes/
│           │   ├── mod.rs
│           │   ├── sessions.rs  # Session routes
│           │   ├── conferences.rs
│           │   ├── recordings.rs
│           │   ├── dtmf.rs
│           │   ├── transcription.rs
│           │   ├── injection.rs
│           │   └── webrtc.rs
│           ├── websocket.rs     # WebSocket events
│           └── ng_protocol.rs   # rtpengine compatibility
│
├── src/
│   └── main.rs                  # Binary entry point
│
├── config/
│   └── forge.toml               # Default configuration
│
└── tests/
    ├── integration/
    └── benchmarks/
```

---

## Core Components

### Common Types

```rust
/// Unique call identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct CallId(pub String);

/// Conference room identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct RoomId(pub String);

/// Participant identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ParticipantId(pub String);

/// Leg identifier for P2P sessions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegIdentifier {
    LegA,
    LegB,
    ByTag(u32),
}

/// Media direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDirection {
    SendRecv,
    SendOnly,
    RecvOnly,
    Inactive,
}

/// IP version handling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersionConfig {
    V4Only,
    V6Only,
    DualStack,
    Bridge4to6,
    Bridge6to4,
}
```

---

## RTP Core

### RTP Header Structure

```rust
#[repr(C, packed)]
pub struct RtpHeader {
    pub version_flags: u8,      // V=2, P, X, CC
    pub marker_payload_type: u8, // M, PT
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    // CSRC list follows if CC > 0
}

impl RtpHeader {
    pub const SIZE: usize = 12;
    
    pub fn version(&self) -> u8 { (self.version_flags >> 6) & 0x03 }
    pub fn padding(&self) -> bool { (self.version_flags & 0x20) != 0 }
    pub fn extension(&self) -> bool { (self.version_flags & 0x10) != 0 }
    pub fn csrc_count(&self) -> u8 { self.version_flags & 0x0F }
    pub fn marker(&self) -> bool { (self.marker_payload_type & 0x80) != 0 }
    pub fn payload_type(&self) -> u8 { self.marker_payload_type & 0x7F }
}
```

### SRTP Implementation

Supported profiles:
- `SRTP_AES128_CM_HMAC_SHA1_80` (RFC 3711)
- `SRTP_AES128_CM_HMAC_SHA1_32`
- `SRTP_AEAD_AES_128_GCM` (RFC 7714)
- `SRTP_AEAD_AES_256_GCM`

Features:
- Key derivation per RFC 3711
- 128-bit sliding window replay protection
- Roll-over counter (ROC) management
- RTCP encryption

### Jitter Buffer

Adaptive jitter buffer with:
- BTreeMap storage by sequence number
- Target delay adaptation: `target = 2 * measured_jitter`
- Configurable min/max delay bounds
- Packet loss detection
- Late packet handling
- PLC (Packet Loss Concealment) support

---

## Media Engine

### Engine Configuration

```rust
pub struct EngineConfig {
    /// Port range for RTP/RTCP (default: 30000-40000)
    pub port_range: RangeInclusive<u16>,
    
    /// Network interfaces with optional advertised addresses
    pub interfaces: Vec<InterfaceConfig>,
    
    /// TOS/DSCP value (default: 0xB8 = EF)
    pub tos: u8,
    
    /// Kernel offload settings
    pub kernel_offload: KernelOffloadConfig,
    
    /// Transcoding settings
    pub transcoding: TranscodingConfig,
    
    /// Session timeout
    pub session_timeout: Duration,
}

pub struct InterfaceConfig {
    pub name: String,
    pub address: IpAddr,
    pub advertised_address: Option<IpAddr>, // For NAT
}
```

### Port Pool

```rust
pub struct PortPool {
    range: RangeInclusive<u16>,
    strategy: AllocationStrategy,
    allocated: DashSet<u16>,
}

pub enum AllocationStrategy {
    Sequential,
    Random,
    RoundRobin,
}
```

Allocates RTP/RTCP pairs (even/odd ports).

### Socket Management

- Creates UDP sockets with `socket2` for fine-grained control
- Sets `SO_REUSEADDR`, `SO_REUSEPORT`
- Configures TOS/DSCP via `setsockopt(IP_TOS)` / `setsockopt(IPV6_TCLASS)`
- Handles IPv4/IPv6 bridging scenarios

---

## Session Management

### Media Session

```rust
pub struct MediaSession {
    pub call_id: CallId,
    pub streams: Vec<MediaStream>,
    pub created_at: Instant,
    pub state: SessionState,
}

pub struct MediaStream {
    pub stream_type: MediaType,
    pub leg_a: StreamEndpoint,
    pub leg_b: StreamEndpoint,
    pub transcoder: Option<Transcoder>,
    pub dtmf_manager: DtmfManager,
}

pub struct StreamEndpoint {
    pub local_addr: SocketAddr,
    pub remote_addr: Option<SocketAddr>,
    pub socket: Arc<UdpSocket>,
    pub srtp_context: Option<SrtpContext>,
    pub jitter_buffer: JitterBuffer,
}
```

### Session Operations

| Operation | Description |
|-----------|-------------|
| `offer` | Create session from SDP offer, allocate ports |
| `answer` | Complete session setup with SDP answer |
| `delete` | Tear down session, release resources |

---

## Transcoding

### Supported Codecs

| Codec | Payload Type | Sample Rate | Bitrate |
|-------|--------------|-------------|---------|
| PCMU (G.711 μ-law) | 0 | 8000 Hz | 64 kbps |
| PCMA (G.711 A-law) | 8 | 8000 Hz | 64 kbps |
| G.722 | 9 | 16000 Hz | 64 kbps |
| G.729 | 18 | 8000 Hz | 8 kbps |
| Opus | dynamic | 48000 Hz | 6-510 kbps |
| telephone-event | dynamic | 8000 Hz | - |

### Transcoding Pipeline

```rust
pub struct Transcoder {
    decoder: Box<dyn Decoder>,
    encoder: Box<dyn Encoder>,
    resampler: Option<Resampler>,
}

pub trait Decoder: Send + Sync {
    fn decode(&mut self, input: &[u8]) -> Result<Vec<i16>, CodecError>;
    fn sample_rate(&self) -> u32;
}

pub trait Encoder: Send + Sync {
    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>, CodecError>;
    fn sample_rate(&self) -> u32;
    fn frame_size(&self) -> usize;
}
```

---

## Kernel Offload

### Backends

1. **xt_RTPENGINE** (netfilter module)
   - Control via `/proc/rtpengine/control`
   - Kernel-space packet forwarding
   - Automatic userspace fallback

2. **eBPF/XDP**
   - Uses `libbpf-rs`
   - BPF_MAP_TYPE_HASH for stream lookup
   - Attached to network interface

### Interface

```rust
pub trait KernelOffload: Send + Sync {
    async fn add_stream(&self, config: KernelStreamConfig) -> Result<(), OffloadError>;
    async fn remove_stream(&self, stream_id: &str) -> Result<(), OffloadError>;
    async fn get_stats(&self, stream_id: &str) -> Result<StreamStats, OffloadError>;
}
```

---

## Audio Conferencing

### Room Configuration

```rust
pub struct RoomConfig {
    pub max_participants: usize,    // Default: 100
    pub sample_rate: u32,           // Default: 48000
    pub frame_size_ms: u32,         // Default: 20
    pub mixing_mode: MixingMode,
    pub enable_vad: bool,
    pub enable_agc: bool,
    pub recording: Option<RecordingConfig>,
}

pub enum MixingMode {
    All,                    // Mix all participants
    LastN(usize),          // Mix N loudest speakers
    DominantSpeaker,       // Lecture mode
    RoleBased,             // Custom logic
}
```

### Audio Mixer

- Sample-level mixing with i32 accumulation (prevents overflow)
- Volume scaling per participant
- Clipping to i16 range
- Personalized mixes (everyone except self)

### Participant Roles

```rust
pub enum ParticipantRole {
    Participant,  // Full send/receive
    Moderator,    // Full + controls
    Listener,     // Receive only
    Presenter,    // Priority speaker
}
```

### Dominant Speaker Detection

- Per-participant energy tracking with exponential smoothing (decay: 0.95)
- Speech threshold detection
- Hysteresis to prevent rapid switching (min 500ms)
- Returns sorted list by energy

---

## Recording System

### Recording Targets

```rust
pub enum RecordingTarget {
    SessionLeg { call_id: CallId, leg: LegIdentifier },
    SessionAllLegsSeparate { call_id: CallId },
    SessionAllLegsMixed { call_id: CallId, stereo: bool },
    ConferenceParticipant { room_id: RoomId, participant_id: ParticipantId },
    ConferenceMixed { room_id: RoomId },
    ConferenceSelectiveMix { room_id: RoomId, participants: Vec<ParticipantId> },
}
```

### Audio Formats

| Format | Extension | Description |
|--------|-----------|-------------|
| WAV | .wav | PCM, μ-law, or A-law |
| Opus | .opus | Ogg container |
| MP3 | .mp3 | LAME encoder (feature-gated) |
| FLAC | .flac | Lossless (feature-gated) |
| Raw PCM | .pcm | Headerless PCM |

### Storage Backends

1. **FileStorage**: Local filesystem with configurable subdirectory patterns
2. **S3Storage**: AWS S3 or compatible (MinIO, etc.) with multipart upload
3. **StreamingStorage**: Real-time WebSocket streaming

### Features

- Pause/resume
- Metadata and tags
- Silence detection (mark/pause/skip)
- Duration and file size limits
- Real-time WebSocket events

---

## DTMF System

### Detection Modes

```rust
pub enum DtmfMode {
    Rfc2833Only,   // RFC 2833 telephone-event only
    InBandOnly,    // Goertzel algorithm only
    Both,          // RFC 2833 preferred, in-band fallback
}
```

### RFC 2833 Support

- Telephone-event payload parsing (PT typically 101)
- End bit detection for digit completion
- Volume extraction (dBm0)
- Packet generation with proper timing

### In-Band Detection

- Goertzel algorithm for frequency detection
- DTMF frequency pairs:
  - Low: 697, 770, 852, 941 Hz
  - High: 1209, 1336, 1477, 1633 Hz
- Twist detection (balance between frequencies)
- Minimum duration validation (40ms default)

### Digit Buffer

```rust
pub struct DtmfBufferConfig {
    pub max_digits: usize,              // Default: 20
    pub inter_digit_timeout: Duration,  // Default: 5s
    pub total_timeout: Duration,        // Default: 30s
    pub terminator: Option<DtmfDigit>,  // e.g., #
    pub min_digits: usize,              // Default: 1
}
```

---

## Real-Time Transcription

### Supported Providers

| Provider | Features |
|----------|----------|
| Deepgram | Real-time, diarization, interim results |
| Google Speech-to-Text | Multiple languages, word timestamps |
| AWS Transcribe | Medical/call analytics variants |
| Azure Speech Services | Custom models, pronunciation |
| OpenAI Whisper | High accuracy, multilingual |

### Transcription Session

```rust
pub struct TranscriptionConfig {
    pub provider: TranscriptionProvider,
    pub language: String,           // e.g., "en-US"
    pub interim_results: bool,
    pub word_timestamps: bool,
    pub diarization: bool,
    pub max_speakers: Option<u8>,
    pub custom_vocabulary: Vec<String>,
    pub profanity_filter: bool,
    pub auto_punctuation: bool,
}
```

### Results

```rust
pub struct TranscriptionResult {
    pub id: String,
    pub text: String,
    pub confidence: f32,
    pub is_final: bool,
    pub start_ms: u64,
    pub end_ms: u64,
    pub words: Vec<WordResult>,
    pub speaker: Option<String>,
    pub language: Option<String>,
}
```

---

## Audio Injection

### Audio Sources

```rust
pub enum AudioSource {
    File { path: String },
    RemoteFile { url: String },
    Tts { text: String, voice: TtsVoice, provider: TtsProvider },
    Tone { frequency: f32, duration_ms: u32 },
    Dtmf { digits: String, digit_duration_ms: u32, gap_ms: u32 },
    Silence { duration_ms: u32 },
    Stream { url: String },
    Raw { samples: Vec<i16>, sample_rate: u32 },
}
```

### Mix Modes

```rust
pub enum MixMode {
    Mix,                        // Add to existing audio
    Replace,                    // Replace existing audio
    Duck { duck_level: u8 },    // Reduce existing audio volume
    OnSilence,                  // Only play if silence detected
}
```

### TTS Providers

- Google Cloud Text-to-Speech
- Amazon Polly
- Azure Speech Services
- ElevenLabs
- OpenAI TTS

### Features

- File format support: WAV, MP3, OGG, FLAC (via Symphonia)
- Automatic resampling
- Fade in/out
- Looping with configurable count
- Volume control

---

## WebRTC Support

### ICE Implementation

- Full ICE agent with:
  - Host candidate gathering
  - Server reflexive (STUN)
  - Relay (TURN)
- Trickle ICE support
- ICE restart capability
- Controlling/controlled role handling

### DTLS-SRTP

- DTLS 1.2 handshake
- Certificate generation or loading
- Fingerprint verification (SHA-256, SHA-384)
- SRTP key export
- Supported profiles:
  - SRTP_AEAD_AES_128_GCM
  - SRTP_AES128_CM_SHA1_80

### Peer Connection API

```rust
impl PeerConnection {
    pub async fn create_offer(&self, options: Option<OfferOptions>) -> Result<SessionDescription>;
    pub async fn create_answer(&self, options: Option<AnswerOptions>) -> Result<SessionDescription>;
    pub async fn set_local_description(&self, desc: SessionDescription) -> Result<()>;
    pub async fn set_remote_description(&self, desc: SessionDescription) -> Result<()>;
    pub async fn add_ice_candidate(&self, candidate: IceCandidate) -> Result<()>;
    pub fn add_transceiver(&self, kind: MediaKind, init: Option<TransceiverInit>) -> Arc<RtpTransceiver>;
    pub fn create_data_channel(&self, label: &str, options: Option<DataChannelInit>) -> Result<Arc<DataChannel>>;
}
```

### Signaling Server

WebSocket-based signaling with:
- Room management
- Peer discovery
- SDP exchange
- ICE candidate trickle
- JSON message protocol

### WebRTC-SIP Bridge

- Bidirectional media bridging
- Codec transcoding (e.g., Opus ↔ G.711)
- SRTP key translation
- DTMF relay

---

## Control API

### HTTP Endpoints

#### Sessions
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/sessions` | Create session |
| GET | `/v1/sessions` | List sessions |
| GET | `/v1/sessions/:call_id` | Get session |
| DELETE | `/v1/sessions/:call_id` | Delete session |
| POST | `/v1/sessions/:call_id/offer` | Process offer |
| POST | `/v1/sessions/:call_id/answer` | Process answer |

#### Conferences
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/conferences` | Create room |
| GET | `/v1/conferences/:room_id` | Get room |
| DELETE | `/v1/conferences/:room_id` | Destroy room |
| POST | `/v1/conferences/:room_id/participants` | Add participant |
| DELETE | `/v1/conferences/:room_id/participants/:id` | Remove participant |
| POST | `/v1/conferences/:room_id/mute-all` | Mute all |

#### Recording
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/recordings` | Start recording |
| GET | `/v1/recordings/:id` | Get recording info |
| DELETE | `/v1/recordings/:id` | Stop recording |
| POST | `/v1/recordings/:id/pause` | Pause |
| POST | `/v1/recordings/:id/resume` | Resume |

#### DTMF
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/sessions/:call_id/dtmf/send` | Send DTMF |
| POST | `/v1/sessions/:call_id/dtmf/collect` | Start collecting |
| GET | `/v1/sessions/:call_id/dtmf/events` | WebSocket events |

#### Transcription
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/transcription/start` | Start transcription |
| POST | `/v1/transcription/:id/stop` | Stop transcription |
| GET | `/v1/transcription/:id/stream` | WebSocket results |

#### Audio Injection
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/audio/inject` | Inject audio |
| POST | `/v1/sessions/:call_id/audio/play` | Play file |
| POST | `/v1/sessions/:call_id/audio/say` | TTS |

#### WebRTC
| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/webrtc/connections` | Create connection |
| POST | `/v1/webrtc/connections/:id/offer` | Create offer |
| POST | `/v1/webrtc/connections/:id/answer` | Create answer |
| POST | `/v1/webrtc/bridge` | Create bridge |
| GET | `/v1/webrtc/signaling` | WebSocket signaling |

#### High Availability
| Method | Path | Description |
|--------|------|-------------|
| GET | `/v1/ha/status` | HA status |
| POST | `/v1/ha/failover` | Trigger failover |

### ng Protocol (rtpengine compatibility)

Bencode-encoded UDP protocol for drop-in rtpengine replacement:
- `offer` / `answer` / `delete` / `query` / `ping`

---

## Configuration

### Example Configuration File

```toml
# /etc/forge/config.toml

[engine]
# Port range for RTP/RTCP
port_range = { start = 30000, end = 40000 }

# TOS/DSCP (0xB8 = EF for voice)
tos = 0xB8

# Session timeout
session_timeout_secs = 300

[[engine.interfaces]]
name = "eth0"
address = "192.168.1.100"
advertised_address = "203.0.113.50"  # Public IP for NAT

[[engine.interfaces]]
name = "eth1"
address = "10.0.0.100"

[engine.kernel_offload]
enabled = true
backend = "rtpengine"  # or "ebpf"
control_path = "/proc/rtpengine/control"

[engine.transcoding]
enabled = true
codec_preferences = ["opus", "pcmu", "pcma"]

[engine.conference]
default_max_participants = 100
max_rooms = 1000
auto_destroy_empty_rooms = true
default_mixing_mode = "last3"
enable_vad = true
enable_agc = true

[engine.recording]
enabled = true
default_format = "opus"
filename_template = "{call_id}_{leg}_{timestamp}.{format}"

[engine.recording.file_storage]
base_path = "/var/lib/forge/recordings"
subdir_pattern = "{year}/{month}/{day}"

[engine.recording.s3_storage]
bucket = "forge-recordings"
prefix = "calls/"
region = "us-east-1"

[engine.dtmf]
mode = "both"
rfc2833_payload_type = 101
min_duration_ms = 40

[engine.transcription]
enabled = true
default_provider = "deepgram"
max_concurrent_sessions = 100

[engine.transcription.providers.deepgram]
api_key = "${DEEPGRAM_API_KEY}"

[engine.audio_injection]
enabled = true
cache_dir = "/var/cache/forge/audio"
max_cache_size = 1073741824

[engine.audio_injection.tts.google]
api_key = "${GOOGLE_TTS_API_KEY}"

[engine.webrtc]
enabled = true

[engine.webrtc.ice]
stun_servers = ["stun:stun.l.google.com:19302"]
transport_policy = "all"
port_range = { start = 49152, end = 65535 }

[[engine.webrtc.ice.turn_servers]]
urls = ["turn:turn.example.com:3478"]
username = "user"
credential = "${TURN_PASSWORD}"

[api]
http_bind = "0.0.0.0:8080"
https_bind = "0.0.0.0:8443"
ws_bind = "0.0.0.0:8081"
tls_cert = "/etc/forge/cert.pem"
tls_key = "/etc/forge/key.pem"
```

---

## Integration with Siphon

### MediaHandler Bridge

```rust
pub struct MediaHandler {
    engine: Arc<ForgeEngine>,
}

impl MediaHandler {
    /// Handle incoming INVITE
    pub async fn handle_invite(&self, invite: &SipMessage) -> Result<SdpResponse> {
        let call_id = invite.call_id();
        let sdp = invite.body().parse::<Sdp>()?;
        
        let response = self.engine.offer(&call_id, &sdp).await?;
        Ok(response)
    }

    /// Handle 200 OK with SDP answer
    pub async fn handle_answer(&self, response: &SipMessage) -> Result<()> {
        let call_id = response.call_id();
        let sdp = response.body().parse::<Sdp>()?;
        
        self.engine.answer(&call_id, &sdp).await?;
        Ok(())
    }

    /// Handle BYE
    pub async fn handle_bye(&self, bye: &SipMessage) -> Result<()> {
        let call_id = bye.call_id();
        self.engine.delete(&call_id).await?;
        Ok(())
    }
}
```

---

## Dependencies

### Core Dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
socket2 = "0.5"
parking_lot = "0.12"
dashmap = "5"
uuid = { version = "1", features = ["v4"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Web framework
axum = { version = "0.7", features = ["ws"] }
tower = "0.4"
tower-http = { version = "0.5", features = ["cors", "trace"] }

# Crypto
aes = "0.8"
aes-gcm = "0.10"
hmac = "0.12"
sha1 = "0.10"
sha2 = "0.10"

# WebRTC/DTLS
openssl = { version = "0.10", optional = true }
tokio-tungstenite = "0.21"
```

### Optional Dependencies

```toml
[dependencies]
# Opus codec
opus = { version = "0.3", optional = true }

# MP3 encoding
mp3lame-encoder = { version = "0.1", optional = true }

# FLAC encoding
flac-bound = { version = "0.3", optional = true }

# eBPF
libbpf-rs = { version = "0.22", optional = true }

# S3 storage
aws-sdk-s3 = { version = "1", optional = true }

# Audio file decoding
symphonia = { version = "0.5", features = ["all"], optional = true }

[features]
default = ["opus"]
full = ["opus", "mp3", "flac", "ebpf", "s3"]
opus = ["dep:opus"]
mp3 = ["dep:mp3lame-encoder"]
flac = ["dep:flac-bound"]
ebpf = ["dep:libbpf-rs"]
s3 = ["dep:aws-sdk-s3"]
```

### Build Profile

```toml
[profile.release]
lto = true
codegen-units = 1
panic = "abort"
```

---

## SIPREC (RFC 7865/7866)

### Overview

SIPREC provides standards-based session recording via the Session Recording Protocol (RFC 7866) and metadata format (RFC 7865). Forge supports **both** the Session Recording Client (SRC) and Session Recording Server (SRS) roles:

- **SRC Role**: Forks media from active calls to external recording servers
- **SRS Role**: Receives and stores recordings from external SRC implementations

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           Communication Session                                  │
│  ┌─────────────┐                                           ┌─────────────┐      │
│  │   Caller    │◄──────────── RTP/SRTP ──────────────────►│   Callee    │      │
│  └─────────────┘                                           └─────────────┘      │
│                                    │                                             │
│                                    │ Media Tap                                   │
│                                    ▼                                             │
│  ┌─────────────────────────────────────────────────────────────────────────┐    │
│  │                    Forge Session Recording Client (SRC)                  │    │
│  │                                                                          │    │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐  │    │
│  │  │   Metadata   │  │    RTP       │  │    SRTP      │  │   Failover  │  │    │
│  │  │  Generator   │  │   Forwarder  │  │  Key Fwd     │  │   Manager   │  │    │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └─────────────┘  │    │
│  └─────────────────────────────────────────────────────────────────────────┘    │
│                                    │                                             │
│                        SIP INVITE + RTP Streams                                  │
│                                    ▼                                             │
│  ┌─────────────────────────────────────────────────────────────────────────┐    │
│  │                    Session Recording Server (SRS)                        │    │
│  │                    (External third-party server)                         │    │
│  └─────────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### Metadata Format (RFC 7865)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<recording xmlns="urn:ietf:params:xml:ns:recording:1">
  <datamode>complete</datamode>
  
  <session session_id="call-123">
    <start-time>2024-12-06T10:30:00Z</start-time>
  </session>
  
  <participant participant_id="p1">
    <nameID>
      <aor>sip:alice@example.com</aor>
      <name>Alice</name>
    </nameID>
    <associate-time type="start"/>
  </participant>
  
  <participant participant_id="p2">
    <nameID>
      <aor>sip:bob@example.com</aor>
      <name>Bob</name>
    </nameID>
    <associate-time type="start"/>
  </participant>
  
  <stream stream_id="s1" session_id="call-123">
    <label>audio-caller</label>
  </stream>
  
  <stream stream_id="s2" session_id="call-123">
    <label>audio-callee</label>
  </stream>
</recording>
```

### Core Types

```rust
/// SIPREC recording modes
pub enum RecordingMode {
    Full,       // Record entire session
    Selective,  // Record on demand
    Pausable,   // Supports pause/resume
}

/// Participant in communication session
pub struct Participant {
    pub id: String,
    pub aor: String,           // Address of Record
    pub name: Option<String>,
    pub role: ParticipantRole, // Caller, Callee, Participant
    pub streams: Vec<String>,  // Associated media streams
}

/// SIPREC session info
pub struct SiprecSession {
    pub session_id: RecordingSessionId,
    pub communication_session: CommunicationSession,
    pub srs: Arc<SrsConnection>,
    pub state: SiprecSessionState,
    pub streams: HashMap<String, SiprecStream>,
}

/// SRS configuration
pub struct SrsConfig {
    pub id: SrsId,
    pub uri: String,           // SIP URI
    pub priority: u8,          // Failover priority
    pub weight: u8,            // Load balancing weight
    pub transport: Transport,  // UDP/TCP/TLS
    pub auth: Option<SrsAuth>,
}
```

### API Routes

| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/siprec/sessions` | Start SIPREC recording |
| GET | `/v1/siprec/sessions` | List active recordings |
| GET | `/v1/siprec/sessions/:id` | Get recording details |
| DELETE | `/v1/siprec/sessions/:id` | Stop recording |
| POST | `/v1/siprec/sessions/:id/pause` | Pause recording |
| POST | `/v1/siprec/sessions/:id/resume` | Resume recording |
| POST | `/v1/siprec/sessions/:id/update` | Update metadata |
| GET | `/v1/siprec/srs` | List configured SRS |
| GET | `/v1/siprec/srs/:id/health` | SRS health status |

### Configuration

```toml
[siprec]
enabled = true
default_mode = "full"
forward_srtp_keys = true
metadata_version = "rfc7865"

[[siprec.srs]]
id = "srs-primary"
uri = "sip:srs.example.com:5060"
priority = 1
transport = "tcp"
health_check_interval_secs = 30

[[siprec.srs]]
id = "srs-backup"
uri = "sip:srs-backup.example.com:5060"
priority = 2
transport = "tcp"

[siprec.retry]
max_attempts = 3
initial_delay_ms = 100
max_delay_ms = 5000
```

### SRS (Session Recording Server) Role

When operating as an SRS, Forge receives SIPREC INVITEs from external SRCs and stores the recorded media.

```rust
/// SRS - Receives and stores recording sessions
pub struct SessionRecordingServer {
    config: SrsConfig,
    sessions: DashMap<Uuid, SrsSession>,
    storage: Arc<dyn RecordingStorage>,
    sip_server: Arc<SipServer>,
}

pub struct SrsSession {
    pub id: Uuid,
    pub metadata: RecordingMetadata,
    pub state: RecordingState,
    pub streams: Vec<RecordingStream>,
    pub dialog: SipDialog,
}

impl SessionRecordingServer {
    /// Handle incoming SIPREC INVITE
    pub async fn handle_invite(&self, request: &SipRequest) -> Result<SipResponse> {
        // Verify Require: siprec header
        if !request.header("Require").map(|h| h.contains("siprec")).unwrap_or(false) {
            return Ok(SipResponse::bad_request("Missing Require: siprec"));
        }
        
        // Parse multipart body (SDP + metadata XML)
        let (sdp, metadata) = self.parse_siprec_body(request)?;
        
        // Allocate RTP ports and start recording
        let session = self.create_recording_session(sdp, metadata).await?;
        
        // Build 200 OK with answer SDP
        Ok(SipResponse::ok().body(session.answer_sdp))
    }
}
```

#### SRS Configuration

```toml
[siprec.srs_role]
enabled = true
listen_addr = "0.0.0.0:5060"
rtp_port_range = { start = 40000, end = 50000 }
max_sessions = 1000

# Storage backend for recordings
[siprec.srs_role.storage]
type = "s3"  # or "file", "gcs"
bucket = "recordings"
region = "us-east-1"

# Recording format
[siprec.srs_role.recording]
format = "opus"  # opus, wav, mp3
separate_streams = true  # One file per participant
metadata_storage = "database"  # Store metadata in DB
```

---

## Real-Time AI Streaming

### Overview

Forge provides bidirectional audio streaming to AI services for conversational AI, virtual agents, and real-time analytics. Supports multiple providers with a unified interface.

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              AI Streaming Layer                                  │
│                                                                                  │
│  ┌──────────────────┐    ┌──────────────────┐    ┌──────────────────┐          │
│  │   OpenAI         │    │   Google         │    │   Amazon         │          │
│  │   Realtime API   │    │   Dialogflow CX  │    │   Lex            │          │
│  │   (GPT-4o)       │    │   Speech-to-Text │    │   Transcribe     │          │
│  └────────┬─────────┘    └────────┬─────────┘    └────────┬─────────┘          │
│           │                       │                       │                     │
│           └───────────────────────┼───────────────────────┘                     │
│                                   │                                              │
│                    ┌──────────────▼──────────────┐                              │
│                    │    AI Connector Manager      │                              │
│                    │                              │                              │
│                    │  • Session Management        │                              │
│                    │  • Audio Format Conversion   │                              │
│                    │  • VAD Integration           │                              │
│                    │  • Barge-in Detection        │                              │
│                    │  • Tool/Function Execution   │                              │
│                    └──────────────┬──────────────┘                              │
│                                   │                                              │
│  ┌────────────────────────────────▼────────────────────────────────────────┐    │
│  │                        Audio Pipeline                                     │    │
│  │                                                                           │    │
│  │   RTP Input ──► Decode ──► Resample ──► AI Service                       │    │
│  │                                              │                            │    │
│  │   RTP Output ◄── Encode ◄── Resample ◄──────┘                           │    │
│  └───────────────────────────────────────────────────────────────────────────┘    │
│                                                                                  │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### Supported Providers

| Provider | Features | Audio Format | Connection |
|----------|----------|--------------|------------|
| **OpenAI Realtime** | Full duplex, function calling, voice selection | PCM16/G.711 | WebSocket |
| **Google Dialogflow CX** | Intent detection, context, webhooks | LINEAR16/MULAW | gRPC |
| **Amazon Lex** | Intent detection, slots, session attrs | PCM/Opus | WebSocket |
| **Azure Speech/Bot** | Speech recognition, bot integration | PCM/Opus | WebSocket |
| **Deepgram** | Low-latency STT, diarization | LINEAR16 | WebSocket |
| **Custom** | User-defined endpoints | Configurable | WebSocket/gRPC |

### Streaming Modes

```rust
pub enum AiStreamMode {
    SpeechToText,     // STT only
    TextToSpeech,     // TTS only (playback)
    Conversational,   // Full duplex conversation
    IntentDetection,  // Detect intents (Dialogflow/Lex)
    SentimentAnalysis,// Analyze sentiment
}
```

### Core Types

```rust
/// AI connector configuration
pub struct AiConnectorConfig {
    pub provider: AiProvider,
    pub credentials: AiCredentials,
    pub mode: AiStreamMode,
    pub input_format: AudioFormat,
    pub output_format: AudioFormat,
    pub language: String,
    pub model: Option<String>,      // Provider-specific model
    pub voice: Option<String>,      // TTS voice
    pub system_prompt: Option<String>,
    pub tools: Vec<AiTool>,         // Function definitions
    pub vad_config: VadConfig,
    pub barge_in: BargeInConfig,
}

/// Voice Activity Detection config
pub struct VadConfig {
    pub enabled: bool,
    pub silence_threshold: f32,    // 0.0-1.0
    pub min_speech_ms: u32,
    pub end_of_speech_ms: u32,
    pub server_vad: bool,          // Use provider's VAD
}

/// Barge-in configuration
pub struct BargeInConfig {
    pub enabled: bool,
    pub energy_threshold: f32,
    pub min_duration_ms: u32,
    pub action: BargeInAction,     // StopImmediately, FadeOut, CompleteSentence
}

/// Tool/function definition
pub struct AiTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

/// AI events
pub enum AiEvent {
    SessionStarted { session_id, provider, timestamp },
    SessionEnded { session_id, summary, timestamp },
    SpeechStarted { timestamp },
    SpeechEnded { timestamp },
    TranscriptDelta { text, is_final, timestamp },
    IntentDetected { intent, confidence, timestamp },
    ResponseTextDelta { text, is_final, timestamp },
    AiSpeechStarted { timestamp },
    AiSpeechEnded { timestamp },
    ResponseComplete { timestamp },
    ToolCall { call_id, name, arguments, timestamp },
    BargeIn { timestamp },
    Error { code, message, timestamp },
}
```

### OpenAI Realtime Integration

```rust
// Start conversational AI session
let session = ai_manager.start_session(StartAiSessionRequest {
    call_id: call_id.clone(),
    leg: LegIdentifier::A,
    config: AiConnectorConfig {
        provider: AiProvider::OpenAi,
        mode: AiStreamMode::Conversational,
        model: Some("gpt-4o-realtime-preview-2024-12-17".into()),
        voice: Some("alloy".into()),
        system_prompt: Some("You are a helpful assistant...".into()),
        tools: vec![
            AiTool {
                name: "lookup_order".into(),
                description: "Look up order by number".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "order_number": { "type": "string" }
                    },
                    "required": ["order_number"]
                }),
            },
        ],
        vad_config: VadConfig {
            enabled: true,
            server_vad: true, // Let OpenAI detect speech
            end_of_speech_ms: 500,
            ..Default::default()
        },
        barge_in: BargeInConfig {
            enabled: true,
            action: BargeInAction::StopImmediately,
            ..Default::default()
        },
        ..Default::default()
    },
    ..Default::default()
}).await?;
```

### API Routes

| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/ai/sessions` | Start AI streaming session |
| GET | `/v1/ai/sessions` | List active sessions |
| GET | `/v1/ai/sessions/:id` | Get session details |
| DELETE | `/v1/ai/sessions/:id` | Stop session |
| POST | `/v1/ai/sessions/:id/text` | Send text message |
| POST | `/v1/ai/sessions/:id/interrupt` | Interrupt AI |
| POST | `/v1/ai/sessions/:id/tool-result` | Return tool result |
| GET | `/v1/ai/sessions/:id/events` | WebSocket event stream |
| GET | `/v1/ai/providers` | List available providers |
| GET | `/v1/ai/providers/:name/status` | Provider health |

### Configuration

```toml
[ai_stream]
enabled = true
max_sessions = 1000
audio_buffer_ms = 100

# OpenAI Realtime API
[ai_stream.providers.openai]
enabled = true
default_model = "gpt-4o-realtime-preview-2024-12-17"
default_voice = "alloy"

[ai_stream.providers.openai.credentials]
api_key = "${OPENAI_API_KEY}"

# Google Dialogflow CX
[ai_stream.providers.google]
enabled = true

[ai_stream.providers.google.credentials]
project_id = "my-project"
region = "us-central1"

[ai_stream.providers.google.options]
agent_id = "my-agent-id"

# Amazon Lex
[ai_stream.providers.amazon]
enabled = true

[ai_stream.providers.amazon.credentials]
region = "us-east-1"

[ai_stream.providers.amazon.options]
bot_id = "my-bot-id"
bot_alias_id = "TSTALIASID"

# Default VAD settings
[ai_stream.vad]
enabled = true
silence_threshold = 0.3
min_speech_ms = 100
end_of_speech_ms = 500
server_vad = true

# Default barge-in settings
[ai_stream.barge_in]
enabled = true
energy_threshold = 0.2
min_duration_ms = 100
action = "stop_immediately"
```

### Usage Examples

**Start AI session for a call:**
```bash
curl -X POST http://localhost:8080/v1/ai/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "call-123",
    "leg": "a",
    "provider": "openai",
    "mode": "conversational",
    "voice": "alloy",
    "system_prompt": "You are a helpful customer service agent.",
    "tools": [{
      "name": "transfer_call",
      "description": "Transfer call to a department",
      "parameters": {
        "type": "object",
        "properties": {
          "department": { "type": "string" }
        }
      }
    }]
  }'
```

**Handle tool call:**
```bash
curl -X POST http://localhost:8080/v1/ai/sessions/ai-789/tool-result \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "call_abc123",
    "result": { "status": "transferred", "queue_position": 3 }
  }'
```

**Interrupt AI:**
```bash
curl -X POST http://localhost:8080/v1/ai/sessions/ai-789/interrupt
```

---

## License

Copyright © 2024 Ferrous Communications. All rights reserved.
