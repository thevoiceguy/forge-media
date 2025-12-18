//! Prometheus metrics for HA operations
//!
//! This module provides observability into HA operations including:
//! - Redis Sentinel queries (success/failure)
//! - Primary lock renewals (success/failure)
//! - VIP activations and deactivations
//! - Failover operations
//!
//! Enable with the `metrics` feature flag.

#[cfg(feature = "metrics")]
use lazy_static::lazy_static;
#[cfg(feature = "metrics")]
use prometheus::{Counter, Histogram, IntCounter, IntGauge, Registry};
#[cfg(feature = "metrics")]
use std::sync::Arc;

#[cfg(feature = "metrics")]
lazy_static! {
    /// Global metrics registry
    pub static ref REGISTRY: Registry = Registry::new();

    // Redis Sentinel Metrics
    /// Total Sentinel queries attempted
    pub static ref SENTINEL_QUERIES_TOTAL: IntCounter = IntCounter::new(
        "forge_ha_sentinel_queries_total",
        "Total number of Redis Sentinel master discovery queries"
    ).expect("Failed to create sentinel_queries_total metric");

    /// Failed Sentinel queries
    pub static ref SENTINEL_QUERY_FAILURES: IntCounter = IntCounter::new(
        "forge_ha_sentinel_query_failures_total",
        "Total number of failed Redis Sentinel queries"
    ).expect("Failed to create sentinel_query_failures metric");

    /// Sentinel query duration histogram
    pub static ref SENTINEL_QUERY_DURATION: Histogram = Histogram::with_opts(
        prometheus::HistogramOpts::new(
            "forge_ha_sentinel_query_duration_seconds",
            "Time taken to discover master via Sentinel"
        ).buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0])
    ).expect("Failed to create sentinel_query_duration metric");

    // Primary Lock Metrics
    /// Total lock renewal attempts
    pub static ref LOCK_RENEWALS_TOTAL: IntCounter = IntCounter::new(
        "forge_ha_lock_renewals_total",
        "Total number of primary lock renewal attempts"
    ).expect("Failed to create lock_renewals_total metric");

    /// Failed lock renewals (lost primary role)
    pub static ref LOCK_RENEWAL_FAILURES: IntCounter = IntCounter::new(
        "forge_ha_lock_renewal_failures_total",
        "Total number of failed primary lock renewals (lost primary role)"
    ).expect("Failed to create lock_renewal_failures metric");

    /// Lock renewal duration
    pub static ref LOCK_RENEWAL_DURATION: Histogram = Histogram::with_opts(
        prometheus::HistogramOpts::new(
            "forge_ha_lock_renewal_duration_seconds",
            "Time taken to renew primary lock"
        ).buckets(vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1])
    ).expect("Failed to create lock_renewal_duration metric");

    // VIP Metrics
    /// VIP activation attempts
    pub static ref VIP_ACTIVATIONS_TOTAL: IntCounter = IntCounter::new(
        "forge_ha_vip_activations_total",
        "Total number of VIP activation attempts"
    ).expect("Failed to create vip_activations_total metric");

    /// VIP activation failures
    pub static ref VIP_ACTIVATION_FAILURES: IntCounter = IntCounter::new(
        "forge_ha_vip_activation_failures_total",
        "Total number of failed VIP activations"
    ).expect("Failed to create vip_activation_failures metric");

    /// VIP activation latency
    pub static ref VIP_ACTIVATION_DURATION: Histogram = Histogram::with_opts(
        prometheus::HistogramOpts::new(
            "forge_ha_vip_activation_duration_seconds",
            "Time taken to activate VIP during promotion"
        ).buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0])
    ).expect("Failed to create vip_activation_duration metric");

    /// VIP deactivation attempts
    pub static ref VIP_DEACTIVATIONS_TOTAL: IntCounter = IntCounter::new(
        "forge_ha_vip_deactivations_total",
        "Total number of VIP deactivation attempts"
    ).expect("Failed to create vip_deactivations_total metric");

    // Failover Metrics
    /// Total failover operations
    pub static ref FAILOVERS_TOTAL: IntCounter = IntCounter::new(
        "forge_ha_failovers_total",
        "Total number of failover operations initiated"
    ).expect("Failed to create failovers_total metric");

    /// Failed failover operations
    pub static ref FAILOVER_FAILURES: IntCounter = IntCounter::new(
        "forge_ha_failover_failures_total",
        "Total number of failed failover operations"
    ).expect("Failed to create failover_failures metric");

    /// Failover duration (detection + promotion + recovery)
    pub static ref FAILOVER_DURATION: Histogram = Histogram::with_opts(
        prometheus::HistogramOpts::new(
            "forge_ha_failover_duration_seconds",
            "Total time taken to complete failover"
        ).buckets(vec![1.0, 5.0, 10.0, 20.0, 30.0, 40.0, 60.0, 120.0])
    ).expect("Failed to create failover_duration metric");

    /// Sessions recovered during failover
    pub static ref SESSIONS_RECOVERED: IntCounter = IntCounter::new(
        "forge_ha_sessions_recovered_total",
        "Total number of sessions successfully recovered during failover"
    ).expect("Failed to create sessions_recovered metric");

    /// Conferences recovered during failover
    pub static ref CONFERENCES_RECOVERED: IntCounter = IntCounter::new(
        "forge_ha_conferences_recovered_total",
        "Total number of conferences successfully recovered during failover"
    ).expect("Failed to create conferences_recovered metric");

    // Heartbeat Metrics
    /// Heartbeat publish count
    pub static ref HEARTBEATS_PUBLISHED: IntCounter = IntCounter::new(
        "forge_ha_heartbeats_published_total",
        "Total number of heartbeats published"
    ).expect("Failed to create heartbeats_published metric");

    /// Heartbeat publish failures
    pub static ref HEARTBEAT_FAILURES: IntCounter = IntCounter::new(
        "forge_ha_heartbeat_failures_total",
        "Total number of failed heartbeat publishes"
    ).expect("Failed to create heartbeat_failures metric");

    /// Primary failure detection time
    pub static ref PRIMARY_FAILURE_DETECTION_DURATION: Histogram = Histogram::with_opts(
        prometheus::HistogramOpts::new(
            "forge_ha_primary_failure_detection_seconds",
            "Time taken to detect primary failure"
        ).buckets(vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 90.0, 120.0])
    ).expect("Failed to create primary_failure_detection_duration metric");

    // Role Metrics
    /// Current HA role (0=Unknown, 1=Primary, 2=Standby)
    pub static ref CURRENT_ROLE: IntGauge = IntGauge::new(
        "forge_ha_current_role",
        "Current HA role (0=Unknown, 1=Primary, 2=Standby)"
    ).expect("Failed to create current_role metric");

    /// Current health state (0=Unknown, 1=Healthy, 2=Degraded, 3=Failed)
    pub static ref CURRENT_HEALTH_STATE: IntGauge = IntGauge::new(
        "forge_ha_current_health_state",
        "Current health state (0=Unknown, 1=Healthy, 2=Degraded, 3=Failed)"
    ).expect("Failed to create current_health_state metric");
}

#[cfg(feature = "metrics")]
/// Register all metrics with the global registry
pub fn register_metrics() -> Result<(), prometheus::Error> {
    REGISTRY.register(Box::new(SENTINEL_QUERIES_TOTAL.clone()))?;
    REGISTRY.register(Box::new(SENTINEL_QUERY_FAILURES.clone()))?;
    REGISTRY.register(Box::new(SENTINEL_QUERY_DURATION.clone()))?;

    REGISTRY.register(Box::new(LOCK_RENEWALS_TOTAL.clone()))?;
    REGISTRY.register(Box::new(LOCK_RENEWAL_FAILURES.clone()))?;
    REGISTRY.register(Box::new(LOCK_RENEWAL_DURATION.clone()))?;

    REGISTRY.register(Box::new(VIP_ACTIVATIONS_TOTAL.clone()))?;
    REGISTRY.register(Box::new(VIP_ACTIVATION_FAILURES.clone()))?;
    REGISTRY.register(Box::new(VIP_ACTIVATION_DURATION.clone()))?;
    REGISTRY.register(Box::new(VIP_DEACTIVATIONS_TOTAL.clone()))?;

    REGISTRY.register(Box::new(FAILOVERS_TOTAL.clone()))?;
    REGISTRY.register(Box::new(FAILOVER_FAILURES.clone()))?;
    REGISTRY.register(Box::new(FAILOVER_DURATION.clone()))?;
    REGISTRY.register(Box::new(SESSIONS_RECOVERED.clone()))?;
    REGISTRY.register(Box::new(CONFERENCES_RECOVERED.clone()))?;

    REGISTRY.register(Box::new(HEARTBEATS_PUBLISHED.clone()))?;
    REGISTRY.register(Box::new(HEARTBEAT_FAILURES.clone()))?;
    REGISTRY.register(Box::new(PRIMARY_FAILURE_DETECTION_DURATION.clone()))?;

    REGISTRY.register(Box::new(CURRENT_ROLE.clone()))?;
    REGISTRY.register(Box::new(CURRENT_HEALTH_STATE.clone()))?;

    Ok(())
}

#[cfg(feature = "metrics")]
/// Get metrics text output in Prometheus format
pub fn gather_metrics() -> String {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

// Stub implementations when metrics feature is disabled
#[cfg(not(feature = "metrics"))]
pub fn register_metrics() -> Result<(), ()> {
    Ok(())
}

#[cfg(not(feature = "metrics"))]
pub fn gather_metrics() -> String {
    String::from("# Metrics feature not enabled\n")
}
