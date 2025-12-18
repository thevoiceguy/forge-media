//! Event bus observability endpoints

use axum::{routing::get, Json, Router};
use serde::Serialize;
use std::sync::Arc;

use super::sessions::AppState;

#[derive(Debug, Serialize)]
pub struct EventBusMetricsResponse {
    pub active_rooms: usize,
    pub global_subscribers: usize,
    pub rooms: Vec<RoomSubscribers>,
}

#[derive(Debug, Serialize)]
pub struct RoomSubscribers {
    pub room_id: String,
    pub subscribers: usize,
}

/// GET /v1/events/metrics - current event bus subscriber counts
async fn get_event_bus_metrics(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Json<EventBusMetricsResponse> {
    let rooms = state.event_bus.room_subscriber_counts().await;
    let active_rooms = rooms.len();
    let global_subscribers = state.event_bus.global_subscriber_count();

    Json(EventBusMetricsResponse {
        active_rooms,
        global_subscribers,
        rooms: rooms
            .into_iter()
            .map(|(room_id, subscribers)| RoomSubscribers { room_id, subscribers })
            .collect(),
    })
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/v1/events/metrics", get(get_event_bus_metrics))
}
