//! API routes

pub mod health;
pub mod sessions;

use axum::Router;
use std::sync::Arc;

/// Create the main API router with all routes
pub fn create_router() -> Router<Arc<sessions::AppState>> {
    Router::new()
        .merge(health::routes())
        .merge(sessions::routes())
}
