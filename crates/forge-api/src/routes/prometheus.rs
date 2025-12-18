//! Prometheus metrics endpoint

use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use std::sync::{Arc, OnceLock};

use super::sessions::AppState;

/// Prometheus metrics handle (shared across requests)
static PROM_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

pub struct MetricsHandle {
    handle: PrometheusHandle,
}

impl MetricsHandle {
    /// Initialize Prometheus metrics exporter
    pub fn init() -> Self {
        let handle = PROM_HANDLE.get_or_init(|| {
            PrometheusBuilder::new()
                // Configure histogram buckets for latency metrics
                .set_buckets_for_metric(
                    Matcher::Suffix("duration_seconds".to_string()),
                    &[
                        0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
                    ],
                )
                .unwrap()
                // Configure histogram buckets for packet counts
                .set_buckets_for_metric(
                    Matcher::Suffix("packets".to_string()),
                    &[
                        1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
                        10000.0,
                    ],
                )
                .unwrap()
                .install_recorder()
                .expect("Failed to install Prometheus recorder")
        });

        Self {
            handle: handle.clone(),
        }
    }

    /// Get the Prometheus handle for rendering metrics
    pub fn handle(&self) -> &PrometheusHandle {
        &self.handle
    }
}

/// Create Prometheus routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/metrics/prometheus", get(get_prometheus_metrics))
}

/// GET /metrics
///
/// Returns metrics in Prometheus text format
async fn get_prometheus_metrics(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    // Get metrics from the Prometheus handle
    let metrics = state.metrics_handle.handle().render().into_response();

    Ok(metrics)
}
