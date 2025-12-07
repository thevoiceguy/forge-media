# Forge SIP Server

A complete SIP User Agent Server (UAS) that integrates with Forge Media Engine for RTP forwarding.

## Overview

This server demonstrates **full SIP + RTP integration** using:
- **Siphon-RS**: SIP message parsing and UAS helpers
- **Forge Media Engine**: RTP forwarding via HTTP API
- **ForgeClient**: Type-safe Rust HTTP client for Forge API

## Features

### Complete Call Flow

```
SIP Client → INVITE → Forge SIP Server
                        ↓
                    Create Forge Session (allocate RTP ports)
                        ↓
SIP Client ← 100 Trying ←
SIP Client ← 180 Ringing ←
                        ↓
SIP Client ← 200 OK (with SDP) ←
                        ↓
                    Start Forge Session (activate RTP forwarding)
                        ↓
SIP Client → ACK →
                        ↓
               [Call Established - RTP flows through Forge]
                        ↓
SIP Client → BYE →
                        ↓
                    Delete Forge Session (cleanup ports)
                        ↓
SIP Client ← 200 OK ←
```

### Supported SIP Methods

- ✅ **INVITE** - Creates Forge session, allocates RTP ports, sends SDP answer
- ✅ **ACK** - Confirms call establishment
- ✅ **BYE** - Terminates call, deletes Forge session
- ✅ **CANCEL** - Cancels pending call
- ✅ **OPTIONS** - Basic keepalive/discovery

### Integration Points

1. **On INVITE** → `forge_client.create_session()` → Get RTP port for SDP
2. **After 200 OK** → `forge_client.start_session()` → Activate forwarding
3. **On BYE** → `forge_client.delete_session()` → Cleanup ports

## Running

### Prerequisites

**Terminal 1: Start Forge Media Engine**
```bash
cargo run
# Wait for: ✓ API server listening on 0.0.0.0:8081
```

### Start SIP Server

**Terminal 2: Start SIP Server**
```bash
cargo run -p forge-sip-server
```

Expected output:
```
🔨 Forge SIP Server - SIP+RTP Integration Example

Configuration:
  SIP Listen: 0.0.0.0:5060
  Forge API: http://localhost:8081
  Local IP: 127.0.0.1

Testing Forge API connection...
✓ Forge API is healthy (version 0.1.0)
✓ SIP transport listening on UDP 0.0.0.0:5060

✓ Forge SIP Server is ready
Waiting for SIP calls on 0.0.0.0:5060...

Test with a SIP client:
  sip:test@127.0.0.1:5060
```

## Testing

### With SIP Client (Linphone, Zoiper, etc.)

1. Configure your SIP client:
   - **Server**: `127.0.0.1:5060`
   - **Username**: anything (e.g., `test`)
   - **Transport**: UDP

2. Make a call to: `sip:test@127.0.0.1:5060`

3. Watch the logs:

```
← Invite from 127.0.0.1:xxxxx
Processing INVITE for call: abc123-def456...
Creating Forge session...
✓ Forge session created on ports RTP=10000 RTCP=10001
→ Sending 200 OK with SDP (RTP port 10000)
Starting Forge session...
✓ RTP forwarding active for call abc123-def456...

Received ACK for call: abc123-def456...
✓ Call abc123-def456 established

[RTP flows through Forge port 10000]

← Bye from 127.0.0.1:xxxxx
Processing BYE for call: abc123-def456...
Stopping Forge session...
✓ Forge session stopped, ports deallocated
✓ Call abc123-def456 terminated
```

### With SIPp (Load Testing)

```bash
# Basic INVITE scenario
sipp -sn uac 127.0.0.1:5060

# With RTP
sipp -sn uac -rtp_echo 127.0.0.1:5060
```

## Architecture

```
┌──────────────┐                    ┌─────────────────┐
│  SIP Client  │                    │  Forge SIP      │
│ (Softphone)  │ ←─── SIP ─────────→│  Server (5060)  │
└──────────────┘                    └────────┬────────┘
                                             │ HTTP API
                                             │
                                    ┌────────▼────────┐
                                    │  Forge Media    │
                                    │  Engine (8081)  │
                                    └────────┬────────┘
                                             │ UDP RTP
┌──────────────┐                    ┌────────▼────────┐
│  RTP         │ ←──── RTP ────────→│  RTP Ports      │
│  Endpoint A  │                    │  (10000-20000)  │
└──────────────┘                    └────────┬────────┘
                                             │ RTP
                                    ┌────────▼────────┐
                                    │  RTP            │
                                    │  Endpoint B     │
                                    └─────────────────┘
```

## Code Structure

### Main Components

- **`AppState`** - Holds UAS, ForgeClient, call tracking, socket
- **`CallState`** - Tracks active call with Forge session info
- **`handle_invite()`** - INVITE handler with Forge integration
- **`handle_bye()`** - BYE handler with cleanup
- **`create_sdp_answer()`** - Generates SDP with Forge RTP port

### Key Integration Pattern

```rust
// 1. Create Forge session
let forge_session = forge_client.create_session(&call_id).await?;

// 2. Use RTP port in SDP
let sdp = create_sdp_answer(&local_ip, forge_session.rtp_port)?;
send_200_ok_with_sdp(&sdp);

// 3. Start RTP forwarding
forge_client.start_session(&call_id).await?;

// 4. On call end, cleanup
forge_client.delete_session(&call_id).await?;
```

## SDP Example

The server generates SDP with Forge's allocated RTP port:

```sdp
v=0
o=forge-sip 1701234567 0 IN IP4 127.0.0.1
s=Forge SIP Session
c=IN IP4 127.0.0.1
t=0 0
m=audio 10000 RTP/AVP 0 8 101
a=rtpmap:0 PCMU/8000
a=rtpmap:8 PCMA/8000
a=rtpmap:101 telephone-event/8000
a=sendrecv
```

## Codecs Supported

- **PCMU** (G.711 μ-law) - Codec 0
- **PCMA** (G.711 A-law) - Codec 8
- **telephone-event** - DTMF (RFC 2833) - Codec 101

## Configuration

Current configuration in `main.rs`:

```rust
let sip_bind = "0.0.0.0:5060";      // SIP listen address
let forge_api_url = "http://localhost:8081";  // Forge API
let local_ip = "127.0.0.1";          // IP for SDP
```

For production:
- Auto-detect local IP
- Support multiple interfaces
- Add TLS transport
- Implement authentication

## Limitations

This is a **demonstration/example** server. For production:

- ❌ No SIP authentication
- ❌ No SIP registration
- ❌ Auto-accepts all calls (no user interaction)
- ❌ Single local IP only
- ❌ UDP transport only (no TCP/TLS)
- ❌ Basic SDP (no codec negotiation)
- ❌ No NAT traversal helpers

## Extending

### Add Authentication

```rust
let uas = UserAgentServer::new(local_uri, contact_uri)
    .with_authenticator(Arc::new(my_authenticator));

// In handle_invite:
if !request.has_authorization() {
    return send_401_challenge(&request);
}
```

### Add Registration

Integrate with siphon-rs `sip-registrar` crate:

```rust
use sip_registrar::BasicRegistrar;

let registrar = BasicRegistrar::new(location_store, authenticator);
// Handle REGISTER requests
```

### Add Codec Negotiation

Parse incoming SDP, match codecs, generate appropriate answer:

```rust
use sip_sdp::SdpSession;

let offer = SdpSession::parse(&request.body)?;
let answer = negotiate_codecs(&offer, &supported_codecs)?;
```

## Dependencies

- **sip-core**: SIP message types
- **sip-parse**: SIP message parsing/serialization
- **sip-uas**: User Agent Server helpers
- **forge-test-daemon**: ForgeClient HTTP wrapper
- **tokio**: Async runtime
- **dashmap**: Concurrent call tracking
- **chrono**: Timestamps for SDP

## Status

✅ SIP INVITE/ACK/BYE flow working
✅ Forge session lifecycle integrated
✅ RTP port allocation via Forge
✅ SDP generation with Forge ports
✅ Call state tracking
✅ Graceful shutdown with cleanup

**Ready for SIP client testing!**

## Next Steps

1. **Test with real SIP clients** (Linphone, Zoiper, X-Lite)
2. **Test RTP forwarding** with two clients calling through server
3. **Add authentication** for production security
4. **Implement REGISTER** for user management
5. **Add codec negotiation** for broader compatibility
6. **Support TCP/TLS** for enterprise deployments

## Support

For issues or questions:
- Check Forge Media Engine logs (Terminal 1)
- Check SIP Server logs (Terminal 2)
- Verify port 5060 is available
- Ensure Forge API is accessible on port 8081
