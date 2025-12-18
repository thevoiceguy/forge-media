//! Real-time event infrastructure for WebSocket notifications
//!
//! Provides event types and an EventBus for pub/sub of conference events

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, warn};

/// Conference event types that can be broadcast to WebSocket clients
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConferenceEvent {
    /// A participant joined a conference room
    ParticipantJoined {
        room_id: String,
        participant_id: String,
        timestamp: u64,
    },
    /// A participant left a conference room
    ParticipantLeft {
        room_id: String,
        participant_id: String,
        timestamp: u64,
    },
    /// A participant started speaking
    ParticipantSpeaking {
        room_id: String,
        participant_id: String,
        is_speaking: bool,
        timestamp: u64,
    },
    /// Room recording started
    RecordingStarted { room_id: String, timestamp: u64 },
    /// Room recording stopped
    RecordingStopped { room_id: String, timestamp: u64 },
    /// Participant recording started
    ParticipantRecordingStarted {
        room_id: String,
        participant_id: String,
        timestamp: u64,
    },
    /// Participant recording stopped
    ParticipantRecordingStopped {
        room_id: String,
        participant_id: String,
        timestamp: u64,
    },
    /// Participant state changed (muted, on-hold, etc.)
    ParticipantStateChanged {
        room_id: String,
        participant_id: String,
        state: String,
        timestamp: u64,
    },
}

impl ConferenceEvent {
    /// Get the room ID associated with this event
    pub fn room_id(&self) -> &str {
        match self {
            ConferenceEvent::ParticipantJoined { room_id, .. }
            | ConferenceEvent::ParticipantLeft { room_id, .. }
            | ConferenceEvent::ParticipantSpeaking { room_id, .. }
            | ConferenceEvent::RecordingStarted { room_id, .. }
            | ConferenceEvent::RecordingStopped { room_id, .. }
            | ConferenceEvent::ParticipantRecordingStarted { room_id, .. }
            | ConferenceEvent::ParticipantRecordingStopped { room_id, .. }
            | ConferenceEvent::ParticipantStateChanged { room_id, .. } => room_id,
        }
    }

    /// Get the participant ID if this event is participant-specific
    pub fn participant_id(&self) -> Option<&str> {
        match self {
            ConferenceEvent::ParticipantJoined { participant_id, .. }
            | ConferenceEvent::ParticipantLeft { participant_id, .. }
            | ConferenceEvent::ParticipantSpeaking { participant_id, .. }
            | ConferenceEvent::ParticipantRecordingStarted { participant_id, .. }
            | ConferenceEvent::ParticipantRecordingStopped { participant_id, .. }
            | ConferenceEvent::ParticipantStateChanged { participant_id, .. } => {
                Some(participant_id)
            }
            _ => None,
        }
    }

    /// Get the event timestamp in milliseconds since epoch
    pub fn timestamp(&self) -> u64 {
        match self {
            ConferenceEvent::ParticipantJoined { timestamp, .. }
            | ConferenceEvent::ParticipantLeft { timestamp, .. }
            | ConferenceEvent::ParticipantSpeaking { timestamp, .. }
            | ConferenceEvent::RecordingStarted { timestamp, .. }
            | ConferenceEvent::RecordingStopped { timestamp, .. }
            | ConferenceEvent::ParticipantRecordingStarted { timestamp, .. }
            | ConferenceEvent::ParticipantRecordingStopped { timestamp, .. }
            | ConferenceEvent::ParticipantStateChanged { timestamp, .. } => *timestamp,
        }
    }
}

/// Event bus for publishing and subscribing to conference events
#[derive(Clone)]
pub struct EventBus {
    /// Map of room-specific event channels
    room_channels: Arc<RwLock<HashMap<String, broadcast::Sender<ConferenceEvent>>>>,
    /// Global event channel (all rooms)
    global_channel: broadcast::Sender<ConferenceEvent>,
    /// Channel capacity (number of events buffered)
    capacity: usize,
}

impl EventBus {
    /// Create a new EventBus with the specified channel capacity
    ///
    /// # Arguments
    /// * `capacity` - Number of events to buffer per channel (default: 100)
    pub fn new(capacity: usize) -> Self {
        let (global_tx, _) = broadcast::channel(capacity);

        Self {
            room_channels: Arc::new(RwLock::new(HashMap::new())),
            global_channel: global_tx,
            capacity,
        }
    }

    /// Publish an event to all subscribers
    ///
    /// Events are sent to both the global channel and the room-specific channel
    pub async fn publish(&self, event: ConferenceEvent) {
        let room_id = event.room_id().to_string();

        // Send to global channel
        if let Err(e) = self.global_channel.send(event.clone()) {
            warn!("Failed to send event to global channel: {}", e);
        }

        // Send to room-specific channel
        let channels = self.room_channels.read().await;
        if let Some(tx) = channels.get(&room_id) {
            if let Err(e) = tx.send(event.clone()) {
                warn!("Failed to send event to room {} channel: {}", room_id, e);
            }
            if tx.receiver_count() == 0 {
                drop(channels);
                self.prune_room(&room_id).await;
            }
        } else {
            debug!("No subscribers for room {}, event not sent", room_id);
        }

        debug!("Published event: {:?}", event);
    }

    /// Subscribe to all conference events (global)
    ///
    /// Returns a receiver that will receive all events from all rooms
    pub fn subscribe_global(&self) -> broadcast::Receiver<ConferenceEvent> {
        self.global_channel.subscribe()
    }

    /// Subscribe to events for a specific room
    ///
    /// Returns a receiver that will only receive events for the specified room
    pub async fn subscribe_room(&self, room_id: &str) -> broadcast::Receiver<ConferenceEvent> {
        let mut channels = self.room_channels.write().await;

        let tx = channels
            .entry(room_id.to_string())
            .or_insert_with(|| broadcast::channel(self.capacity).0);

        tx.subscribe()
    }

    /// Unsubscribe from a room (cleanup when room is deleted)
    ///
    /// This removes the room's channel and cleans up resources
    pub async fn unsubscribe_room(&self, room_id: &str) {
        let mut channels = self.room_channels.write().await;
        if channels.remove(room_id).is_some() {
            debug!("Removed event channel for room {}", room_id);
        }
    }

    /// Remove a room channel if no subscribers remain
    pub async fn prune_room(&self, room_id: &str) {
        let mut channels = self.room_channels.write().await;
        if let Some(sender) = channels.get(room_id) {
            if sender.receiver_count() == 0 {
                channels.remove(room_id);
                debug!("Pruned empty event channel for room {}", room_id);
            }
        }
    }

    /// Get the number of active room subscriptions
    pub async fn active_rooms(&self) -> usize {
        self.room_channels.read().await.len()
    }

    /// Get the number of subscribers to the global channel
    pub fn global_subscriber_count(&self) -> usize {
        self.global_channel.receiver_count()
    }

    /// Get subscriber counts for each room
    pub async fn room_subscriber_counts(&self) -> Vec<(String, usize)> {
        self.room_channels
            .read()
            .await
            .iter()
            .map(|(room, tx)| (room.clone(), tx.receiver_count()))
            .collect()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_event_bus_global_subscription() {
        let bus = EventBus::new(10);
        let mut rx = bus.subscribe_global();

        let event = ConferenceEvent::ParticipantJoined {
            room_id: "test-room".to_string(),
            participant_id: "alice".to_string(),
            timestamp: 12345,
        };

        bus.publish(event.clone()).await;

        let received = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("Timeout waiting for event")
            .expect("Failed to receive event");

        assert_eq!(received.room_id(), event.room_id());
        assert_eq!(received.participant_id(), event.participant_id());
    }

    #[tokio::test]
    async fn test_event_bus_room_subscription() {
        let bus = EventBus::new(10);
        let mut rx = bus.subscribe_room("test-room").await;

        let event = ConferenceEvent::RecordingStarted {
            room_id: "test-room".to_string(),
            timestamp: 12345,
        };

        bus.publish(event.clone()).await;

        let received = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("Timeout waiting for event")
            .expect("Failed to receive event");

        assert_eq!(received.room_id(), "test-room");
    }

    #[tokio::test]
    async fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new(10);
        let mut rx1 = bus.subscribe_global();
        let mut rx2 = bus.subscribe_global();

        let event = ConferenceEvent::ParticipantSpeaking {
            room_id: "test-room".to_string(),
            participant_id: "alice".to_string(),
            is_speaking: true,
            timestamp: 12345,
        };

        bus.publish(event.clone()).await;

        let received1 = timeout(Duration::from_millis(100), rx1.recv())
            .await
            .expect("Timeout waiting for event on rx1")
            .expect("Failed to receive event on rx1");

        let received2 = timeout(Duration::from_millis(100), rx2.recv())
            .await
            .expect("Timeout waiting for event on rx2")
            .expect("Failed to receive event on rx2");

        assert_eq!(received1.room_id(), event.room_id());
        assert_eq!(received2.room_id(), event.room_id());
    }

    #[tokio::test]
    async fn test_event_bus_room_isolation() {
        let bus = EventBus::new(10);
        let mut rx_room1 = bus.subscribe_room("room1").await;
        let mut rx_room2 = bus.subscribe_room("room2").await;

        let event1 = ConferenceEvent::ParticipantJoined {
            room_id: "room1".to_string(),
            participant_id: "alice".to_string(),
            timestamp: 12345,
        };

        bus.publish(event1.clone()).await;

        // room1 should receive the event
        let received = timeout(Duration::from_millis(100), rx_room1.recv())
            .await
            .expect("Timeout waiting for event")
            .expect("Failed to receive event");

        assert_eq!(received.room_id(), "room1");

        // room2 should NOT receive the event
        let result = timeout(Duration::from_millis(100), rx_room2.recv()).await;
        assert!(result.is_err(), "room2 should not receive room1 events");
    }

    #[tokio::test]
    async fn test_event_serialization() {
        let event = ConferenceEvent::ParticipantJoined {
            room_id: "test-room".to_string(),
            participant_id: "alice".to_string(),
            timestamp: 12345,
        };

        let json = serde_json::to_string(&event).expect("Failed to serialize");
        let deserialized: ConferenceEvent =
            serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(deserialized.room_id(), event.room_id());
        assert_eq!(deserialized.participant_id(), event.participant_id());
        assert_eq!(deserialized.timestamp(), event.timestamp());
    }

    #[tokio::test]
    async fn test_unsubscribe_room() {
        let bus = EventBus::new(10);

        // Subscribe to room
        let _rx = bus.subscribe_room("test-room").await;
        assert_eq!(bus.active_rooms().await, 1);

        // Unsubscribe
        bus.unsubscribe_room("test-room").await;
        assert_eq!(bus.active_rooms().await, 0);
    }

    #[test]
    fn test_event_helpers() {
        let event = ConferenceEvent::ParticipantStateChanged {
            room_id: "room1".to_string(),
            participant_id: "alice".to_string(),
            state: "muted".to_string(),
            timestamp: 12345,
        };

        assert_eq!(event.room_id(), "room1");
        assert_eq!(event.participant_id(), Some("alice"));
        assert_eq!(event.timestamp(), 12345);

        let event2 = ConferenceEvent::RecordingStarted {
            room_id: "room1".to_string(),
            timestamp: 12345,
        };

        assert_eq!(event2.room_id(), "room1");
        assert_eq!(event2.participant_id(), None);
    }
}
