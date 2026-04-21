//! API routes

pub mod ai;
pub mod conference_ai;
pub mod conferences;
pub mod event_bus;
pub mod ha;
pub mod health;
pub mod metrics;
pub mod prometheus;
pub mod sessions;
pub mod webrtc;
pub mod websocket;

use axum::Router;
use std::sync::Arc;

/// Create the main API router with all routes (operator + admin).
///
/// Used when `ApiServerConfig::admin_bind` is `None`. When a separate
/// admin listener is configured, [`create_public_router`] +
/// [`create_admin_router`] are used instead so that admin endpoints only
/// accept connections on the admin listener.
pub fn create_router() -> Router<Arc<sessions::AppState>> {
    Router::new()
        .merge(health::routes())
        .merge(ha::routes())
        .merge(sessions::routes())
        .merge(conferences::routes())
        .merge(conference_ai::routes())
        .merge(webrtc::routes())
        .merge(ai::routes())
        .merge(metrics::routes())
        .merge(prometheus::routes())
        .merge(websocket::routes())
        .merge(event_bus::routes())
}

/// Router containing only routes that are safe to expose on the public
/// listener when a separate admin listener is configured. Admin-only
/// mutation endpoints (currently `POST /ha/transfer-primary`) are moved
/// to [`create_admin_router`].
pub fn create_public_router() -> Router<Arc<sessions::AppState>> {
    Router::new()
        .merge(health::routes())
        .merge(ha::public_routes())
        .merge(sessions::routes())
        .merge(conferences::routes())
        .merge(conference_ai::routes())
        .merge(webrtc::routes())
        .merge(ai::routes())
        .merge(metrics::routes())
        .merge(prometheus::routes())
        .merge(websocket::routes())
        .merge(event_bus::routes())
}

/// Router containing only admin-scoped routes. When `admin_bind` is set,
/// this router is bound there and to no other listener (C6 defense in depth:
/// firewall-restrict the admin interface to internal management networks).
pub fn create_admin_router() -> Router<Arc<sessions::AppState>> {
    Router::new()
        .merge(health::routes())
        .merge(ha::admin_routes())
}
