# Testing Forge with Two Softphones

This guide shows how to test the complete Forge Media Engine integration with two SIP clients.

## Setup

### Terminal 1: Forge Media Engine (already running)
```bash
cargo run
# Should show: ✓ API server listening on 0.0.0.0:8081
```

### Terminal 2: Forge SIP Server
```bash
cargo run -p forge-sip-server
```

Expected output:
```
🔨 Forge SIP Server - Multi-User Edition
Supports registration and call routing with Forge RTP

Configuration:
  SIP Listen: 0.0.0.0:5061
  Forge API: http://localhost:8081
  Domain: 127.0.0.1

✓ Forge API healthy (version 0.1.0)
✓ SIP listening on 0.0.0.0:5061

✓ Server ready!

Configure your softphones:
  Domain: 127.0.0.1
  Port: 5061
  Transport: UDP
  Username: alice (or bob, charlie, etc.)
  Password: <anything> (no auth)

Then call between users: alice calls bob
```

## Configure Softphones

### Softphone 1: Alice

**Linphone / Zoiper / MicroSIP / X-Lite:**
- **Display Name**: Alice
- **Username**: alice
- **Domain**: 127.0.0.1
- **Port**: 5061
- **Transport**: UDP
- **Password**: anything (no authentication)
- **Disable** authentication if possible

### Softphone 2: Bob

- **Display Name**: Bob
- **Username**: bob
- **Domain**: 127.0.0.1
- **Port**: 5061
- **Transport**: UDP
- **Password**: anything
- **Disable** authentication

## Testing Steps

### 1. Register Both Softphones

Start both softphones. They should automatically register.

**Server logs should show:**
```
← Register from 127.0.0.1:xxxxx
REGISTER from user 'alice' (expires: 3600s)
✓ User 'alice' registered
Registered users: 1
  - alice

← Register from 127.0.0.1:yyyyy
REGISTER from user 'bob' (expires: 3600s)
✓ User 'bob' registered
Registered users: 2
  - alice
  - bob
```

### 2. Make a Call (Alice → Bob)

From Alice's softphone, dial: **`bob`** or **`sip:bob@127.0.0.1`**

**Server logs should show:**
```
← Invite from 127.0.0.1:xxxxx
INVITE: alice → bob (call abc123-def456...)
Found callee 'bob' at 127.0.0.1:yyyyy
Creating Forge session...
✓ Forge session: RTP=10000 RTCP=10001
→ 200 OK to alice (RTP port 10000)
Starting RTP forwarding...
✓ RTP forwarding ACTIVE
  Both alice and bob should send RTP to 10000

← Ack from 127.0.0.1:xxxxx
ACK for call abc123-def456...
✓ Call established - RTP should be flowing through Forge
```

### 3. Verify RTP Forwarding

**Terminal 3: Monitor Forge session**
```bash
watch -n 1 'curl -s http://localhost:8081/v1/sessions | jq'
```

You should see:
```json
{
  "status": "ok",
  "data": {
    "sessions": [
      {
        "call_id": "abc123-def456...",
        "state": "Active",
        "rtp_port": 10000,
        "rtcp_port": 10001,
        "participant_a": {
          "id": "A",
          "packets_received": 150,
          "bytes_received": 24000,
          "packets_sent": 150,
          "bytes_sent": 24000
        },
        "participant_b": {
          "id": "B",
          "packets_received": 150,
          "bytes_received": 24000,
          "packets_sent": 150,
          "bytes_sent": 24000
        }
      }
    ],
    "count": 1
  }
}
```

**Key indicators RTP is flowing:**
- `packets_received` and `packets_sent` are incrementing
- Both participants A and B show traffic
- `state`: "Active"

### 4. Talk and Listen

- **Alice speaks** → Bob should hear
- **Bob speaks** → Alice should hear

RTP packets flow:
```
Alice → RTP to 10000 → Forge learns Alice's endpoint
Bob → RTP to 10000 → Forge learns Bob's endpoint
Forge → Forwards Alice's RTP to Bob
Forge → Forwards Bob's RTP to Alice
```

### 5. Check Forge Logs

**Terminal 1** (Forge) should show RTP activity:
```
[forge_rtp::forwarding] Starting RTP forwarding loop for session abc123...
```

### 6. End Call

Hang up from either Alice or Bob's softphone.

**Server logs:**
```
← Bye from 127.0.0.1:xxxxx
BYE for call abc123-def456...
Stopping Forge session...
✓ Forge session stopped
✓ Call terminated
```

**Forge API shows no sessions:**
```bash
curl http://localhost:8081/v1/sessions
# {"status":"ok","data":{"sessions":[],"count":0}}
```

## Validation Checklist

- [x] Both users registered (server shows 2 users)
- [x] Call connects (Alice calls Bob, Bob's phone rings)
- [x] RTP forwarding started (server logs show "RTP forwarding ACTIVE")
- [x] Packets flowing (curl shows incrementing counters)
- [x] Audio works (both parties can hear each other)
- [x] Call ends cleanly (BYE terminates, Forge session deleted)

## Troubleshooting

### Softphones can't register

**Issue**: Registration fails
**Solution**:
- Check server is running on port 5061: `netstat -an | grep 5061`
- Use UDP transport (not TCP)
- Disable authentication in softphone settings
- Use 127.0.0.1 as domain (not localhost)

### Call doesn't connect

**Issue**: Alice calls Bob but gets 404 Not Found
**Solution**:
- Verify both users are registered: check server logs
- Dial exactly `bob` or `sip:bob@127.0.0.1`
- Check softphone is using correct domain (127.0.0.1)

### No audio / RTP not flowing

**Issue**: Call connects but no audio
**Solution**:
1. Check Forge session is Active:
   ```bash
   curl http://localhost:8081/v1/sessions/<call-id>
   ```
2. Verify Forge ports are open: `netstat -an | grep 10000`
3. Check firewall isn't blocking UDP 10000-20000
4. Verify softphones are using PCMU/PCMA codecs (not opus/G.729)
5. Check softphones are sending to correct IP (should be 127.0.0.1)

### Packets sent but not received

**Issue**: One direction has packets, other doesn't
**Solution**:
- This is normal initially - Forge learns endpoints from first packet
- Wait 1-2 seconds for both endpoints to send RTP
- Check both softphones are "in call" state (not on hold)

## Advanced Testing

### Test with tcpdump

Monitor RTP packets:
```bash
# Terminal 4
sudo tcpdump -i lo -n udp port 10000
```

You should see:
```
IP 127.0.0.1.xxxxx > 127.0.0.1.10000: UDP, length 172
IP 127.0.0.1.10000 > 127.0.0.1.xxxxx: UDP, length 172
IP 127.0.0.1.yyyyy > 127.0.0.1.10000: UDP, length 172
IP 127.0.0.1.10000 > 127.0.0.1.yyyyy: UDP, length 172
```

### Multiple simultaneous calls

1. Register alice, bob, charlie
2. Alice calls Bob (uses port 10000)
3. Charlie calls Bob (uses port 10002)
4. Both calls active simultaneously

### Load testing

Use SIPp to generate load:
```bash
# Start Forge
cargo run

# Start SIP server
cargo run -p forge-sip-server

# Generate 10 concurrent calls
sipp -sn uac -r 10 -m 100 127.0.0.1:5061
```

## Success Criteria

✅ **Registration works** - Both users show in server logs
✅ **Call routing works** - Alice can call Bob by username
✅ **Forge integration works** - Sessions created with RTP ports
✅ **RTP forwarding works** - Packet counters increment on both sides
✅ **Audio works** - Both parties hear each other
✅ **Cleanup works** - BYE deletes Forge session, ports released

🎉 **Complete end-to-end SIP + RTP integration validated!**
