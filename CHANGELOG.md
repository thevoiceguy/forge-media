# Changelog

All notable changes to the Forge Media Engine project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2025-12-16

### Added - Conference AI Integration

#### forge-conference-processor v0.4.0
- **AI as Virtual Conference Participant** - AI joins as first-class participant
  - `ConferenceAIManager` lifecycle management
  - Virtual participant ID `__ai__` in AudioMixer
  - Bidirectional audio routing (conference ↔ AI)
  - Three async tasks: audio routing, response polling, DTMF forwarding
  - Automatic sample rate conversion (48kHz conference ↔ 16kHz AI)
  - State management (Connecting, Active, Speaking, Terminated)

- **DTMF Forwarding** - Automatic DTMF event routing to AI
  - Event bus subscription for participant DTMF events
  - Filters for "End" events to avoid duplicates
  - Forwards as text: "[DTMF: User pressed '5' via RFC 2833]"
  - Enables IVR scenarios in conferences
  - Support for RFC 2833, Inband, SIP INFO detection

- **Audio Modes**
  - **Mixed Mode** (✅ Implemented) - AI hears combined audio from all participants
    - Single audio stream
    - Lower CPU usage (~1-2% per session)
    - Good for conversation, Q&A, facilitation
  - **Individual Mode** (⚠️ Not Yet Implemented) - Per-participant labeled streams
    - Requires mixer enhancement for per-participant buffer access
    - Better speaker identification
    - Required for accurate transcription with speaker attribution
    - Higher CPU (~2-4% per session)

- **Conference Room Methods**
  - `attach_ai()` - Attach AI manager with event bus
  - `detach_ai()` - Remove AI and cleanup tasks
  - `has_ai()` - Check if AI is attached
  - `ai_state()` - Get current AI state

#### forge-engine v0.4.0
- **DTMF Forwarding Support** in AISessionManager
  - `send_dtmf_event()` method for manual forwarding
  - Supports all detection methods (RFC 2833, Inband, SIP INFO)
  - Integration with OpenAI Realtime API
  - Formats as text message to AI

#### forge-api v0.4.0
- **Conference AI Endpoints**
  - `POST /v1/conferences/:room_id/ai` - Attach AI to conference
    - Request: api_key, model, voice, instructions, temperature, audio_mode
    - Returns: room_id, state, model, voice, audio_mode, participants_heard
    - Status: 201 Created, 404 Not Found, 409 Conflict
  - `GET /v1/conferences/:room_id/ai` - Get AI status
    - Returns current state and configuration
    - Status: 200 OK, 404 Not Found
  - `DELETE /v1/conferences/:room_id/ai` - Detach AI
    - Graceful cleanup of tasks and resources
    - Status: 204 No Content, 404 Not Found

- **AppState Enhancement**
  - Added `core_event_bus` field for media events (DTMF)
  - Separate from WebSocket event bus
  - Passed to conference AI manager on attachment

### Documentation
- **Conference AI Integration Guide** (docs/CONFERENCE_AI_INTEGRATION.md)
  - 572-line comprehensive guide
  - Quick start with curl examples
  - Architecture diagrams (audio flow, component stack)
  - Complete API reference for all 3 endpoints
  - Audio modes comparison (Mixed vs Individual)
  - DTMF integration with IVR example
  - Configuration options and constants
  - 4 real-world examples:
    - Meeting assistant
    - Language translation
    - Conference moderator
    - Dynamic attach/detach
  - Comprehensive troubleshooting guide
  - Best practices section

### Tests
- **Integration Tests** - 9 new tests in conference_ai_tests.rs
  - test_attach_ai_to_conference
  - test_attach_ai_already_attached_error
  - test_attach_ai_invalid_audio_mode
  - test_attach_ai_individual_mode_not_implemented
  - test_get_ai_status_not_attached
  - test_detach_ai_not_attached
  - test_attach_ai_missing_api_key
  - test_attach_ai_invalid_temperature
  - test_attach_ai_to_nonexistent_room

- **Test Coverage**
  - All 31 conference tests passing (22 existing + 9 new)
  - 4 unit tests in ai_manager.rs
  - Validation, error handling, status codes
  - Edge cases and state management

### Architecture
- **Event Bus Separation**
  - `crate::EventBus` - WebSocket conference state events
  - `forge_core::EventBus` - Media events (DTMF, audio)
  - Clear separation of concerns

- **Task Management**
  - Audio routing task: 20ms polling interval
  - AI response polling: 100ms interval
  - DTMF forwarding: Event-driven
  - Graceful task cleanup on detach

### Recording Integration
- AI audio automatically included in conference recordings
- AI is regular participant in mixer
- Room mix includes AI voice
- Can add AI metadata to recording info

### Use Cases
- Voice assistants in meetings
- Meeting moderation and facilitation
- Real-time translation
- IVR systems in conferences
- Meeting notes and summaries
- Q&A bots

### Changed
- forge-conference-processor: 0.3.0 → 0.4.0
- forge-engine: 0.2.0 → 0.4.0 (DTMF forwarding added)
- forge-api: 0.2.0 → 0.4.0

## [0.3.0] - 2025-12-16

### Added - Conference Features

#### forge-conference-processor v0.3.0
- **Audio Feedback System** - Play sound files at conference events
  - `AudioFeedbackPlayer` for loading and decoding WAV files
  - Support for 8, 16, 24, and 32-bit PCM WAV files
  - Automatic stereo-to-mono conversion
  - Sample rate resampling using linear interpolation
  - `ConferenceSounds` struct for pre-loaded conference sounds
  - Integration via virtual participant in mixer
  - Sounds: join, exit, alone, recording start/stop, PIN prompts, etc.

- **Capacity Management** - Control conference size and access
  - `max_channels` - Limit number of participants per room
  - Automatic capacity enforcement with `ConferenceFull` error
  - Per-room configuration overrides

- **Wait-for-Moderator** - Hold participants until host joins
  - `wait_for_moderator` flag in room configuration
  - Automatic waiting room management
  - Host tracking with `hosts` set
  - `WaitingForModerator` error for held participants
  - Automatic release when first host joins
  - Automatic hold when last host leaves

- **Meeting Requirements** - Enforce minimum participation
  - `min_users` - Minimum participants before meeting starts
  - `min_recording_participants` - Auto-start recording threshold
  - Automatic recording start when threshold reached

- **Conference Lock** - Control room access
  - `default_locked` - Lock conference by default
  - `is_locked` state management
  - `ConferenceLocked` error for denied entry

- **Room Configuration System** - Per-room customization
  - `RoomConfig` with optional overrides for all settings
  - `EffectiveRoomConfig` merging room + global defaults
  - Per-room PINs, capacity, DTMF, meeting requirements
  - Audio feedback sound paths (12 configurable sounds)

- **Helper Methods**
  - `is_host()`, `host_count()`, `waiting_count()`
  - `is_at_capacity()`, `meets_min_users_requirement()`
  - `get_effective_config()`, `waiting_participants()`
  - `promote_to_host()`

#### forge-api v0.3.0
- **Conference Configuration Endpoints**
  - `POST /v1/conferences/:room_id/configure` - Configure room settings
  - `GET /v1/conferences/:room_id/config` - Get room configuration

- **Participant Management Endpoints**
  - `GET /v1/conferences/:room_id/participants` - List with host status
  - `GET /v1/conferences/:room_id/waiting` - List waiting participants
  - `POST /v1/conferences/:room_id/participants/:id/promote` - Promote to host

- **Enhanced Participant Request**
  - `is_host` field in `AddParticipantRequest`
  - Direct host join support

### Configuration
- **conference.toml** - Comprehensive conference configuration file
  - Security settings (PINs, lockout, default locked state)
  - DTMF command bindings (participant and host commands)
  - Audio settings (sample rate, buffer size, VAD)
  - Recording settings (format, auto-record)
  - Capacity settings (max channels, wait for moderator)
  - Meeting requirements (min users, min recording participants)
  - Audio feedback (12 configurable sound file paths)
  - Extensive inline documentation (195 lines)

### Dependencies
- Added `hound = "3.5"` for WAV file decoding

### Tests
- 8 new tests for audio feedback system
- WAV loading, resampling, stereo conversion tests
- All 39 conference processor tests passing
- All API tests passing

## [Unreleased - AI Integration]

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
