# Changelog

All notable changes to the Forge Media Engine project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added - AI Integration

#### forge-ai-stream
- **OpenAI Realtime API Connector** - Full WebSocket-based integration
  - Bidirectional audio streaming (PCM16, G.711 µ-law/A-law)
  - Session configuration (model, voice, instructions, temperature)
  - Voice Activity Detection (VAD) / turn detection
  - Function calling support with JSON schemas
  - Event streaming (transcription, function calls, interruptions)
  - Connection statistics and monitoring
  - 12 comprehensive tests

#### forge-engine - AI Integration Module
- **AISessionManager** - Lifecycle management for AI sessions
  - Session creation, attachment, detachment
  - Audio routing to/from AI
  - Event bus integration for DTMF forwarding
  - Session statistics and status monitoring
  - 18 comprehensive tests

- **Audio Routing** - Bidirectional RTP ↔ AI audio flow
  - RTP → AI: Audio tap from forwarding loop (non-blocking)
  - AI → RTP: Response injection with codec conversion
  - Automatic sample rate conversion (8kHz/16kHz/24kHz)
  - Linear interpolation resampler
  - G.711 µ-law/A-law and Opus encoding support
  - Special AI SSRC (0xA1A1A1A1) for tracking
  - 10 audio routing tests

- **DTMF Integration** - Automatic DTMF forwarding to AI
  - EventBus subscription for DTMF events
  - RFC 2833, Inband, SIP INFO detection methods
  - Sent as text to AI: "[DTMF: User pressed '5' via rfc2833]"
  - Enables IVR scenarios without custom programming

#### forge-api - AI REST Endpoints
- **POST /v1/sessions/:id/ai** - Attach AI to session
- **GET /v1/sessions/:id/ai** - Get AI status and statistics
- **DELETE /v1/sessions/:id/ai** - Detach AI from session
- **POST /v1/sessions/:id/ai/function-response** - Send function results
- Complete request/response validation
- Error handling and status codes

#### forge-siprec - AI Recording Metadata
- **add_ai_metadata()** - Add AI provider/model/voice to recordings
- **add_ai_participant()** - Create virtual AI participant in SIPREC
- Extension data for compliance recording
- 6 new tests for AI metadata

#### forge-codecs
- Made G.711 encode functions public for AI audio encoding

### Documentation
- **AI Integration Guide** (docs/AI_INTEGRATION.md)
  - Complete API reference
  - Configuration guide
  - Audio routing architecture
  - DTMF integration examples
  - SIPREC recording with AI metadata
  - Troubleshooting guide
  - Performance considerations
  - Security best practices

- **Example Scripts** (examples/)
  - ai_integration_example.sh - Basic AI voice agent
  - ai_ivr_example.sh - IVR with DTMF
  - ai_function_calling_example.sh - Function calling demo
  - README.md - Complete examples guide

- **Updated README.md**
  - AI Integration section with quick start
  - API reference for AI endpoints
  - Link to comprehensive guide

### Test Coverage
- forge-ai-stream: 12 tests (OpenAI connector)
- forge-engine: 28 tests (18 AI + 10 audio routing)
- forge-siprec: 6 new AI metadata tests
- All 46 new tests passing

### Changed
- forge-ai-stream version: 0.1.0 → 0.2.0
- forge-engine version: 0.1.0 → 0.2.0
- forge-api version: 0.1.0 → 0.2.0
- forge-codecs version: 0.1.0 → 0.1.1

## [0.2.0] - 2025-12-15

### Added - forge-dtmf

#### Core Features
- **Digit Buffer with Timeouts** - `DtmfBuffer` for IVR digit collection
  - Configurable inter-digit timeout (default: 3 seconds)
  - Total collection timeout (default: 30 seconds)
  - Maximum digits limit
  - Terminator digit support (e.g., # to end input)
  - 8 comprehensive tests

- **DTMF Relay** - `DtmfRelay` for method conversion
  - Inband audio → RFC 2833 conversion
  - RFC 2833 → Tone generation instructions
  - Multi-digit state management
  - 5 comprehensive tests

- **Unified DTMF Processor** - `DtmfProcessor` high-level API
  - Single interface combining all detection methods
  - Automatic deduplication with priority handling
  - Optional digit buffering
  - Flexible configuration
  - 6 comprehensive tests

- **Integration Tests** - 5 end-to-end tests
  - Complete DTMF flow validation
  - Multi-method deduplication
  - Relay conversion testing
  - All 16 DTMF digits validation

#### Test Coverage
- Total: 47 tests passing (up from 22)
- All DTMF digits (0-9, *, #, A-D) validated
- RFC 2833 parsing and generation
- Goertzel inband detection
- Event deduplication with priority
- Digit buffering and timeouts
- Method conversion and relay

### Documentation
- Comprehensive inline documentation for all new modules
- Usage examples in module docs
- Integration test examples

### Changed
- forge-dtmf version bumped to 0.2.0
- Improved module organization with new exports

## [0.1.0] - 2025-12-15

### Added - forge-siprec

#### Phase 2: SIPREC Implementation (RFC 7865/7866)

- **SIP Message Builder** - Complete SIP signaling for SIPREC
  - INVITE, BYE request generation
  - SDP with multipart MIME (SDP + metadata XML)
  - Dialog state machine (Initial → Confirmed → Terminated)
  - Call-ID, tags, CSeq management

- **Metadata Generation** - RFC 7865 XML metadata
  - RecordingSession with participants and streams
  - Participant roles (caller, callee)
  - Media stream descriptions
  - RTP session information
  - Extension data support
  - XML serialization/deserialization

- **RTP Media Forking** - MediaForker for stream duplication
  - ForkedStream management
  - Packet forwarding to multiple destinations
  - Statistics tracking (packets, bytes, errors)
  - Stream lifecycle management

- **SRTP Key Management** - SrtpKeyManager for secure recording
  - SDP crypto attribute parsing (a=crypto)
  - Key material extraction (master key + salt)
  - Multiple crypto suite support (AES-CM-128, AEAD-AES-128/256-GCM)
  - SSRC-based key lookup
  - Base64 encoding/decoding

- **SRC Implementation** - SessionRecordingClient
  - Full recording session lifecycle
  - Metadata generation with participants
  - SIP dialog management
  - SDP generation with media streams
  - SRTP key extraction and forwarding
  - Failover to backup SRS support

- **SRS Implementation** - SessionRecordingServer
  - Recording session acceptance
  - Length-prefixed RTP packet storage
  - Metadata XML persistence
  - Session statistics (packet/byte counts)
  - Concurrent file I/O with tokio
  - Session limit enforcement

- **End-to-End Tests** - 3 comprehensive integration tests
  - Full SRC→SRS recording flow
  - SRTP key extraction validation
  - Primary/backup SRS failover

#### Test Coverage
- 40 tests passing across all modules
- RFC 7865/7866 compliance validated
- All SIPREC components tested

### Added - forge-dtmf (Initial)

- **RFC 2833** - Telephone-event RTP payload
  - Event parsing and generation
  - Rfc2833Detector, Rfc2833Generator
  - All 16 DTMF digits support

- **Inband Detection** - Goertzel algorithm
  - GoertzelDetector for frequency analysis
  - Configurable thresholds
  - 100ms minimum detection duration

- **Event Deduplication** - DtmfDeduplicator
  - Priority-based filtering
  - 100ms deduplication window

#### Test Coverage
- 22 tests passing for core functionality

[Unreleased]: https://github.com/forge-media/forge-media/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/forge-media/forge-media/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/forge-media/forge-media/releases/tag/v0.1.0
