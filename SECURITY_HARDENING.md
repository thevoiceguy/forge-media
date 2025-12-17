# Forge Media Engine - Security Hardening Guide

**Version:** 1.0
**Date:** 2025-12-16
**Status:** Action Required

This document outlines critical security vulnerabilities discovered during code review and provides implementation guidance for fixes.

---

## Critical Issues Summary

| ID | Severity | Component | Status | Remediation Time |
|----|----------|-----------|--------|------------------|
| SEC-001 | 🔴 HIGH | Rate Limiting | ✅ FIXED | - |
| SEC-002 | 🔴 HIGH | AI API Keys | ✅ FIXED | 4-6 hours |
| SEC-003 | 🟡 MEDIUM | Recording Paths | ✅ FIXED | 2-3 hours |
| SEC-004 | 🟡 MEDIUM | RTP Port Allocation | ✅ FIXED | 2-3 hours |
| SEC-005 | 🔵 LOW | Security Defaults | ✅ FIXED | 2-3 hours |

---

## SEC-001: Rate Limiting X-Forwarded-For Spoofing [FIXED]

### Status: ✅ FIXED (2025-12-16)

### Vulnerability
Rate limiting middleware trusted the `X-Forwarded-For` header without validating the request originated from a trusted proxy, allowing clients to spoof IPs and bypass rate limits or block legitimate users.

### Impact
- **Bypass**: Attackers could set arbitrary `X-Forwarded-For` headers to evade rate limits
- **DoS**: Attackers could spoof IP addresses of legitimate users to exhaust their quotas
- **Tracking Evasion**: Made rate-based abuse detection ineffective

### Fix Implemented

**Changes:**
1. Added `trusted_proxies: Vec<IpAddr>` to `ApiServerConfig` (default: empty)
2. Updated `RateLimiter` to accept trusted proxy configuration
3. Modified middleware to:
   - Always use socket address by default
   - Only honor `X-Forwarded-For` when request originates from trusted proxy IP
   - Extract first IP from comma-separated list when trusted

**Modified Files:**
- `crates/forge-api/src/server.rs` - Added `trusted_proxies` config field
- `crates/forge-api/src/middleware/rate_limit.rs` - Secure IP extraction logic

**Configuration Example:**
```toml
# forge.toml
[api]
trusted_proxies = ["10.0.1.100", "10.0.1.101"]  # Nginx/HAProxy IPs
```

**Testing:**
```bash
# Direct request - uses socket address
curl -H "X-Forwarded-For: 1.2.3.4" http://localhost:8080/health
# Rate limit applies to actual socket IP, not spoofed header

# Through trusted proxy - honors X-Forwarded-For
# (Only if request comes from 10.0.1.100)
```

---

## SEC-002: AI API Key Exposure and SSRF [FIXED]

### Severity: 🔴 HIGH / Status: ✅ FIXED (redacted secrets + endpoint allowlist)

### Vulnerability
AI integration endpoints accept raw API keys in HTTP requests, store them in memory without encryption, log them in plaintext, and accept arbitrary custom endpoints without validation.

**Vulnerable Code:**
```rust
// crates/forge-api/src/routes/ai.rs:17-38
pub struct AttachAIRequest {
    pub api_key: String,  // Plaintext in request, logs, memory
    pub endpoint: Option<String>,  // No validation, SSRF vector
    // ...
}

// crates/forge-api/src/routes/ai.rs:95-98
let config = AISessionConfig {
    api_key: request.api_key,  // Stored in memory
    endpoint: None,  // No host validation
    // ...
};

// crates/forge-engine/src/ai_integration.rs
// AISession stores api_key in plaintext
```

### Impact
1. **Memory Scraping**: API keys visible in core dumps, memory inspection tools
2. **Log Leakage**: Keys appear in application logs, tracing output
3. **SSRF**: Attacker can specify malicious endpoints to:
   - Probe internal network services
   - Exfiltrate data to external hosts
   - Abuse cloud metadata services (169.254.169.254)
4. **Metrics Exposure**: Keys may appear in Prometheus metrics/error messages

### Remediation Steps

#### 1. API Key Redaction

**Implementation Highlights:**
- Added `SecureString` type with custom `Serialize`/`Deserialize` implementations (redacts in Display/Debug/JSON) and switched `AISessionConfig` + `AIConnectorConfig` to hold secrets safely.
- Custom `Serialize` always outputs `"[REDACTED]"` to prevent leakage in API responses, logs, and metrics.
- Custom `Deserialize` rejects `"[REDACTED]"` placeholder to prevent config mistakes.
- API endpoints now accept optional AI endpoints but enforce HTTPS/WSS, block private/loopback hosts, and require hosts to be in an allowlist (`api.ai_allowed_endpoints`).
- Comprehensive endpoint validation: blocks private IPs (RFC 1918), loopback, link-local, and AWS metadata endpoints.
- Configurable allowlist defaults to major AI providers (OpenAI, Anthropic, Deepgram, ElevenLabs).
- AI attach flows convert incoming keys to `SecureString` before storage; default configs and tests updated to avoid logging real keys.
- **11 comprehensive tests added** for `SecureString` covering serialization, deserialization, redaction, and edge cases.

#### 2. Endpoint Validation

**Add to `ApiServerConfig`:**
```rust
pub struct ApiServerConfig {
    // ... existing fields
    pub ai_allowed_endpoints: Vec<String>,  // Allowlist of AI provider endpoints
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            // ...
            ai_allowed_endpoints: vec![
                "https://api.openai.com".to_string(),
                "https://api.anthropic.com".to_string(),
                "https://api.deepgram.com".to_string(),
                "https://api.elevenlabs.io".to_string(),
            ],
        }
    }
}
```

**Add Validation Function:**
```rust
// crates/forge-api/src/routes/ai.rs
use url::Url;

fn validate_ai_endpoint(endpoint: &str, allowed_endpoints: &[String]) -> Result<(), String> {
    // Parse URL
    let url = Url::parse(endpoint)
        .map_err(|e| format!("Invalid endpoint URL: {}", e))?;

    // Enforce HTTPS
    if url.scheme() != "https" {
        return Err("Endpoint must use HTTPS".to_string());
    }

    // Check against allowlist
    let host = url.host_str().ok_or("Endpoint must have a host")?;
    if !allowed_endpoints.iter().any(|allowed| {
        let allowed_url = Url::parse(allowed).ok();
        allowed_url.map(|u| u.host_str() == Some(host)).unwrap_or(false)
    }) {
        return Err(format!("Endpoint host '{}' not in allowlist", host));
    }

    // Block private/internal IPs
    if host.starts_with("127.") || host.starts_with("169.254.")
        || host.starts_with("10.") || host.starts_with("192.168.")
        || host == "localhost" {
        return Err("Private/internal endpoints not allowed".to_string());
    }

    Ok(())
}
```

**Apply in Handler:**
```rust
// In attach_ai()
if let Some(ref endpoint) = request.endpoint {
    validate_ai_endpoint(endpoint, &state.config.ai_allowed_endpoints)
        .map_err(|e| ApiError::InvalidRequest(e))?;
}
```

#### 3. Secret Store Integration (Optional)

For production deployments, load API keys from a secret management system:

```rust
// Example: Kubernetes secrets
use std::fs;

fn load_api_key_from_secret(provider: &str) -> Result<SecureString, Error> {
    let path = format!("/var/run/secrets/forge/ai/{}", provider);
    let key = fs::read_to_string(path)?;
    Ok(SecureString::new(key.trim().to_string()))
}

// Example: AWS Secrets Manager
#[cfg(feature = "aws-secrets")]
async fn load_from_aws_secrets(secret_id: &str) -> Result<SecureString, Error> {
    let client = aws_sdk_secretsmanager::Client::new(&aws_config::load_from_env().await);
    let response = client.get_secret_value()
        .secret_id(secret_id)
        .send()
        .await?;
    Ok(SecureString::new(response.secret_string().unwrap().to_string()))
}
```

### Testing

**Test Cases:**
1. Verify API keys are redacted in logs: `grep -r "sk-" logs/` should return nothing
2. Test SSRF prevention:
   ```bash
   # Should fail - private IP
   curl -X POST /v1/sessions/test/ai \
     -d '{"api_key":"test","endpoint":"http://169.254.169.254/metadata"}'

   # Should fail - HTTP not HTTPS
   curl -X POST /v1/sessions/test/ai \
     -d '{"api_key":"test","endpoint":"http://api.openai.com"}'

   # Should fail - not in allowlist
   curl -X POST /v1/sessions/test/ai \
     -d '{"api_key":"test","endpoint":"https://evil.com"}'

   # Should succeed
   curl -X POST /v1/sessions/test/ai \
     -d '{"api_key":"test","endpoint":"https://api.openai.com"}'
   ```

---

## SEC-003: Recording Directory Path Traversal [FIXED]

### Severity: 🟡 MEDIUM / Status: ✅ FIXED (jail + canonicalization)

### Remediation
- Added `recording_root_jail` to config (defaults to `/var/lib/forge`) and require `recording_base_dir` to canonicalize inside it.
- Reject symlinked jail roots or recording dirs; create missing dirs safely; enforce writable check with PID-scoped temp file.
- Config samples updated with `recording_root_jail` for explicit bounding.

---

## SEC-004: RTP Port Prediction [FIXED]

### Severity: 🟡 MEDIUM / Status: ✅ FIXED (randomized allocations)

### Remediation
- Port pool now pre-shuffles even ports and picks a random available port on each allocation to reduce predictability (`crates/forge-rtp/src/port_pool.rs`).
- Deallocation returns ports to the pool while keeping randomness; specific-port allocations still enforced for range/evenness.
- Added `rand` dependency and updated tests to validate allocation validity without relying on deterministic ordering.

---

## SEC-005: Insecure Defaults [FIXED]

### Severity: 🔵 LOW / Status: ✅ FIXED (safer defaults + guardrails)

### Remediation
- Default API bind is now localhost-only; CORS disabled by default and allowlist starts empty.
- Startup guard prevents binding on non-loopback without authentication tokens configured; sample configs include a public-binding snippet requiring TLS and tokens.
```rust
impl ApiServer {
    pub async fn new(config: ApiServerConfig) -> Result<Self, Error> {
        // Validate secure configuration
        if config.require_auth && config.auth_tokens.is_empty() {
            return Err(Error::msg(
                "Authentication required but no auth_tokens configured. \
                 Set require_auth=false for testing only."
            ));
        }

        if config.enable_https && (config.tls_cert.is_none() || config.tls_key.is_none()) {
            return Err(Error::msg(
                "HTTPS enabled but tls_cert or tls_key not configured. \
                 Set enable_https=false for testing only."
            ));
        }

        if config.enable_cors && !config.enable_https {
            tracing::warn!(
                "⚠️  CORS enabled over HTTP - credentials may be exposed. \
                 Enable HTTPS for production."
            );
        }

        // ... rest of initialization
    }
}
```

**Configuration Template:**
```toml
# config/forge-production.toml
[api]
bind_addr = "0.0.0.0:8080"
enable_https = true
https_bind = "0.0.0.0:8443"
tls_cert = "/etc/forge/certs/server.crt"
tls_key = "/etc/forge/certs/server.key"

enable_cors = true
allowed_origins = ["https://app.example.com"]

auth_tokens = ["${FORGE_API_TOKEN}"]  # Load from environment
require_auth = true

trusted_proxies = ["10.0.1.100", "10.0.1.101"]

[api.rate_limiting]
requests_per_window = 100
window_secs = 60

[rtp]
port_range_min = 20000
port_range_max = 30000
randomize_allocation = true

[recording]
base_dir = "/var/lib/forge/recordings"
root_jail = "/var/lib/forge"

[ai]
allowed_endpoints = [
    "https://api.openai.com",
    "https://api.anthropic.com"
]
```

---

## Implementation Priority

### Immediate (Before Production)
1. ✅ **SEC-001**: Rate limiting (DONE)
2. 🔴 **SEC-002**: AI API key redaction and SSRF protection
3. 🟡 **SEC-003**: Recording path validation

### High Priority (Next Sprint)
4. 🟡 **SEC-004**: RTP port randomization
5. 🔵 **SEC-005**: Secure defaults

### Recommended (Architecture Review)
- Implement secret rotation mechanism
- Add audit logging for sensitive operations
- Security testing framework (fuzzing, penetration tests)
- Regular dependency vulnerability scanning (cargo-audit)

---

## Testing Checklist

### SEC-002 (AI Keys)
- [ ] API keys redacted in application logs
- [ ] API keys redacted in tracing output
- [ ] API keys redacted in Prometheus metrics
- [ ] API keys redacted in error messages
- [ ] SSRF blocked: `http://169.254.169.254`
- [ ] SSRF blocked: `http://localhost`
- [ ] SSRF blocked: `https://internal-service.local`
- [ ] HTTP endpoints rejected
- [ ] Endpoint allowlist enforced
- [ ] Core dump doesn't expose keys (test with ASAN/valgrind)

### SEC-003 (Recording Paths)
- [ ] Path traversal blocked: `../../../etc/passwd`
- [ ] Symlink traversal blocked
- [ ] Only paths under jail root allowed
- [ ] Directory created with correct permissions (750)
- [ ] Test write uses temp file, not production name

### SEC-004 (RTP Ports)
- [ ] Port allocation is non-sequential
- [ ] Cannot predict next allocated port
- [ ] Exhaustion alerts at 80% usage
- [ ] Prometheus metric `forge_rtp_port_pool_utilization` exposed
- [ ] Per-tenant isolation (if configured)

### SEC-005 (Defaults)
- [ ] HTTPS required by default
- [ ] CORS disabled by default
- [ ] Auth required by default
- [ ] Startup fails if insecure config (without explicit override)
- [ ] Production config template validates

---

## Deployment Hardening

### System-Level
```bash
# Create dedicated user
sudo useradd -r -s /sbin/nologin -d /var/lib/forge forge

# Set directory permissions
sudo mkdir -p /var/lib/forge/{recordings,prompts,siprec}
sudo chown -R forge:forge /var/lib/forge
sudo chmod 750 /var/lib/forge

# Set binary capabilities (if using privileged ports)
sudo setcap 'cap_net_bind_service=+ep' /usr/local/bin/forge-media

# AppArmor profile
sudo install -m 644 apparmor/forge-media /etc/apparmor.d/
sudo apparmor_parser -r /etc/apparmor.d/forge-media
```

### Container Security
```dockerfile
# Dockerfile
FROM rust:1.75-bookworm as builder
# ... build steps ...

FROM debian:bookworm-slim
RUN useradd -r -u 1001 -g users forge
USER forge:users

# Drop capabilities
RUN apt-get update && apt-get install -y libcap2-bin
RUN setcap 'cap_net_bind_service=+ep' /usr/local/bin/forge-media

# Read-only root filesystem
VOLUME ["/var/lib/forge"]
ENTRYPOINT ["/usr/local/bin/forge-media"]
```

### Kubernetes Deployment
```yaml
apiVersion: v1
kind: SecurityContext
spec:
  runAsNonRoot: true
  runAsUser: 1001
  readOnlyRootFilesystem: true
  allowPrivilegeEscalation: false
  capabilities:
    drop:
      - ALL
    add:
      - NET_BIND_SERVICE
  seccompProfile:
    type: RuntimeDefault
```

---

## References

- [OWASP Top 10 2021](https://owasp.org/www-project-top-ten/)
- [CWE-918: SSRF](https://cwe.mitre.org/data/definitions/918.html)
- [CWE-22: Path Traversal](https://cwe.mitre.org/data/definitions/22.html)
- [RFC 3550: RTP](https://datatracker.ietf.org/doc/html/rfc3550)
- [Rust Security Best Practices](https://anssi-fr.github.io/rust-guide/)

---

**Last Updated:** 2025-12-16
**Next Review:** 2025-12-23
**Owner:** Security Team / DevOps
