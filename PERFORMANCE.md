# Performance Testing and Metrics Guide

This document describes the performance characteristics, metrics, and load testing tools for Forge Media Engine.

## Table of Contents

1. [Prometheus Metrics](#prometheus-metrics)
2. [Load Testing](#load-testing)
3. [Performance Targets](#performance-targets)
4. [Profiling Guide](#profiling-guide)
5. [Optimization Tips](#optimization-tips)

---

## Prometheus Metrics

Forge exposes comprehensive metrics via Prometheus format at `/metrics` endpoint.

### SDP Negotiation Metrics

| Metric Name | Type | Labels | Description |
|------------|------|--------|-------------|
| `sdp_negotiation_total` | Counter | - | Total number of SDP negotiations attempted |
| `sdp_negotiation_failures_total` | Counter | `reason` | Failed SDP negotiations by failure reason |
| `sdp_negotiation_duration_seconds` | Histogram | - | Time taken for SDP negotiation |
| `sdp_codecs_negotiated_total` | Counter | `codec` | Count of codecs successfully negotiated |

**Failure Reasons:**
- `missing_local_address` - local_address not provided with sdp_offer
- `invalid_profile` - Unknown SDP profile name
- `parse_error` - Failed to parse SDP offer
- `no_common_codec` - No codec match between offer and local capabilities
- `negotiation_error` - Other negotiation errors

**Performance Target:** <1ms p99 latency

### Transcoding Metrics

| Metric Name | Type | Labels | Description |
|------------|------|--------|-------------|
| `forge_transcoding_duration_seconds` | Histogram | `from_codec`, `to_codec` | Per-packet transcoding latency |
| `forge_transcoding_packets_total` | Counter | `from_codec`, `to_codec` | Packets successfully transcoded |
| `forge_transcoding_bytes_total` | Counter | `from_codec`, `to_codec` | Bytes transcoded |
| `forge_transcoding_errors_total` | Counter | `from_codec`, `to_codec` | Transcoding failures |

**Codec Labels:**
- `PCMU` - G.711 μ-law
- `PCMA` - G.711 A-law
- `Opus` - Opus codec (requires `audio-opus` or `audio-all` profile)

**Performance Target:** <5ms per frame

### Conference Metrics

| Metric Name | Type | Labels | Description |
|------------|------|--------|-------------|
| `forge_conference_rooms_active` | Gauge | - | Number of active conference rooms |
| `forge_conference_rooms_created_total` | Counter | - | Total conference rooms created |
| `forge_conference_rooms_deleted_total` | Counter | - | Total conference rooms deleted |
| `forge_conference_participants_active` | Gauge | `room_id` | Active participants per room |
| `forge_conference_participants_joined_total` | Counter | `room_id` | Participants joined (cumulative) |
| `forge_conference_participants_left_total` | Counter | `room_id` | Participants left (cumulative) |
| `forge_conference_mixing_duration_seconds` | Histogram | `room_id` | Time to mix audio for all participants |
| `forge_conference_mix_operations_total` | Counter | `room_id` | Number of mix operations performed |
| `forge_conference_recordings_active` | Gauge | `room_id` | Whether room is being recorded (0 or 1) |
| `forge_conference_recordings_started_total` | Counter | `room_id` | Room recordings started |
| `forge_conference_recordings_stopped_total` | Counter | `room_id` | Room recordings stopped |
| `forge_conference_participant_recordings_started_total` | Counter | `room_id`, `participant_id` | Participant recordings started |
| `forge_conference_participant_recordings_stopped_total` | Counter | `room_id`, `participant_id` | Participant recordings stopped |

**Performance Target:** <20ms mixing latency for 10 participants

### Session Metrics

| Metric Name | Type | Labels | Description |
|------------|------|--------|-------------|
| `forge_active_sessions` | Gauge | - | Number of currently active sessions |
| `forge_rtp_packets_received_total` | Counter | - | RTP packets received |
| `forge_rtp_bytes_received_total` | Counter | - | RTP bytes received |
| `forge_rtp_packets_sent_total` | Counter | - | RTP packets sent |
| `forge_rtp_bytes_sent_total` | Counter | - | RTP bytes sent |

### RTCP Metrics

| Metric Name | Type | Labels | Description |
|------------|------|--------|-------------|
| `forge_rtcp_packets_received_total` | Counter | - | RTCP packets received |
| `forge_rtcp_bytes_received_total` | Counter | - | RTCP bytes received |
| `forge_rtcp_packets_sent_total` | Counter | - | RTCP packets sent |
| `forge_rtcp_bytes_sent_total` | Counter | - | RTCP bytes sent |
| `forge_rtcp_packet_loss_fraction` | Gauge | - | Packet loss fraction from receiver reports |
| `forge_rtcp_cumulative_lost_packets` | Gauge | - | Cumulative lost packets |
| `forge_rtcp_jitter` | Gauge | - | Interarrival jitter |
| `forge_rtcp_highest_seq` | Gauge | - | Highest sequence number received |

### DTMF Metrics

| Metric Name | Type | Labels | Description |
|------------|------|--------|-------------|
| `forge_dtmf_events_total` | Counter | `method`, `digit` | DTMF events detected |
| `forge_dtmf_rfc2833_events_total` | Counter | `digit`, `event_type` | RFC 2833 events |
| `forge_dtmf_inband_events_total` | Counter | `digit`, `event_type` | Inband DTMF events |
| `forge_dtmf_duplicates_suppressed_total` | Counter | `method`, `digit` | Duplicate events suppressed |

---

## Load Testing

Forge includes two load testing tools:

### 1. Shell Script (`load_test.sh`)

Quick load test using curl for basic validation.

```bash
# Run with defaults (100 sessions, 10 rooms, 5 participants each)
./load_test.sh

# Custom configuration
SESSIONS=200 ROOMS=20 PARTICIPANTS=10 ./load_test.sh

# Point to different server
FORGE_URL=http://192.168.1.100:8080 ./load_test.sh
```

**Tests Performed:**
1. Concurrent session creation
2. SDP negotiation performance
3. Conference room and participant creation
4. Metrics collection

### 2. Rust Load Test Tool (`examples/load_test.rs`)

Advanced load testing with detailed metrics.

```bash
# Basic load test
cargo run --release --example load_test -- --sessions 100

# Conference-focused test
cargo run --release --example load_test -- --conferences 20 --participants 10

# Full test with SDP negotiation
cargo run --release --example load_test -- \
  --sessions 100 \
  --conferences 10 \
  --participants 5 \
  --test-sdp \
  --duration 60

# Available options
cargo run --release --example load_test -- --help
```

**Features:**
- Concurrent session creation
- SDP negotiation testing
- Conference room stress testing
- Automatic cleanup
- Performance metrics collection
- Success rate calculation

**Example Output:**
```
========================================
Load Test Results
========================================
Sessions:
  Created: 100
  Failed: 0
  Success Rate: 100.00%

SDP Negotiation:
  Successful: 50
  Failed: 0
  Success Rate: 100.00%

Conferences:
  Rooms Created: 10
  Total Participants: 50
  Avg Participants/Room: 5.00

Performance:
  Total Duration: 2543ms
  Throughput: 39.32 ops/sec
========================================
```

---

## Performance Targets

These are the target performance characteristics for Forge Media Engine Phase 2:

### SDP Negotiation
- **Latency:** <1ms p99
- **Throughput:** >1000 negotiations/sec
- **Success Rate:** >99.9%

### Codec Transcoding
- **Latency:** <5ms per frame (20ms audio frame)
- **CPU:** <10% per transcoding stream on modern CPU
- **Supported Pairs:** PCMU ↔ PCMA, PCMU ↔ Opus, PCMA ↔ Opus

### Conference Mixing
- **Mixing Latency:** <20ms for 10 participants
- **Max Participants:** 50 per room (soft limit)
- **Max Rooms:** 100+ concurrent rooms
- **Recording:** No measurable performance impact

### Sessions
- **Concurrent Sessions:** 100+ with transcoding
- **Max Sessions:** 1000+ without transcoding
- **Session Creation:** <10ms
- **Port Allocation:** <1ms

### Network
- **RTP Processing:** Line rate for 1000+ concurrent streams
- **Packet Loss Handling:** Graceful degradation
- **Jitter Buffer:** Not implemented (direct forwarding)

---

## Profiling Guide

### CPU Profiling

Use `cargo flamegraph` to identify hotspots:

```bash
# Install flamegraph tool
cargo install flamegraph

# Profile the load test
sudo cargo flamegraph --example load_test -- --sessions 100 --conferences 10

# Profile the main server (run in one terminal)
sudo cargo flamegraph --bin forge-media

# Generate load (run in another terminal)
./load_test.sh
```

The flamegraph will be saved as `flamegraph.svg`.

**Expected Hotspots:**
- Codec encoding/decoding (opus, g711)
- Audio mixing (sample accumulation)
- RTP packet parsing
- Socket I/O

### Memory Profiling

Use `valgrind` or `heaptrack` for memory analysis:

```bash
# Install heaptrack
sudo apt-get install heaptrack

# Profile memory usage
heaptrack cargo run --release --example load_test -- --sessions 100

# Analyze results
heaptrack_gui heaptrack.*.gz
```

### Metrics-Based Profiling

Monitor Prometheus metrics during load:

```bash
# Start load test in background
./load_test.sh &

# Watch metrics in real-time
watch -n 1 'curl -s http://localhost:8080/metrics | grep -E "forge_transcoding_duration|forge_conference_mixing_duration|sdp_negotiation_duration"'
```

### Tracing

Enable debug logs for detailed tracing:

```bash
# Run with detailed tracing
RUST_LOG=forge=debug cargo run --release

# Trace specific modules
RUST_LOG=forge_engine::forwarding=trace cargo run --release
```

---

## Optimization Tips

### 1. Transcoding Optimization

**Problem:** High CPU usage during transcoding

**Solutions:**
- Minimize codec conversions - use same codec end-to-end when possible
- Consider hardware acceleration (future: add codec-specific acceleration)
- Batch process multiple frames if latency permits
- Use CPU affinity to pin transcoding threads

**Configuration:**
```rust
// In future configs
MediaSessionConfig {
    // Prefer direct forwarding when possible
    prefer_passthrough: true,
    // Enable hardware acceleration (future)
    hw_accel: true,
}
```

### 2. Conference Mixing Optimization

**Problem:** Mixing latency increases with participant count

**Solutions:**
- Keep participant count per room <20 for best performance
- Use multiple smaller rooms instead of one large room
- Enable Voice Activity Detection (VAD) to skip silent participants
- Adjust frame size (larger frames = less frequent mixing)

**Current Implementation:**
- Frame size: 480 samples (10ms at 48kHz)
- Mixing algorithm: Simple accumulation with clipping
- No VAD-based skipping (yet)

### 3. Session Scaling

**Problem:** Port exhaustion or session creation slowdown

**Solutions:**
- Configure port pool size appropriately
- Use port reuse with SO_REUSEPORT
- Monitor `forge_active_sessions` gauge
- Implement session cleanup for idle sessions

**Configuration:**
```rust
SessionManagerConfig {
    port_pool_config: PortPoolConfig::new(20000, 60000).unwrap(),
}
```

### 4. Memory Optimization

**Problem:** High memory usage with many sessions

**Solutions:**
- Tune RTP buffer sizes
- Implement periodic cleanup of completed sessions
- Use object pools for frequent allocations
- Monitor with `heaptrack` or `valgrind`

### 5. Network Optimization

**Problem:** Packet processing bottleneck

**Future XDP Integration:**
- Kernel bypass for RTP forwarding
- Hardware offload for packet filtering
- Reduced context switches
- See `XDP_INTEGRATION_PLAN.md` for details

---

## Continuous Performance Testing

### CI/CD Integration

Add performance regression tests to CI:

```yaml
# .github/workflows/performance.yml
name: Performance Tests

on: [push, pull_request]

jobs:
  perf:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Build
        run: cargo build --release
      - name: Run Load Test
        run: |
          cargo run --release &
          sleep 5
          ./load_test.sh
      - name: Check Metrics
        run: |
          # Verify performance targets met
          curl -s http://localhost:8080/metrics | grep sdp_negotiation_duration
```

### Automated Benchmarking

Run benchmarks on each commit:

```bash
# Run all benchmarks
cargo bench --no-default-features

# Compare with baseline
cargo bench --no-default-features -- --save-baseline main
git checkout feature-branch
cargo bench --no-default-features -- --baseline main
```

### Production Monitoring

Monitor these key metrics in production:

**Critical Alerts:**
- `sdp_negotiation_duration_seconds` p99 > 5ms
- `forge_transcoding_duration_seconds` p99 > 10ms
- `forge_conference_mixing_duration_seconds` p99 > 50ms
- Session creation failure rate > 1%

**Capacity Alerts:**
- `forge_active_sessions` > 800 (80% of 1000 target)
- `forge_conference_rooms_active` > 80 (80% of 100 target)
- CPU usage > 80%
- Memory usage > 80%

---

## Troubleshooting Performance Issues

### High CPU Usage

1. Check transcoding metrics:
   ```bash
   curl -s http://localhost:8080/metrics | grep forge_transcoding
   ```

2. Verify codec usage:
   - Are sessions using different codecs unnecessarily?
   - Can you standardize on PCMU/PCMA?

3. Profile with flamegraph to identify hotspots

### High Memory Usage

1. Check active session count:
   ```bash
   curl -s http://localhost:8080/metrics | grep forge_active_sessions
   ```

2. Look for session leaks:
   - Are sessions being properly cleaned up?
   - Check session TTL and cleanup intervals

3. Profile with heaptrack

### High Latency

1. Check mixing duration:
   ```bash
   curl -s http://localhost:8080/metrics | grep mixing_duration
   ```

2. Reduce participant count per room

3. Verify network conditions:
   - Check for packet loss: `forge_rtcp_packet_loss_fraction`
   - Monitor jitter: `forge_rtcp_jitter`

### Session Creation Failures

1. Check metrics:
   ```bash
   curl -s http://localhost:8080/metrics | grep sdp_negotiation_failures
   ```

2. Common causes:
   - Port exhaustion (increase port pool range)
   - No common codec (use `audio-all` profile)
   - Invalid SDP format

---

## Additional Resources

- [Interoperability Testing Guide](INTEROP_TESTING.md)
- [Development Plan](DEVELOPMENT_PLAN.md)
- [Prometheus Documentation](https://prometheus.io/docs/)
- [Grafana Dashboard Examples](https://grafana.com/grafana/dashboards/)

For questions or issues, please open an issue on GitHub.
