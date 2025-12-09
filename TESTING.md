# Forge Media Engine - End-to-End Testing Guide

This guide walks you through testing the complete Forge media engine with real SIP softphones.

## 🎯 What We're Testing

- ✅ **RTP Forwarding** - Bidirectional audio between two SIP endpoints
- ✅ **Symmetric RTP Learning** - Automatic endpoint discovery
- ✅ **QoS Marking** - TOS/DSCP (0xB8/EF) for voice packets
- ✅ **Port Allocation** - Dynamic RTP/RTCP port pairs
- ✅ **Session Management** - Complete call lifecycle
- ✅ **SIP Integration** - Registration and call routing

## 🚀 Quick Start

```bash
# Terminal 1: Start Forge engine
RUST_LOG=forge=info ./target/release/forge-media

# Terminal 2: Start SIP server
LOCAL_IP=127.0.0.1 ./target/release/forge-sip-server

# Configure two SIP softphones:
#   Domain: 127.0.0.1 (or your LOCAL_IP)
#   Port: 5060, Transport: UDP
#   Username: alice / bob
#   Password: <anything>

# Register both phones, then call bob from alice!
```

## 📋 Full Testing Guide

See complete step-by-step instructions, troubleshooting, and verification steps at:
https://github.com/thevoiceguy/forge-media/blob/main/TESTING.md

## ✅ Success Criteria

- Both phones register
- Call connects (alice → bob)
- **Bidirectional audio works!** 🎉
- Statistics show RTP flowing
- QoS markings applied (TOS 0xB8)
- Clean call termination
