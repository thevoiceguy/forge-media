# Known Issues

This document tracks known issues, bugs, and technical debt in the Forge Media Engine.

**Last Updated:** 2025-12-13

---

## Critical Issues

None currently.

---

## High Priority Issues

None currently.

---

## Medium Priority Issues

### METRICS-001: Missing SRTP Packet Counters

**Status:** Resolved
**Severity:** Medium
**Component:** forge-rtp (SRTP), forge-api (Metrics)
**Discovered:** 2025-12-13 (Sprint 5)
**Resolved:** 2025-12-15

**Description:**
Prometheus metrics for SRTP packet counts are not implemented. According to the Sprint 5 plan (task 5.7), we should track:
- `forge_srtp_packets_encrypted_total`
- `forge_srtp_packets_decrypted_total`

**Location:**
- `crates/forge-rtp/src/srtp.rs` - `protect_rtp()` and `unprotect_rtp()` methods
- `crates/forge-rtp/src/srtp.rs` - `protect_rtcp()` and `unprotect_rtcp()` methods

**Impact:**
- Cannot monitor SRTP packet processing rates
- Cannot track encryption/decryption failures
- Missing observability for security-critical operations

**Resolution:**
Added comprehensive Prometheus metrics for SRTP/SRTCP packet tracking:

**RTP Metrics:**
- `forge_srtp_packets_encrypted_total` - Total RTP packets encrypted
- `forge_srtp_packets_decrypted_total` - Total RTP packets decrypted
- `forge_srtp_replay_attacks_blocked_total` - Replay attacks detected and blocked

**RTCP Metrics:**
- `forge_srtcp_packets_encrypted_total` - Total RTCP packets encrypted
- `forge_srtcp_packets_decrypted_total` - Total RTCP packets decrypted

**Implementation:**
- Added metrics dependency to forge-rtp/Cargo.toml
- Counters added to protect_rtp(), unprotect_rtp(), protect_rtcp(), unprotect_rtcp()
- Replay attack counter added to replay window check

**Related Files:**
```
crates/forge-rtp/src/srtp.rs:523  - protect_rtp()
crates/forge-rtp/src/srtp.rs:600  - unprotect_rtp()
crates/forge-rtp/src/srtp.rs:830  - protect_rtcp()
crates/forge-rtp/src/srtp.rs:927  - unprotect_rtcp()
```

---

### WEBRTC-001: ICE Candidate Count Not Exposed

**Status:** Resolved
**Severity:** Medium
**Component:** forge-webrtc, forge-ice
**Discovered:** 2025-12-13 (Sprint 5)
**Resolved:** 2025-12-15

**Description:**
The `PeerConnection` struct does not expose the number of gathered ICE candidates. The API route has a placeholder that returns 0.

**Location:**
- `crates/forge-webrtc/src/peer.rs` - PeerConnection struct
- `crates/forge-api/src/routes/webrtc.rs:191-197` - Placeholder code

**Impact:**
- Cannot report actual ICE candidate counts in metrics
- `forge_webrtc_ice_candidates_gathered` gauge always reports 0
- Limited visibility into ICE gathering success

**Resolution:**
Implemented ICE candidate count tracking:
1. Added `local_candidate_count()` method to `PeerConnection`
2. Updated `create_connection()` API route to report actual count
3. Metric `forge_webrtc_ice_candidates_gathered` now shows real values

**Implementation:**
- Added async method in PeerConnection that queries IceAgent
- Updated metrics in create_connection after SDP offer generation
- Gauge metric now reflects actual gathered candidates

**Related Files:**
```
crates/forge-webrtc/src/peer.rs:46       - PeerConnection struct
crates/forge-ice/src/agent.rs:147        - get_local_candidates()
crates/forge-api/src/routes/webrtc.rs:191 - Metric placeholder
```

---

## Low Priority Issues

### TEST-001: Browser Interoperability Tests Missing

**Status:** Resolved
**Severity:** Low (requires manual testing infrastructure)
**Component:** Testing
**Discovered:** 2025-12-13 (Sprint 5)
**Resolved:** 2025-12-15

**Description:**
No browser interoperability tests exist for WebRTC functionality. According to Sprint 5 task 5.4, we should test with Chrome, Firefox, and Safari.

**Location:**
- `crates/forge-webrtc/tests/` - Missing browser_interop.rs

**Impact:**
- Cannot verify browser compatibility
- Risk of incompatibility issues in production
- No automated regression testing with browsers

**Implementation Plan:**
1. Create HTML test page that:
   - Connects to API at `/v1/webrtc/connections`
   - Performs WebRTC negotiation
   - Verifies ICE, DTLS, audio flow
2. Document manual testing procedure
3. Consider using Selenium/WebDriver for automation
4. Test with:
   - Chrome (latest stable)
   - Firefox (latest stable)
   - Safari (latest stable on macOS)

**Related Files:**
```
crates/forge-webrtc/tests/browser_interop.rs - To be created
docs/testing/webrtc-browser-testing.md      - To be created
```

---

### TEST-002: WebRTC Integration Tests Incomplete

**Status:** Resolved
**Severity:** Low
**Component:** Testing
**Discovered:** 2025-12-13 (Sprint 5)
**Resolved:** 2025-12-15

**Description:**
WebRTC integration tests (Sprint 5 task 5.6) were basic. Needed comprehensive end-to-end tests covering:
- Full offer/answer negotiation with multiple candidates
- ICE connectivity checks with various network configurations
- DTLS handshake verification with fingerprint checks
- SRTP packet encryption/decryption roundtrip
- Connection state transitions
- Error handling and recovery

**Location:**
- `crates/forge-webrtc/tests/integration_test.rs` - Created comprehensive tests

**Impact:**
- Had limited test coverage for WebRTC flows
- Risk of regressions in connection establishment
- Could not verify edge cases

**Resolution:**
Created comprehensive integration test suite with 11 tests:

**Basic Functionality:**
- `test_peer_connection_creation` - Connection initialization
- `test_connection_id_uniqueness` - ID generation validation
- `test_dtls_fingerprint_format` - SHA-256 fingerprint validation

**SDP Testing:**
- `test_sdp_offer_generation` - Full SDP structure validation
- `test_sdp_ice_credentials` - ICE ufrag/password verification
- `test_sdp_dtls_setup` - DTLS setup attribute validation

**ICE Testing:**
- `test_ice_candidate_gathering` - Host candidate verification
- `test_add_ice_candidate` - Candidate addition validation

**State Management:**
- `test_connection_state_getters` - Local/remote SDP getters
- `test_multiple_offers_forbidden` - Invalid state transitions

**Performance:**
- `test_ice_gathering_performance` - Sub-1-second gathering benchmark

**Test Results:**
- All 11 integration tests pass
- 3 unit tests pass
- 1 doc test passes
- Total: 15 tests passing

**Related Files:**
```
crates/forge-webrtc/tests/integration_test.rs:1-235 - Full test suite
```

---

## Technical Debt

### DEBT-001: Unused Fields in PeerConnection

**Status:** Resolved
**Severity:** Low
**Component:** forge-webrtc
**Discovered:** 2025-12-13
**Resolved:** 2025-12-15

**Description:**
Several fields in `PeerConnection` struct were defined but never used:
- `dtls_context: Option<DtlsContext>` - Obsolete
- `rtp_socket: Option<Arc<RtpSocketPair>>` - Future use
- `stun_servers: Vec<String>` - Future use

Generated dead_code warnings during compilation.

**Location:**
- `crates/forge-webrtc/src/peer.rs:58` - dtls_context
- `crates/forge-webrtc/src/peer.rs:68` - rtp_socket
- `crates/forge-webrtc/src/peer.rs:76` - stun_servers

**Impact:**
- Code clutter
- Compiler warnings
- Confusion about intended usage

**Resolution:**
Cleaned up unused fields to eliminate warnings:

1. **Removed `dtls_context`**: Obsolete field - we use `dtls_connection` instead (added in DEBT-002 fix)
   - Removed field declaration
   - Removed initialization
   - Removed unused import

2. **Marked `rtp_socket` with `#[allow(dead_code)]`**: Will be used for RTP media flow
   - Added TODO comment: "Implement RTP media flow using this socket"
   - Marked with `#[allow(dead_code)]` to suppress warning

3. **Marked `stun_servers` with `#[allow(dead_code)]`**: Will be used for ICE restart/re-negotiation
   - Added TODO comment: "Use for ICE restart and re-negotiation"
   - Marked with `#[allow(dead_code)]` to suppress warning

**Verification:**
- `cargo check --package forge-webrtc` produces no warnings
- All tests still pass

**Related Files:**
```
crates/forge-webrtc/src/peer.rs:47-76 - PeerConnection struct (cleaned)
```

---

## Resolved Issues

### SRTP-001: AES-256-GCM Key Derivation (Resolved 2025-12-15)

**Issue:** SRTP encryption failed for AES-256-GCM profile with "Invalid Length" error. Root cause was hardcoded use of `Aes128` even for 32-byte keys.

**Resolution:** Modified `aes_cm_prf()` to dynamically select AES-128 or AES-256 based on master key length (16 vs 32 bytes).

**Verification:**
- Added 2 new tests for AES-256-GCM
- Uncommented 6 benchmark tests
- All tests pass

**Files Modified:**
- `crates/forge-rtp/src/srtp.rs` - Fixed aes_cm_prf(), added tests
- `benches/srtp_bench.rs` - Uncommented AES-256-GCM benchmarks

**Commit:** 5578239

---

### DEBT-002: DTLS Handshake Background Task (Resolved 2025-12-15)

**Issue:** DTLS handshake was created but not driven by a background task, preventing WebRTC media flow.

**Resolution:** Implemented complete DTLS background task system:
- Background task spawns after ICE succeeds
- Packet demultiplexing (STUN/DTLS/SRTP) based on RFC 5764
- Full handshake state machine with timeout handling
- SRTP key extraction when handshake completes

**Files Modified:**
- `crates/forge-webrtc/src/peer.rs` - Added DTLS task and drive functions
- `crates/forge-ice/src/agent.rs` - Added get_socket() method

**Commit:** 5578239

---

## Issue Status Definitions

- **Open:** Issue identified, not yet addressed
- **In Progress:** Actively being worked on
- **Blocked:** Waiting on external dependency or decision
- **Resolved:** Issue has been fixed
- **Won't Fix:** Issue acknowledged but will not be addressed

## Severity Definitions

- **Critical:** System is broken or has security vulnerability
- **High:** Major functionality impaired or significant user impact
- **Medium:** Feature incomplete or performance degraded
- **Low:** Minor issue or cosmetic problem

---

## Contributing

When adding issues to this log:

1. Assign a unique ID (COMPONENT-NNN)
2. Include severity and status
3. Provide clear description and location
4. Document impact and workarounds
5. Add implementation plan if known
6. Link related files with line numbers

When resolving issues:

1. Move to "Resolved Issues" section
2. Add resolution date and commit hash
3. Keep for historical reference (don't delete)
4. Update related documentation

---

*This document should be updated whenever new issues are discovered or existing issues are resolved.*
