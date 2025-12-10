//! Metrics endpoint for monitoring

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;

use super::sessions::AppState;

/// System metrics response
#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    /// Total active sessions
    pub active_sessions: usize,
    /// Port pool statistics
    pub port_pool: PortPoolMetrics,
    /// Aggregated session statistics
    pub sessions: SessionMetrics,
    /// System uptime in seconds
    pub uptime_seconds: u64,
}

/// Port pool metrics
#[derive(Debug, Serialize)]
pub struct PortPoolMetrics {
    /// Number of allocated port pairs
    pub allocated: usize,
    /// Number of available port pairs
    pub available: usize,
    /// Total capacity
    pub capacity: usize,
    /// Utilization percentage (0-100)
    pub utilization_percent: f64,
}

/// Aggregated session metrics
#[derive(Debug, Serialize)]
pub struct SessionMetrics {
    /// Total packets received across all sessions
    pub total_packets_received: u64,
    /// Total bytes received across all sessions
    pub total_bytes_received: u64,
    /// Total packets sent across all sessions
    pub total_packets_sent: u64,
    /// Total bytes sent across all sessions
    pub total_bytes_sent: u64,
}

lazy_static::lazy_static! {
    static ref START_TIME: Instant = Instant::now();
}

/// Create metrics routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/v1/metrics", get(get_metrics))
}

/// GET /v1/metrics
///
/// Returns current system metrics including session counts, port pool utilization,
/// and aggregated traffic statistics.
async fn get_metrics(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, StatusCode> {
    // Get active session count
    let active_sessions = state.session_manager.session_count();

    // Get port pool stats
    let (allocated, available) = state.session_manager.port_pool_stats().await;
    let capacity = allocated + available;
    let utilization_percent = if capacity > 0 {
        (allocated as f64 / capacity as f64) * 100.0
    } else {
        0.0
    };

    let port_pool = PortPoolMetrics {
        allocated,
        available,
        capacity,
        utilization_percent,
    };

    // Aggregate session statistics
    let sessions_list = state.session_manager.list_sessions();
    let mut total_packets_received = 0u64;
    let mut total_bytes_received = 0u64;
    let mut total_packets_sent = 0u64;
    let mut total_bytes_sent = 0u64;

    for session in sessions_list {
        let stats_a = session.participant_a_stats().await;
        let stats_b = session.participant_b_stats().await;

        total_packets_received += stats_a.packets_received + stats_b.packets_received;
        total_bytes_received += stats_a.bytes_received + stats_b.bytes_received;
        total_packets_sent += stats_a.packets_sent + stats_b.packets_sent;
        total_bytes_sent += stats_a.bytes_sent + stats_b.bytes_sent;
    }

    let sessions = SessionMetrics {
        total_packets_received,
        total_bytes_received,
        total_packets_sent,
        total_bytes_sent,
    };

    // Calculate uptime
    let uptime_seconds = START_TIME.elapsed().as_secs();

    let metrics = MetricsResponse {
        active_sessions,
        port_pool,
        sessions,
        uptime_seconds,
    };

    Ok(Json(metrics))
}
