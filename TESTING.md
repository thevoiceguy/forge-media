# Forge Media Engine - End-to-End Testing Guide

This guide walks you through testing the complete Forge media engine with real SIP softphones.

## 🎯 What We're Testing

- ✅ **RTP Forwarding** - Bidirectional audio between two SIP endpoints
- ✅ **Symmetric RTP Learning** - Automatic endpoint discovery
- ✅ **QoS Marking** - TOS/DSCP (0xB8/EF) for voice packets
- ✅ **Port Allocation** - Dynamic RTP/RTCP port pairs
- ✅ **Session Management** - Complete call lifecycle
- ✅ **SIP Integration** - Registration and call routing
- ✅ **Network Testing** - Works across your local network

## 🚀 Quick Start

```bash
# Terminal 1: Start Forge engine (listens on all interfaces)
RUST_LOG=forge=info ./target/release/forge-media

# Terminal 2: Start SIP server (set your actual IP)
LOCAL_IP=192.168.1.100 ./target/release/forge-sip-server

# Configure two SIP softphones on different devices:
#   Domain: 192.168.1.100 (your server's IP)
#   Port: 5060, Transport: UDP
#   Username: alice / bob
#   Password: <anything>

# Register both phones, then call bob from alice!
```

## 📋 Network Configuration

### Find Your Server IP

```bash
# Linux/Mac
ip addr show | grep "inet "
# or
ifconfig | grep "inet "

# Look for your local network IP (e.g., 192.168.1.x or 10.0.0.x)
```

### Firewall Rules

```bash
# Allow Forge API (HTTP)
sudo ufw allow 8080/tcp

# Allow SIP signaling
sudo ufw allow 5060/udp

# Allow RTP/RTCP media
sudo ufw allow 30000:40000/udp
```

## ✅ Success Criteria

- Both phones register from different devices
- Call connects (alice → bob)
- **Bidirectional audio works across the network!** 🎉
- Statistics show RTP flowing
- QoS markings applied (TOS 0xB8)
- Clean call termination

## 🔍 Testing Scenarios

### Scenario 1: Same Machine (Localhost)
```bash
LOCAL_IP=127.0.0.1 ./target/release/forge-sip-server
# Both softphones on the same computer
```

### Scenario 2: Local Network
```bash
LOCAL_IP=192.168.1.100 ./target/release/forge-sip-server
# Softphones on different computers/phones on your WiFi
```

### Scenario 3: Mobile Devices
- Use Linphone or Zoiper app on iOS/Android
- Connect to your WiFi network
- Configure with your server's IP
- Call between mobile and desktop!

## 📱 Recommended Softphones

- **Desktop**: Linphone, Zoiper, MicroSIP
- **Mobile**: Linphone (iOS/Android), Zoiper (iOS/Android)
- **Web**: tryit.jssip.net

## 🎉 Congratulations!

If audio works across devices, you've validated:
- Sprint 1: API Layer ✓
- Sprint 2: RTP Forwarding ✓  
- Sprint 2 Enhancement: QoS Support ✓
- **Production-ready for local network deployment!**
