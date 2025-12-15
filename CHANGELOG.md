# Changelog

All notable changes to the Forge Media Engine project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
