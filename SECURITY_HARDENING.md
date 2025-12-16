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
| SEC-002 | 🔴 HIGH | AI API Keys | ⚠️ PENDING | 4-6 hours |
| SEC-003 | 🟡 MEDIUM | Recording Paths | ⚠️ PENDING | 2-3 hours |
| SEC-004 | 🟡 MEDIUM | RTP Port Allocation | ⚠️ PENDING | 2-3 hours |
| SEC-005 | 🔵 LOW | Security Defaults | ⚠️ PENDING | 2-3 hours |

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

## SEC-002: AI API Key Exposure and SSRF [PENDING]

### Severity: 🔴 HIGH

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

**Implementation:**
```rust
// crates/forge-core/src/types.rs - New secure string type
use std::fmt;

/// Secure string that redacts its value in logs and debug output
#[derive(Clone)]
pub struct SecureString(String);

impl SecureString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecureString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Display for SecureString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl serde::Serialize for SecureString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("[REDACTED]")
    }
}
```

**Usage:**
```rust
// Update AISessionConfig
pub struct AISessionConfig {
    pub api_key: SecureString,  // Was: String
    // ...
}

// Update request handling
let config = AISessionConfig {
    api_key: SecureString::new(request.api_key),
    // ...
};
```

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

## SEC-003: Recording Directory Path Traversal [PENDING]

### Severity: 🟡 MEDIUM

### Vulnerability
Recording directory validation follows symlinks and creates directories at any configured path without bounds checking.

**Vulnerable Code:**
```rust
// crates/forge-api/src/server.rs:240-260
let recording_dir = PathBuf::from(&config.recording_base_dir);
fs::create_dir_all(&recording_dir)?;  // No canonicalization
fs::write(recording_dir.join("test.txt"), "test")?;  // Follows symlinks
```

### Impact
- **Path Traversal**: Malicious config could write to `/etc/passwd`, `/root/.ssh/authorized_keys`
- **Symlink Attack**: Attacker with filesystem access could symlink recording dir to sensitive location
- **Arbitrary File Write**: Test write could overwrite existing files

### Remediation

**Implementation:**
```rust
use std::fs;
use std::path::{Path, PathBuf};

/// Validate and create recording directory securely
fn setup_recording_directory(base_dir: &Path, root: &Path) -> Result<PathBuf, Error> {
    // 1. Canonicalize the requested path
    let canonical = base_dir.canonicalize()
        .or_else(|_| {
            // If doesn't exist, create then canonicalize
            fs::create_dir_all(base_dir)?;
            base_dir.canonicalize()
        })?;

    // 2. Ensure it's within allowed root
    if !canonical.starts_with(root) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("Recording directory must be under {}", root.display())
        ));
    }

    // 3. Check for symlinks in path components
    for component in canonical.components() {
        let component_path = PathBuf::from(component.as_os_str());
        if component_path.is_symlink() {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Symlinks not allowed in recording path"
            ));
        }
    }

    // 4. Test write to temp file (not .txt which could clash)
    let test_path = canonical.join(format!(".forge-test-{}", std::process::id()));
    fs::write(&test_path, b"test")?;
    fs::remove_file(&test_path)?;

    Ok(canonical)
}
```

**Configuration:**
```rust
impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            // ...
            recording_base_dir: PathBuf::from("/var/lib/forge/recordings"),
            recording_root_jail: PathBuf::from("/var/lib/forge"),  // NEW: Jail root
        }
    }
}
```

**Apply in ApiServer::new():**
```rust
// Validate recording directory
let recording_dir = setup_recording_directory(
    &config.recording_base_dir,
    &config.recording_root_jail,
).map_err(|e| format!("Invalid recording directory: {}", e))?;

info!("✓ Recording directory validated: {}", recording_dir.display());
```

**Deployment Best Practices:**
1. Run Forge under dedicated low-privilege user (`forge:forge`)
2. Set strict permissions on `/var/lib/forge`: `chmod 750`, `chown forge:forge`
3. Use AppArmor/SELinux profiles to restrict file access
4. Mount recording directory with `nosuid,nodev,noexec` options

---

## SEC-004: RTP Port Prediction [PENDING]

### Severity: 🟡 MEDIUM

### Vulnerability
RTP port allocation is sequential and deterministic, making active ports easy to predict for traffic analysis and hijacking attempts.

**Vulnerable Code:**
```rust
// crates/forge-rtp/src/port_pool.rs:50-82
pub async fn allocate_pair(&self) -> Result<(u16, u16), PortAllocationError> {
    for port in (self.range_start..=self.range_end).step_by(2) {
        if self.allocated.insert(port) {
            return Ok((port, port + 1));
        }
    }
    // Sequential allocation is predictable
}
```

### Impact
- **Port Scanning**: Attacker can predict which ports are likely in use
- **Traffic Analysis**: Sequential allocation reveals call volume patterns
- **Session Hijacking**: Easier to target active RTP streams
- **DoS**: Can pre-emptively bind predicted ports

### Remediation

**Randomized Allocation:**
```rust
use rand::seq::SliceRandom;
use rand::thread_rng;

pub struct PortPool {
    allocated: DashSet<u16>,
    available: Vec<u16>,  // Pre-shuffled port list
    exhaustion_threshold: f32,  // Alert at 80% usage
}

impl PortPool {
    pub fn new(range_start: u16, range_end: u16) -> Self {
        // Generate list of even ports (RTP uses even, RTCP uses odd)
        let mut ports: Vec<u16> = (range_start..=range_end)
            .step_by(2)
            .collect();

        // Shuffle for random allocation
        ports.shuffle(&mut thread_rng());

        Self {
            allocated: DashSet::new(),
            available: ports,
            exhaustion_threshold: 0.8,
        }
    }

    pub async fn allocate_pair(&self) -> Result<(u16, u16), PortAllocationError> {
        // Check for exhaustion
        let usage = self.allocated.len() as f32 / self.available.len() as f32;
        if usage > self.exhaustion_threshold {
            tracing::warn!(
                "Port pool {}% exhausted ({}/{})",
                (usage * 100.0) as u32,
                self.allocated.len(),
                self.available.len()
            );
        }

        // Try random ports from pre-shuffled list
        for &port in &self.available {
            if self.allocated.insert(port) {
                return Ok((port, port + 1));
            }
        }

        Err(PortAllocationError::Exhausted)
    }
}
```

**Additional Hardening:**
1. **Per-Tenant Pools**: Isolate port ranges by tenant/customer
2. **Exhaustion Metrics**: Expose `forge_rtp_port_pool_utilization` gauge
3. **Rate Limiting**: Limit session creation rate per IP/tenant

---

## SEC-005: Insecure Defaults [PENDING]

### Severity: 🔵 LOW (but important for production)

### Vulnerability
Default configuration is permissive for development convenience but insecure for production.

**Current Defaults:**
```rust
// crates/forge-api/src/server.rs:46-70
impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            enable_cors: true,  // ❌ Enabled by default
            allowed_origins: vec!["http://localhost:3000".to_string()],  // ❌ HTTP
            auth_tokens: Vec::new(),  // ❌ No auth by default
            enable_https: false,  // ❌ HTTPS disabled
            trusted_proxies: Vec::new(),  // ✅ Good - deny by default
            // ...
        }
    }
}
```

### Impact
- **CORS Bypass**: Any origin can access API in default config
- **No Authentication**: API fully open without auth tokens
- **Plaintext Communication**: HTTPS not enforced
- **Credential Sniffing**: Auth tokens transmitted over HTTP

### Remediation

**Secure Defaults:**
```rust
impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),  // Localhost only
            enable_cors: false,  // Require explicit enablement
            allowed_origins: Vec::new(),  // Must configure
            auth_tokens: Vec::new(),  // Must configure
            require_auth: true,  // NEW: Fail if no auth configured
            enable_https: true,  // NEW: Require HTTPS by default
            https_bind: Some("0.0.0.0:8443".parse().unwrap()),
            tls_cert: None,  // Must configure
            tls_key: None,  // Must configure
            trusted_proxies: Vec::new(),  // Deny by default
            // ...
        }
    }
}
```

**Startup Validation:**
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
