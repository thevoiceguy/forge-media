//! High Availability (HA) routes for cluster status and management

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use super::sessions::AppState;
use crate::middleware::auth::{RequireAdmin, RequireReadOnly};

/// Create HA routes (all endpoints: status + admin + health).
///
/// Served on the main listener when `admin_bind` is not configured.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ha/status", get(get_ha_status))
        .route("/ha/transfer-primary", post(transfer_primary))
        .route("/ha/health", get(health_probe))
}

/// HA routes that are safe for the public listener: status (read-only)
/// and health (unauthenticated). Admin-only mutation endpoints are
/// served by [`admin_routes`] instead.
pub fn public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ha/status", get(get_ha_status))
        .route("/ha/health", get(health_probe))
}

/// HA admin-only routes. Served on the admin listener when
/// `ApiServerConfig::admin_bind` is set; otherwise rolled into the main
/// router via [`routes`].
pub fn admin_routes() -> Router<Arc<AppState>> {
    Router::new().route("/ha/transfer-primary", post(transfer_primary))
}

/// HA cluster status response
#[derive(Debug, Serialize, Deserialize)]
pub struct HAStatusResponse {
    /// Whether HA is enabled
    pub enabled: bool,
    /// Current instance ID
    pub instance_id: Option<String>,
    /// Current role (Primary, Standby, Unknown)
    pub role: String,
    /// Health state (Healthy, Degraded, Failed)
    pub health_state: String,
    /// Number of active sessions on this instance
    pub session_count: usize,
    /// Number of active conferences on this instance
    pub conference_count: usize,
    /// Failover count
    pub failover_count: Option<u64>,
    /// Last failover timestamp
    pub last_failover: Option<String>,
    /// Redis connection status
    pub redis_connected: Option<bool>,
}

/// GET /ha/status - Get HA cluster status
///
/// Returns detailed information about the HA cluster state, including:
/// - Current role (Primary/Standby)
/// - Health state
/// - Session/conference counts
/// - Failover history
pub async fn get_ha_status(
    _auth: RequireReadOnly,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    #[cfg(feature = "ha")]
    {
        if let Some(ha_manager) = &state.ha_manager {
            let status = ha_manager.get_status().await;

            info!(
                "HA status requested: role={}, health={}",
                status.role, status.health_state
            );

            Json(HAStatusResponse {
                enabled: true,
                instance_id: Some(status.instance_id),
                role: status.role,
                health_state: status.health_state,
                session_count: status.session_count,
                conference_count: status.conference_count,
                failover_count: Some(status.failover_count),
                last_failover: status.last_failover,
                redis_connected: Some(status.redis_connected),
            })
        } else {
            // HA feature enabled but not configured
            Json(HAStatusResponse {
                enabled: false,
                instance_id: None,
                role: "Unknown".to_string(),
                health_state: "N/A".to_string(),
                session_count: state.session_manager.session_count(),
                conference_count: 0,
                failover_count: None,
                last_failover: None,
                redis_connected: None,
            })
        }
    }

    #[cfg(not(feature = "ha"))]
    {
        // HA feature not compiled in
        Json(HAStatusResponse {
            enabled: false,
            instance_id: None,
            role: "Single".to_string(),
            health_state: "N/A".to_string(),
            session_count: state.session_manager.session_count(),
            conference_count: 0,
            failover_count: None,
            last_failover: None,
            redis_connected: None,
        })
    }
}

/// POST /ha/transfer-primary - Initiate manual failover (graceful primary transfer)
///
/// This endpoint allows operators to manually transfer primary role to another instance.
/// The current primary will gracefully step down, allowing the standby to become primary.
///
/// **Requires Admin scope.** (audit finding C6) HA failover disrupts the
/// cluster and must not be triggerable by any bearer-token holder; keep
/// admin tokens short-lived and rotate separately from operator tokens.
/// For additional isolation, bind this listener to an admin-only interface
/// via `ApiServerConfig::admin_bind`.
///
/// Returns:
/// - 200 OK: Transfer initiated successfully
/// - 503 Service Unavailable: HA not enabled or not currently primary
pub async fn transfer_primary(
    _auth: RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    #[cfg(feature = "ha")]
    {
        if let Some(ha_manager) = &state.ha_manager {
            if !ha_manager.is_primary().await {
                warn!("Primary transfer requested but this instance is not primary");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Not currently primary - cannot transfer",
                )
                    .into_response();
            }

            info!("Manual primary transfer initiated");

            match ha_manager.step_down().await {
                Ok(()) => {
                    info!("Primary transfer successful - stepped down");
                    (StatusCode::OK, "Primary transfer initiated successfully").into_response()
                }
                Err(e) => {
                    warn!("Failed to step down as primary: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to transfer primary: {}", e),
                    )
                        .into_response()
                }
            }
        } else {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "HA not enabled on this instance",
            )
                .into_response()
        }
    }

    #[cfg(not(feature = "ha"))]
    {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "HA feature not compiled in",
        )
            .into_response()
    }
}

/// GET /health - Health check endpoint (used by load balancers)
///
/// Returns:
/// - 200 OK: Instance is healthy and ready to accept traffic (Primary)
/// - 503 Service Unavailable: Instance is standby or degraded
///
/// This endpoint is designed for cloud load balancer health checks.
/// Only the primary instance returns 200, ensuring traffic routes to the active server.
pub async fn health_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    #[cfg(feature = "ha")]
    {
        if let Some(ha_manager) = &state.ha_manager {
            if ha_manager.is_primary().await && ha_manager.is_healthy().await {
                // Primary and healthy - accept traffic
                StatusCode::OK
            } else {
                // Standby or degraded - reject traffic
                StatusCode::SERVICE_UNAVAILABLE
            }
        } else {
            // HA not configured - always healthy
            StatusCode::OK
        }
    }

    #[cfg(not(feature = "ha"))]
    {
        // HA not compiled in - always healthy
        StatusCode::OK
    }
}

/// GET /ha/health - HA-aware health probe for load balancers
///
/// - 200 when primary and healthy (or HA disabled)
/// - 503 when standby or degraded
pub async fn health_probe(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    #[cfg(feature = "ha")]
    {
        if let Some(ha_manager) = &state.ha_manager {
            if ha_manager.is_primary().await && ha_manager.is_healthy().await {
                return StatusCode::OK;
            }
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }

    // HA disabled or not compiled; treat as healthy
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ha_status_response_serialization() {
        let response = HAStatusResponse {
            enabled: true,
            instance_id: Some("test-instance".to_string()),
            role: "Primary".to_string(),
            health_state: "Healthy".to_string(),
            session_count: 42,
            conference_count: 3,
            failover_count: Some(2),
            last_failover: Some("2025-12-17T10:00:00Z".to_string()),
            redis_connected: Some(true),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"enabled\":true"));
        assert!(json.contains("\"role\":\"Primary\""));
    }
}
