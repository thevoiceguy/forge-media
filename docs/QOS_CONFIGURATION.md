# QoS/TOS Configuration Guide

Forge Media Engine supports configurable Type of Service (TOS) / Differentiated Services Code Point (DSCP) marking for RTP/RTCP packets, enabling proper Quality of Service handling in networks.

## Overview

TOS/DSCP marking allows network devices (routers, switches) to prioritize voice and video traffic over other data. This is crucial for:
- **VoIP Quality**: Reducing latency and jitter for voice calls
- **SBC Deployments**: Ensuring consistent QoS marking regardless of incoming traffic
- **Enterprise Networks**: Proper traffic classification and prioritization

## Configuration Levels

Forge supports TOS configuration at two levels:

### 1. Global Default (config/forge.toml)

Set the default TOS value for all sessions:

```toml
[engine]
# TOS/DSCP value for QoS (0xB8 = EF - Expedited Forwarding for voice)
tos = 184  # 0xB8 in decimal
```

### 2. Per-Session Override (API)

Override TOS for specific sessions via the API:

```bash
curl -X POST http://localhost:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "voice-call-123",
    "tos": 184
  }'
```

## Common TOS/DSCP Values

| TOS (Hex) | TOS (Dec) | DSCP | Class | Use Case |
|-----------|-----------|------|-------|----------|
| 0xB8 | 184 | EF (46) | Expedited Forwarding | **Voice (default)** |
| 0xA0 | 160 | AF41 (34) | Assured Forwarding 4-1 | Video conferencing |
| 0x88 | 136 | AF31 (26) | Assured Forwarding 3-1 | Streaming video |
| 0x68 | 104 | AF21 (18) | Assured Forwarding 2-1 | Low-latency data |
| 0x00 | 0 | BE (0) | Best Effort | Default/no priority |

### DSCP to TOS Conversion

The relationship between DSCP and TOS:
```
TOS = DSCP << 2
DSCP = TOS >> 2
```

Examples:
- DSCP 46 (EF) = TOS 0xB8 (184)
- DSCP 34 (AF41) = TOS 0xA0 (160)
- DSCP 26 (AF31) = TOS 0x88 (136)

## API Reference

### Create Session with Custom TOS

**Endpoint**: `POST /v1/sessions`

**Request Body**:
```json
{
  "call_id": "optional-call-id",
  "tos": 184,
  "sdp": "optional SDP offer",
  "from_tag": "optional from tag",
  "to_tag": "optional to tag"
}
```

**Parameters**:
- `tos` (optional, number, 0-255): TOS/DSCP value for QoS marking
  - If not specified, uses global default from config (default: 184/0xB8/EF)
  - Valid range: 0-255
  - Common values documented above

**Example: Voice Call (EF)**
```bash
curl -X POST http://localhost:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "voice-123",
    "tos": 184
  }'
```

**Example: Video Call (AF41)**
```bash
curl -X POST http://localhost:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "video-123",
    "tos": 160
  }'
```

**Example: Best Effort (no priority)**
```bash
curl -X POST http://localhost:8080/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "test-123",
    "tos": 0
  }'
```

## SBC Use Case: TOS Override

In Session Border Controller (SBC) deployments, you may want to **override all incoming TOS markings** to ensure consistent QoS treatment for outgoing traffic, regardless of what the far end sends.

### Scenario

- **Incoming RTP**: May have `tos 0x0` (best effort) or `tos 0x80` (unknown)
- **Outgoing RTP**: Should always have `tos 0xB8` (EF) for proper voice prioritization

### Implementation

Forge automatically **rewrites TOS markings** on all outgoing packets:

1. **Receive**: RTP packet arrives with any TOS value
2. **Forward**: Forge sends it out with configured TOS value (global or per-session)
3. **Result**: All outgoing traffic has consistent QoS marking

This is exactly what you observed in your testing:
```
Incoming: tos 0x0, tos 0x80
Outgoing: tos 0xb8  ✅ (all packets marked correctly!)
```

### Configuration for SBC

**Option 1: Global override (all sessions)**
```toml
[engine]
tos = 184  # All sessions use EF by default
```

**Option 2: Per-session override (selective)**
```bash
# Voice calls
curl -X POST http://localhost:8080/v1/sessions \
  -d '{"call_id": "voice-1", "tos": 184}'

# Video calls
curl -X POST http://localhost:8080/v1/sessions \
  -d '{"call_id": "video-1", "tos": 160}'

# Test calls (best effort)
curl -X POST http://localhost:8080/v1/sessions \
  -d '{"call_id": "test-1", "tos": 0}'
```

## Verification

### Check TOS Marking with tcpdump

```bash
# Monitor outgoing packets on port 30000
sudo tcpdump -i any -n 'udp port 30000' -vvv | grep "tos"
```

Expected output:
```
IP (tos 0xb8, ttl 64, id 19189, offset 0, flags [DF], proto UDP (17), length 200)
```

### Verify in Logs

With `RUST_LOG=forge=debug`, you'll see:
```
Creating session with custom TOS: 0xA0 (DSCP=0x28)
Set IPv4 TOS to 0xA0 (DSCP=0x28)
```

## Network Configuration

For QoS marking to be effective:

1. **Linux Capabilities**: May require elevated privileges (sudo)
2. **Network Equipment**: Routers/switches must honor DSCP markings
3. **QoS Policies**: Configure downstream network devices to prioritize based on DSCP

## Performance Impact

TOS marking has **negligible performance impact**:
- Set once at socket creation time
- No per-packet overhead
- Works with XDP kernel bypass (when enabled)

## Best Practices

1. **Voice Calls**: Use EF (0xB8/184) - highest priority
2. **Video Calls**: Use AF41 (0xA0/160) - high priority
3. **Test Traffic**: Use BE (0x0/0) - no priority
4. **SBC Deployments**: Always override to ensure consistent marking
5. **Enterprise**: Follow your organization's QoS policy

## Troubleshooting

### TOS not being set

**Problem**: Packets don't have expected TOS marking

**Solutions**:
- Run forge-media with `sudo` (required for setting TOS on Linux)
- Check logs for socket creation errors
- Verify kernel allows TOS setting

### Network not honoring QoS

**Problem**: TOS is set but no QoS improvement

**Solutions**:
- Verify network equipment supports DSCP
- Check QoS policies on routers/switches
- Use `tcpdump` to verify TOS is actually set on packets
- Contact network administrator

## References

- RFC 2474: Definition of the Differentiated Services Field (DSCP)
- RFC 3246: An Expedited Forwarding PHB
- RFC 2597: Assured Forwarding PHB Group
