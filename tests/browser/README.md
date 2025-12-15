# Browser Interoperability Tests for Forge Media WebRTC

This directory contains browser-based tests for validating WebRTC functionality across different browsers.

## Test Files

- **webrtc-test.html** - Interactive WebRTC test page with full UI
- **serve.py** - Simple HTTP server for serving test pages

## Prerequisites

1. **Forge Media Server Running**
   ```bash
   cargo run --release --features dtls
   ```
   Default URL: `http://localhost:8080`

2. **Modern Browser**
   - Chrome 74+ (recommended)
   - Firefox 66+
   - Safari 12.1+
   - Edge 79+

## Quick Start

### Option 1: Using Python Server (Recommended)

```bash
# From the tests/browser directory
python3 serve.py

# Open in browser:
# http://localhost:8000/webrtc-test.html
```

### Option 2: Direct File Access

Some browsers (Chrome) restrict WebRTC features when opening `file://` URLs.
Use a local server instead.

## Running Tests

### 1. Basic Connection Test

1. Open `webrtc-test.html` in your browser
2. Verify the "Configuration" section shows correct:
   - API URL: `http://localhost:8080`
   - STUN servers configured
   - Browser detected correctly
3. Click "Create Connection"
4. Watch the Event Log for:
   - ✅ "Connection created: webrtc-XXXXX"
   - ✅ "Set remote SDP offer"
   - ✅ "Created local SDP answer"
   - ✅ "Sent SDP answer to server"
5. Verify Statistics section shows:
   - ICE State: "connected" or "completed"
   - Connection State: "connected"
   - Candidates: > 0
   - Round Trip Time: < 100ms (local)

### 2. Audio Test

1. After connection established, click "Start Microphone"
2. Allow microphone permission when prompted
3. Watch Event Log for:
   - ✅ "Microphone access granted"
   - ✅ "Added audio track to connection"
4. Verify audio controls appear
5. If testing with another peer, verify audio is heard

### 3. ICE Candidate Gathering

1. During connection, monitor Event Log
2. Verify ICE candidates are logged:
   - "ICE candidate: candidate:..."
   - Multiple candidates should be gathered
3. Check Statistics:
   - Candidates count should increment
   - ICE State should progress: new → checking → connected

### 4. Connection Stability

1. Keep connection open for 30 seconds
2. Monitor Statistics:
   - Round Trip Time should remain stable
   - Connection State should stay "connected"
   - No errors in Event Log
3. Click "Close Connection"
4. Verify clean shutdown:
   - "Connection closed" logged
   - All buttons reset correctly

## Browser-Specific Testing

### Chrome Testing

```bash
# Open Chrome with verbose logging
google-chrome --enable-logging --v=1 http://localhost:8000/webrtc-test.html

# Check WebRTC internals
# Open: chrome://webrtc-internals/
```

**Expected Results:**
- ✅ ICE gathering completes quickly (< 2s)
- ✅ DTLS handshake succeeds
- ✅ SRTP encryption active
- ✅ Audio flows bidirectionally

### Firefox Testing

```bash
# Open Firefox
firefox http://localhost:8000/webrtc-test.html

# Check WebRTC logs
# Open: about:webrtc
```

**Expected Results:**
- ✅ ICE candidates gathered
- ✅ Connection establishes
- ✅ Stats available in about:webrtc

### Safari Testing

```bash
# Open Safari (macOS only)
open -a Safari http://localhost:8000/webrtc-test.html
```

**Expected Results:**
- ✅ Microphone permission works
- ✅ ICE gathering succeeds
- ✅ Connection stable

**Known Limitations:**
- Safari may have stricter CORS policies
- Some WebRTC stats may not be available

## Automated Test Checklist

Use this checklist for systematic testing:

### Connection Establishment
- [ ] Server responds to connection creation
- [ ] SDP offer received and valid
- [ ] Local answer created successfully
- [ ] ICE candidates generated (minimum 1)
- [ ] ICE connectivity checks pass
- [ ] DTLS handshake completes
- [ ] Connection reaches "connected" state

### Media Flow
- [ ] Microphone access granted
- [ ] Audio track added to connection
- [ ] No audio glitches or dropouts
- [ ] Bidirectional audio works (if applicable)
- [ ] Can mute/unmute successfully
- [ ] Media stats show packets flowing

### Error Handling
- [ ] Invalid API URL shows error
- [ ] Network failure handled gracefully
- [ ] Microphone denial handled properly
- [ ] Connection closure is clean
- [ ] Reconnection works after failure

### Performance
- [ ] RTT < 100ms (local)
- [ ] RTT < 500ms (remote)
- [ ] No packet loss visible in stats
- [ ] Connection stable for 1 minute
- [ ] No memory leaks (check browser DevTools)

## Troubleshooting

### "Connection failed" Error

**Possible causes:**
1. Forge Media server not running
2. Wrong API URL in configuration
3. CORS not enabled on server
4. Firewall blocking connection

**Solution:**
```bash
# Check server is running
curl http://localhost:8080/health

# Check WebRTC endpoint
curl -X POST http://localhost:8080/v1/webrtc/connections \
  -H "Content-Type: application/json" \
  -d '{"stun_servers": ["stun:stun.l.google.com:19302"]}'
```

### "Microphone access denied"

**Solution:**
- Check browser permissions (chrome://settings/content/microphone)
- Ensure page is served over HTTPS or localhost
- Try different browser

### ICE Connection Failed

**Possible causes:**
1. STUN server unreachable
2. Firewall blocking UDP
3. Network restrictions

**Solution:**
```bash
# Test STUN server connectivity
nc -u stun.l.google.com 19302

# Try different STUN servers
stun:stun.l.google.com:19302
stun:stun1.l.google.com:19302
stun:stun2.l.google.com:19302
```

### No Audio Received

**Possible causes:**
1. Track not added properly
2. SRTP keys mismatch
3. Audio element not playing

**Solution:**
- Check browser console for errors
- Verify audio element has srcObject
- Check remote track in chrome://webrtc-internals/

## Multi-Browser Testing

### Side-by-Side Test

1. Open test page in Chrome
2. Create connection in Chrome
3. Open same page in Firefox
4. Create different connection in Firefox
5. Verify both work independently

### Sequential Testing

```bash
# Chrome
google-chrome http://localhost:8000/webrtc-test.html

# Wait for connection to establish and close

# Firefox
firefox http://localhost:8000/webrtc-test.html

# Wait for connection to establish and close

# Safari (macOS)
open -a Safari http://localhost:8000/webrtc-test.html
```

## Reporting Issues

When reporting issues, include:

1. **Browser Info**: Name and version (shown in test page)
2. **Event Log**: Copy full log from test page
3. **Statistics**: Screenshot of stats section
4. **Server Logs**: Relevant forge-media server output
5. **Browser Console**: Any JavaScript errors
6. **Network Tab**: Failed requests (if any)

## Test Coverage

This test suite validates:

- ✅ WebRTC API compatibility
- ✅ SDP offer/answer exchange
- ✅ ICE candidate gathering
- ✅ DTLS handshake
- ✅ SRTP encryption
- ✅ Media track handling
- ✅ Connection state management
- ✅ Error handling
- ✅ Statistics collection

## Next Steps

After validating browser tests:

1. Run integration tests: `cargo test --package forge-webrtc`
2. Check metrics: `curl http://localhost:9090/metrics | grep webrtc`
3. Review server logs for any warnings
4. Test with real STUN/TURN servers for production

## References

- [WebRTC API Specification](https://www.w3.org/TR/webrtc/)
- [RTCPeerConnection](https://developer.mozilla.org/en-US/docs/Web/API/RTCPeerConnection)
- [getUserMedia](https://developer.mozilla.org/en-US/docs/Web/API/MediaDevices/getUserMedia)
- [Forge Media WebRTC Documentation](../../docs/webrtc.md)
