# Prometheus Alerting Guide

This guide provides production-ready alert rules for monitoring Forge Media deployments.

## Overview

Forge Media exposes comprehensive metrics at `/metrics/prometheus` covering:
- High Availability (HA) operations
- Event bus subscription tracking
- Session and conference metrics
- System resource usage

## Quick Start

### 1. Configure Prometheus Scraping

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'forge-media'
    scrape_interval: 15s
    scrape_timeout: 10s
    metrics_path: '/metrics/prometheus'
    static_configs:
      - targets:
        - 'forge-primary:8080'
        - 'forge-standby:8080'
    relabel_configs:
      - source_labels: [__address__]
        target_label: instance
```

### 2. Load Alert Rules

```yaml
# prometheus.yml
rule_files:
  - 'alerts/forge_media.yml'
```

### 3. Configure Alertmanager

```yaml
# alertmanager.yml
route:
  group_by: ['alertname', 'cluster']
  group_wait: 10s
  group_interval: 10s
  repeat_interval: 12h
  receiver: 'forge-alerts'

receivers:
  - name: 'forge-alerts'
    slack_configs:
      - api_url: 'YOUR_SLACK_WEBHOOK'
        channel: '#forge-alerts'
        title: 'Forge Media Alert'
        text: '{{ range .Alerts }}{{ .Annotations.description }}{{ end }}'
```

---

## Critical Alert Rules

### High Availability Alerts

```yaml
# alerts/forge_media_ha.yml
groups:
- name: forge_media_ha
  interval: 30s
  rules:

  # CRITICAL: No primary instance available
  - alert: ForgeNoPrimaryInstance
    expr: count(forge_ha_current_role == 1) == 0
    for: 30s
    labels:
      severity: critical
      category: availability
    annotations:
      summary: "No primary Forge Media instance detected"
      description: |
        All Forge Media instances are in standby or unhealthy state.
        No instance is accepting traffic.

        Current instances: {{ $value }}
        Action: Investigate HA cluster state and Redis connectivity.

  # CRITICAL: Multiple primary instances (split-brain)
  - alert: ForgeSplitBrainDetected
    expr: count(forge_ha_current_role == 1) > 1
    for: 10s
    labels:
      severity: critical
      category: integrity
    annotations:
      summary: "Split-brain detected: Multiple primary instances"
      description: |
        {{ $value }} Forge Media instances believe they are primary.
        This indicates a split-brain scenario that can cause data inconsistency.

        Action: IMMEDIATE - Shut down all but one instance and investigate Redis connectivity.

  # CRITICAL: Primary instance unhealthy
  - alert: ForgePrimaryInstanceUnhealthy
    expr: forge_ha_current_role == 1 and forge_ha_current_health_state > 1
    for: 1m
    labels:
      severity: critical
      category: health
    annotations:
      summary: "Primary Forge instance {{ $labels.instance }} is unhealthy"
      description: |
        The primary instance is reporting degraded or failed health state.
        Health state: {{ $value }} (1=Healthy, 2=Degraded, 3=Failed)

        Action: Check application logs and system resources.

  # WARNING: Failover detected
  - alert: ForgeFailoverDetected
    expr: increase(forge_ha_failovers_total[5m]) > 0
    labels:
      severity: warning
      category: availability
    annotations:
      summary: "Forge Media failover occurred"
      description: |
        A failover was detected in the last 5 minutes.
        Total failovers: {{ $value }}

        Action: Review logs to determine cause of primary failure.

  # CRITICAL: Failover failed
  - alert: ForgeFailoverFailed
    expr: increase(forge_ha_failover_failures_total[5m]) > 0
    labels:
      severity: critical
      category: availability
    annotations:
      summary: "Forge Media failover failed"
      description: |
        Failover process failed {{ $value }} times in the last 5 minutes.
        This may indicate Redis issues or configuration problems.

        Action: URGENT - Check Redis connectivity and HA configuration.

  # WARNING: Lock renewal failures
  - alert: ForgeLockRenewalFailures
    expr: rate(forge_ha_lock_renewal_failures_total[5m]) > 0.1
    for: 2m
    labels:
      severity: warning
      category: stability
    annotations:
      summary: "Primary lock renewal failures on {{ $labels.instance }}"
      description: |
        The primary instance is failing to renew its election lock.
        Failure rate: {{ $value | humanize }} per second

        This may lead to involuntary step-down and failover.
        Action: Check Redis latency and network connectivity.

  # WARNING: Slow failover
  - alert: ForgeSlowFailover
    expr: histogram_quantile(0.95, forge_ha_failover_duration_seconds_bucket) > 40
    for: 5m
    labels:
      severity: warning
      category: performance
    annotations:
      summary: "Slow failover detected (P95 > 40s)"
      description: |
        Failover operations are taking longer than expected.
        P95 duration: {{ $value | humanize }}s (target: <30-40s)

        Action: Investigate Redis performance and state recovery overhead.
```

### Event Bus and Resource Alerts

```yaml
# alerts/forge_media_resources.yml
groups:
- name: forge_media_resources
  interval: 30s
  rules:

  # CRITICAL: Event bus channel leak detected
  - alert: ForgeEventBusChannelLeak
    expr: forge_event_bus_active_rooms > 100
    for: 10m
    labels:
      severity: critical
      category: memory_leak
    annotations:
      summary: "Potential event bus channel leak detected"
      description: |
        Abnormally high number of active event bus channels: {{ $value }}

        This may indicate WebSocket clients are not properly cleaning up subscriptions.
        Expected: <50 for typical deployments

        Action: Check /v1/events/metrics endpoint for details and review WebSocket disconnect handling.

  # WARNING: High subscriber count per room
  - alert: ForgeHighRoomSubscriberCount
    expr: max(forge_event_bus_room_subscribers) > 50
    for: 5m
    labels:
      severity: warning
      category: scalability
    annotations:
      summary: "High subscriber count in event bus room"
      description: |
        A conference room has {{ $value }} WebSocket subscribers.
        This may impact broadcast performance.

        Action: Consider implementing subscriber limits or message batching.

  # WARNING: Session count approaching limit
  - alert: ForgeHighSessionCount
    expr: forge_media_active_sessions > 4000
    for: 5m
    labels:
      severity: warning
      category: capacity
    annotations:
      summary: "High active session count: {{ $value }}"
      description: |
        Active sessions: {{ $value }} (port pool supports up to 5000)

        Approaching port pool capacity for this instance.
        Action: Consider scaling horizontally or increasing port range.

  # CRITICAL: Port pool exhaustion
  - alert: ForgePortPoolExhaustion
    expr: forge_media_available_ports < 100
    for: 1m
    labels:
      severity: critical
      category: capacity
    annotations:
      summary: "Port pool nearly exhausted"
      description: |
        Only {{ $value }} ports remaining in the pool.
        New session creation will start failing.

        Action: URGENT - Terminate idle sessions or increase port range.
```

### AI Integration Alerts

```yaml
# alerts/forge_media_ai.yml
groups:
- name: forge_media_ai
  interval: 30s
  rules:

  # WARNING: High AI session failure rate
  - alert: ForgeAISessionFailures
    expr: rate(forge_ai_session_failures_total[5m]) > 0.1
    for: 3m
    labels:
      severity: warning
      category: ai_integration
    annotations:
      summary: "High AI session failure rate"
      description: |
        AI sessions are failing at {{ $value | humanize }} per second.

        Possible causes:
        - OpenAI API issues
        - Network connectivity problems
        - Invalid API keys
        - Rate limiting

        Action: Check AI integration logs and OpenAI status.

  # WARNING: AI session timeout
  - alert: ForgeAISessionTimeout
    expr: histogram_quantile(0.95, forge_ai_session_duration_seconds_bucket) > 300
    for: 5m
    labels:
      severity: warning
      category: ai_performance
    annotations:
      summary: "AI sessions taking too long (P95 > 5 minutes)"
      description: |
        AI session P95 duration: {{ $value | humanize }}s

        Action: Investigate AI API latency and session complexity.
```

### Redis and Infrastructure Alerts

```yaml
# alerts/forge_media_redis.yml
groups:
- name: forge_media_redis
  interval: 30s
  rules:

  # CRITICAL: Sentinel discovery failures
  - alert: ForgeSentinelDiscoveryFailures
    expr: rate(forge_ha_sentinel_query_failures_total[5m]) > 0.5
    for: 2m
    labels:
      severity: critical
      category: infrastructure
    annotations:
      summary: "Redis Sentinel master discovery failing"
      description: |
        Sentinel queries failing at {{ $value | humanize }} per second.

        This will prevent failover and may cause service disruption.
        Action: Check Redis Sentinel cluster health and network connectivity.

  # WARNING: Slow Sentinel queries
  - alert: ForgeSentinelSlowQueries
    expr: histogram_quantile(0.95, forge_ha_sentinel_query_duration_seconds_bucket) > 1.0
    for: 5m
    labels:
      severity: warning
      category: infrastructure
    annotations:
      summary: "Slow Redis Sentinel queries (P95 > 1s)"
      description: |
        Sentinel query P95 latency: {{ $value | humanize }}s

        Action: Check Redis Sentinel performance and network latency.

  # WARNING: VIP activation failures
  - alert: ForgeVIPActivationFailures
    expr: increase(forge_ha_vip_activation_failures_total[10m]) > 0
    labels:
      severity: warning
      category: networking
    annotations:
      summary: "VIP activation failures detected"
      description: |
        {{ $value }} VIP activation attempts failed in the last 10 minutes.

        This may prevent proper failover.
        Action: Check Keepalived logs (on-prem) or cloud LB configuration.

  # WARNING: Slow VIP activation
  - alert: ForgeSlowVIPActivation
    expr: histogram_quantile(0.95, forge_ha_vip_activation_duration_seconds_bucket) > 5.0
    for: 5m
    labels:
      severity: warning
      category: networking
    annotations:
      summary: "Slow VIP activation (P95 > 5s)"
      description: |
        VIP activation P95 latency: {{ $value | humanize }}s

        This adds to failover time.
        Action: Investigate VIP manager performance (VRRP convergence or cloud API latency).
```

---

## Dashboard Queries

### HA Status Dashboard

```promql
# Current HA role (0=Unknown, 1=Primary, 2=Standby)
forge_ha_current_role

# Health state by instance
forge_ha_current_health_state

# Failover count over time
increase(forge_ha_failovers_total[1h])

# Average failover duration
rate(forge_ha_failover_duration_seconds_sum[5m]) / rate(forge_ha_failover_duration_seconds_count[5m])

# Sessions recovered during failover
rate(forge_ha_sessions_recovered_total[5m])

# Lock renewal success rate
1 - (rate(forge_ha_lock_renewal_failures_total[5m]) / rate(forge_ha_lock_renewals_total[5m]))
```

### Event Bus Dashboard

```promql
# Active room channels
forge_event_bus_active_rooms

# Global subscribers
forge_event_bus_global_subscribers

# Subscribers per room (top 10)
topk(10, forge_event_bus_room_subscribers)

# Channel creation rate
rate(forge_event_bus_room_created_total[5m])

# Channel pruning rate (cleanup operations)
rate(forge_event_bus_room_pruned_total[5m])
```

### Session and Conference Dashboard

```promql
# Active sessions by instance
forge_media_active_sessions

# Session creation rate
rate(forge_media_sessions_created_total[5m])

# Session termination rate
rate(forge_media_sessions_terminated_total[5m])

# Active conferences
forge_media_active_conferences

# Conference participants
sum(forge_media_conference_participants)

# Average session duration
rate(forge_media_session_duration_seconds_sum[5m]) / rate(forge_media_session_duration_seconds_count[5m])
```

---

## Testing Alert Rules

### Validate Syntax

```bash
promtool check rules alerts/forge_media.yml
```

### Test Alert Query

```bash
# Check if any alerts would fire
curl -G 'http://prometheus:9090/api/v1/query' \
  --data-urlencode 'query=ALERTS{alertname="ForgeNoPrimaryInstance"}'
```

### Simulate Conditions

```bash
# Force instance to standby (triggers ForgeNoPrimaryInstance after 30s)
curl -X POST http://forge-primary:8080/ha/transfer-primary

# Create many event bus subscriptions (triggers ForgeEventBusChannelLeak)
for i in {1..150}; do
  wscat -c ws://forge:8080/ws/events &
done
```

---

## Alert Severity Guidelines

| Severity | Description | Response Time | Examples |
|----------|-------------|---------------|----------|
| **critical** | Service unavailable or data loss risk | < 5 minutes | No primary, split-brain, port exhaustion |
| **warning** | Degraded performance or potential issues | < 30 minutes | High latency, approaching limits |
| **info** | Informational events | None required | Scheduled failover, maintenance |

---

## Related Documentation

- [Health Check Endpoints](./HEALTH_ENDPOINTS.md)
- [HA Implementation Plan](./HA_IMPLEMENTATION_PLAN.md)
- [Deployment Guide](./deployment/)
