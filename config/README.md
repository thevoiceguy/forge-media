# Forge Configuration

This directory contains configuration files for the Forge Media Engine.

## Configuration Files

- **`forge.toml`** - Active configuration (used by the server)
- **`forge.toml.example`** - Example configuration with all options documented

## Configuration Loading

Forge looks for configuration files in the following order:

1. `/etc/forge/config.toml` (system-wide)
2. `./config/forge.toml` (repository)
3. `./forge.toml` (repository root)

If no configuration file is found, Forge uses built-in defaults.

## Quick Start

### Basic Usage (Userspace Forwarding)

```toml
[api]
http_bind = "0.0.0.0:8080"

[engine]
port_range_start = 30000
port_range_end = 40000
```

### Enable XDP/eBPF Acceleration

```toml
[engine.xdp]
enabled = true
interface = "lo"        # or "eth0" for production
mode = "generic"        # or "native" for best performance
fallback = true
```

**Note**: XDP requires:
- Linux kernel >= 5.4
- Compile with: `cargo build --release --features xdp`
- Run with: `sudo ./target/release/forge-media`

## XDP/eBPF Acceleration

### What is XDP?

XDP (eXpress Data Path) is a Linux kernel technology that allows processing network packets at the earliest possible point in the network stack, providing:

- **Sub-10µs latency** (vs ~75µs userspace)
- **10x throughput** (500k+ PPS per core)
- **75% less CPU** for media forwarding

### When to Use XDP?

**Use XDP if:**
- You need carrier-grade performance (<10ms latency)
- Handling 1000+ concurrent sessions
- Running on dedicated media server hardware
- Have Linux kernel >= 5.4

**Use Userspace if:**
- Running on older systems
- Development/testing environment
- Don't have root privileges
- Maximum compatibility needed

### XDP Modes

#### Generic Mode (`mode = "generic"`)

- **Technology**: XDP_SKB (software-based)
- **Compatibility**: Works on ALL network interfaces
- **Performance**: ~50-100µs latency (still better than userspace)
- **Use case**: Development, testing, older NICs
- **Requirements**: Any Linux kernel >= 5.4

#### Native Mode (`mode = "native"`)

- **Technology**: XDP_DRV (driver-based)
- **Compatibility**: Requires NIC driver support
- **Performance**: <10µs latency (best performance)
- **Use case**: Production with modern NICs
- **Requirements**: Supported NIC (see below)

### Supported NICs for Native Mode

Modern NICs with XDP driver support:

- **Intel**: i40e (XL710, X710), ixgbe (82599, X520, X540), ice
- **Mellanox**: mlx4, mlx5 (ConnectX-4, ConnectX-5, ConnectX-6)
- **Netronome**: nfp (Agilio SmartNICs)
- **Broadcom**: bnxt_en
- **Virtio**: virtio_net (cloud VMs)
- **Amazon**: ena (AWS EC2)

Check your NIC with:
```bash
ethtool -i <interface> | grep driver
```

### XDP Operation

Forge uses a **hybrid architecture**:

1. **Fast Path (Kernel/XDP)** - 95% of packets
   - RTP packet forwarding after endpoints learned
   - Simple header rewrite and forwarding
   - Zero-copy, sub-10µs latency
   - Bypasses full kernel network stack

2. **Slow Path (Userspace)** - 5% of packets
   - Session establishment (first 2 packets - learning)
   - RTCP processing (packet loss, jitter, statistics)
   - Complex media processing (transcoding, DTMF, etc.)
   - Control plane (API, signaling, management)

**Packet Flow:**
```
Session Start → Learning (2 pkts to userspace)
             → XDP activated (BPF map entries inserted)
             → RTP forwarding (kernel fast path)
             → RTCP (userspace for statistics)
Session End  → XDP deactivated (BPF map cleanup)
```

### Verifying XDP

After starting with XDP enabled, verify it's working:

```bash
# Check if XDP is attached
sudo ip link show <interface> | grep -i xdp

# List loaded XDP programs
sudo bpftool prog list | grep xdp

# View XDP program details
sudo bpftool prog show id <id>

# Dump BPF maps (after session created)
sudo bpftool map list
sudo bpftool map dump name FORWARD_MAP
sudo bpftool map dump name STATS_MAP
```

### Troubleshooting XDP

**XDP fails to load:**
- Check kernel version: `uname -r` (need >= 5.4)
- Check if compiled with xdp feature: `cargo build --features xdp`
- Run with sudo: `sudo ./target/release/forge-media`
- Check logs for specific error messages

**"Operation not supported" error:**
- Your NIC doesn't support native mode
- Use `mode = "generic"` instead

**"Permission denied" error:**
- Need root privileges or CAP_BPF capability
- Run with: `sudo ./target/release/forge-media`

**XDP loads but no performance improvement:**
- Verify XDP is actually attached: `ip link show`
- Check if sessions are created and endpoints learned
- Verify BPF map has entries: `bpftool map dump name FORWARD_MAP`
- Check logs for "XDP fast path activated" messages

### Fallback Behavior

When `fallback = true` (recommended):

1. Try to load XDP on startup
2. If XDP fails → log warning, continue with userspace
3. All features work normally, just without XDP acceleration
4. Sessions use traditional Tokio UDP forwarding

When `fallback = false`:
1. Try to load XDP on startup
2. If XDP fails → system logs warning but XDP marked unavailable
3. Sessions continue with userspace forwarding

## Port Configuration

### RTP/RTCP Port Range

```toml
[engine]
port_range_start = 30000
port_range_end = 40000
```

**Capacity**: Each session uses 2 ports (RTP + RTCP), so with a 10,000 port range you can handle 5,000 concurrent sessions.

**Firewall**: Ensure this UDP port range is open in your firewall:
```bash
# iptables example
sudo iptables -A INPUT -p udp --dport 30000:40000 -j ACCEPT

# firewalld example
sudo firewall-cmd --add-port=30000-40000/udp --permanent
sudo firewall-cmd --reload
```

## API Configuration

### Authentication

For production, **always** set authentication tokens:

```toml
[api]
auth_tokens = ["your-secure-token-here"]
```

Generate secure tokens with:
```bash
openssl rand -hex 32
```

Use the token in API requests:
```bash
curl -H "Authorization: Bearer your-secure-token-here" \
  http://localhost:8080/api/sessions
```

### HTTPS/TLS

For production, enable HTTPS:

```toml
[api]
enable_https = true
https_bind = "0.0.0.0:8443"
tls_cert = "/etc/forge/certs/fullchain.pem"
tls_key = "/etc/forge/certs/privkey.pem"
```

Generate self-signed cert for testing:
```bash
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout key.pem -out cert.pem -days 365 \
  -subj "/CN=forge.example.com"
```

Use Let's Encrypt for production:
```bash
sudo certbot certonly --standalone -d forge.example.com
```

## Examples

### Development Setup

```toml
[api]
http_bind = "127.0.0.1:8080"
auth_tokens = []  # No auth for development

[engine]
port_range_start = 30000
port_range_end = 35000

[engine.xdp]
enabled = true
interface = "lo"
mode = "generic"
fallback = true
```

### Production Setup

```toml
[api]
http_bind = "0.0.0.0:8080"
enable_https = true
https_bind = "0.0.0.0:8443"
tls_cert = "/etc/letsencrypt/live/forge.example.com/fullchain.pem"
tls_key = "/etc/letsencrypt/live/forge.example.com/privkey.pem"
auth_tokens = ["secure-token-1", "secure-token-2"]

[engine]
port_range_start = 30000
port_range_end = 40000
session_timeout_secs = 300
tos = 184  # EF for voice QoS

[engine.xdp]
enabled = true
interface = "eth0"
mode = "native"  # Best performance
fallback = true
```

## Further Reading

- [XDP/eBPF Documentation](../docs/xdp.md) (if exists)
- [Forge API Documentation](../docs/api.md) (if exists)
- [Linux XDP Documentation](https://www.kernel.org/doc/html/latest/networking/af_xdp.html)
