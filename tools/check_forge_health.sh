#!/bin/bash
#
# Forge Media Engine Health Check Script for Keepalived
#
# This script checks if the Forge Media Engine is healthy and ready to accept traffic.
# Used by Keepalived to determine which instance should hold the VIP.
#
# Exit codes:
#   0 = Healthy (primary or ready to become primary)
#   1 = Unhealthy (not ready for traffic)
#
# Installation:
#   1. Copy to /usr/local/bin/check_forge_health.sh
#   2. chmod +x /usr/local/bin/check_forge_health.sh
#   3. Update Keepalived config to reference this script
#
# Configuration:
#   Set these environment variables or edit defaults below:
#   - FORGE_HEALTH_URL: Health check endpoint URL
#   - FORGE_TIMEOUT: Timeout in seconds for health check
#   - FORGE_LOG_FILE: Log file path (optional)

set -euo pipefail

# Configuration
FORGE_HEALTH_URL="${FORGE_HEALTH_URL:-http://localhost:8080/health}"
FORGE_TIMEOUT="${FORGE_TIMEOUT:-3}"
FORGE_LOG_FILE="${FORGE_LOG_FILE:-/var/log/forge/keepalived-health.log}"
FORGE_LOG_ENABLED="${FORGE_LOG_ENABLED:-false}"

# Logging function
log() {
    if [ "$FORGE_LOG_ENABLED" = "true" ]; then
        echo "[$(date +'%Y-%m-%d %H:%M:%S')] $1" >> "$FORGE_LOG_FILE"
    fi
}

# Perform health check
check_health() {
    local response_code

    # Use curl to check the health endpoint
    # -f: Fail silently on HTTP errors
    # -s: Silent mode (no progress bar)
    # -o /dev/null: Discard response body
    # -w "%{http_code}": Output only HTTP status code
    # --max-time: Timeout in seconds
    # --connect-timeout: Connection timeout
    response_code=$(curl -f -s -o /dev/null -w "%{http_code}" \
        --max-time "$FORGE_TIMEOUT" \
        --connect-timeout "$FORGE_TIMEOUT" \
        "$FORGE_HEALTH_URL" 2>/dev/null || echo "000")

    # Check response code
    if [ "$response_code" = "200" ]; then
        log "Health check PASSED: HTTP $response_code"
        return 0
    else
        log "Health check FAILED: HTTP $response_code"
        return 1
    fi
}

# Main execution
if check_health; then
    # Healthy - exit 0 (success)
    exit 0
else
    # Unhealthy - exit 1 (failure)
    exit 1
fi
