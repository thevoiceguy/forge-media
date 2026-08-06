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
            let handle = PrometheusBuilder::new()
                // Per-metric buckets. Full matchers win over the generic
                // suffix matchers below regardless of insertion order.
                .set_buckets_for_metric(
                    Matcher::Full(forge_engine::metrics::M_VAD_NEURAL_INFERENCE.to_string()),
                    &forge_engine::metrics::VAD_NEURAL_INFERENCE_SECONDS_BUCKETS,
                )
                .unwrap()
                .set_buckets_for_metric(
                    Matcher::Full(forge_engine::metrics::M_TRANSCODING_DURATION.to_string()),
                    &forge_engine::metrics::TRANSCODING_DURATION_SECONDS_BUCKETS,
                )
                .unwrap()
                .set_buckets_for_metric(
                    Matcher::Full(forge_conference::metrics::M_MIXING_DURATION.to_string()),
                    &forge_conference::metrics::MIXING_DURATION_SECONDS_BUCKETS,
                )
                .unwrap()
                .set_buckets_for_metric(
                    Matcher::Full(crate::metrics::M_WEBRTC_ESTABLISHMENT.to_string()),
                    &crate::metrics::WEBRTC_ESTABLISHMENT_SECONDS_BUCKETS,
                )
                .unwrap()
                .set_buckets_for_metric(
                    Matcher::Full(crate::metrics::M_SDP_NEGOTIATION_DURATION.to_string()),
                    &crate::metrics::SDP_NEGOTIATION_SECONDS_BUCKETS,
                )
                .unwrap()
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
                .expect("Failed to install Prometheus recorder");
            // Descriptions only reach a recorder that is already
            // installed, so this must come after install_recorder().
            crate::metrics::describe_metrics();
            handle
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
