# Health Check Endpoints

Forge Media provides multiple health check endpoints for different operational purposes.

## Endpoints Overview

| Endpoint | Purpose | Auth Required | HA-Aware |
|----------|---------|---------------|----------|
| `GET /health` | General application health | No | Yes |
| `GET /ha/health` | Load balancer health probe | No | Yes |
| `GET /metrics/prometheus` | Prometheus metrics (includes health) | No | No |

## `/health` - General Health Check

**Endpoint:** `GET /health`

**Purpose:** General application health status for monitoring and diagnostics.

**Response:**
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "uptime_seconds": 3600
}
```

**HTTP Status Codes:**
- `200 OK` - Application is healthy and ready to serve traffic
  - When HA is **disabled**: Always returns 200
  - When HA is **enabled**: Returns 200 only when instance is primary and healthy
- `503 Service Unavailable` - Application is running but not ready
  - When HA is **enabled**: Returns 503 when instance is standby or degraded

**Use Cases:**
- General monitoring dashboards
- Health checks from service meshes
- Application-level diagnostics

**Example:**
```bash
curl http://localhost:8080/health

# Check if healthy (exit 0 if 200, exit 1 otherwise)
curl -f http://localhost:8080/health
```

---

## `/ha/health` - HA Load Balancer Probe

**Endpoint:** `GET /ha/health`

**Purpose:** Specialized health probe for load balancers in HA deployments.

**Response:**
- Returns only HTTP status code (no body)

**HTTP Status Codes:**
- `200 OK` - Instance is primary and healthy (load balancer should route traffic here)
- `503 Service Unavailable` - Instance is standby or degraded (load balancer should not route here)

**Behavior:**
- When HA feature is **disabled**: Always returns `200 OK`
- When HA feature is **enabled**:
  - Primary instance (healthy): `200 OK`
  - Standby instance: `503 Service Unavailable`
  - Primary instance (degraded): `503 Service Unavailable`

**Use Cases:**
- Cloud load balancer health checks (GCP, AWS, Azure, Linode)
- VRRP/Keepalived health scripts
- HAProxy health checks
- Kubernetes liveness/readiness probes

**Example:**
```bash
# Load balancer health check
curl -f http://10.0.1.10:8080/ha/health || echo "Instance is standby or unhealthy"

# Keepalived health check script
#!/bin/bash
curl -sf http://localhost:8080/ha/health > /dev/null
exit $?
```

---

## Configuration Examples

### Cloud Load Balancer (GCP)

```yaml
healthCheck:
  checkIntervalSec: 5
  timeoutSec: 3
  healthyThreshold: 2
  unhealthyThreshold: 2
  port: 8080
  requestPath: /ha/health
  portSpecification: USE_FIXED_PORT
```

### AWS Application Load Balancer

```json
{
  "HealthCheckProtocol": "HTTP",
  "HealthCheckPath": "/ha/health",
  "HealthCheckIntervalSeconds": 10,
  "HealthCheckTimeoutSeconds": 5,
  "HealthyThresholdCount": 2,
  "UnhealthyThresholdCount": 2
}
```

### Keepalived (On-Premises VRRP)

```bash
# /etc/keepalived/keepalived.conf
vrrp_script check_forge {
    script "/usr/local/bin/check_forge_health.sh"
    interval 5
    weight -30
    fall 3
    rise 2
}

vrrp_instance FORGE_HA {
    state BACKUP
    interface eth0
    virtual_router_id 51
    priority 100
    advert_int 1

    authentication {
        auth_type PASS
        auth_pass SECRET_PASSWORD
    }

    virtual_ipaddress {
        10.0.1.100/24
    }

    track_script {
        check_forge
    }
}
```

```bash
# /usr/local/bin/check_forge_health.sh
#!/bin/bash
curl -sf http://localhost:8080/ha/health > /dev/null
exit $?
```

### HAProxy

```conf
backend forge_media
    balance roundrobin
    option httpchk GET /ha/health
    http-check expect status 200

    server forge-01 10.0.1.10:8080 check inter 5s fall 3 rise 2
    server forge-02 10.0.1.11:8080 check inter 5s fall 3 rise 2 backup
```

### Kubernetes

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: forge-media
spec:
  containers:
  - name: forge-media
    image: forge-media:latest
    livenessProbe:
      httpGet:
        path: /health
        port: 8080
      initialDelaySeconds: 10
      periodSeconds: 10
      timeoutSeconds: 3
      failureThreshold: 3
    readinessProbe:
      httpGet:
        path: /ha/health
        port: 8080
      initialDelaySeconds: 5
      periodSeconds: 5
      timeoutSeconds: 2
      successThreshold: 2
      failureThreshold: 2
```

---

## Monitoring and Alerting

### Prometheus Metrics

Health status is also exposed via Prometheus metrics at `/metrics/prometheus`:

```prometheus
# HELP forge_ha_current_role Current HA role (0=Unknown, 1=Primary, 2=Standby)
# TYPE forge_ha_current_role gauge
forge_ha_current_role 1

# HELP forge_ha_current_health_state Current health state (0=Unknown, 1=Healthy, 2=Degraded, 3=Failed)
# TYPE forge_ha_current_health_state gauge
forge_ha_current_health_state 1
```

### Sample Prometheus Alert Rules

```yaml
groups:
- name: forge_media_health
  interval: 30s
  rules:

  # Alert when instance is unhealthy
  - alert: ForgeInstanceUnhealthy
    expr: forge_ha_current_health_state > 1
    for: 1m
    labels:
      severity: critical
    annotations:
      summary: "Forge Media instance {{ $labels.instance }} is unhealthy"
      description: "Health state: {{ $value }} (2=Degraded, 3=Failed)"

  # Alert when no primary instance is detected
  - alert: ForgeNoPrimary
    expr: count(forge_ha_current_role == 1) == 0
    for: 30s
    labels:
      severity: critical
    annotations:
      summary: "No primary Forge Media instance detected"
      description: "All instances are standby or unhealthy"

  # Alert when multiple primaries are detected (split-brain)
  - alert: ForgeSplitBrain
    expr: count(forge_ha_current_role == 1) > 1
    for: 10s
    labels:
      severity: critical
    annotations:
      summary: "Multiple primary Forge Media instances detected"
      description: "Split-brain scenario: {{ $value }} primaries active"
```

---

## Troubleshooting

### Health Check Always Returns 503

**Possible Causes:**
1. Instance is in standby mode (expected behavior in HA)
2. HA manager failed to initialize
3. Redis connection failed
4. Instance lost primary election

**Debugging:**
```bash
# Check HA status
curl http://localhost:8080/ha/status | jq

# Check logs for HA initialization
journalctl -u forge-media | grep -i "ha\|primary\|standby"

# Check Redis connectivity
redis-cli -h <redis-host> PING

# Check primary election lock
redis-cli -h <redis-host> GET forge:ha:election:primary
```

### Load Balancer Not Routing to Primary

**Check:**
1. Verify health check path is `/ha/health` (not `/health`)
2. Check load balancer timeout settings (should be < 5 seconds)
3. Verify network connectivity from load balancer to instance
4. Check if primary instance is actually healthy: `curl http://<primary-ip>:8080/ha/health`

### Keepalived Not Failing Over

**Check:**
1. Verify health check script has execute permissions: `chmod +x /usr/local/bin/check_forge_health.sh`
2. Test script manually: `/usr/local/bin/check_forge_health.sh; echo $?`
3. Check Keepalived logs: `journalctl -u keepalived`
4. Verify VRRP priority configuration (primary should have higher priority)

---

## Best Practices

1. **Use `/ha/health` for load balancer health checks** - It's specifically designed for routing decisions
2. **Use `/health` for general monitoring** - Better for dashboards and service mesh integration
3. **Set appropriate timeouts** - Health checks should complete within 3-5 seconds
4. **Configure retries** - Use at least 2-3 consecutive failures before marking unhealthy
5. **Monitor both endpoints** - Alert when `/health` fails but load balancer health is OK
6. **Test failover regularly** - Verify health checks trigger proper failover behavior
7. **Check metrics** - Use `/metrics/prometheus` for detailed health insights

---

## Related Documentation

- [HA Implementation Guide](./HA_IMPLEMENTATION_PLAN.md)
- [Prometheus Metrics](./METRICS.md)
- [Security Guide](./SECURITY.md)
- [Deployment Guide](./deployment/)
