# Known Issues

This document tracks known issues, bugs, and technical debt in the Forge Media Engine.

**Last Updated:** 2025-12-13

---

## Critical Issues

None currently.

---

## High Priority Issues

### SRTP-001: AES-256-GCM Key Derivation Failure

**Status:** Open
**Severity:** High
**Component:** forge-rtp (SRTP)
**Discovered:** 2025-12-13 (Sprint 5)

**Description:**
SRTP encryption fails for AEAD-AES-256-GCM profile with error "Failed to create AES cipher: Invalid Length". The AES-128-CM-HMAC-SHA1-80 and AEAD-AES-128-GCM profiles work correctly.

**Location:**
- `crates/forge-rtp/src/srtp.rs` - Key derivation or cipher initialization for AES-256-GCM
- `benches/srtp_bench.rs:105` - Where error surfaces during benchmarking

**Impact:**
- AES-256-GCM SRTP profile cannot be used
- Benchmarks for AES-256-GCM are disabled
- Limits cipher suite negotiation options for high-security requirements

**Workaround:**
- Use AEAD-AES-128-GCM or AES-128-CM-HMAC-SHA1-80 profiles
- AES-256-GCM tests commented out in `benches/srtp_bench.rs`

**Investigation Needed:**
1. Check key derivation length for AES-256-GCM (should be 32 bytes)
2. Verify `derive_session_keys()` handles 256-bit keys correctly
3. Check if encryption key extraction matches expected length
4. Review RFC 7714 Section 8.1 for key derivation specifics

**Related Files:**
```
crates/forge-rtp/src/srtp.rs:115  - derive_session_keys()
crates/forge-rtp/src/srtp.rs:88   - SrtpKeyMaterial::new()
benches/srtp_bench.rs:210         - Disabled AES-256-GCM tests
```

---

## Medium Priority Issues

### METRICS-001: Missing SRTP Packet Counters

**Status:** Open
**Severity:** Medium
**Component:** forge-rtp (SRTP), forge-api (Metrics)
**Discovered:** 2025-12-13 (Sprint 5)

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

**Workaround:**
- Use WebRTC connection-level metrics as proxy
- Monitor at application layer instead of SRTP layer

**Implementation Plan:**
1. Add `metrics` dependency to `forge-rtp/Cargo.toml`
2. Add counters in `protect_rtp()` and `unprotect_rtp()`:
   ```rust
   counter!("forge_srtp_packets_encrypted_total", 1);
   counter!("forge_srtp_packets_decrypted_total", 1);
   ```
3. Consider adding error counters:
   ```rust
   counter!("forge_srtp_encryption_errors_total", 1);
   counter!("forge_srtp_decryption_errors_total", 1);
   counter!("forge_srtp_replay_attacks_blocked_total", 1);
   ```

**Related Files:**
```
crates/forge-rtp/src/srtp.rs:523  - protect_rtp()
crates/forge-rtp/src/srtp.rs:600  - unprotect_rtp()
crates/forge-rtp/src/srtp.rs:830  - protect_rtcp()
crates/forge-rtp/src/srtp.rs:927  - unprotect_rtcp()
```

---

### WEBRTC-001: ICE Candidate Count Not Exposed

**Status:** Open
**Severity:** Medium
**Component:** forge-webrtc, forge-ice
**Discovered:** 2025-12-13 (Sprint 5)

**Description:**
The `PeerConnection` struct does not expose the number of gathered ICE candidates. The API route has a placeholder that returns 0.

**Location:**
- `crates/forge-webrtc/src/peer.rs` - PeerConnection struct
- `crates/forge-api/src/routes/webrtc.rs:191-197` - Placeholder code

**Impact:**
- Cannot report actual ICE candidate counts in metrics
- `forge_webrtc_ice_candidates_gathered` gauge always reports 0
- Limited visibility into ICE gathering success

**Implementation Plan:**
1. Add method to `PeerConnection`:
   ```rust
   pub async fn local_candidate_count(&self) -> usize {
       self.ice_agent.lock().await.get_local_candidates().len()
   }
   ```
2. Update API route to call this method
3. Update metric in `create_connection()` endpoint

**Related Files:**
```
crates/forge-webrtc/src/peer.rs:46       - PeerConnection struct
crates/forge-ice/src/agent.rs:147        - get_local_candidates()
crates/forge-api/src/routes/webrtc.rs:191 - Metric placeholder
```

---

## Low Priority Issues

### TEST-001: Browser Interoperability Tests Missing

**Status:** Open
**Severity:** Low (requires manual testing infrastructure)
**Component:** Testing
**Discovered:** 2025-12-13 (Sprint 5)

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

**Status:** Open
**Severity:** Low
**Component:** Testing
**Discovered:** 2025-12-13 (Sprint 5)

**Description:**
WebRTC integration tests (Sprint 5 task 5.6) are basic. Need comprehensive end-to-end tests covering:
- Full offer/answer negotiation with multiple candidates
- ICE connectivity checks with various network configurations
- DTLS handshake verification with fingerprint checks
- SRTP packet encryption/decryption roundtrip
- Connection state transitions
- Error handling and recovery

**Location:**
- `crates/forge-webrtc/tests/integration.rs` - Needs expansion

**Impact:**
- Limited test coverage for WebRTC flows
- Risk of regressions in connection establishment
- Cannot verify edge cases

**Implementation Plan:**
1. Expand existing integration tests
2. Add test cases for:
   - Multiple ICE candidates (host, srflx, relay)
   - ICE failure scenarios
   - DTLS handshake timeout/retry
   - Mismatched fingerprints
   - SRTP key rollover
3. Use property-based testing for robustness

**Related Files:**
```
crates/forge-webrtc/tests/integration.rs - Expand existing tests
```

---

## Technical Debt

### DEBT-001: Unused Fields in PeerConnection

**Status:** Open
**Severity:** Low
**Component:** forge-webrtc
**Discovered:** 2025-12-13

**Description:**
Several fields in `PeerConnection` struct are defined but never used:
- `dtls_context: Option<DtlsContext>`
- `rtp_socket: Option<Arc<RtpSocketPair>>`
- `stun_servers: Vec<String>`

Generates dead_code warnings during compilation.

**Location:**
- `crates/forge-webrtc/src/peer.rs:57` - dtls_context
- `crates/forge-webrtc/src/peer.rs:60` - rtp_socket
- `crates/forge-webrtc/src/peer.rs:66` - stun_servers

**Impact:**
- Code clutter
- Compiler warnings
- Confusion about intended usage

**Action Required:**
- Either implement usage of these fields
- Or remove them if not needed
- Or mark with `#[allow(dead_code)]` with TODO comment

**Related Files:**
```
crates/forge-webrtc/src/peer.rs:46-73 - PeerConnection struct
```

---

### DEBT-002: DTLS Handshake Not Driven by Background Task

**Status:** Open (Documented in code)
**Severity:** Low (Placeholder exists)
**Component:** forge-webrtc
**Discovered:** 2025-12-13

**Description:**
The DTLS handshake is created but not driven by a background task. See comment in code:
```rust
// TODO: In production, DTLS handshake should be driven by a background task
// that continuously processes incoming DTLS packets and sends outgoing ones.
```

**Location:**
- `crates/forge-webrtc/src/peer.rs:342-363` - DTLS creation but not driven

**Impact:**
- DTLS handshake will not complete
- Cannot establish secure media channel
- Connection will appear "connected" but no media flows

**Implementation Plan:**
1. Spawn background task after ICE succeeds
2. Task should:
   - Read DTLS packets from selected ICE pair
   - Call `DtlsConnection::handshake()` with incoming packets
   - Send outgoing packets over ICE pair
   - Extract SRTP keys when handshake completes
3. Update connection state based on DTLS completion

**Related Files:**
```
crates/forge-webrtc/src/peer.rs:342      - DTLS creation location
crates/forge-rtp/src/dtls.rs:387         - DtlsConnection::handshake()
```

---

## Resolved Issues

None yet.

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
