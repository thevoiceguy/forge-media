# Forge Test Daemon

A demonstration tool that shows how to integrate the Forge Media Engine HTTP API into a SIP application.

## Overview

This test daemon provides a complete example of:
- Connecting to the Forge Media Engine HTTP API
- Creating media sessions via REST endpoints
- Managing RTP forwarding lifecycle
- Proper cleanup and resource deallocation

## Features

### ForgeClient HTTP Wrapper

The `forge_client.rs` module provides a clean, type-safe Rust client for the Forge API:

```rust
let client = ForgeClient::new("http://localhost:8081");

// Health check
let health = client.health_check().await?;

// Create session
let session = client.create_session("call-id-123").await?;
// Returns: SessionResponse with call_id, state, rtp_port, rtcp_port

// Start RTP forwarding
client.start_session("call-id-123").await?;

// Get session info with statistics
let session = client.get_session("call-id-123").await?;

// List all active sessions
let sessions = client.list_sessions().await?;

// Delete session (stops forwarding, deallocates ports)
client.delete_session("call-id-123").await?;
```

### Integration Tests

The daemon runs 5 automated tests:

1. **Create Session** - Allocates RTP/RTCP port pair
2. **Get Session** - Retrieves session information
3. **Start Session** - Activates RTP forwarding (state: Initializing → Active)
4. **List Sessions** - Shows all active sessions
5. **Delete Session** - Cleanup and port deallocation

## Running

### Prerequisites

Make sure the Forge Media Engine is running:

```bash
# Terminal 1: Start Forge
cargo run

# Wait for:
# ✓ API server listening on 0.0.0.0:8081
```

### Run Integration Tests

```bash
# Terminal 2: Run test daemon
cargo run -p forge-test-daemon
```

Expected output:

```
🔨 Forge Test Daemon - API Integration Test
Forge API: http://localhost:8081
Testing Forge API connection...
✓ Forge API is healthy
  Version: 0.1.0

=== Running Integration Tests ===

Test 1: Creating session...
✓ Session created successfully
  Call-ID: test-integration-call-001
  State: Initializing
  RTP Port: 10000
  RTCP Port: 10001

Test 2: Retrieving session info...
✓ Session info retrieved

Test 3: Starting RTP forwarding...
✓ RTP forwarding started
  State: Active

Test 4: Listing all active sessions...
✓ Active sessions: 1

=== Session Information ===

Active Session:
  Call-ID: test-integration-call-001
  State: Active
  RTP Port: 10000
  RTCP Port: 10001

Press Ctrl+C to cleanup and exit
```

Press Ctrl+C to trigger cleanup:

```
Shutting down...
Test 5: Deleting session...
✓ Session deleted successfully
  Ports deallocated
  RTP forwarding stopped

✓ Integration tests complete
```

## Integration with Siphon

To integrate Forge with a real SIP application (like Siphon), follow this pattern:

### 1. Create ForgeClient

```rust
use forge_client::ForgeClient;

let forge = Arc::new(ForgeClient::new("http://localhost:8081"));
```

### 2. On SIP INVITE - Create Session

```rust
// When receiving INVITE
let call_id = extract_call_id(&invite);
let session = forge.create_session(&call_id).await?;

// Use session.rtp_port and session.rtcp_port in SDP answer
let sdp = create_sdp_answer(session.rtp_port)?;
send_200_ok_with_sdp(&sdp);
```

### 3. On 200 OK - Start Forwarding

```rust
// After call is answered
forge.start_session(&call_id).await?;
// RTP forwarding is now active
```

### 4. During Call - Monitor Statistics

```rust
// Optional: Poll for statistics
let session = forge.get_session(&call_id).await?;
println!("Packets received: {}", session.participant_a.packets_received);
```

### 5. On BYE - Cleanup

```rust
// When call ends
forge.delete_session(&call_id).await?;
// Ports are deallocated, forwarding stopped
```

## Architecture

```
┌─────────────────┐
│  SIP Application│
│   (Siphon)      │
└────────┬────────┘
         │ HTTP REST API
         │
┌────────▼────────┐
│  Forge Media    │
│  Engine (8081)  │
└────────┬────────┘
         │ UDP RTP
         │
┌────────▼────────┐
│  RTP Endpoints  │
│  (10000-20000)  │
└─────────────────┘
```

## Key Concepts

### Symmetric RTP

Forge uses **symmetric RTP** which means:
- No endpoint configuration needed upfront
- Forge learns participant addresses from first RTP packet
- Bidirectional forwarding starts automatically
- Works seamlessly through NAT

### Port Allocation

- Each session gets a unique RTP/RTCP port pair
- Default range: 10000-20000 (configurable)
- Ports are automatically deallocated on session deletion
- Port pool prevents conflicts

### Session Lifecycle

```
Create → Initializing → Start → Active → Delete → (ports released)
```

- **Initializing**: Session created, ports allocated, no forwarding yet
- **Active**: RTP forwarding enabled, learning participant endpoints
- **Deleted**: Forwarding stopped, ports returned to pool

## Testing RTP Forwarding

Once the test daemon creates an active session on port 10000, you can test RTP forwarding:

### Manual Test with netcat

```bash
# Terminal 3: Participant A
echo "Hello from A" | nc -u localhost 10000

# Terminal 4: Participant B
echo "Hello from B" | nc -u localhost 10000

# Forge will relay packets between A and B once both endpoints are learned
```

### With Real RTP Clients

Point SIP clients (softphones, Asterisk, FreeSWITCH) to the allocated ports:

```
# In SDP
m=audio 10000 RTP/AVP 0 8
c=IN IP4 127.0.0.1
```

Forge will automatically:
1. Learn endpoint addresses from incoming RTP
2. Forward packets bidirectionally
3. Track statistics (packets, bytes)

## Files

- `src/main.rs` - Integration test runner
- `src/forge_client.rs` - HTTP API client wrapper
- `Cargo.toml` - Dependencies (tokio, reqwest, serde, tracing)

## Next Steps

To build a full SIP integration:

1. **Add SIP Stack**: Use siphon-rs (already added as submodule)
2. **Handle INVITE**: Create Forge sessions on incoming calls
3. **SDP Integration**: Extract/insert RTP ports in SDP offer/answer
4. **Call State**: Map SIP dialog state to Forge session lifecycle
5. **Error Handling**: Handle port exhaustion, network failures
6. **Statistics**: Poll session stats for monitoring/CDR

## Dependencies

- **Forge Media Engine**: Must be running on port 8081
- **Rust 1.75+**: For compilation
- **tokio**: Async runtime
- **reqwest**: HTTP client
- **serde**: JSON serialization

## Status

✅ All integration tests passing
✅ ForgeClient fully implemented
✅ Session lifecycle verified
✅ Port allocation/deallocation working
✅ RTP forwarding activation confirmed

**Ready for Siphon integration!**
