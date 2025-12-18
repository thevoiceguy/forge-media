# API Authentication Configuration

This document explains how authentication works in the Forge Media Engine API and how to configure it securely.

## Overview

As of version 0.4.0, Forge implements **secure-by-default authentication** to prevent accidental deployments without proper access control. The API uses bearer token authentication for all non-public endpoints.

## Default Behavior

### Auto-Generated Tokens

If no authentication configuration is provided, Forge will **automatically generate a secure random token** on startup and print it to stderr:

```
⚠️  WARNING: No FORGE_API_TOKEN set. Auto-generated token: a1b2c3d4-e5f6-7890-abcd-ef1234567890
   Set FORGE_API_TOKEN environment variable or add to config file.
   To disable auth explicitly, set 'disable_auth = true' in [api] config.
```

**Important:** Save this token immediately! You'll need it to authenticate API requests.

### Public Endpoints

The following endpoints are always accessible without authentication:
- `GET /health` - Basic health check
- `GET /ha/health` - HA-specific health probe (for load balancers)

All other endpoints require a valid bearer token.

## Configuration Methods

### Method 1: Environment Variable (Recommended)

Set the `FORGE_API_TOKEN` environment variable before starting Forge:

```bash
export FORGE_API_TOKEN="your-secure-token-here"
./forge-media
```

**Docker/Kubernetes:**
```yaml
env:
  - name: FORGE_API_TOKEN
    valueFrom:
      secretKeyRef:
        name: forge-secrets
        key: api-token
```

### Method 2: Configuration File

Add tokens to your `config.toml`:

```toml
[api]
http_bind = "0.0.0.0:8080"

# Single token
auth_tokens = ["your-secure-token-here"]

# Multiple tokens (for different clients)
auth_tokens = [
    "token-for-client-a",
    "token-for-client-b",
    "token-for-monitoring"
]
```

### Method 3: Disable Authentication (Not Recommended)

To explicitly disable authentication (e.g., for development or internal networks):

```toml
[api]
http_bind = "0.0.0.0:8080"
disable_auth = true
```

⚠️ **WARNING:** Only use `disable_auth = true` in trusted environments. This exposes all API endpoints without access control.

## Using Bearer Tokens

Include the token in the `Authorization` header of all API requests:

```bash
curl -H "Authorization: Bearer your-token-here" \
     http://localhost:8080/v1/sessions
```

**JavaScript/TypeScript:**
```typescript
const response = await fetch('http://localhost:8080/v1/sessions', {
  headers: {
    'Authorization': 'Bearer your-token-here'
  }
});
```

**Python:**
```python
import requests

headers = {'Authorization': 'Bearer your-token-here'}
response = requests.get('http://localhost:8080/v1/sessions', headers=headers)
```

## Token Best Practices

### 1. Generate Strong Tokens

Use cryptographically secure random tokens:

```bash
# Linux/macOS
openssl rand -hex 32

# Or use UUIDs
uuidgen
```

### 2. Rotate Tokens Regularly

Configure multiple tokens to enable rotation without downtime:

```toml
[api]
auth_tokens = [
    "current-token",
    "new-token"  # Add new token first
]
```

After all clients migrate to `new-token`, remove `current-token`.

### 3. Use Secret Management

**Never commit tokens to version control!**

- Store in environment variables
- Use secret management systems (Vault, AWS Secrets Manager, etc.)
- Use Kubernetes Secrets
- Inject at runtime via CI/CD

### 4. Scope Tokens by Client

Use different tokens for different purposes:

```toml
[api]
auth_tokens = [
    "sip-gateway-token",     # For SIP gateway integration
    "monitoring-token",      # For Prometheus/metrics
    "admin-token"            # For administrative tasks
]
```

This enables:
- Per-client token rotation
- Access audit trails (via logs)
- Token revocation without affecting other clients

## Security Considerations

### Production Deployments

For production deployments:

1. ✅ **Always set `FORGE_API_TOKEN`** or configure `auth_tokens`
2. ✅ **Never use `disable_auth = true`** in production
3. ✅ **Use HTTPS/TLS** to encrypt tokens in transit
4. ✅ **Rotate tokens** every 90 days minimum
5. ✅ **Monitor authentication failures** via logs/metrics
6. ✅ **Use rate limiting** (enabled by default)

### Network Isolation

Even with authentication, consider:

- Placing Forge behind a reverse proxy (nginx, Traefik)
- Using network firewalls to restrict access
- VPN/private networks for sensitive deployments
- API gateways for advanced access control

### Rate Limiting

Forge includes built-in rate limiting to prevent brute-force attacks:

```toml
[api]
rate_limit_requests_per_window = 120  # Max requests per window
rate_limit_window_secs = 60          # Window duration in seconds
```

Default: 120 requests per 60 seconds (2 req/sec average).

## Troubleshooting

### "Unauthorized: Missing or invalid token"

**Cause:** No `Authorization` header or invalid token.

**Solution:**
1. Check that you're including `Authorization: Bearer <token>` header
2. Verify token matches configured `auth_tokens` or `FORGE_API_TOKEN`
3. Check logs for the configured token (if auto-generated)

### "Rate limit exceeded"

**Cause:** Too many requests from the same IP.

**Solution:**
1. Wait for the rate limit window to reset
2. Reduce request frequency
3. Adjust `rate_limit_requests_per_window` if needed for your use case

### Token Not Working After Restart

**Cause:** Auto-generated token changes on each restart.

**Solution:** Set `FORGE_API_TOKEN` environment variable or configure `auth_tokens` in config file to persist the same token across restarts.

## Migration from Previous Versions

### Upgrading from < 0.4.0

**Before (no auth):**
```toml
[api]
http_bind = "0.0.0.0:8080"
# No auth configuration
```

**After (secure by default):**

**Option A - Set environment variable:**
```bash
export FORGE_API_TOKEN="your-token"
```

**Option B - Update config:**
```toml
[api]
http_bind = "0.0.0.0:8080"
auth_tokens = ["your-token"]
```

**Option C - Explicitly disable (not recommended):**
```toml
[api]
http_bind = "0.0.0.0:8080"
disable_auth = true
```

Update all API clients to include the `Authorization: Bearer <token>` header.

## Examples

### Complete Configuration

```toml
[api]
http_bind = "0.0.0.0:8080"
ws_bind = "0.0.0.0:8081"
enable_cors = true
cors_origins = ["https://app.example.com"]

# Authentication
auth_tokens = [
    "prod-api-token-abc123",
    "monitoring-token-xyz789"
]

# Rate limiting
rate_limit_requests_per_window = 200
rate_limit_window_secs = 60

# TLS (recommended for production)
tls_cert = "/etc/forge/certs/server.crt"
tls_key = "/etc/forge/certs/server.key"
```

### Docker Compose with Secrets

```yaml
version: '3.8'
services:
  forge-media:
    image: forge-media:latest
    environment:
      FORGE_API_TOKEN: ${FORGE_API_TOKEN}
    ports:
      - "8080:8080"
    volumes:
      - ./config.toml:/etc/forge/config.toml
    secrets:
      - api_token

secrets:
  api_token:
    external: true
```

### Kubernetes with Secrets

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: forge-secrets
type: Opaque
stringData:
  api-token: "your-secure-token-here"

---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-media
spec:
  template:
    spec:
      containers:
      - name: forge-media
        image: forge-media:latest
        env:
        - name: FORGE_API_TOKEN
          valueFrom:
            secretKeyRef:
              name: forge-secrets
              key: api-token
```

## See Also

- [Health Endpoints](./HEALTH_ENDPOINTS.md) - Health check configuration
- [Prometheus Metrics](./PROMETHEUS_ALERTS.md) - Monitoring and alerting
- [HA Setup](./ha-setup.md) - High availability configuration
