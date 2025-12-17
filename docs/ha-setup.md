# High Availability Setup Guide

This guide explains how to deploy Forge Media Engine in a High Availability (HA) configuration for production environments.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Cloud Deployment](#cloud-deployment)
  - [Google Cloud Platform (GCP)](#google-cloud-platform-gcp)
  - [Amazon Web Services (AWS)](#amazon-web-services-aws)
  - [Azure](#azure)
  - [Linode](#linode)
- [On-Premises Deployment](#on-premises-deployment)
- [Configuration](#configuration)
- [Testing Failover](#testing-failover)
- [Monitoring](#monitoring)
- [Troubleshooting](#troubleshooting)

## Overview

Forge Media Engine supports Active-Passive High Availability with:

- **Automatic Failover**: Standby instance detects primary failure and takes over within 25-40 seconds
- **State Replication**: Redis-based session and conference state synchronization
- **Minimal Packet Loss**: Brief RTP disruption (20-100ms) during failover
- **Pre-allocated Ports**: Each instance uses a dedicated port range to prevent conflicts
- **Multiple Deployment Modes**: Cloud load balancers or on-premises VRRP/Keepalived

**Key Metrics:**
- **Detection Time**: 25-30 seconds (3 missed heartbeats)
- **Failover Time**: 30-40 seconds total
- **RTP Packet Loss**: 20-100ms during socket rebind
- **State Persistence**: Sessions and conferences preserved during failover

## Architecture

```
┌─────────────────────────────────────────────────────┐
│        Load Balancer / VRRP VIP (10.0.1.100)       │
│         Routes to Primary (health checks)           │
└────────────────┬────────────────────────────────────┘
                 │
        ┌────────┴─────────┐
        │                  │
┌───────▼──────────┐  ┌────▼──────────────┐
│  Primary         │  │  Standby          │
│  10.0.1.10:8080  │  │  10.0.1.11:8080  │
│  Ports: 30-35K   │  │  Ports: 35-40K    │
│  Health: 200     │  │  Health: 503      │
└────────┬─────────┘  └─────────┬─────────┘
         │                      │
         │   ┌──────────────────┤
         │   │                  │
         └───▼──────────────────▼────┐
             │   Redis Cluster        │
             │   10.0.1.20:6379      │
             │   - Session state      │
             │   - Heartbeats         │
             │   - Election locks     │
             └────────────────────────┘
```

## Prerequisites

### Software Requirements

- **Forge Media Engine**: v0.2.0 or later (compiled with `--features ha`)
- **Redis**: v6.0 or later (for state synchronization)
- **curl**: For health checks
- **Keepalived**: v2.0+ (for on-premises deployments only)

### Hardware Requirements (Per Instance)

- **CPU**: 4+ cores
- **RAM**: 8GB+ (16GB recommended for high-capacity deployments)
- **Network**: 1Gbps+ (10Gbps for high-density RTP forwarding)
- **Disk**: 50GB+ for logs and recordings

### Network Requirements

- **Primary ↔ Standby**: Low-latency connection (<5ms RTT)
- **Instances ↔ Redis**: Low-latency connection (<2ms RTT)
- **UDP Ports**: 30000-40000 (RTP/RTCP)
- **TCP Ports**: 8080 (HTTP API), 6379 (Redis)

## Cloud Deployment

Cloud deployments use native load balancer health checks for automatic traffic routing.

### Google Cloud Platform (GCP)

#### 1. Create Redis Instance

```bash
gcloud redis instances create forge-ha-redis \
    --size=5 \
    --region=us-central1 \
    --tier=standard-ha \
    --redis-version=redis_6_x
```

#### 2. Create Instance Template

```bash
gcloud compute instance-templates create forge-media-template \
    --machine-type=n2-standard-4 \
    --image-family=ubuntu-2204-lts \
    --image-project=ubuntu-os-cloud \
    --boot-disk-size=50GB \
    --metadata-from-file=startup-script=startup.sh \
    --tags=forge-media,http-server
```

#### 3. Create Instance Group

```bash
gcloud compute instance-groups managed create forge-media-group \
    --base-instance-name=forge-media \
    --size=2 \
    --template=forge-media-template \
    --zone=us-central1-a
```

#### 4. Create Health Check

```bash
gcloud compute health-checks create http forge-media-health \
    --port=8080 \
    --request-path=/health \
    --check-interval=10s \
    --timeout=5s \
    --unhealthy-threshold=3 \
    --healthy-threshold=2
```

#### 5. Create Load Balancer

```bash
# Backend service
gcloud compute backend-services create forge-media-backend \
    --protocol=TCP \
    --health-checks=forge-media-health \
    --global

# Add instance group
gcloud compute backend-services add-backend forge-media-backend \
    --instance-group=forge-media-group \
    --instance-group-zone=us-central1-a \
    --global

# Forwarding rule
gcloud compute forwarding-rules create forge-media-lb \
    --load-balancing-scheme=EXTERNAL \
    --ports=8080 \
    --backend-service=forge-media-backend \
    --global
```

#### 6. Configuration

`/etc/forge/config.toml`:
```toml
[engine.ha]
enabled = true
role = "auto"
deployment_mode = "cloud"
port_range = { start = 30000, end = 34999 }

[engine.ha.redis]
url = "redis://10.0.0.3:6379/0"
key_prefix = "forge:ha:"
heartbeat_interval_secs = 10
failover_timeout_secs = 25

[engine.ha.cloud]
provider = "gcp"
health_check_path = "/health"
standby_returns_503 = true
```

### Amazon Web Services (AWS)

#### 1. Create Redis Cluster (ElastiCache)

```bash
aws elasticache create-replication-group \
    --replication-group-id forge-ha-redis \
    --replication-group-description "Forge HA Redis" \
    --engine redis \
    --cache-node-type cache.m5.large \
    --num-cache-clusters 2 \
    --automatic-failover-enabled
```

#### 2. Create Launch Template

```bash
aws ec2 create-launch-template \
    --launch-template-name forge-media-template \
    --launch-template-data '{
        "ImageId": "ami-0c55b159cbfafe1f0",
        "InstanceType": "c5.xlarge",
        "UserData": "..."
    }'
```

#### 3. Create Auto Scaling Group

```bash
aws autoscaling create-auto-scaling-group \
    --auto-scaling-group-name forge-media-asg \
    --launch-template LaunchTemplateName=forge-media-template \
    --min-size 2 \
    --max-size 2 \
    --desired-capacity 2 \
    --vpc-zone-identifier "subnet-12345678,subnet-87654321"
```

#### 4. Create Application Load Balancer

```bash
# Target group
aws elbv2 create-target-group \
    --name forge-media-targets \
    --protocol TCP \
    --port 8080 \
    --vpc-id vpc-12345678 \
    --health-check-protocol HTTP \
    --health-check-path /health

# Load balancer
aws elbv2 create-load-balancer \
    --name forge-media-lb \
    --type network \
    --subnets subnet-12345678 subnet-87654321

# Listener
aws elbv2 create-listener \
    --load-balancer-arn arn:aws:elasticloadbalancing:... \
    --protocol TCP \
    --port 8080 \
    --default-actions Type=forward,TargetGroupArn=arn:aws:elasticloadbalancing:...
```

#### 5. Configuration

`/etc/forge/config.toml`:
```toml
[engine.ha]
enabled = true
role = "auto"
deployment_mode = "cloud"
port_range = { start = 30000, end = 34999 }

[engine.ha.redis]
url = "redis://forge-ha-redis.abc123.0001.use1.cache.amazonaws.com:6379/0"

[engine.ha.cloud]
provider = "aws"
standby_returns_503 = true
```

### Azure

Similar setup using Azure Load Balancer and Azure Cache for Redis.

### Linode

Similar setup using Linode NodeBalancer and managed Redis.

## On-Premises Deployment

On-premises deployments use VRRP (Keepalived) for VIP management.

### 1. Install Dependencies

```bash
# On both primary and standby
sudo apt-get update
sudo apt-get install -y keepalived curl redis-server

# Enable IP forwarding
echo "net.ipv4.ip_forward = 1" | sudo tee -a /etc/sysctl.conf
sudo sysctl -p
```

### 2. Configure Redis

**On Redis server (10.0.1.20):**

`/etc/redis/redis.conf`:
```conf
bind 10.0.1.20
port 6379
protected-mode yes
requirepass YOUR_REDIS_PASSWORD
maxmemory 4gb
maxmemory-policy allkeys-lru
```

### 3. Configure Forge (Primary)

**On primary (10.0.1.10):**

`/etc/forge/config.toml`:
```toml
[engine]
port_range = { start = 30000, end = 34999 }

[engine.ha]
enabled = true
instance_id = "primary-01"
role = "auto"
deployment_mode = "onprem"
port_range = { start = 30000, end = 34999 }

[engine.ha.redis]
url = "redis://:YOUR_REDIS_PASSWORD@10.0.1.20:6379/0"
heartbeat_interval_secs = 10
failover_timeout_secs = 25

[engine.ha.onprem]
vip = "10.0.1.100"
interface = "eth0"
virtual_router_id = 51
priority = 100
auth_password = "YOUR_VRRP_PASSWORD"
```

### 4. Configure Forge (Standby)

**On standby (10.0.1.11):**

Same as primary, but with:
- `instance_id = "standby-01"`
- `port_range = { start = 35000, end = 39999 }`
- `priority = 50`

### 5. Install Health Check Script

**On both instances:**

```bash
sudo cp tools/check_forge_health.sh /usr/local/bin/
sudo chmod +x /usr/local/bin/check_forge_health.sh

# Test the script
/usr/local/bin/check_forge_health.sh && echo "Health check passed"
```

### 6. Configure Keepalived (Primary)

**On primary (10.0.1.10):**

`/etc/keepalived/keepalived.conf`:
```conf
global_defs {
    router_id FORGE_MEDIA_PRIMARY
    enable_script_security
    script_user forge
}

vrrp_script check_forge {
    script "/usr/local/bin/check_forge_health.sh"
    interval 5
    weight -30
    fall 3
    rise 2
    timeout 3
}

vrrp_instance FORGE_HA {
    state MASTER
    interface eth0
    virtual_router_id 51
    priority 100
    advert_int 1

    authentication {
        auth_type PASS
        auth_pass YOUR_VRRP_PASSWORD
    }

    virtual_ipaddress {
        10.0.1.100/24 dev eth0
    }

    track_script {
        check_forge
    }
}
```

### 7. Configure Keepalived (Standby)

**On standby (10.0.1.11):**

Same as primary, but with:
- `state BACKUP`
- `priority 50`

### 8. Start Services

**On both instances:**

```bash
# Start Forge
sudo systemctl start forge-media
sudo systemctl enable forge-media

# Start Keepalived
sudo systemctl start keepalived
sudo systemctl enable keepalived

# Check status
systemctl status forge-media
systemctl status keepalived
```

### 9. Verify VIP

```bash
# Check which instance holds the VIP
ip addr show eth0 | grep 10.0.1.100

# Test connectivity
curl http://10.0.1.100:8080/health
curl http://10.0.1.100:8080/ha/status
```

## Configuration

### Full Example Configuration

See above cloud and on-premises sections for complete examples.

### Environment Variables

Forge HA can also be configured via environment variables:

```bash
FORGE_HA_ENABLED=true
FORGE_HA_REDIS_URL=redis://10.0.1.20:6379/0
FORGE_HA_INSTANCE_ID=primary-01
FORGE_HA_DEPLOYMENT_MODE=cloud
```

## Testing Failover

### Manual Failover Test

**Cloud Deployment:**
```bash
# Trigger graceful failover
curl -X POST http://10.0.1.100:8080/ha/transfer-primary

# Check new primary
curl http://10.0.1.100:8080/ha/status
```

**On-Premises Deployment:**
```bash
# Stop primary Forge instance
sudo systemctl stop forge-media

# Watch VIP migrate (should take ~5s)
watch -n 1 'ip addr show eth0 | grep 10.0.1.100'

# Verify standby became primary
curl http://10.0.1.100:8080/ha/status
```

### Automated Failover Test

```bash
# Simulate primary crash
sudo kill -9 $(pgrep forge-media | head -1)

# Monitor failover (should complete in 30-40s)
watch -n 1 'curl -s http://10.0.1.100:8080/ha/status | jq .role'
```

### Load Test During Failover

```bash
# Start load test (in separate terminal)
while true; do
    curl -X POST http://10.0.1.100:8080/v1/sessions \
        -H "Content-Type: application/json" \
        -d '{"call_id":"test-'$(date +%s%N)'","participant_a":"A","participant_b":"B"}'
    sleep 0.1
done

# Trigger failover (in another terminal)
sudo systemctl stop forge-media  # On primary

# Observe: Brief disruption, then sessions continue on standby
```

## Monitoring

### Key Metrics to Monitor

- **Heartbeat Status**: Monitor Redis keys `forge:ha:instance:*`
- **Failover Count**: Check `/ha/status` endpoint
- **Session Count**: Verify session preservation across failover
- **RTP Packet Loss**: Monitor during failover events
- **Redis Latency**: Must remain <5ms for reliable failover

### Prometheus Metrics

```
forge_ha_role{instance="primary-01"} 1  # 1=primary, 0=standby
forge_ha_failover_total 2
forge_ha_heartbeat_last_seen_seconds 5
forge_ha_redis_latency_ms 1.2
```

### Log Monitoring

```bash
# Forge logs
journalctl -u forge-media -f | grep -i "ha\|failover"

# Keepalived logs
journalctl -u keepalived -f
```

## Troubleshooting

### Split Brain (Both Instances Primary)

**Symptoms**: Both instances claim to be primary

**Causes**:
- Network partition between instances
- Redis connection issues
- Clock skew

**Resolution**:
```bash
# Check Redis connectivity from both instances
redis-cli -h 10.0.1.20 -a YOUR_PASSWORD PING

# Check time synchronization
timedatectl status

# Force standby role on one instance
curl -X POST http://10.0.1.11:8080/ha/transfer-primary
```

### Failover Not Triggering

**Symptoms**: Primary fails but standby doesn't take over

**Checks**:
1. Health check script working: `/usr/local/bin/check_forge_health.sh`
2. Keepalived running: `systemctl status keepalived`
3. Redis connectivity: `redis-cli -h 10.0.1.20 PING`
4. Heartbeat visible: `redis-cli GET forge:ha:instance:primary`

### High Packet Loss During Failover

**Symptoms**: >100ms RTP disruption during failover

**Causes**:
- High Redis latency
- Network congestion
- Too many sessions to recover

**Mitigation**:
- Reduce Redis latency (<2ms)
- Increase network bandwidth
- Consider session limits

## Best Practices

1. **Always use Redis persistence** (RDB + AOF)
2. **Monitor Redis health** (latency, memory, connections)
3. **Test failover regularly** (monthly or quarterly)
4. **Use dedicated VLANs** for heartbeat traffic
5. **Set up alerting** on failover events
6. **Document failover procedures** for on-call staff
7. **Keep time synchronized** (NTP) across all instances
8. **Monitor port pool exhaustion** (alert at 80% usage)

## Support

For issues or questions:
- GitHub Issues: https://github.com/anthropics/forge-media/issues
- Documentation: https://github.com/anthropics/forge-media/docs
