//! API routes

pub mod conferences;
pub mod health;
pub mod metrics;
pub mod prometheus;
pub mod sessions;
pub mod webrtc;
pub mod websocket;

use axum::Router;
use std::sync::Arc;

/// Create the main API router with all routes
pub fn create_router() -> Router<Arc<sessions::AppState>> {
    Router::new()
        .merge(health::routes())
        .merge(sessions::routes())
        .merge(conferences::routes())
        .merge(webrtc::routes())
        .merge(metrics::routes())
        .merge(prometheus::routes())
        .merge(websocket::routes())
}
