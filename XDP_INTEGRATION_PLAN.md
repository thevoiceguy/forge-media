# eBPF/XDP Integration Plan for Forge Media Engine

## Executive Summary

**Goal**: Achieve carrier-grade, sub-10ms RTP packet forwarding latency using eBPF/XDP while preserving existing session management and symmetric RTP learning.

**Approach**: Hybrid architecture with XDP fast path (95% of packets) and userspace slow path (session setup, RTCP, errors).

**Key Decisions**:
- **Scope**: Document first, implementation follows
- **Crate**: Aya (pure Rust, no LLVM dependency, Tokio integration)
- **Mode**: XDP_SKB (generic) for development, XDP_DRV (native) for production
- **Feature**: Optional via `--features xdp` with graceful fallback
- **Timeline**: 12 weeks full implementation (can be phased)

## Hybrid Architecture Philosophy

**Core Principle**: Leverage eBPF/XDP for simple, high-volume data plane tasks while keeping complex, stateful logic in userspace.

### Division of Responsibilities

#### eBPF/XDP (Kernel) - High-Volume, Simple Tasks

**What Runs in Kernel**:
1. **RTP Packet Forwarding** (Fast Path - 95% of packets)
   - Established media streams with known endpoints
   - Simple header rewrite: source IP:port → destination IP:port
   - XDP_REDIRECT or XDP_TX for direct NIC-to-NIC forwarding
   - Bypasses full kernel network stack
   - **Performance**: <5µs per packet, 500k+ PPS throughput

2. **Initial Packet Filtering / DDoS Mitigation**
   - Drop malformed packets (XDP_DROP) at earliest point
   - Validate UDP checksums
   - Port range filtering (30000-40000)
   - Rate limiting per source IP (future enhancement)
   - **Performance**: Minimal CPU overhead, line-rate filtering

3. **Simple NAT / Address Translation**
   - Lightweight header rewriting for RTP streams
   - IP address and port translation
   - MAC address rewriting for L2 forwarding
   - **Performance**: Zero-copy, sub-microsecond overhead

4. **Statistics Collection**
   - Per-session packet/byte counters
   - Simple atomic increments in BPF maps
   - Exported to userspace via map reads
   - **Performance**: Lock-free, minimal overhead

**What Does NOT Run in Kernel**:
- ❌ Session establishment logic
- ❌ Endpoint learning (first 2 packets)
- ❌ RTCP processing
- ❌ Complex state machines
- ❌ Memory allocation
- ❌ Transcoding or media processing

#### Userspace (Application) - Complex, Stateful Tasks

**What Runs in Userspace**:
1. **Signaling Stack (SIP/H.323)**
   - Full SIP state machine (INVITE, ACK, BYE, etc.)
   - SDP negotiation and parsing
   - Session establishment and teardown
   - Registration and authentication
   - **Current Implementation**: `crates/forge-sip/` unchanged

2. **Complex Media Processing**
   - Transcoding (codec conversion)
   - Deep packet inspection
   - Quality of Service (QoS) enforcement
   - SRTP encryption/decryption
   - DTMF detection and injection
   - Audio mixing for conferences
   - **Current Implementation**: `crates/forge-media-processor/` unchanged

3. **Control Plane**
   - Session orchestration and lifecycle management
   - Configuration management
   - Logging and distributed tracing
   - Prometheus metrics aggregation
   - API endpoints (REST/gRPC)
   - **Current Implementation**: `crates/forge-engine/`, `crates/forge-api/` enhanced with XDP triggers

4. **Symmetric RTP Learning**
   - First packet from participant A → learn endpoint
   - Second packet from participant B → learn endpoint
   - After learning → activate XDP fast path
   - **Current Implementation**: `crates/forge-engine/src/forwarding.rs:111-207` preserved

5. **RTCP Processing**
   - Sender Reports (SR) / Receiver Reports (RR)
   - Packet loss calculation
   - Jitter measurement
   - Round-trip time estimation
   - **Current Implementation**: `crates/forge-engine/src/forwarding.rs:209-340` unchanged

6. **Exception / Fallback Handling**
   - Packets requiring complex logic (XDP_PASS to userspace)
   - Unknown sources (not in forwarding map)
   - Error recovery and logging
   - Graceful degradation if XDP unavailable
   - **Implementation**: Existing Tokio forwarding loop as fallback

### Why This Hybrid Approach?

| Aspect | Kernel (eBPF/XDP) | Userspace |
|--------|-------------------|-----------|
| **Latency** | <5µs | 20-75µs |
| **Throughput** | 500k+ PPS/core | 50k PPS/core |
| **Complexity** | Simple forwarding only | Full features |
| **Development** | C + BPF verifier | Rust + full stdlib |
| **Debugging** | Limited (bpftool, printk) | Full tracing/logs |
| **Safety** | Kernel verified | Userspace isolation |
| **Flexibility** | Fixed logic | Dynamic behavior |
| **State** | BPF maps (limited) | Full data structures |

**Result**: 10x performance for simple forwarding, full flexibility for complex logic.

## Architecture Overview

### Current Bottleneck
```
File: crates/forge-engine/src/forwarding.rs:59-122

Current Path (per-packet latency: 20-75µs):
NIC → kernel → recv_from() → userspace parse → lookup participant
→ serialize → send_to() → kernel → NIC

Hotspot: packet.to_bytes() serialization (5-20µs waste)
```

### Proposed Hybrid Architecture
```
┌─────────────────────────────────────────────────────────┐
│                     USERSPACE                            │
│  ┌──────────────┐  ┌─────────────┐  ┌────────────┐     │
│  │ Session Mgr  │  │ XDP Manager │  │ Event Poll │     │
│  │ (Tokio)      │◄─┤ (Aya)       │◄─┤ (RingBuf)  │     │
│  └──────┬───────┘  └──────┬──────┘  └─────▲──────┘     │
│         │                  │                │            │
│         │ Learning         │ Update         │ Events     │
└─────────┼──────────────────┼────────────────┼────────────┘
          │                  │                │
          │                  ▼                │
┌─────────┼──────────────────────────────────┼────────────┐
│         │           KERNEL (XDP)            │            │
│         │                                   │            │
│    ┌────▼─────┐    ┌──────────────┐   ┌───┴──────┐     │
│    │ Learning │    │ XDP Program  │   │ RingBuf  │     │
│    │ (2 pkts) │    │ (Fast Path)  │   │ (Events) │     │
│    └──────────┘    └──────┬───────┘   └──────────┘     │
│                            │                             │
│                     ┌──────▼────────┐                    │
│                     │ Forward Map   │                    │
│                     │ (5-tuple →    │                    │
│                     │  dest IP:port)│                    │
│                     └───────────────┘                    │
└─────────────────────────────────────────────────────────┘
```

**Fast Path (95%)**: After endpoints learned, XDP forwards directly (XDP_REDIRECT/XDP_TX)
**Slow Path (5%)**: Learning (first 2 packets), RTCP, errors → userspace (XDP_PASS)

### Packet Flow Decision Tree

```
┌───────────────────────────────────────────────────────────┐
│                    PACKET ARRIVES                          │
└────────────────────┬──────────────────────────────────────┘
                     │
                     ▼
            ┌────────────────┐
            │  XDP Program   │
            │  (Kernel Hook) │
            └───────┬────────┘
                    │
        ┌───────────┴───────────┐
        │                       │
    UDP packet?               Other
        │                    protocol
        ▼                       │
   Port 30000-40000?           │
        │                       │
    ┌───┴───┐                  │
    │       │                  │
   YES     NO                  │
    │       │                  │
    │       └──────────────────┴─────► XDP_PASS
    │                                  (to userspace/drop)
    ▼
  Even port?
  (RTP, not RTCP)
    │
    ├─ NO (odd) ──────────────────────► XDP_PASS
    │                                   (RTCP → userspace)
    ▼
  Lookup in forward_map
  (src_ip:port + dst_port)
    │
    ├─ NOT FOUND ─────────────────────► XDP_PASS + Event
    │                                   (Learning → userspace)
    │
    ▼
  FOUND (known session)
    │
    ├─ Rewrite IP/UDP headers
    ├─ Update statistics (atomic)
    └──────────────────────────────────► XDP_TX / XDP_REDIRECT
                                         (Fast path forwarding)
```

### Exception Handling Mechanisms

**1. XDP_PASS (Primary Fallback)**
- Packet released to normal kernel network stack
- Reaches Tokio UDP socket via recv_from()
- Full userspace processing available
- **Used for**: Learning, RTCP, unknown sources, errors

**2. AF_XDP (Optional Future Enhancement)**
- Zero-copy path from XDP to userspace
- Dedicated receive queue bypassing kernel stack
- Requires XDP socket setup (not in Phase 1)
- **Used for**: High-performance userspace processing if needed
- **Trade-off**: More complex than XDP_PASS, not needed initially

**3. Traditional Stack (Always Available)**
- Current Tokio implementation unchanged
- Fallback if XDP unavailable (old kernel, permissions)
- All features work exactly as before
- **Used for**: Development, compatibility, graceful degradation

## BPF Map Design

### Map 1: Forward Map (Core Routing)
```c
// Key: UDP 5-tuple
struct forward_key {
    __u32 src_ip;      // Source IP
    __u16 src_port;    // Source port
    __u16 dst_port;    // Our RTP port (30000-40000)
    __u32 dst_ip;      // Our IP (for multi-homed)
    __u16 protocol;    // UDP=17
    __u16 _padding;
}; // 16 bytes aligned

// Value: Forward destination
struct forward_value {
    __u32 dest_ip;      // Where to forward
    __u16 dest_port;    // Destination port
    __u16 src_port;     // Our source port
    __u64 last_seen;    // Timestamp
}; // 16 bytes

Type: BPF_MAP_TYPE_HASH
Max entries: 10,000 (supports 5,000 sessions bidirectional)
```

### Map 2: Statistics
```c
struct session_stats {
    __u64 packets_forwarded;
    __u64 bytes_forwarded;
    __u64 packets_dropped;
    __u64 last_packet_ts;
};

Type: BPF_MAP_TYPE_HASH
Key: session_id (u32)
Max entries: 5,000
```

### Map 3: Configuration
```c
struct xdp_config {
    __u32 port_range_start;  // 30000
    __u32 port_range_end;    // 40000
    __u32 flags;
    __u32 timeout_ms;
};

Type: BPF_MAP_TYPE_ARRAY
Entries: 1 (global config)
```

### Map 4: Event Ring Buffer
```c
struct event {
    __u8 event_type;     // LEARN, ERROR, TIMEOUT
    __u32 src_ip;
    __u16 src_port;
    __u16 dst_port;
    __u64 timestamp;
};

Type: BPF_MAP_TYPE_RINGBUF
Size: 256KB
```

## XDP Program Logic

**File**: `crates/forge-kernel/src/bpf/rtp_forward.bpf.c`

```c
SEC("xdp")
int xdp_rtp_forward(struct xdp_md *ctx) {
    // 1. Parse Ethernet → IP → UDP
    // 2. Check if UDP destination port in RTP range (30000-40000)
    // 3. Skip RTCP (odd ports) → XDP_PASS
    // 4. Lookup forward_key in map
    //    - Found: Rewrite headers + XDP_REDIRECT
    //    - Not found: Send event + XDP_PASS (learning)
    // 5. Update statistics
    // 6. Return XDP_TX or XDP_REDIRECT
}
```

**Decision Flow**:
```
Packet arrives → Parse UDP
├─ Dst port in range 30000-40000?
│  ├─ No → XDP_PASS (not RTP)
│  └─ Yes → Is port even?
│     ├─ No (odd) → XDP_PASS (RTCP)
│     └─ Yes → Lookup in map
│        ├─ Found → Rewrite + XDP_TX (fast path)
│        └─ Not found → Event + XDP_PASS (learn)
```

## Userspace Integration

### New Crate: forge-kernel

**Structure**:
```
crates/forge-kernel/
├── Cargo.toml
├── build.rs              # Compile BPF program
├── src/
│   ├── lib.rs            # Public API
│   ├── xdp_manager.rs    # XDP lifecycle
│   ├── map_sync.rs       # Map updates
│   ├── event_poller.rs   # Ring buffer poller
│   └── error.rs
└── src/bpf/
    └── rtp_forward.bpf.c # XDP program
```

**Dependencies**:
```toml
[dependencies]
aya = "0.12"
aya-log = "0.2"
tokio = { workspace = true }
tracing = { workspace = true }
```

### Modified Crate: forge-engine

**File**: `crates/forge-engine/src/forwarding.rs`

Add XDP integration:
```rust
pub struct ForwardingEngine {
    #[cfg(feature = "xdp")]
    xdp_manager: Option<Arc<XdpManager>>,
    mode: ForwardingMode,
}

impl ForwardingEngine {
    // Try to initialize XDP on startup
    pub async fn new_with_xdp(interface: &str) -> Result<Self> {
        let xdp_manager = XdpManager::new(interface, XdpMode::Generic)
            .await
            .ok(); // Graceful fallback

        let mode = if xdp_manager.is_some() {
            ForwardingMode::XdpGeneric
        } else {
            ForwardingMode::UserspaceOnly
        };

        Ok(Self { xdp_manager, mode })
    }
}
```

**File**: `crates/forge-engine/src/session.rs`

Add fast path activation:
```rust
impl MediaSession {
    // Called after both endpoints learned
    pub async fn activate_xdp_fast_path(&self) -> Result<()> {
        if let Some(xdp) = &self.xdp_manager {
            let a_addr = self.participant_a.remote_addr.unwrap();
            let b_addr = self.participant_b.remote_addr.unwrap();

            // Insert bidirectional rules
            xdp.insert_forward_rule(ForwardRule {
                src: a_addr,
                dst: b_addr,
                local_port: self.ports.rtp_port,
            }).await?;

            xdp.insert_forward_rule(ForwardRule {
                src: b_addr,
                dst: a_addr,
                local_port: self.ports.rtp_port,
            }).await?;
        }
        Ok(())
    }
}
```

## Session Lifecycle with XDP

```
1. Session Created (API call)
   ├─ Allocate ports (30000-40000)
   ├─ Create Tokio sockets
   └─ State: Initializing (no XDP entry yet)

2. First RTP Packet (from participant A)
   ├─ No map entry → XDP_PASS to userspace
   ├─ Learn participant A address
   └─ State: Learning

3. Second RTP Packet (from participant B)
   ├─ No map entry → XDP_PASS to userspace
   ├─ Learn participant B address
   ├─ Both endpoints known → activate_xdp_fast_path()
   └─ State: Active (XDP enabled)

4. Subsequent RTP Packets
   ├─ Map entry exists → XDP forwards directly
   └─ Userspace only sees RTCP

5. Session Terminates
   ├─ Remove map entries
   └─ Deallocate ports
```

## Synchronization Protocol

**Userspace → Kernel** (Map Updates):
1. Session creation: No action (wait for learning)
2. First endpoint learned: Still no action
3. Both endpoints learned: Insert bidirectional map entries
4. Session termination: Delete map entries

**Kernel → Userspace** (Events):
1. Unknown source: Ring buffer event → validate/learn
2. Errors: Ring buffer event → log/alert
3. Statistics: Periodic map reads

**Consistency**: Userspace is source of truth, kernel lags slightly (eventual consistency OK)

## Implementation Phases

### Phase 1: Foundation (Weeks 1-2)
**Goal**: Set up infrastructure

**Tasks**:
- [ ] Create `forge-kernel` crate structure
- [ ] Add Aya dependencies to workspace
- [ ] Implement basic XDP loader (empty program)
- [ ] Test XDP_SKB attach/detach on loopback
- [ ] Add feature flag `xdp` to forge-engine

**Files Modified**:
- `Cargo.toml` (workspace)
- `crates/forge-kernel/Cargo.toml` (new)
- `crates/forge-kernel/src/lib.rs` (new)
- `crates/forge-engine/Cargo.toml`

**Validation**: Can attach/detach empty XDP program

### Phase 2: XDP Program (Weeks 3-4)
**Goal**: Implement kernel forwarding logic

**Tasks**:
- [ ] Write XDP program (rtp_forward.bpf.c)
- [ ] Implement UDP parsing
- [ ] Implement map lookup
- [ ] Implement header rewrite
- [ ] Add ring buffer events
- [ ] Test with bpf_printk and bpftool

**Files Created**:
- `crates/forge-kernel/src/bpf/rtp_forward.bpf.c`
- `crates/forge-kernel/build.rs`

**Validation**: XDP program loads, verifier accepts, basic forwarding works

### Phase 3: Userspace Manager (Weeks 5-6)
**Goal**: Rust interface to XDP

**Tasks**:
- [ ] Implement XdpManager (load, attach, maps)
- [ ] Implement map synchronization (insert/delete rules)
- [ ] Implement event poller (ring buffer reader)
- [ ] Add graceful fallback logic
- [ ] Unit tests

**Files Created**:
- `crates/forge-kernel/src/xdp_manager.rs`
- `crates/forge-kernel/src/map_sync.rs`
- `crates/forge-kernel/src/event_poller.rs`

**Validation**: Can insert/delete/query maps from Rust

### Phase 4: Engine Integration (Weeks 7-8)
**Goal**: Integrate with existing forwarding engine

**Tasks**:
- [ ] Modify ForwardingEngine for hybrid mode
- [ ] Add XDP initialization in main()
- [ ] Implement activate_xdp_fast_path() in session
- [ ] Hook into session lifecycle
- [ ] Add cleanup on termination

**Files Modified**:
- `crates/forge-engine/src/forwarding.rs`
- `crates/forge-engine/src/session.rs`
- `crates/forge-engine/src/manager.rs`
- `src/main.rs`

**Validation**: Sessions activate XDP after learning

### Phase 5: Testing (Weeks 9-10)
**Goal**: Comprehensive testing

**Tasks**:
- [ ] Unit tests (map operations, parsing)
- [ ] Integration tests (mock XDP)
- [ ] System tests (real XDP on loopback)
- [ ] Load testing (SIPp with 1000+ sessions)
- [ ] Performance benchmarks

**Files Created**:
- `crates/forge-kernel/tests/`
- `benches/xdp_forwarding.rs`

**Validation**: All tests pass, performance gains measured

### Phase 6: Production Hardening (Weeks 11-12)
**Goal**: Production readiness

**Tasks**:
- [ ] Error handling improvements
- [ ] Monitoring (Prometheus metrics)
- [ ] Configuration (TOML config)
- [ ] Documentation (README, examples)
- [ ] CI/CD integration

**Files Modified**:
- `crates/forge-kernel/README.md` (new)
- `docs/xdp.md` (new)
- `.github/workflows/test.yml`

**Validation**: Ready for production testing

## Critical Files to Modify

### Primary (Must Modify):

1. **crates/forge-engine/src/forwarding.rs** (Lines 38-340)
   - Current: Pure userspace forwarding loop
   - Change: Add XDP manager, hybrid mode
   - Complexity: Medium (preserve existing logic)

2. **crates/forge-engine/src/session.rs** (Lines 105-290)
   - Current: Session lifecycle without XDP
   - Change: Add XDP activation hooks
   - Complexity: Low (add method calls)

3. **crates/forge-kernel/src/** (NEW CRATE)
   - XDP manager, map sync, event poller
   - Complexity: High (new BPF code)

### Secondary (May Modify):

4. **crates/forge-rtp/src/socket.rs** (Lines 56-79)
   - Optional: Add SO_REUSEPORT for hybrid mode
   - Complexity: Low

5. **src/main.rs** (Lines 35-60)
   - Add XDP initialization
   - Complexity: Low

## Testing Strategy

### Unit Tests (No XDP Required)
```rust
#[test]
fn test_forward_key_serialization() { }

#[test]
fn test_map_insert_delete() { }
```

### Integration Tests (Mock XDP)
```rust
#[tokio::test]
async fn test_session_lifecycle_with_xdp() {
    let mock_xdp = MockXdpManager::new();
    // ...
}
```

### System Tests (Real XDP on loopback)
```bash
$ sudo cargo test --features xdp --test integration_xdp
```

### Performance Benchmarks
```bash
$ cargo bench --features xdp
```

**Expected Results**:
- Latency: 75µs → <10µs (7.5x improvement)
- Throughput: 50k PPS → 500k+ PPS (10x improvement)
- CPU: 60% → 15% at 1k sessions (4x reduction)

## Configuration

**File**: `config/forge.toml`
```toml
[kernel]
xdp_enabled = true
xdp_interface = "lo"     # "eth0" in production
xdp_mode = "generic"     # "native" in production
xdp_fallback = true      # Fallback to userspace if XDP fails
```

**Environment Variables**:
```bash
FORGE_XDP_ENABLED=1      # Enable XDP
FORGE_XDP_INTERFACE=lo   # Interface to attach
FORGE_XDP_MODE=generic   # generic or native
```

## Fallback Strategy

**Multi-level Fallback**:
```
1. Try XDP_DRV (native) on production NIC
   ├─ Success → ForwardingMode::XdpNative
   └─ Fail ↓

2. Try XDP_SKB (generic) on same interface
   ├─ Success → ForwardingMode::XdpGeneric
   └─ Fail ↓

3. Pure userspace (current implementation)
   └─ ForwardingMode::UserspaceOnly
```

**XDP_PASS Triggers** (slow path):
- Unknown source (not in map)
- RTCP packets (odd ports)
- Malformed packets
- Ports outside range

## Monitoring & Observability

**Prometheus Metrics**:
```rust
metrics::counter!("forge_xdp_packets_forwarded_total");
metrics::counter!("forge_xdp_packets_dropped_total");
metrics::counter!("forge_xdp_packets_passed_to_userspace");
metrics::histogram!("forge_xdp_forward_latency_us");
metrics::gauge!("forge_xdp_sessions_active");
metrics::gauge!("forge_xdp_map_entries");
```

**Logging**:
```rust
tracing::info!(
    session_id = %session.call_id(),
    mode = ?self.mode,
    "XDP fast path activated"
);
```

**Debugging**:
```bash
# List loaded programs
$ sudo bpftool prog list

# Dump map contents
$ sudo bpftool map dump name session_forward_map

# View statistics
$ sudo bpftool prog show id <id> --json | jq .stats

# Trace events
$ sudo cat /sys/kernel/debug/tracing/trace_pipe
```

## Risk Mitigation

### Technical Risks

**Risk**: XDP not available (old kernel, permissions)
- **Mitigation**: Automatic fallback to userspace
- **Impact**: No performance gain, but no breakage

**Risk**: NIC doesn't support XDP_DRV
- **Mitigation**: Use XDP_SKB (generic mode)
- **Impact**: 50% of native performance (still better than userspace)

**Risk**: BPF verifier rejection
- **Mitigation**: Simplify program, add bounds checks
- **Detection**: Load-time error with verifier log

### Operational Risks

**Risk**: State synchronization bugs
- **Mitigation**: Userspace as source of truth, periodic resync
- **Detection**: Monitor map vs session count mismatch

**Risk**: Performance regression
- **Mitigation**: A/B testing, gradual rollout
- **Detection**: Benchmark suite, production metrics

## Success Criteria

### Performance
- [ ] p99 latency < 10µs (vs 75µs baseline)
- [ ] Throughput > 500k PPS (vs 50k baseline)
- [ ] CPU usage < 20% at 1k sessions (vs 60% baseline)

### Reliability
- [ ] No packet loss vs userspace baseline
- [ ] Graceful degradation if XDP fails
- [ ] No session management regressions

### Maintainability
- [ ] Clear separation: kernel (fast path) vs userspace (control)
- [ ] Comprehensive tests and documentation
- [ ] Easy to disable (feature flag + config)

## Next Steps

1. **Review this plan** with team
2. **Validate assumptions** about XDP_SKB on localhost
3. **Set up development environment** (Linux with kernel ≥5.4)
4. **Begin Phase 1** (foundation) when approved

## References

- **XDP Documentation**: https://www.kernel.org/doc/html/latest/networking/af_xdp.html
- **Aya Book**: https://aya-rs.dev/book/
- **BPF Verifier**: https://docs.kernel.org/bpf/verifier.html
- **Current Forwarding Logic**: `crates/forge-engine/src/forwarding.rs:38-340`
- **Session Management**: `crates/forge-engine/src/session.rs:105-290`
