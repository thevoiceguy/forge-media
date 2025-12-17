# High Availability Implementation Plan for Forge Media Engine

## Executive Summary

This plan implements **Active-Passive HA** with hot standby failover, Redis-based state replication, and support for both cloud load balancers and on-premises VRRP/Keepalived deployments. The design accepts brief packet loss (20-100ms) during failover while maintaining call state and allowing active conferences to continue.

**Key Decisions (User Approved):**
- ✅ **HA Model**: Active-Passive (Primary + Hot Standby)
- ✅ **RTP Failover**: Brief packet loss acceptable (20-100ms)
- ✅ **Port Strategy**: Pre-allocated port ranges per instance
- ✅ **State Storage**: Redis for all session state

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│              Cloud Load Balancer / VRRP VIP         │
│         (Routes to Primary, health checks)          │
└────────────────┬────────────────────────────────────┘
                 │
        ┌────────┴─────────┐
        │                  │
┌───────▼──────────┐  ┌────▼──────────────┐
│  Primary         │  │  Standby          │
│  Role: Primary   │  │  Role: Standby    │
│  Ports: 30-35K   │  │  Ports: 35-40K    │
│  Returns: 200    │  │  Returns: 503     │
└────────┬─────────┘  └─────────┬─────────┘
         │                      │
         │   ┌──────────────────┤
         │   │                  │
         └───▼──────────────────▼────┐
         │       Redis Cluster        │
         │  - Session state           │
         │  - Conference state        │
         │  - Port allocations        │
         │  - Health heartbeats       │
         │  - Primary election lock   │
         └────────────────────────────┘
```

## Critical Implementation Components

### 1. Redis Schema for State Storage

**Key Namespaces:**
```
forge:ha:sessions:{call_id}              # MediaSession state
forge:ha:conferences:{room_id}           # ConferenceRoom state
forge:ha:ports:{instance_id}             # Port allocation tracking
forge:ha:instance:{instance_id}          # Instance health/status
forge:ha:election:primary                # Primary election lock (15s TTL)
```

**Session State Structure** (JSON in Redis):
```json
{
  "call_id": "uuid",
  "state": "Active|OnHold|Terminating",
  "participant_a": {
    "id": "uuid",
    "remote_addr": "1.2.3.4:5060",
    "codec": {"payload_type": 0, "codec": "PCMU", "clock_rate": 8000},
    "stats": {"packets_received": 1234, "bytes_received": 123456}
  },
  "participant_b": { /* same structure */ },
  "ports": {"rtp_port": 30000, "rtcp_port": 30001},
  "created_at": "2025-12-17T10:00:00Z",
  "last_activity": "2025-12-17T10:05:00Z",
  "instance_id": "primary-instance-uuid"
}
```

**TTL Strategy:**
- Session state: 3600s (refreshed on updates)
- Conference state: 7200s (refreshed on updates)
- Instance health: 30s (refreshed every 10s via heartbeat)
- Primary election lock: 15s (refreshed every 5s by primary)

### 2. Failover Process (Step-by-Step)

**Timeline:**
- **T+0s**: Primary fails
- **T+0-25s**: Standby detects missing heartbeats (3 consecutive timeouts)
- **T+25s**: Standby attempts primary election via Redis lock
- **T+26-30s**: New primary loads all session state from Redis
- **T+30s**: New primary binds UDP sockets to recovered ports
- **T+30.020s**: First RTP packet arrives, symmetric RTP relearns addresses
- **T+30.100s**: Bi-directional RTP flows restored

**Packet Loss Window**: 20-100ms during socket rebind

**Detailed Steps:**

1. **Detection Phase** (0-25s):
   - Standby monitors `forge:ha:instance:primary` heartbeat
   - If TTL expires (30s), assume primary failed
   - Verify Redis connectivity (not network partition)

2. **Election Phase** (25-27s):
   - Execute: `SET forge:ha:election:primary {standby_id} NX EX 15`
   - If successful → Become primary
   - If failed → Another standby won, remain standby

3. **Recovery Phase** (27-35s):
   - Load sessions: `SCAN forge:ha:sessions:*`
   - Load conferences: `SCAN forge:ha:conferences:*`
   - Reconstruct port pool from allocated ports
   - Update role: `HSET forge:ha:instance:{self_id} role "primary"`

4. **Resumption Phase** (35-40s):
   - Bind UDP sockets to recovered ports (same as primary)
   - Start RTP forwarding loops
   - Symmetric RTP relearns remote endpoints from first packets
   - Conference mixers resume immediately

### 3. Port Allocation Strategy

**Pre-Allocated Ranges:**
- **Primary**: 30000-34999 (2,500 port pairs = 5,000 sessions max)
- **Standby**: 35000-39999 (2,500 port pairs = 5,000 sessions max)

**Benefits:**
- No port conflicts between instances
- Standby can failover and bind to primary's ports
- Simple, no distributed locking needed

**Configuration:**
```toml
[ha.port_ranges]
primary = { start = 30000, end = 34999 }
standby = { start = 35000, end = 39999 }
```

### 4. Health Monitoring

**Heartbeat Mechanism:**
- Primary publishes health every 10s to `forge:ha:instance:primary`
- Contains: instance_id, role, session_count, last_activity, uptime
- TTL = 30s (expires if 3 heartbeats missed)

**Health Check Endpoints:**
- **Primary**: `GET /health` → 200 OK (healthy), 503 (degraded)
- **Standby**: `GET /health` → 503 Service Unavailable (not accepting traffic)
- **HA Status**: `GET /ha/status` → Detailed HA cluster status

**Load Balancer Integration:**
- Cloud LBs check `/health` every 5-10s
- Only route traffic to instances returning 200
- Automatic failover when primary returns 503

### 5. VIP Management

**Cloud Deployments (GCP/AWS/Azure/Linode):**
- Use native load balancer health checks
- Primary returns 200, standby returns 503
- No explicit VIP management needed

**On-Premises Deployments:**
- Use Keepalived with VRRP for VIP management
- Keepalived config template provided (see Implementation section)
- VIP migrates automatically on primary failure (within 5s)
- Gratuitous ARP updates network immediately

### 6. State Replication Timing

**When to Sync to Redis:**
- ✅ Session creation → Immediate
- ✅ Remote address learned (symmetric RTP) → Within 100ms
- ✅ State transitions → Immediate
- ✅ Port allocation → Immediate
- ✅ Conference participant changes → Immediate
- ✅ Session termination → Immediate (delete after 60s)
- ⏱️ Statistics updates → Every 10 seconds (batched, non-critical)

**Write-Through Pattern:**
- All critical state changes write to Redis first
- Acknowledge to client only after Redis confirms
- Local DashMap acts as read cache for performance

## Implementation Plan (File-by-File)

### Phase 1: Foundation - forge-ha Crate (~1 week)

**File: `/home/siphon/forge-media/crates/forge-ha/src/lib.rs`**
- Export public types: `HAManager`, `HAConfig`, `HARole`
- Re-export from submodules

**File: `/home/siphon/forge-media/crates/forge-ha/src/config.rs`**
```rust
pub struct HAConfig {
    pub enabled: bool,
    pub instance_id: Option<String>,  // Auto-generate if None
    pub role: HARole,  // Auto, Primary, Standby
    pub redis_url: String,
    pub heartbeat_interval: Duration,
    pub failover_timeout: Duration,
    pub port_ranges: PortRanges,
    pub deployment: DeploymentConfig,
}
```

**File: `/home/siphon/forge-media/crates/forge-ha/src/types.rs`**
- `InstanceId`, `HARole` (Primary/Standby), `DeploymentMode` enums
- Serializable state structs for Redis

**File: `/home/siphon/forge-media/crates/forge-ha/src/redis_client.rs`**
- Wrapper around `redis-rs` (follow existing `persistence/redis.rs` pattern)
- `RedisHAClient` with connection pooling and reconnection
- Helper methods: `get_session`, `set_session`, `scan_sessions`, etc.

**File: `/home/siphon/forge-media/crates/forge-ha/src/state_sync.rs`**
- Trait `StateSync` for serialization/deserialization
- `SessionStateSync` - sync `MediaSession` to/from Redis
- `ConferenceStateSync` - sync `ConferenceRoom` to/from Redis
- `PortPoolStateSync` - sync port allocations

**File: `/home/siphon/forge-media/crates/forge-ha/src/heartbeat.rs`**
- `HeartbeatService` - publishes health every 10s
- `HeartbeatMonitor` - watches peer heartbeats
- Failure detection: 3 consecutive misses (30s)

**File: `/home/siphon/forge-media/crates/forge-ha/src/election.rs`**
- `elect_primary()` - acquire Redis lock with `SET ... NX EX`
- `renew_primary_lock()` - refresh lock every 5s
- `step_down()` - voluntary release

**File: `/home/siphon/forge-media/crates/forge-ha/src/failover.rs`**
- `FailoverOrchestrator` - coordinates entire failover
- State machine: Detecting → Electing → Promoting → Recovering
- `execute_failover()` - main orchestration method

**File: `/home/siphon/forge-media/crates/forge-ha/src/vip_manager.rs`**
- Trait `VIPManager` with implementations:
  - `CloudVIPManager` - health endpoint (200/503)
  - `VRRPManager` - Keepalived integration (signal-based)

### Phase 2: Session Serialization (~3 days)

**File: `/home/siphon/forge-media/crates/forge-engine/src/session.rs`**

Add methods:
```rust
impl MediaSession {
    /// Serialize to state for Redis storage
    pub fn to_state(&self) -> SessionState { /* ... */ }

    /// Deserialize from state (recovery)
    pub async fn from_state(
        state: SessionState,
        port_pool: Arc<PortPool>,
        config: MediaSessionConfig,
        event_bus: Option<Arc<EventBus>>,
    ) -> Result<Arc<Self>> { /* ... */ }

    /// Sync current state to Redis
    pub async fn sync_to_redis(&self, redis: &RedisHAClient) -> Result<()> { /* ... */ }
}
```

Call `sync_to_redis()` at:
- Session creation
- Remote address learned (after symmetric RTP)
- State transitions
- Port allocation

### Phase 3: Session Manager Integration (~1 week)

**File: `/home/siphon/forge-media/crates/forge-engine/src/manager.rs`**

Add:
```rust
pub struct SessionManager {
    // ... existing fields ...
    ha_backend: Option<Arc<HABackend>>,
}

impl SessionManager {
    /// Create session (with HA sync)
    pub async fn create_session(&self, call_id: CallId) -> Result<Arc<MediaSession>> {
        let session = /* ... create normally ... */;

        // Sync to Redis if HA enabled
        if let Some(ha) = &self.ha_backend {
            session.sync_to_redis(&ha.redis).await?;
        }

        Ok(session)
    }

    /// Recover all sessions from Redis (called on standby promotion)
    pub async fn recover_sessions_from_redis(&self) -> Result<()> {
        let keys = self.ha_backend.redis.scan_match("forge:ha:sessions:*").await?;

        for key in keys {
            let state: SessionState = self.ha_backend.redis.get(&key).await?;
            let session = MediaSession::from_state(state, self.port_pool.clone(), self.config.clone(), self.event_bus.clone()).await?;
            self.sessions.insert(state.call_id, session);
        }

        Ok(())
    }
}
```

### Phase 4: Port Pool Recovery (~2 days)

**File: `/home/siphon/forge-media/crates/forge-rtp/src/port_pool.rs`**

Add:
```rust
impl PortPool {
    /// Create with pre-allocated ports (for recovery)
    pub fn new_with_allocated(
        range: RangeInclusive<u16>,
        allocated: HashSet<u16>,
    ) -> Result<Self> { /* ... */ }

    /// Sync allocation state to Redis
    pub async fn sync_state(&self, redis: &RedisHAClient, instance_id: &str) -> Result<()> { /* ... */ }
}
```

### Phase 5: API Integration (~3 days)

**File: `/home/siphon/forge-media/crates/forge-api/src/server.rs`**

Initialize HA on startup:
```rust
pub async fn serve(config: ForgeConfig) -> Result<()> {
    // Initialize HA if enabled
    let ha_manager = if config.ha.enabled {
        let manager = HAManager::new(config.ha.clone()).await?;
        manager.start().await?;  // Start heartbeat, monitoring
        Some(Arc::new(manager))
    } else {
        None
    };

    // ... rest of server setup ...
}
```

**File: `/home/siphon/forge-media/crates/forge-api/src/routes/ha.rs`** (new)

Add endpoints:
```rust
/// GET /ha/status - HA cluster status
pub async fn get_ha_status(State(state): State<AppState>) -> Json<HAStatus> { /* ... */ }

/// POST /ha/transfer-primary - Manual failover (graceful)
pub async fn transfer_primary(State(state): State<AppState>) -> StatusCode { /* ... */ }

/// GET /health - Health check (returns 200 if primary, 503 if standby)
pub async fn health_check(State(state): State<AppState>) -> StatusCode {
    if state.ha_manager.is_primary() && state.ha_manager.is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
```

### Phase 6: Configuration (~1 day)

**File: `/home/siphon/forge-media/crates/forge-core/src/config.rs`**

Add to `ForgeConfig`:
```rust
pub struct ForgeConfig {
    // ... existing fields ...
    pub ha: Option<HAConfig>,
}
```

**Example Configuration** (`/etc/forge/config.toml`):
```toml
[ha]
enabled = true
instance_id = "auto"
role = "auto"
deployment_mode = "cloud"  # or "onprem"
port_range = { start = 30000, end = 34999 }

[ha.redis]
url = "redis://10.0.1.10:6379/0"
key_prefix = "forge:ha:"
heartbeat_interval_secs = 10
failover_timeout_secs = 25

[ha.cloud]
provider = "gcp"
health_check_path = "/health"
standby_returns_503 = true

[ha.onprem]
vip = "10.0.1.100"
interface = "eth0"
virtual_router_id = 51
priority = 100
```

### Phase 7: Keepalived Integration (~2 days)

**File: `/etc/keepalived/keepalived.conf`** (template for on-prem)

```bash
vrrp_script check_forge {
    script "/usr/local/bin/check_forge_health.sh"
    interval 5
    weight -30
    fall 3
    rise 2
}

vrrp_instance FORGE_HA {
    state MASTER               # MASTER on primary, BACKUP on standby
    interface eth0
    virtual_router_id 51
    priority 100               # 100 on primary, 50 on standby
    advert_int 1
    authentication {
        auth_type PASS
        auth_pass ${VRRP_PASSWORD}
    }
    virtual_ipaddress {
        10.0.1.100/24
    }
    track_script {
        check_forge
    }
}
```

**File: `/usr/local/bin/check_forge_health.sh`**
```bash
#!/bin/bash
curl -sf http://localhost:8080/health > /dev/null
exit $?
```

### Phase 8: Testing (~1 week)

**Unit Tests** (`crates/forge-ha/src/tests/`):
- Election mechanism (single winner)
- Lock renewal
- Session serialization roundtrip
- Heartbeat timeout detection

**Integration Tests** (`crates/forge-ha/tests/`):
- Full failover with active sessions
- Conference failover preserves state
- Port pool recovery
- Redis failure handling

**Chaos Tests**:
- Primary crash during session creation
- Network partition scenarios
- Redis failure scenarios
- Simultaneous restart

## Configuration Reference

### Full Configuration Example

```toml
[engine]
port_range = { start = 30000, end = 40000 }
session_timeout_secs = 300

[api]
http_bind = "0.0.0.0:8080"

[ha]
enabled = true
instance_id = "auto"
role = "auto"
deployment_mode = "cloud"
port_range = { start = 30000, end = 34999 }

[ha.redis]
url = "redis://10.0.1.10:6379/0"
key_prefix = "forge:ha:"
heartbeat_interval_secs = 10
failover_timeout_secs = 25
session_ttl_secs = 3600
conference_ttl_secs = 7200

[ha.cloud]
provider = "gcp"
health_check_path = "/health"
standby_returns_503 = true

[ha.onprem]
vip = "10.0.1.100"
interface = "eth0"
virtual_router_id = 51
priority = 100
auth_password = "SECRET123"
```

## Success Criteria

- ✅ Two or more instances run simultaneously (primary + standby)
- ✅ Instance failure detected within 25-30 seconds
- ✅ Failover completes within 30-40 seconds total
- ✅ RTP packet loss < 100ms during failover
- ✅ Active conferences remain active during failover
- ✅ Works with cloud load balancers (GCP, AWS, Azure, Linode)
- ✅ Works on-premises with VRRP/Keepalived
- ✅ Configuration-driven deployment

## Implementation Timeline

| Phase | Duration | Description |
|-------|----------|-------------|
| Phase 1 | 1 week | forge-ha crate foundation |
| Phase 2 | 3 days | Session serialization |
| Phase 3 | 1 week | Session manager integration |
| Phase 4 | 2 days | Port pool recovery |
| Phase 5 | 3 days | API integration |
| Phase 6 | 1 day | Configuration |
| Phase 7 | 2 days | Keepalived integration |
| Phase 8 | 1 week | Testing |
| **Total** | **3-4 weeks** | **Full HA implementation** |

## Critical Files Summary

1. **crates/forge-ha/src/state_sync.rs** - Core state synchronization
2. **crates/forge-ha/src/failover.rs** - Failover orchestration
3. **crates/forge-engine/src/session.rs** - Add to_state/from_state methods
4. **crates/forge-engine/src/manager.rs** - HA backend integration
5. **crates/forge-rtp/src/port_pool.rs** - Recovery constructors
6. **crates/forge-api/src/server.rs** - Initialize HA on startup
7. **crates/forge-api/src/routes/ha.rs** - HA endpoints
8. **crates/forge-core/src/config.rs** - HAConfig structure
