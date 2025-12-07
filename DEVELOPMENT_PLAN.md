# Forge Media Engine - Phased Development Plan & Strategy

**Version:** 1.0
**Date:** December 2024
**Project:** Forge Media Engine (forge-media)
**Part of:** Ferrous Communications Platform (FCP)

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Development Philosophy](#development-philosophy)
3. [Phase Overview](#phase-overview)
4. [Detailed Phase Breakdown](#detailed-phase-breakdown)
5. [Testing Strategy](#testing-strategy)
6. [Integration Strategy](#integration-strategy)
7. [Performance Targets](#performance-targets)
8. [Risk Management](#risk-management)
9. [Success Criteria](#success-criteria)

---

## Executive Summary

This document outlines a pragmatic, phased approach to building Forge, a best-in-class media server in Rust. The strategy prioritizes:

1. **Core functionality first** - Get basic RTP working before advanced features
2. **Incremental delivery** - Each phase produces working, testable software
3. **Risk mitigation** - Address complex components early
4. **Integration readiness** - Design for Siphon integration from day one
5. **Performance focus** - Build with carrier-grade requirements in mind

### Timeline Estimate

- **Phase 0 (Foundation)**: 2-3 weeks
- **Phase 1 (Core RTP)**: 3-4 weeks
- **Phase 2 (Media Processing)**: 4-5 weeks
- **Phase 3 (Advanced Features)**: 5-6 weeks
- **Phase 4 (Carrier Grade)**: 4-5 weeks
- **Phase 5 (Polish & Scale)**: 3-4 weeks

**Total**: ~6 months for full carrier-grade implementation

---

## Development Philosophy

### Principles

1. **Rust-First Design**
   - Leverage ownership for zero-copy where possible
   - Use async/await for all I/O operations
   - Prefer compile-time safety over runtime checks

2. **Incremental Complexity**
   - Start with synchronous codec support before async transcoding
   - Implement basic mixing before sophisticated conference features
   - Get UDP/RTP working before adding SRTP/DTLS

3. **Test-Driven**
   - Unit tests for packet parsing, codecs, mixing algorithms
   - Integration tests for end-to-end call flows
   - Benchmarks for critical paths (packet forwarding, mixing, encoding)

4. **Production-Ready from Phase 1**
   - Logging and observability from day one
   - Graceful error handling
   - Configuration validation

5. **API-First**
   - Define API contracts early
   - Document endpoints as you build them
   - Versioned APIs (v1) from the start

---

## Phase Overview

### Visual Roadmap

```
Phase 0: Foundation          [████████] Core types, config, API skeleton
Phase 1: Core RTP            [████████] RTP/RTCP, sessions, port management
Phase 2: Media Processing    [████████] Codecs, transcoding, mixing
Phase 3: Advanced Features   [████████] Recording, DTMF, injection, WebRTC
Phase 4: Carrier Grade       [████████] SBC, HA, SIPREC, AI streaming
Phase 5: Polish & Scale      [████████] Optimization, kernel offload, video
                             └──────────────────────────────────────────┘
                                    6 months (estimated)
```

---

## Detailed Phase Breakdown

## Phase 0: Foundation (2-3 weeks)

**Goal**: Establish project structure, core types, and basic API framework.

### Deliverables

#### 1. Project Structure ✅ COMPLETED
- [x] Workspace Cargo.toml with all crates
- [x] Core crate scaffolding (17 crates)
- [x] Main binary entry point
- [x] Configuration system with TOML support

#### 2. forge-core (Week 1)
- [x] Core types: CallId, RoomId, ParticipantId, LegIdentifier
- [x] Error types with thiserror
- [x] Configuration structures
- [ ] Common traits: Codec, Encoder, Decoder
- [ ] Event system with tokio::sync::broadcast

**Tasks**:
```rust
// Define core traits
pub trait Encoder: Send + Sync {
    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>>;
    fn sample_rate(&self) -> u32;
    fn frame_size(&self) -> usize;
}

pub trait Decoder: Send + Sync {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>>;
    fn sample_rate(&self) -> u32;
}

// Event broadcasting
pub enum ForgeEvent {
    SessionCreated { call_id: CallId, ... },
    SessionTerminated { call_id: CallId, ... },
    ParticipantJoined { room_id: RoomId, ... },
    // ... more events
}
```

#### 3. forge-api (Week 2)
- [ ] Axum HTTP server setup
- [ ] Health check endpoint: GET /health
- [ ] Basic session endpoints (stubs):
  - POST /v1/sessions
  - GET /v1/sessions/:id
  - DELETE /v1/sessions/:id
- [ ] Error responses with standard format
- [ ] CORS middleware
- [ ] Request logging with tracing

#### 4. Documentation (Week 2-3)
- [ ] README.md with quick start
- [ ] CONTRIBUTING.md with development setup
- [ ] API documentation template
- [ ] Architecture decision records (ADRs) directory

#### 5. CI/CD Setup (Week 3)
- [ ] GitHub Actions workflow:
  - cargo build
  - cargo test
  - cargo clippy
  - cargo fmt check
- [ ] Dependency caching
- [ ] Build matrix (stable, nightly)

**Exit Criteria**:
- ✅ Project compiles with `cargo build`
- [ ] Tests pass with `cargo test`
- [ ] API server starts and responds to /health
- [ ] Configuration loads from file

---

## Phase 1: Core RTP (3-4 weeks)

**Goal**: Implement reliable RTP/RTCP packet handling and session management.

### Deliverables

#### 1. forge-rtp (Week 1-2)
- [x] RTP header parsing with zero-copy where possible
- [x] RTP packet building
- [ ] RTCP packet types:
  - SR (Sender Report)
  - RR (Receiver Report)
  - SDES (Source Description)
  - BYE
- [ ] Sequence number tracking with rollover handling
- [ ] Timestamp validation
- [ ] SSRC collision detection

**Key Implementation**:
```rust
// RTP receiver with statistics
pub struct RtpReceiver {
    ssrc: u32,
    packets_received: u64,
    bytes_received: u64,
    packets_lost: u32,
    jitter_ms: f32,
    last_seq: u16,
    base_seq: u16,
    // ... more state
}

impl RtpReceiver {
    pub fn process_packet(&mut self, packet: &RtpPacket) -> Result<()> {
        // Update statistics
        // Detect loss
        // Calculate jitter
    }

    pub fn generate_rtcp_rr(&self) -> RtcpPacket {
        // Build receiver report
    }
}
```

#### 2. forge-engine Core (Week 2-3)
- [ ] Port pool management:
  - Even/odd port pairs for RTP/RTCP
  - Concurrent allocation with DashMap
  - Port reservation and release
- [ ] Socket management:
  - UDP socket creation with socket2
  - TOS/DSCP setting
  - IPv4/IPv6 handling
- [ ] Session registry:
  - Create/read/update/delete sessions
  - Thread-safe access with Arc<RwLock<>>
  - Session timeout handling

**Port Pool**:
```rust
pub struct PortPool {
    range: RangeInclusive<u16>,
    allocated: DashSet<u16>,
    strategy: AllocationStrategy,
}

impl PortPool {
    pub async fn allocate_pair(&self) -> Result<(u16, u16)> {
        // Find available even port
        // Reserve even (RTP) and odd (RTCP)
    }
}
```

#### 3. Basic Media Session (Week 3-4)
- [ ] MediaSession struct with two legs (A and B)
- [ ] RTP forwarding loop:
  - Receive on leg A → forward to leg B
  - Receive on leg B → forward to leg A
- [ ] Symmetric RTP learning
- [ ] Basic packet validation
- [ ] Session statistics

**RTP Forwarding**:
```rust
async fn forward_rtp_task(
    rx_socket: Arc<UdpSocket>,
    tx_socket: Arc<UdpSocket>,
    remote_addr: Arc<RwLock<Option<SocketAddr>>>,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        match rx_socket.recv_from(&mut buf).await {
            Ok((len, src_addr)) => {
                // Learn remote address (symmetric RTP)
                *remote_addr.write().await = Some(src_addr);

                // Forward to other leg
                if let Some(dst) = *remote_addr.read().await {
                    tx_socket.send_to(&buf[..len], dst).await.ok();
                }
            }
            Err(e) => tracing::error!("RTP recv error: {}", e),
        }
    }
}
```

#### 4. API Integration (Week 4)
- [ ] POST /v1/sessions - Create session with SDP offer
- [ ] Response with SDP answer containing allocated ports
- [ ] DELETE /v1/sessions/:id - Tear down session
- [ ] GET /v1/sessions/:id/stats - Session statistics

**Exit Criteria**:
- [ ] Two SIP phones can make a call through Forge
- [ ] Audio flows in both directions
- [ ] No audible artifacts or drops
- [ ] Sessions clean up properly
- [ ] RTCP reports generated correctly

---

## Phase 2: Media Processing (4-5 weeks)

**Goal**: Add codec support, transcoding, and basic audio conferencing.

### Deliverables

#### 1. forge-transcoding (Week 1-2)
- [ ] G.711 μ-law encoder/decoder
- [ ] G.711 A-law encoder/decoder
- [ ] Opus encoder/decoder (with opus crate)
- [ ] Resampler using rubato
- [ ] Transcoding pipeline:
  ```
  RTP → Decoder → PCM → Resampler → Encoder → RTP
  ```

**Codec Implementation**:
```rust
pub struct OpusCodec {
    encoder: opus::Encoder,
    decoder: opus::Decoder,
    sample_rate: u32,
}

impl Encoder for OpusCodec {
    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        let mut output = vec![0u8; 4000];
        let len = self.encoder.encode(samples, &mut output)?;
        output.truncate(len);
        Ok(output)
    }
}
```

#### 2. Transcoding Integration (Week 2)
- [ ] Session-level transcoding configuration
- [ ] Per-leg codec selection
- [ ] Codec negotiation from SDP
- [ ] Async transcoding tasks
- [ ] Packet timing preservation

#### 3. forge-conference (Week 3-4)
- [ ] Conference room creation
- [ ] Participant management
- [ ] Audio mixer:
  - Sample-rate alignment (resample all to 48kHz)
  - Mix all participants except self
  - i32 accumulator to prevent overflow
  - Clipping to i16 range
- [ ] Basic VAD using energy threshold

**Audio Mixing**:
```rust
pub struct AudioMixer {
    sample_rate: u32,
    frame_size: usize,
}

impl AudioMixer {
    pub fn mix(&self, participants: &[Participant]) -> HashMap<ParticipantId, Vec<i16>> {
        let mut outputs = HashMap::new();

        for participant in participants {
            let mut mix = vec![0i32; self.frame_size];

            // Mix all other participants
            for other in participants {
                if other.id != participant.id {
                    for (i, sample) in other.audio_frame.iter().enumerate() {
                        mix[i] += *sample as i32;
                    }
                }
            }

            // Clip to i16 range
            let clipped: Vec<i16> = mix.iter()
                .map(|&s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
                .collect();

            outputs.insert(participant.id.clone(), clipped);
        }

        outputs
    }
}
```

#### 4. Conference API (Week 4-5)
- [ ] POST /v1/conferences - Create room
- [ ] POST /v1/conferences/:id/participants - Add participant
- [ ] DELETE /v1/conferences/:id/participants/:pid - Remove participant
- [ ] POST /v1/conferences/:id/mute-all
- [ ] GET /v1/conferences/:id - Room status

#### 5. forge-sdp (Week 5)
- [ ] SDP parser (basic, for common cases)
- [ ] SDP builder
- [ ] Codec extraction
- [ ] Port/address handling
- [ ] Attribute parsing

**Exit Criteria**:
- [ ] Two G.711 endpoints can call through Forge
- [ ] Opus endpoint can call G.711 endpoint (transcoding works)
- [ ] 3+ participants can join a conference
- [ ] Each participant hears all others
- [ ] No audio quality degradation

---

## Phase 3: Advanced Features (5-6 weeks)

**Goal**: Recording, DTMF, audio injection, and WebRTC support.

### Deliverables

#### 1. forge-recording (Week 1-2)
- [ ] Recording manager
- [ ] Media tap interface
- [ ] WAV encoder
- [ ] Opus/OGG encoder
- [ ] File storage backend
- [ ] Recording per leg
- [ ] Stereo mixed recording
- [ ] Pause/resume

**Recording Architecture**:
```rust
pub struct RecordingManager {
    recordings: DashMap<RecordingId, Arc<Recording>>,
    storage: Arc<dyn RecordingStorage>,
}

pub trait RecordingStorage: Send + Sync {
    async fn write(&self, id: &RecordingId, data: &[u8]) -> Result<()>;
    async fn finalize(&self, id: &RecordingId) -> Result<PathBuf>;
}
```

#### 2. forge-dtmf (Week 2-3)
- [ ] RFC 2833 parsing
- [ ] RFC 2833 generation
- [ ] Goertzel algorithm for in-band detection
- [ ] Digit buffer with timeouts
- [ ] DTMF relay (in-band ↔ RFC 2833)

#### 3. forge-injection (Week 3-4)
- [ ] File playback with symphonia
- [ ] Tone generation (DTMF, comfort noise)
- [ ] TTS integration (Google, AWS, Azure)
- [ ] Mix modes: Mix, Replace, Duck
- [ ] Fade in/out

#### 4. forge-webrtc (Week 4-6)
- [ ] ICE agent (host candidates only first)
- [ ] STUN client
- [ ] DTLS handshake with openssl
- [ ] SRTP key derivation
- [ ] SDP munging for WebRTC
- [ ] WebRTC ↔ SIP bridge

**WebRTC Flow**:
```
1. Client → Forge: WebSocket SDP offer
2. Forge: Perform ICE gathering
3. Forge: Setup DTLS-SRTP
4. Forge → Client: SDP answer
5. Forge ↔ Client: ICE connectivity checks
6. Forge ↔ Client: DTLS handshake
7. Forge ↔ Client: SRTP media flow
```

**Exit Criteria**:
- [ ] Call recording works for both legs
- [ ] DTMF detection works for in-band and RFC 2833
- [ ] Can play audio file into call
- [ ] TTS works with at least one provider
- [ ] WebRTC client can call SIP endpoint through Forge

---

## Phase 4: Carrier Grade (4-5 weeks)

**Goal**: SBC features, SIPREC, AI streaming, and high availability.

### Deliverables

#### 1. forge-sbc (Week 1-2)
- [ ] Media proxy modes:
  - Pass-through
  - Proxy/relay
  - Transcode
  - Hairpin
- [ ] Topology hiding (SDP rewriting)
- [ ] Call admission control (CAC):
  - Max sessions
  - Bandwidth limits
  - Per-IP limits
- [ ] DoS protection:
  - Rate limiting
  - Packet validation
  - Auto-blacklisting
- [ ] RTP flags: symmetric, strict_source, media_handover

#### 2. forge-siprec (Week 2-3)
- [ ] Metadata XML parser/generator (quick-xml)
- [ ] SRC (Session Recording Client):
  - SIPREC INVITE generation
  - Media forking
  - Failover to backup SRS
- [ ] SRS (Session Recording Server):
  - Handle incoming SIPREC INVITEs
  - Store metadata
  - Record media streams
- [ ] SRTP key forwarding

#### 3. forge-ai-stream (Week 3-4)
- [ ] AI connector framework
- [ ] OpenAI Realtime API integration
- [ ] Audio format conversion (PCM16 ↔ G.711)
- [ ] VAD integration
- [ ] Barge-in detection
- [ ] Tool/function calling
- [ ] Event streaming via WebSocket

#### 4. forge-ha (Week 4-5)
- [ ] Session state serialization
- [ ] Redis state backend
- [ ] Heartbeat mechanism
- [ ] Failover coordinator
- [ ] VIP management (gratuitous ARP)
- [ ] Session restoration

**Exit Criteria**:
- [ ] CAC prevents overload
- [ ] SIPREC recording to external SRS works
- [ ] Can receive SIPREC sessions as SRS
- [ ] OpenAI Realtime conversation works
- [ ] HA failover completes in <5 seconds with minimal packet loss

---

## Phase 5: Polish & Scale (3-4 weeks)

**Goal**: Optimization, observability, video support, and kernel offload.

### Deliverables

#### 1. Observability (Week 1)
- [ ] Prometheus metrics:
  - Active sessions
  - Packets/bytes per second
  - Packet loss rate
  - Jitter distribution
  - CPU/memory usage
- [ ] OpenTelemetry traces
- [ ] Structured logging with tracing
- [ ] Health check with dependency status

#### 2. Performance Optimization (Week 1-2)
- [ ] Profile with perf / flamegraph
- [ ] Optimize hot paths:
  - RTP header parsing (zero-copy)
  - Codec transcoding (SIMD?)
  - Audio mixing (parallel?)
- [ ] Reduce allocations
- [ ] Connection pooling for AI/TTS APIs
- [ ] Benchmark suite

#### 3. forge-kernel (Week 2-3)
- [ ] xt_RTPENGINE interface
- [ ] eBPF/XDP program for packet forwarding
- [ ] Kernel module compilation/loading
- [ ] Fallback to userspace
- [ ] Performance comparison

#### 4. Video Support (Week 3-4)
- [ ] forge-video crate
- [ ] H.264 codec with ffmpeg bindings
- [ ] VP8 codec
- [ ] Video transcoding pipeline
- [ ] Video conferencing layouts:
  - Grid
  - Active speaker
  - Picture-in-picture
- [ ] Video recording (MP4, WebM)

**Exit Criteria**:
- [ ] Can handle 1000+ concurrent sessions
- [ ] Packet forwarding latency <1ms (p99)
- [ ] Conference mixing latency <20ms
- [ ] CPU usage <50% at target load
- [ ] Memory usage stable over 24h
- [ ] WebRTC video call works

---

## Testing Strategy

### Unit Tests
```rust
// Example: RTP packet parsing
#[test]
fn test_rtp_packet_parse() {
    let data = vec![/* ... */];
    let packet = RtpPacket::parse(data).unwrap();
    assert_eq!(packet.header.version(), 2);
    assert_eq!(packet.payload.len(), 160);
}
```

### Integration Tests
```rust
// Example: End-to-end call
#[tokio::test]
async fn test_basic_call_flow() {
    let engine = ForgeEngine::new(config).await.unwrap();

    // Create session with SDP offer
    let offer = /* ... */;
    let response = engine.create_session(offer).await.unwrap();

    // Simulate RTP packets
    send_rtp_packet(&response.local_addr, /* ... */).await;

    // Verify forwarding
    let received = recv_rtp_packet(&response.remote_addr).await;
    assert!(received.is_some());
}
```

### Benchmarks
```rust
// Example: Audio mixing performance
fn bench_audio_mixing(c: &mut Criterion) {
    let mixer = AudioMixer::new(48000, 960);
    let participants = create_test_participants(10);

    c.bench_function("mix_10_participants", |b| {
        b.iter(|| {
            mixer.mix(&participants)
        });
    });
}
```

### Load Testing
- Use SIPp for call generation
- Target: 1000 concurrent calls
- Measure: packet loss, jitter, CPU, memory
- Duration: 1 hour sustained load

---

## Integration Strategy

### Siphon Integration

Forge is designed to work seamlessly with the Siphon SIP stack:

```rust
// In Siphon: Handle INVITE
async fn handle_invite(invite: SipMessage) -> Result<SipResponse> {
    // Extract SDP from INVITE
    let sdp_offer = invite.body_as_sdp()?;

    // Send to Forge
    let forge_response = forge_client
        .post("/v1/sessions")
        .json(&CreateSessionRequest {
            call_id: invite.call_id(),
            sdp: sdp_offer,
        })
        .send()
        .await?;

    // Build 200 OK with Forge's SDP answer
    let sdp_answer = forge_response.sdp;
    Ok(SipResponse::ok().body(sdp_answer))
}
```

### API Client Library

Create `forge-client` crate for Siphon:
```rust
pub struct ForgeClient {
    base_url: String,
    client: reqwest::Client,
}

impl ForgeClient {
    pub async fn create_session(&self, request: CreateSessionRequest)
        -> Result<CreateSessionResponse> {
        // HTTP POST to /v1/sessions
    }

    pub async fn delete_session(&self, call_id: &CallId) -> Result<()> {
        // HTTP DELETE
    }
}
```

---

## Performance Targets

### Throughput
- **Concurrent sessions**: 1,000+ (single instance)
- **Packets per second**: 100,000+ (RTP forwarding)
- **Conference participants**: 100 per room
- **Transcoding streams**: 500+ concurrent

### Latency
- **RTP forwarding**: <1ms (p99)
- **RTCP generation**: <100ms
- **Conference mixing**: <20ms (20ms frame size)
- **Transcoding**: <40ms (includes decode, resample, encode)

### Resource Usage
- **Memory**: <2GB at 1000 sessions
- **CPU**: <50% at target load (16 cores)
- **Network**: Line rate (1Gbps+)

### Quality
- **Packet loss**: <0.1% (under normal conditions)
- **Jitter**: <30ms (p95)
- **MOS score**: >4.0 (G.711), >4.2 (Opus)

---

## Risk Management

### Technical Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| **SRTP complexity** | High | Use proven libraries (RustCrypto), extensive testing |
| **WebRTC interop** | Medium | Follow RFC specs strictly, test with multiple browsers |
| **Kernel module stability** | High | Extensive testing, userspace fallback always available |
| **Audio quality issues** | High | Professional audio testing, subjective MOS evaluation |
| **Performance bottlenecks** | Medium | Profile early and often, optimize hot paths |
| **Codec licensing** | Medium | Use open codecs (Opus, G.711), make G.729 optional |

### Dependencies

Critical external dependencies:
- **opus**: Widely used, stable
- **openssl**: Industry standard, well-maintained
- **tokio**: Production-ready async runtime
- **axum**: Modern, performant web framework

Mitigation: Pin dependency versions, monitor for security advisories.

### Integration Risks

- **Siphon API changes**: Define stable contract early, version APIs
- **FCP data model drift**: Regular sync meetings, shared type definitions
- **Deployment complexity**: Docker containers, Kubernetes manifests

---

## Success Criteria

### Phase 0-1: Foundation & Core RTP
✅ **Minimal Viable Product (MVP)**
- [ ] Two SIP endpoints can call through Forge
- [ ] Audio quality is good (no artifacts)
- [ ] Sessions create and tear down cleanly
- [ ] API is functional

### Phase 2: Media Processing
✅ **Production Ready (Basic)**
- [ ] Transcoding works (Opus ↔ G.711)
- [ ] Conference with 10+ participants
- [ ] No audio quality degradation
- [ ] Stable under load (100 sessions)

### Phase 3: Advanced Features
✅ **Feature Complete (Core)**
- [ ] Recording working
- [ ] DTMF detection/generation
- [ ] Audio injection
- [ ] WebRTC support

### Phase 4: Carrier Grade
✅ **Enterprise Ready**
- [ ] SBC features operational
- [ ] SIPREC compliant
- [ ] AI streaming functional
- [ ] HA with <5s failover

### Phase 5: Best in Class
✅ **Industry Leading**
- [ ] 1000+ concurrent sessions
- [ ] <1ms packet forwarding (p99)
- [ ] Video support
- [ ] Kernel offload working
- [ ] Comprehensive observability

---

## Appendix: Technology Stack

### Core Dependencies
- **Runtime**: Tokio (async/await)
- **HTTP**: Axum + Tower
- **Serialization**: Serde (JSON, TOML)
- **Logging**: tracing + tracing-subscriber
- **Error Handling**: thiserror + anyhow

### Media Processing
- **Codecs**: opus, custom G.711 implementation
- **Resampling**: rubato
- **File I/O**: symphonia
- **DSP**: TBD (for VAD, AGC, AEC)

### Networking
- **Sockets**: socket2
- **Crypto**: RustCrypto (aes, aes-gcm, hmac, sha1, sha2)
- **TLS/DTLS**: openssl (vendored)

### Storage & State
- **Concurrency**: parking_lot, dashmap
- **State**: Redis (for HA)

### Video (Phase 5)
- **Codecs**: ffmpeg-next (H.264, VP8)
- **Compositing**: Custom implementation

---

## Next Steps

1. ✅ **Foundation laid** - Project structure created
2. **Week 1-2**: Complete forge-core, start forge-api
3. **Week 3-4**: Implement forge-rtp and basic packet forwarding
4. **Week 5-6**: Add session management, get first call working
5. **Milestone 1**: Demo two SIP phones calling through Forge

---

**Document Status**: Living document, update as development progresses
**Last Updated**: December 2024
**Next Review**: After Phase 1 completion
