# Forge Media - Interoperability Testing Guide

## Overview

This guide provides comprehensive test scenarios for validating Forge Media Engine against major SIP platforms. The goal is to ensure compatibility, identify edge cases, and document any platform-specific quirks.

**Testing Focus:**
- SDP offer/answer negotiation
- Codec compatibility and transcoding
- RTP media flow
- DTMF handling (RFC 2833 and inband)
- Conference bridging
- Recording functionality
- Error handling and recovery

---

## Prerequisites

### Forge Media Setup

1. **Build Forge Media:**
   ```bash
   cargo build --release
   ```

2. **Start Forge API Server:**
   ```bash
   RUST_LOG=forge=info ./target/release/forge-media \
     --bind-addr 0.0.0.0:8080 \
     --port-range-min 10000 \
     --port-range-max 20000
   ```

3. **Verify Server Running:**
   ```bash
   curl http://localhost:8080/health
   # Expected: {"status":"healthy"}
   ```

### Network Configuration

- **Forge Media IP:** `192.168.1.100` (example - use your actual IP)
- **RTP Port Range:** 10000-20000
- **SIP Port:** 5060 (if implementing SIP signaling)

**Important:** Ensure firewall allows:
- TCP/UDP 8080 (API)
- UDP 10000-20000 (RTP)
- UDP 5060 (SIP - if used)

---

## Test Environment Architecture

```
┌─────────────┐         ┌──────────────┐         ┌─────────────┐
│  Asterisk   │ <-----> │ Forge Media  │ <-----> │ FreeSWITCH  │
│ 192.168.1.10│         │ 192.168.1.100│         │192.168.1.20 │
└─────────────┘         └──────────────┘         └─────────────┘
       │                        │                        │
       │                        │                        │
       └────────────────────────┴────────────────────────┘
                                │
                         ┌──────────────┐
                         │   Kamailio   │
                         │ (SIP Proxy)  │
                         │192.168.1.30  │
                         └──────────────┘
```

---

## Platform 1: Asterisk Interoperability

### Setup Instructions

#### 1. Install Asterisk (Ubuntu/Debian)

```bash
sudo apt-get update
sudo apt-get install asterisk asterisk-core-sounds-en
```

#### 2. Configure Asterisk for Testing

**`/etc/asterisk/sip.conf`:**
```ini
[general]
context=default
bindport=5060
bindaddr=0.0.0.0
transport=udp
qualify=yes
disallow=all
allow=ulaw     ; PCMU - G.711 μ-law
allow=alaw     ; PCMA - G.711 A-law
allow=opus     ; Opus codec
directmedia=no ; Force media through Asterisk

[forge-test]
type=friend
host=192.168.1.100
port=5060
context=forge-test
disallow=all
allow=ulaw
allow=alaw
dtmfmode=rfc2833
```

**`/etc/asterisk/extensions.conf`:**
```ini
[forge-test]
exten => 1000,1,Answer()
 same => n,Playback(demo-congrats)
 same => n,Hangup()

exten => 2000,1,Answer()
 same => n,Echo()
 same => n,Hangup()

exten => 3000,1,Answer()
 same => n,MusicOnHold()
 same => n,Hangup()
```

#### 3. Restart Asterisk

```bash
sudo systemctl restart asterisk
sudo asterisk -rx "sip show peers"  # Verify configuration
```

### Test Scenarios

#### Test 1: Basic Call Flow (PCMU)

**Objective:** Verify basic SDP negotiation and RTP flow with PCMU codec.

**Steps:**
1. Create session via Forge API with PCMU offer
2. Send INVITE from Asterisk to Forge
3. Verify 200 OK response with PCMU answer
4. Confirm bidirectional RTP flow
5. Send BYE to terminate

**Expected Results:**
- ✅ SDP negotiation completes successfully
- ✅ Codec agreed: PCMU (PT 0)
- ✅ RTP packets flow both directions
- ✅ Audio quality is clear
- ✅ Call terminates cleanly

**API Test:**
```bash
# Create session with PCMU offer
curl -X POST http://192.168.1.100:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "test-asterisk-1",
    "sdp_offer": "v=0\r\no=- 1234 1234 IN IP4 192.168.1.10\r\ns=-\r\nc=IN IP4 192.168.1.10\r\nt=0 0\r\nm=audio 10000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n",
    "sdp_profile": "audio_only"
  }'
```

**Verification:**
```bash
# Check RTP statistics
sudo tcpdump -i any -n udp portrange 10000-20000

# Asterisk CLI
asterisk -rx "sip show channels"
asterisk -rx "core show channels"
```

#### Test 2: Codec Negotiation (PCMA)

**Objective:** Test codec selection when multiple codecs offered.

**SDP Offer (Asterisk → Forge):**
```
v=0
o=- 5678 5678 IN IP4 192.168.1.10
s=-
c=IN IP4 192.168.1.10
t=0 0
m=audio 12000 RTP/AVP 0 8
a=rtpmap:0 PCMU/8000
a=rtpmap:8 PCMA/8000
```

**Expected SDP Answer (Forge → Asterisk):**
- Should select first matching codec (PCMU or PCMA based on preference)
- Should include only selected codec in answer

**Pass Criteria:**
- ✅ Single codec selected
- ✅ RTP flows with selected codec
- ✅ No codec switching mid-call

#### Test 3: Transcoding (PCMU ↔ PCMA)

**Objective:** Verify automatic transcoding between different codecs.

**Setup:**
1. Session A: Asterisk (PCMU) → Forge
2. Session B: Forge → Asterisk (PCMA)
3. Bridge sessions in Forge conference

**API Commands:**
```bash
# Create conference
curl -X POST http://192.168.1.100:8080/v1/conferences \
  -H "Content-Type: application/json" \
  -d '{"room_id": "transcode-test"}'

# Add Session A (PCMU)
curl -X POST http://192.168.1.100:8080/v1/conferences/transcode-test/participants \
  -H "Content-Type: application/json" \
  -d '{"participant_id": "session-a-pcmu"}'

# Add Session B (PCMA)
curl -X POST http://192.168.1.100:8080/v1/conferences/transcode-test/participants \
  -H "Content-Type: application/json" \
  -d '{"participant_id": "session-b-pcma"}'
```

**Pass Criteria:**
- ✅ Both participants hear each other clearly
- ✅ No audio artifacts or distortion
- ✅ Transcoding latency < 10ms
- ✅ CPU usage remains reasonable

#### Test 4: DTMF (RFC 2833)

**Objective:** Verify DTMF tone transmission and detection.

**Configuration:**
- Asterisk: `dtmfmode=rfc2833`
- Forge: Support RFC 2833 (payload type 101)

**Test Steps:**
1. Establish call with `telephone-event/8000` in SDP
2. Send DTMF tones: 0-9, *, #
3. Verify DTMF events received correctly

**SDP Requirements:**
```
m=audio 10000 RTP/AVP 0 101
a=rtpmap:0 PCMU/8000
a=rtpmap:101 telephone-event/8000
a=fmtp:101 0-15
```

**Verification:**
```bash
# Asterisk: Send DTMF
asterisk -rx "channel originate Local/1000@forge-test application SendDTMF 123#"

# Monitor RTP events
sudo tcpdump -i any -n -X udp port 10000
# Look for PT=101 packets
```

**Pass Criteria:**
- ✅ All DTMF tones (0-9, *, #) detected
- ✅ Tone duration accurate
- ✅ No missed or duplicate tones

#### Test 5: Conference Recording

**Objective:** Test conference recording with Asterisk participants.

**Steps:**
1. Create conference room
2. Add 3 Asterisk participants
3. Start room recording
4. Participants speak
5. Stop recording
6. Verify recording file

**API Commands:**
```bash
# Start recording
curl -X POST http://192.168.1.100:8080/v1/conferences/asterisk-conf/recording \
  -H "Content-Type: application/json" \
  -d '{"output_path": "asterisk-test.wav"}'

# Stop recording
curl -X DELETE http://192.168.1.100:8080/v1/conferences/asterisk-conf/recording
```

**Verification:**
```bash
# Check WAV file
file /var/lib/forge/recordings/asterisk-test.wav
soxi /var/lib/forge/recordings/asterisk-test.wav
# Play back
aplay /var/lib/forge/recordings/asterisk-test.wav
```

**Pass Criteria:**
- ✅ WAV file created successfully
- ✅ All participants audible in recording
- ✅ Audio quality preserved
- ✅ No dropouts or artifacts

---

## Platform 2: FreeSWITCH Interoperability

### Setup Instructions

#### 1. Install FreeSWITCH (Ubuntu/Debian)

```bash
# Add FreeSWITCH repository
wget -O - https://files.freeswitch.org/repo/deb/debian-release/fsstretch-archive-keyring.asc | sudo apt-key add -
echo "deb https://files.freeswitch.org/repo/deb/debian-release/ $(lsb_release -sc) main" | sudo tee /etc/apt/sources.list.d/freeswitch.list

sudo apt-get update
sudo apt-get install freeswitch-meta-all
```

#### 2. Configure FreeSWITCH

**`/etc/freeswitch/sip_profiles/external.xml`:**
```xml
<profile name="external">
  <settings>
    <param name="rtp-ip" value="192.168.1.20"/>
    <param name="sip-ip" value="192.168.1.20"/>
    <param name="sip-port" value="5060"/>
    <param name="inbound-codec-prefs" value="PCMU,PCMA,OPUS"/>
    <param name="outbound-codec-prefs" value="PCMU,PCMA,OPUS"/>
  </settings>
</profile>
```

**`/etc/freeswitch/dialplan/default.xml`:**
```xml
<extension name="forge_test">
  <condition field="destination_number" expression="^(4000)$">
    <action application="answer"/>
    <action application="playback" data="ivr/ivr-welcome.wav"/>
    <action application="hangup"/>
  </condition>
</extension>
```

#### 3. Start FreeSWITCH

```bash
sudo systemctl start freeswitch
sudo fs_cli -x "sofia status"
```

### Test Scenarios

#### Test 6: Opus Codec Support

**Objective:** Verify Opus codec negotiation with FreeSWITCH.

**SDP Offer:**
```
v=0
o=- 9012 9012 IN IP4 192.168.1.20
s=-
c=IN IP4 192.168.1.20
t=0 0
m=audio 14000 RTP/AVP 111
a=rtpmap:111 opus/48000/2
a=fmtp:111 minptime=10; useinbandfec=1
```

**Expected Behavior:**
- ✅ Forge accepts Opus codec
- ✅ High-quality wideband audio (48kHz)
- ✅ Forward error correction (FEC) enabled
- ✅ Low latency maintained

**Verification:**
```bash
# FreeSWITCH CLI
fs_cli -x "show channels"
fs_cli -x "uuid_audio_fork <uuid> start /tmp/recording.wav"
```

#### Test 7: Multi-Codec Negotiation

**Objective:** Test negotiation with all supported codecs.

**SDP Offer (FreeSWITCH → Forge):**
```
m=audio 16000 RTP/AVP 0 8 111 101
a=rtpmap:0 PCMU/8000
a=rtpmap:8 PCMA/8000
a=rtpmap:111 opus/48000/2
a=rtpmap:101 telephone-event/8000
```

**Pass Criteria:**
- ✅ Codec selected based on preference order
- ✅ DTMF support included (PT 101)
- ✅ No codec switching during call

#### Test 8: Conference with FreeSWITCH Participants

**Objective:** Test conference bridging with multiple FreeSWITCH participants.

**Setup:**
- 2 FreeSWITCH endpoints with different codecs
- 1 Asterisk endpoint
- All in same Forge conference

**Expected Results:**
- ✅ All participants hear each other
- ✅ Automatic transcoding where needed
- ✅ Conference mixing quality maintained
- ✅ No echo or feedback

---

## Platform 3: Kamailio Proxy Testing

### Setup Instructions

#### 1. Install Kamailio

```bash
sudo apt-get update
sudo apt-get install kamailio kamailio-mysql-modules
```

#### 2. Configure Kamailio

**`/etc/kamailio/kamailio.cfg`:**
```
#!KAMAILIO

####### Routing Logic ########

# Main request routing logic
request_route {
    # Forward all traffic to Forge Media
    rewritehostport("192.168.1.100:5060");
    route(RELAY);
}

route[RELAY] {
    if (!t_relay()) {
        sl_reply_error();
    }
    exit;
}
```

#### 3. Start Kamailio

```bash
sudo systemctl start kamailio
sudo kamctl monitor
```

### Test Scenarios

#### Test 9: RTP Proxy Mode

**Objective:** Verify SIP proxy correctly forwards SDP and RTP.

**Topology:**
```
Asterisk -> Kamailio -> Forge -> Kamailio -> FreeSWITCH
```

**Pass Criteria:**
- ✅ SIP messages proxied correctly
- ✅ SDP not corrupted during transit
- ✅ RTP flows directly (no proxy) or through Kamailio (if configured)
- ✅ NAT traversal works

#### Test 10: Load Balancing

**Objective:** Test load distribution across multiple Forge instances.

**Setup:**
- 2 Forge Media instances
- Kamailio with dispatcher module
- Send 10 concurrent calls

**Verification:**
```bash
# Kamailio: Check dispatcher status
kamctl dispatcher show

# Monitor call distribution
watch -n1 'curl -s http://192.168.1.100:8080/v1/metrics | jq .active_sessions'
```

**Pass Criteria:**
- ✅ Calls distributed evenly
- ✅ No dropped calls
- ✅ Session affinity maintained

---

## Compatibility Matrix Template

Use this matrix to track test results:

| Feature | Asterisk | FreeSWITCH | Kamailio | Notes |
|---------|----------|------------|----------|-------|
| **Codec Support** |
| PCMU (G.711 μ-law) | ⬜ | ⬜ | N/A | |
| PCMA (G.711 A-law) | ⬜ | ⬜ | N/A | |
| Opus | ⬜ | ⬜ | N/A | |
| **SDP Features** |
| Basic offer/answer | ⬜ | ⬜ | ⬜ | |
| Multi-codec negotiation | ⬜ | ⬜ | ⬜ | |
| Direction attributes | ⬜ | ⬜ | ⬜ | |
| **DTMF** |
| RFC 2833 | ⬜ | ⬜ | N/A | |
| Inband | ⬜ | ⬜ | N/A | |
| **Media Flow** |
| Bidirectional RTP | ⬜ | ⬜ | ⬜ | |
| Transcoding PCMU↔PCMA | ⬜ | ⬜ | N/A | |
| Conference mixing | ⬜ | ⬜ | N/A | |
| **Recording** |
| Session recording | ⬜ | ⬜ | N/A | |
| Conference recording | ⬜ | ⬜ | N/A | |
| **Reliability** |
| Call setup success | ⬜ | ⬜ | ⬜ | |
| Graceful termination | ⬜ | ⬜ | ⬜ | |
| Error recovery | ⬜ | ⬜ | ⬜ | |

**Legend:**
- ✅ Pass
- ❌ Fail
- ⚠️ Pass with issues
- ⬜ Not tested
- N/A: Not applicable

---

## Common Issues and Troubleshooting

### Issue 1: No RTP Packets

**Symptoms:**
- SDP negotiation succeeds
- No audio in either direction

**Diagnosis:**
```bash
# Check RTP ports open
sudo netstat -unlp | grep forge

# Monitor RTP traffic
sudo tcpdump -i any -n udp portrange 10000-20000

# Check firewall
sudo iptables -L -n -v
```

**Solutions:**
- Verify firewall allows UDP 10000-20000
- Check NAT configuration
- Ensure correct IP in SDP (c= line)
- Verify RTP port pool configured correctly

### Issue 2: One-Way Audio

**Symptoms:**
- Audio flows in only one direction

**Diagnosis:**
- Check SDP direction attributes (sendrecv, recvonly, sendonly)
- Verify symmetric RTP
- Check NAT/firewall asymmetry

**Solutions:**
```bash
# Verify SDP directions
curl http://localhost:8080/v1/sessions/<session_id> | jq .sdp_answer

# Test symmetric RTP
# Ensure replies come from same port as sent to
```

### Issue 3: Codec Mismatch

**Symptoms:**
- Call setup fails with "406 Not Acceptable"
- No common codec error

**Diagnosis:**
```bash
# Check supported codecs
curl http://localhost:8080/v1/sessions/<session_id> | jq .negotiated_codecs

# Verify SDP profiles
curl http://localhost:8080/health
```

**Solutions:**
- Ensure both endpoints support common codec
- Check SDP profile configuration
- Verify codec is enabled in Forge build

### Issue 4: Transcoding Quality Issues

**Symptoms:**
- Audio artifacts during transcoding
- Choppy or distorted audio

**Diagnosis:**
```bash
# Check CPU usage
top -p $(pgrep forge-media)

# Check transcoding metrics
curl http://localhost:8080/v1/metrics | jq .transcoding

# Monitor for packet loss
sudo tcpdump -i any -n -c 1000 udp portrange 10000-20000 | grep "length 0"
```

**Solutions:**
- Reduce concurrent transcoding sessions
- Check system resources (CPU, memory)
- Verify sample rate conversion settings
- Consider hardware acceleration

---

## Performance Benchmarks

Expected performance targets for interoperability scenarios:

| Metric | Target | Acceptable | Measurement Method |
|--------|--------|------------|-------------------|
| SDP negotiation time | < 1ms | < 5ms | API response time |
| Transcoding latency | < 5ms | < 10ms | RTP timestamp analysis |
| Conference mixing | < 20ms | < 50ms | End-to-end delay |
| Concurrent sessions | 100+ | 50+ | Load testing |
| Call setup success rate | > 99% | > 95% | Test suite results |
| Audio quality (MOS) | > 4.0 | > 3.5 | PESQ testing |

---

## Test Execution Checklist

### Pre-Testing

- [ ] Forge Media built and running
- [ ] Test platforms installed and configured
- [ ] Network connectivity verified
- [ ] Firewall rules configured
- [ ] Monitoring tools ready (tcpdump, wireshark)

### During Testing

- [ ] Document all test results in compatibility matrix
- [ ] Capture packet traces for failed tests
- [ ] Record audio samples for quality assessment
- [ ] Note any warnings or errors in logs

### Post-Testing

- [ ] Complete compatibility matrix
- [ ] Summarize findings
- [ ] Document platform-specific issues
- [ ] File bug reports for failures
- [ ] Update documentation with workarounds

---

## Next Steps

After completing interoperability testing:

1. **Update Documentation**
   - Add compatibility matrix to README
   - Document known issues
   - Provide platform-specific configuration examples

2. **Address Failures**
   - Prioritize critical failures
   - Implement fixes or workarounds
   - Retest affected scenarios

3. **Performance Testing (Sprint 6)**
   - Load testing with real platforms
   - Stress testing concurrent sessions
   - Profiling bottlenecks

---

## References

- **Asterisk Documentation:** https://wiki.asterisk.org/
- **FreeSWITCH Documentation:** https://freeswitch.org/confluence/
- **Kamailio Documentation:** https://www.kamailio.org/docs/
- **RFC 3261 (SIP):** https://tools.ietf.org/html/rfc3261
- **RFC 3264 (Offer/Answer):** https://tools.ietf.org/html/rfc3264
- **RFC 4566 (SDP):** https://tools.ietf.org/html/rfc4566
- **RFC 2833 (DTMF):** https://tools.ietf.org/html/rfc2833
