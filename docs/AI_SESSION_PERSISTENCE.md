# AI Session Persistence & Recovery

**Status**: Implemented (v0.5.0)

## Overview

AI Session Persistence & Recovery enables AI-powered calls to survive connection drops and server restarts by automatically saving session state and reconnecting with exponential backoff.

## Features

### Core Capabilities
- **State Persistence**: Save AI session state to disk or Redis
- **Automatic Reconnection**: Retry failed connections with exponential backoff (1s → 2s → 4s → 8s → 16s → 32s → 60s max)
- **Server Restart Recovery**: Restore active sessions after server crashes/restarts
- **Health Checks**: Periodic monitoring of persistence backend and session health
- **Graceful Degradation**: Continue operating even if persistence fails

### Persistence Backends

#### Disk Backend
- **Storage**: JSON files on local filesystem
- **Location**: `/var/lib/forge/ai-sessions/` (configurable)
- **Good for**: Single-server deployments, development
- **Pros**: No external dependencies, simple setup
- **Cons**: Not shared across servers

#### Redis Backend
- **Storage**: Redis key-value store with TTL
- **Requires**: Compile with `--features persistence-redis`
- **Good for**: Production, load-balanced deployments
- **Pros**: Shared state, high availability, automatic expiration
- **Cons**: Requires Redis server

## Configuration

### Basic Setup (Disk)

Add to your `forge.toml`:

```toml
[engine.ai_persistence]
enabled = true
backend = "disk"
disk_path = "/var/lib/forge/ai-sessions"
max_reconnect_attempts = 10
health_check_interval_secs = 30
auto_reconnect = true
```

### Production Setup (Redis)

```toml
[engine.ai_persistence]
enabled = true
backend = "redis"
redis_url = "redis://localhost:6379"
redis_key_prefix = "forge:ai:session:"
redis_ttl_secs = 86400  # 24 hours
max_reconnect_attempts = 10
health_check_interval_secs = 30
auto_reconnect = true
```

### High-Availability Redis

```toml
[engine.ai_persistence]
enabled = true
backend = "redis"
redis_url = "redis://:your-password@redis-ha.example.com:6379/0"
redis_key_prefix = "prod:forge:ai:"
redis_ttl_secs = 172800  # 48 hours
max_reconnect_attempts = 20  # More aggressive retries
health_check_interval_secs = 15  # More frequent checks
auto_reconnect = true
```

## Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | bool | `false` | Enable AI session persistence |
| `backend` | `"disk"` \| `"redis"` | `"disk"` | Persistence backend type |
| `disk_path` | path | `/var/lib/forge/ai-sessions` | Directory for disk storage |
| `redis_url` | string | `None` | Redis connection URL |
| `redis_key_prefix` | string | `forge:ai:session:` | Prefix for Redis keys |
| `redis_ttl_secs` | u64 | `86400` | Redis key TTL (24h) |
| `max_reconnect_attempts` | u32 | `10` | Max reconnection attempts before failure |
| `health_check_interval_secs` | u64 | `30` | Health check interval |
| `auto_reconnect` | bool | `true` | Enable automatic reconnection |

## Reconnection Behavior

### Exponential Backoff

The system uses exponential backoff for reconnection attempts:

```
Attempt 1:  1 second
Attempt 2:  2 seconds
Attempt 3:  4 seconds
Attempt 4:  8 seconds
Attempt 5: 16 seconds
Attempt 6: 32 seconds
Attempt 7+: 60 seconds (capped)
```

**Total retry time with 10 attempts**: ~2 minutes

### Connection States

| State | Description |
|-------|-------------|
| `Connected` | Session is active and connected to AI service |
| `Disconnected` | Session lost connection, eligible for reconnection |
| `Reconnecting` | Currently attempting to reconnect |
| `Failed` | Exceeded max reconnection attempts |
| `Terminated` | Explicitly closed, no reconnection |

### State Transitions

```
┌──────────────┐
│  Connected   │ ──┐
└──────────────┘   │ Connection drop
       ▲           ▼
       │    ┌──────────────┐
       │    │ Disconnected │
       │    └──────────────┘
       │           │ Auto-reconnect
       │           ▼
       │    ┌──────────────┐     Max attempts exceeded
       │    │ Reconnecting │ ────────────────────────┐
       │    └──────────────┘                         │
       │           │ Success                         ▼
       └───────────┘                          ┌──────────┐
                                              │  Failed  │
                                              └──────────┘
```

## Usage

### Programmatic Usage

```rust
use forge_engine::{AISessionManager, AIPersistenceConfig};
use forge_engine::persistence::DiskBackend;
use std::sync::Arc;

// Create persistence backend
let backend = Arc::new(
    DiskBackend::new("/var/lib/forge/ai-sessions".into())
        .await?
);

// Create manager with persistence
let manager = Arc::new(AISessionManager::new_with_persistence(backend));

// Restore sessions on startup
manager.restore_sessions(None).await?;

// Start health check task
let health_task = manager.clone().start_health_check_task(
    std::time::Duration::from_secs(30)
);

// ... use manager normally ...

// Sessions are automatically persisted
manager.attach_ai(call_id, config, None).await?;
```

### Redis Backend

```rust
#[cfg(feature = "persistence-redis")]
use forge_engine::persistence::RedisBackend;

// Create Redis backend
let backend = Arc::new(
    RedisBackend::new_with_options(
        "redis://localhost:6379",
        "forge:ai:session:",
        86400,  // 24h TTL
    ).await?
);

let manager = Arc::new(AISessionManager::new_with_persistence(backend));
```

### Manual Reconnection

```rust
// Manually trigger reconnection
manager.try_reconnect(&call_id, None).await?;

// Handle disconnection with auto-reconnect
manager.handle_disconnection(&call_id, true).await?;
```

## Deployment

### System Requirements

#### Disk Backend
- Writable directory: `/var/lib/forge/ai-sessions` (or configured path)
- Disk space: ~10KB per session
- Permissions: forge process must have read/write access

#### Redis Backend
- Redis server 5.0+
- Network connectivity to Redis
- Compile with: `cargo build --features persistence-redis`

### Docker Setup

#### Disk Backend
```yaml
services:
  forge:
    image: forge-media:latest
    volumes:
      - ./ai-sessions:/var/lib/forge/ai-sessions
    environment:
      - FORGE_AI_PERSISTENCE_ENABLED=true
      - FORGE_AI_PERSISTENCE_BACKEND=disk
```

#### Redis Backend
```yaml
services:
  forge:
    image: forge-media:latest
    depends_on:
      - redis
    environment:
      - FORGE_AI_PERSISTENCE_ENABLED=true
      - FORGE_AI_PERSISTENCE_BACKEND=redis
      - FORGE_AI_PERSISTENCE_REDIS_URL=redis://redis:6379

  redis:
    image: redis:7-alpine
    volumes:
      - redis-data:/data
    command: redis-server --appendonly yes

volumes:
  redis-data:
```

### Kubernetes Setup

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: forge-config
data:
  forge.toml: |
    [engine.ai_persistence]
    enabled = true
    backend = "redis"
    redis_url = "redis://forge-redis:6379"
    max_reconnect_attempts = 15
    health_check_interval_secs = 20
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-media
spec:
  replicas: 3
  template:
    spec:
      containers:
      - name: forge
        image: forge-media:latest
        volumeMounts:
        - name: config
          mountPath: /etc/forge
      volumes:
      - name: config
        configMap:
          name: forge-config
```

## Monitoring

### Health Checks

The health check task periodically:
1. Pings the persistence backend (PING for Redis, write test for disk)
2. Lists all persisted sessions
3. Detects disconnected sessions
4. Triggers automatic reconnection for eligible sessions

### Logs

Key log patterns:

```
INFO  AI attached to call abc123
INFO  Saved AI session state for call abc123 to Redis (TTL: 86400s)
WARN  Handling unexpected disconnection for call abc123
INFO  Reconnecting AI session for call abc123 (attempt 1/10) after 1s
INFO  Successfully reconnected AI session for call abc123
ERROR Max reconnection attempts (10) exceeded for call abc123
```

### Metrics

Monitor these indicators:
- Session save/load latency
- Reconnection success rate
- Failed reconnections by cause
- Health check pass/fail rate
- Active sessions vs persisted sessions

## Troubleshooting

### Common Issues

#### Sessions Not Persisting

**Symptoms**: Sessions lost after restart
**Causes**:
- `enabled = false` in config
- Disk path not writable
- Redis connection failed

**Solutions**:
```bash
# Check config
grep "ai_persistence" /etc/forge/forge.toml

# Check disk permissions
ls -la /var/lib/forge/ai-sessions

# Test Redis connection
redis-cli -u redis://localhost:6379 PING
```

#### Reconnection Failures

**Symptoms**: Sessions not reconnecting
**Causes**:
- Max attempts exceeded
- Invalid API key
- Network issues

**Solutions**:
```bash
# Check logs
journalctl -u forge-media | grep "reconnect"

# Increase max attempts
[engine.ai_persistence]
max_reconnect_attempts = 20

# Check API key validity
curl -H "Authorization: Bearer $OPENAI_API_KEY" \
     https://api.openai.com/v1/models
```

#### Redis Connection Issues

**Symptoms**: "Failed to connect to Redis" errors
**Causes**:
- Redis not running
- Wrong URL
- Authentication failure

**Solutions**:
```bash
# Test Redis
redis-cli -u redis://localhost:6379 PING

# Check Redis logs
docker logs redis

# Verify URL format
redis_url = "redis://[username][:password]@host:port[/db]"
```

### Performance Tuning

#### Disk Backend
- Use SSD for better latency
- Periodically clean old sessions
- Monitor disk space usage

#### Redis Backend
- Use Redis Cluster for high availability
- Enable AOF persistence for durability
- Monitor Redis memory usage
- Set appropriate TTL values

## Security Considerations

### Sensitive Data

AI session state includes:
- API keys (in config)
- Conversation history (optional)
- Call metadata

**Recommendations**:
1. **Disk**: Restrict file permissions to forge user only
   ```bash
   chmod 700 /var/lib/forge/ai-sessions
   chown forge:forge /var/lib/forge/ai-sessions
   ```

2. **Redis**: Use authentication and TLS
   ```toml
   redis_url = "rediss://:password@redis.example.com:6380"
   ```

3. **Encryption**: Consider encrypting sensitive fields

### Network Security

For Redis deployments:
- Use private networks
- Enable Redis AUTH
- Use TLS (rediss://)
- Firewall Redis port (6379)

## Migration

### Enabling Persistence on Existing System

1. **Add configuration** to `forge.toml`
2. **Restart server** to load config
3. **Verify** persistence is working:
   ```bash
   # Disk backend
   ls -la /var/lib/forge/ai-sessions/

   # Redis backend
   redis-cli --scan --pattern "forge:ai:session:*"
   ```

4. **Monitor logs** for persistence activity

### Migrating Between Backends

To migrate from disk to Redis:

```bash
# 1. Export from disk
cat /var/lib/forge/ai-sessions/*.json > backup.json

# 2. Update config to use Redis
[engine.ai_persistence]
backend = "redis"
redis_url = "redis://localhost:6379"

# 3. Restart server
systemctl restart forge-media

# 4. Verify new sessions use Redis
redis-cli --scan --pattern "forge:ai:session:*"
```

## Best Practices

1. **Development**: Use disk backend for simplicity
2. **Production**: Use Redis backend for reliability
3. **Multi-Server**: Always use Redis backend
4. **Monitoring**: Enable health checks and log monitoring
5. **Backups**: Regularly backup Redis or disk storage
6. **TTL**: Set appropriate TTL based on call duration patterns
7. **Cleanup**: Implement periodic cleanup of old sessions

## API Reference

### AISessionManager Methods

```rust
/// Create manager with persistence
pub fn new_with_persistence(
    persistence: Arc<dyn PersistenceBackend>
) -> Self

/// Restore all persisted sessions
pub async fn restore_sessions(
    &self,
    event_bus: Option<Arc<EventBus>>
) -> Result<()>

/// Manually reconnect a session
pub async fn try_reconnect(
    &self,
    call_id: &CallId,
    event_bus: Option<Arc<EventBus>>
) -> Result<()>

/// Start health check background task
pub fn start_health_check_task(
    self: Arc<Self>,
    check_interval: Duration
) -> JoinHandle<()>

/// Handle unexpected disconnection
pub async fn handle_disconnection(
    &self,
    call_id: &CallId,
    auto_reconnect: bool
) -> Result<()>
```

### PersistenceBackend Trait

```rust
#[async_trait]
pub trait PersistenceBackend: Send + Sync {
    async fn save(&self, session: &PersistedAISession) -> Result<()>;
    async fn load(&self, call_id: &CallId) -> Result<Option<PersistedAISession>>;
    async fn delete(&self, call_id: &CallId) -> Result<()>;
    async fn list_all(&self) -> Result<HashMap<CallId, PersistedAISession>>;
    async fn health_check(&self) -> Result<bool>;
}
```

## Examples

### Example 1: Simple Disk Persistence

```rust
use forge_engine::{AISessionManager, persistence::DiskBackend};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // Create disk backend
    let backend = Arc::new(
        DiskBackend::new("/var/lib/forge/ai-sessions".into()).await?
    );

    // Create manager
    let manager = Arc::new(AISessionManager::new_with_persistence(backend));

    // Restore sessions on startup
    manager.restore_sessions(None).await?;

    // Sessions are now automatically persisted
    Ok(())
}
```

### Example 2: Redis with Health Checks

```rust
use forge_engine::{AISessionManager, persistence::RedisBackend};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // Create Redis backend
    let backend = Arc::new(
        RedisBackend::new("redis://localhost:6379").await?
    );

    // Create manager
    let manager = Arc::new(AISessionManager::new_with_persistence(backend));

    // Start health checks
    let _health_task = manager.clone().start_health_check_task(
        Duration::from_secs(30)
    );

    // Restore existing sessions
    manager.restore_sessions(None).await?;

    Ok(())
}
```

## See Also

- [AI Integration Guide](./AI_INTEGRATION.md)
- [Conference AI Integration](./CONFERENCE_AI_INTEGRATION.md)
- [Configuration Reference](../config/forge.toml.example)

## Changelog

### v0.5.0 (2025-01-XX)
- ✨ Initial implementation of AI Session Persistence & Recovery
- ✅ Disk backend with JSON storage
- ✅ Redis backend with optional feature flag
- ✅ Exponential backoff reconnection (1s → 60s)
- ✅ Health check monitoring
- ✅ Automatic session restoration on startup
- ✅ Configuration via forge.toml
- ✅ Comprehensive test coverage

---

**Next Steps**: Explore [Conference AI Integration](./CONFERENCE_AI_INTEGRATION.md) for multi-participant AI scenarios.
