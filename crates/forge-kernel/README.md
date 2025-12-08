# forge-kernel - eBPF/XDP Integration for High-Performance RTP Forwarding

Pure Rust eBPF/XDP implementation for carrier-grade, sub-10ms RTP packet forwarding in the Forge Media Engine.

## Overview

This crate provides kernel-level packet forwarding using eBPF/XDP technology to achieve:
- **<5µs latency** per packet (vs 20-75µs in userspace)
- **500k+ PPS** throughput per core (10x improvement)
- **Zero-copy** packet processing
- **Minimal CPU usage** (<20% at 1000 sessions)

## Architecture

### Hybrid Approach

**Kernel Fast Path (95% of packets)**:
- RTP packet forwarding after endpoint learning
- Simple header rewriting (IP/UDP)
- Direct NIC-to-NIC forwarding (XDP_TX)

**Userspace Slow Path (5% of packets)**:
- Symmetric RTP learning (first 2 packets)
- RTCP processing
- Complex media operations
- Error handling

### Components

1. **forge-kernel-ebpf** - Kernel eBPF program (runs in kernel)
   - UDP parsing with bounds checking
   - 5-tuple map lookup
   - Header rewriting
   - Ring buffer events

2. **forge-kernel** - Userspace manager (this crate)
   - XdpManager: Load and attach XDP programs
   - Map synchronization: Insert/delete forward rules
   - Event polling: Read ring buffer events
   - Graceful fallback when BPF not available

## Implementation Status

### ✅ Phase 1: Foundation
- forge-kernel crate structure
- Aya dependencies
- XDP feature flag
- Stub implementation

### ✅ Phase 2: XDP Program
- Complete UDP parsing (Ethernet → IP → UDP)
- Map lookup and forwarding logic
- Header rewrite with checksum offload
- Ring buffer events (UnknownSource, ForwardSuccess, ParseError)
- 3 BPF maps: FORWARD_MAP, STATS_MAP, EVENTS

### ✅ Phase 3: Userspace Manager
- XdpManager implementation
- load_from_bytecode() - Load compiled BPF
- insert_forward_rule() / remove_forward_rule()
- Graceful degradation (stub mode)
- Unit tests

### 🚧 Phase 4: Engine Integration (Next Steps)
- Connect XdpManager to forge-engine
- Session lifecycle hooks
- Hybrid forwarding mode

## Usage

### Basic Example

```rust
use forge_kernel::{XdpManager, XdpMode, ForwardKey, ForwardValue};

// Create XDP manager
let mut manager = XdpManager::new("eth0", XdpMode::Generic).await?;

// Load BPF program (when compiled)
let bytecode = include_bytes!("../forge-kernel-ebpf/target/bpfel-unknown-none/release/forge_kernel_ebpf");
manager.load_from_bytecode(bytecode).await?;

// Insert forwarding rule
let key = ForwardKey {
    src_ip: 0x0100007f, // 127.0.0.1 (network byte order)
    src_port: 5060u16.to_be(),
    dst_port: 30000u16.to_be(),
    dst_ip: 0x0100007f,
    protocol: 17, // UDP
    _padding: [0; 3],
};

let value = ForwardValue {
    dest_ip: 0x0100007f,
    dest_port: 5070u16.to_be(),
    src_ip: 0x0100007f,
    src_port: 30000u16.to_be(),
    last_seen: 0,
};

manager.insert_forward_rule(key, value).await?;

// Check status
assert!(manager.is_loaded());
```

## BPF Maps

### FORWARD_MAP
- **Type**: HashMap
- **Key**: ForwardKey (5-tuple: src_ip, src_port, dst_port, dst_ip, protocol)
- **Value**: ForwardValue (dest_ip, dest_port, src_ip, src_port, last_seen)
- **Size**: 10,000 entries (supports 5,000 bidirectional sessions)

### STATS_MAP
- **Type**: HashMap
- **Key**: u32 (session ID)
- **Value**: SessionStats (packets, bytes, timestamps)
- **Size**: 10,000 entries

### EVENTS
- **Type**: RingBuf
- **Size**: 256KB
- **Events**: UnknownSource, ForwardSuccess, ParseError

## Building

### Prerequisites

To compile the eBPF program, you need:
```bash
# Install bpf-linker
cargo install bpf-linker

# Install LLVM (for BPF target)
rustup component add rust-src --toolchain stable-x86_64-unknown-linux-gnu
```

### Compile eBPF Program

```bash
cd crates/forge-kernel-ebpf
cargo build --release --target=bpfel-unknown-none
```

### Build Userspace Manager

```bash
cargo build -p forge-kernel
```

### With XDP Feature

```bash
cargo build -p forge-engine --features xdp
```

## Testing

```bash
# Unit tests (no BPF required)
cargo test -p forge-kernel

# Integration tests (requires sudo + Linux)
sudo cargo test -p forge-kernel --test integration_xdp
```

## Development

### Stub Mode

When BPF bytecode is not available, `XdpManager` operates in stub mode:
- `new()` succeeds but `is_loaded()` returns false
- API methods log warnings but don't fail
- Allows development without bpf-linker

### Loading BPF Programs

Production deployment:
```rust
// Embed bytecode at compile time
const BPF_PROGRAM: &[u8] = include_bytes!("path/to/compiled.o");
manager.load_from_bytecode(BPF_PROGRAM).await?;
```

Development with external file:
```rust
let bytecode = std::fs::read("forge-kernel-ebpf.o")?;
manager.load_from_bytecode(&bytecode).await?;
```

## Performance

### Expected Metrics

| Metric | Userspace | XDP (Generic) | XDP (Native) |
|--------|-----------|---------------|--------------|
| Latency (p99) | 75µs | 15µs | <5µs |
| Throughput | 50k PPS | 200k PPS | 500k+ PPS |
| CPU (1k sessions) | 60% | 30% | <20% |

### Packet Decision Flow

```
Packet arrives → Parse Ethernet/IP/UDP
├─ Not IPv4/UDP → XDP_PASS (ignore)
├─ Not port 30000-40000 → XDP_PASS (not RTP)
├─ Odd port (RTCP) → XDP_PASS (to userspace)
├─ Lookup in FORWARD_MAP
│  ├─ Found → Rewrite headers → XDP_TX (fast path ⚡)
│  └─ Not found → Send event → XDP_PASS (learning 🔄)
```

## Security

- **Bounds checking**: All pointer accesses validated by BPF verifier
- **No heap allocation**: Stack-only, no dynamic memory
- **Capability-based**: Requires CAP_NET_ADMIN and CAP_BPF
- **Sandboxed**: eBPF programs cannot crash the kernel

## Limitations

- **Linux only**: XDP is Linux-specific
- **Kernel ≥5.4**: Recommended for full XDP support
- **NIC support**: Native mode requires driver support
- **Simple forwarding only**: Complex logic stays in userspace

## Troubleshooting

### XDP Program Won't Load

```bash
# Check kernel version
uname -r  # Should be ≥5.4

# Verify bpf syscall support
cat /proc/sys/kernel/unprivileged_bpf_disabled

# Check NIC driver support
ethtool -i eth0 | grep driver
```

### BPF Verifier Errors

```bash
# View verifier log
sudo dmesg | grep -i bpf

# Use bpftool for debugging
sudo bpftool prog list
sudo bpftool prog show id <id>
```

### Performance Issues

```bash
# Check XDP mode
sudo ip link show eth0 | grep xdp

# Verify map sizes
sudo bpftool map list
sudo bpftool map dump name FORWARD_MAP
```

## References

- [XDP Documentation](https://www.kernel.org/doc/html/latest/networking/af_xdp.html)
- [Aya Book](https://aya-rs.dev/book/)
- [BPF Verifier](https://docs.kernel.org/bpf/verifier.html)
- [Integration Plan](../../XDP_INTEGRATION_PLAN.md)

## License

MIT OR Apache-2.0
