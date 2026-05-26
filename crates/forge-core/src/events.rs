//! Event system for broadcasting state changes
//!
//! This module provides a publish-subscribe event system for notifying
//! components about state changes throughout the Forge engine.

use crate::types::{CallId, ParticipantId, RecordingId, RoomId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{self, error::SendError};
use tracing::warn;

/// Default event channel capacity
pub const DEFAULT_EVENT_CAPACITY: usize = 4096;

/// DTMF detection method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DtmfDetectionMethod {
    /// RFC 2833 (telephone-event) RTP payload
    Rfc2833,
    /// Inband audio frequency detection
    Inband,
    /// SIP INFO message
    SipInfo,
}

/// DTMF event type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DtmfEventKind {
    /// Digit press started
    Start,
    /// Digit press continues
    Continue,
    /// Digit press ended
    End,
}

/// All possible events that can occur in the Forge engine
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ForgeEvent {
    // Session events
    SessionCreated {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    SessionActive {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    SessionTerminated {
        call_id: CallId,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    SessionOnHold {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    SessionResumed {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },

    // Conference events
    ConferenceCreated {
        room_id: RoomId,
        timestamp: DateTime<Utc>,
    },
    ConferenceDestroyed {
        room_id: RoomId,
        timestamp: DateTime<Utc>,
    },
    ParticipantJoined {
        room_id: RoomId,
        participant_id: ParticipantId,
        timestamp: DateTime<Utc>,
    },
    ParticipantLeft {
        room_id: RoomId,
        participant_id: ParticipantId,
        timestamp: DateTime<Utc>,
    },
    ParticipantMuted {
        room_id: RoomId,
        participant_id: ParticipantId,
        timestamp: DateTime<Utc>,
    },
    ParticipantUnmuted {
        room_id: RoomId,
        participant_id: ParticipantId,
        timestamp: DateTime<Utc>,
    },
    DominantSpeakerChanged {
        room_id: RoomId,
        participant_id: ParticipantId,
        timestamp: DateTime<Utc>,
    },

    // Recording events
    RecordingStarted {
        recording_id: RecordingId,
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    RecordingStopped {
        recording_id: RecordingId,
        call_id: CallId,
        duration_ms: u64,
        timestamp: DateTime<Utc>,
    },
    RecordingPaused {
        recording_id: RecordingId,
        timestamp: DateTime<Utc>,
    },
    RecordingResumed {
        recording_id: RecordingId,
        timestamp: DateTime<Utc>,
    },
    RecordingFailed {
        recording_id: RecordingId,
        error: String,
        timestamp: DateTime<Utc>,
    },

    // Media events
    MediaTimeout {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    MediaActive {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    DtmfDigitDetected {
        call_id: CallId,
        digit: char,
        duration_ms: Option<u32>,
        method: DtmfDetectionMethod,
        event_type: DtmfEventKind,
        timestamp: DateTime<Utc>,
    },

    /// Voice-activity detector flipped to the speech state for this
    /// call. Published once per `Silence`→`Speech` transition (the
    /// detector's hysteresis filters out per-frame jitter; see
    /// `forge_vad::VadConfig::min_speech_duration_ms`). Subscribers
    /// typically pair this with [`Self::SpeechStopped`] to bracket
    /// utterances.
    SpeechStarted {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },

    /// Voice-activity detector flipped back to silence. `duration_ms`
    /// is the wall-clock time since the matching `SpeechStarted` for
    /// the same `call_id`.
    SpeechStopped {
        call_id: CallId,
        timestamp: DateTime<Utc>,
        duration_ms: u64,
    },

    // Quality events
    QualityDegraded {
        call_id: CallId,
        packet_loss_percent: f32,
        jitter_ms: f32,
        timestamp: DateTime<Utc>,
    },
    QualityRestored {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },

    /// Per-RR snapshot from a received RTCP Sender or Receiver Report.
    ///
    /// Fired on **every** received RR block, not just on threshold-based
    /// quality changes — the cadence is whatever the remote endpoint's
    /// RTCP send interval is (per RFC 3550 §6.2, typically every 5 s).
    /// `[`QualityDegraded`] is reserved for threshold-driven semantics
    /// that may layer on top of this stream later.
    ///
    /// All three RR-derived quality fields ride together so consumers
    /// can update their state from one event rather than three.
    RtcpReportReceived {
        call_id: CallId,
        /// Interarrival jitter in milliseconds, derived from the RR's
        /// jitter field (RFC 3550 §6.4.1) converted to ms via the RTP
        /// clock rate of the corresponding media stream.
        jitter_ms: f32,
        /// Cumulative-loss ratio in `[0.0, 1.0]`, derived from the RR's
        /// `fraction_lost` field (8-bit fixed-point, RFC 3550 §6.4.1)
        /// divided by 256.
        packet_loss_ratio: f32,
        /// Mean round-trip time in milliseconds, derived from RTCP
        /// SR/RR exchanges per RFC 3550 §A.7. `None` until forge-engine
        /// originates its own SRs (deferred to 0.3.1 per siphon-ai
        /// DEV_PLAN_0.3.0.md §9 decision 10) — distinct from
        /// `Some(0.0)` ("we measured an RTT of zero", degenerate in
        /// practice).
        rtt_ms: Option<f32>,
        timestamp: DateTime<Utc>,
    },

    // Transcription events
    TranscriptionStarted {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    TranscriptionResult {
        call_id: CallId,
        text: String,
        confidence: f32,
        is_final: bool,
        timestamp: DateTime<Utc>,
    },
    TranscriptionStopped {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },

    // AI streaming events
    AiSessionStarted {
        call_id: CallId,
        provider: String,
        timestamp: DateTime<Utc>,
    },
    AiSessionEnded {
        call_id: CallId,
        timestamp: DateTime<Utc>,
    },
    AiToolCall {
        call_id: CallId,
        tool_name: String,
        arguments: String,
        timestamp: DateTime<Utc>,
    },

    // System events
    EngineStarted {
        timestamp: DateTime<Utc>,
    },
    EngineStopping {
        timestamp: DateTime<Utc>,
    },
    ResourceLimitReached {
        resource: String,
        current: u64,
        limit: u64,
        timestamp: DateTime<Utc>,
    },
}

impl ForgeEvent {
    /// Get the timestamp of this event
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Self::SessionCreated { timestamp, .. }
            | Self::SessionActive { timestamp, .. }
            | Self::SessionTerminated { timestamp, .. }
            | Self::SessionOnHold { timestamp, .. }
            | Self::SessionResumed { timestamp, .. }
            | Self::ConferenceCreated { timestamp, .. }
            | Self::ConferenceDestroyed { timestamp, .. }
            | Self::ParticipantJoined { timestamp, .. }
            | Self::ParticipantLeft { timestamp, .. }
            | Self::ParticipantMuted { timestamp, .. }
            | Self::ParticipantUnmuted { timestamp, .. }
            | Self::DominantSpeakerChanged { timestamp, .. }
            | Self::RecordingStarted { timestamp, .. }
            | Self::RecordingStopped { timestamp, .. }
            | Self::RecordingPaused { timestamp, .. }
            | Self::RecordingResumed { timestamp, .. }
            | Self::RecordingFailed { timestamp, .. }
            | Self::MediaTimeout { timestamp, .. }
            | Self::MediaActive { timestamp, .. }
            | Self::DtmfDigitDetected { timestamp, .. }
            | Self::SpeechStarted { timestamp, .. }
            | Self::SpeechStopped { timestamp, .. }
            | Self::QualityDegraded { timestamp, .. }
            | Self::QualityRestored { timestamp, .. }
            | Self::RtcpReportReceived { timestamp, .. }
            | Self::TranscriptionStarted { timestamp, .. }
            | Self::TranscriptionResult { timestamp, .. }
            | Self::TranscriptionStopped { timestamp, .. }
            | Self::AiSessionStarted { timestamp, .. }
            | Self::AiSessionEnded { timestamp, .. }
            | Self::AiToolCall { timestamp, .. }
            | Self::EngineStarted { timestamp }
            | Self::EngineStopping { timestamp }
            | Self::ResourceLimitReached { timestamp, .. } => *timestamp,
        }
    }

    /// Get a human-readable name for this event type
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "session_created",
            Self::SessionActive { .. } => "session_active",
            Self::SessionTerminated { .. } => "session_terminated",
            Self::SessionOnHold { .. } => "session_on_hold",
            Self::SessionResumed { .. } => "session_resumed",
            Self::ConferenceCreated { .. } => "conference_created",
            Self::ConferenceDestroyed { .. } => "conference_destroyed",
            Self::ParticipantJoined { .. } => "participant_joined",
            Self::ParticipantLeft { .. } => "participant_left",
            Self::ParticipantMuted { .. } => "participant_muted",
            Self::ParticipantUnmuted { .. } => "participant_unmuted",
            Self::DominantSpeakerChanged { .. } => "dominant_speaker_changed",
            Self::RecordingStarted { .. } => "recording_started",
            Self::RecordingStopped { .. } => "recording_stopped",
            Self::RecordingPaused { .. } => "recording_paused",
            Self::RecordingResumed { .. } => "recording_resumed",
            Self::RecordingFailed { .. } => "recording_failed",
            Self::MediaTimeout { .. } => "media_timeout",
            Self::MediaActive { .. } => "media_active",
            Self::DtmfDigitDetected { .. } => "dtmf_digit_detected",
            Self::SpeechStarted { .. } => "speech_started",
            Self::SpeechStopped { .. } => "speech_stopped",
            Self::QualityDegraded { .. } => "quality_degraded",
            Self::QualityRestored { .. } => "quality_restored",
            Self::RtcpReportReceived { .. } => "rtcp_report_received",
            Self::TranscriptionStarted { .. } => "transcription_started",
            Self::TranscriptionResult { .. } => "transcription_result",
            Self::TranscriptionStopped { .. } => "transcription_stopped",
            Self::AiSessionStarted { .. } => "ai_session_started",
            Self::AiSessionEnded { .. } => "ai_session_ended",
            Self::AiToolCall { .. } => "ai_tool_call",
            Self::EngineStarted { .. } => "engine_started",
            Self::EngineStopping { .. } => "engine_stopping",
            Self::ResourceLimitReached { .. } => "resource_limit_reached",
        }
    }
}

/// Event bus for publishing and subscribing to events
///
/// This uses Tokio's broadcast channel for efficient multi-subscriber event distribution.
///
/// # Example
///
/// ```rust,ignore
/// use forge_core::{EventBus, ForgeEvent, CallId};
/// use chrono::Utc;
///
/// let event_bus = EventBus::new();
///
/// // Subscribe to events
/// let mut rx = event_bus.subscribe();
///
/// // Publish an event
/// event_bus.publish(ForgeEvent::SessionCreated {
///     call_id: CallId::generate(),
///     timestamp: Utc::now(),
/// });
///
/// // Receive event
/// if let Ok(event) = rx.recv().await {
///     println!("Received: {:?}", event);
/// }
/// ```
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ForgeEvent>,
}

impl EventBus {
    /// Create a new event bus with default capacity
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_EVENT_CAPACITY)
    }

    /// Create a new event bus with specified capacity
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of events that can be buffered
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = if capacity == 0 {
            warn!("Requested EventBus capacity of 0; defaulting to capacity 1");
            1
        } else {
            capacity
        };

        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event to all subscribers
    ///
    /// # Arguments
    ///
    /// * `event` - The event to publish
    ///
    /// # Returns
    ///
    /// The number of subscribers that received the event, or an error if no subscribers exist
    pub fn publish(&self, event: ForgeEvent) -> Result<usize, SendError<ForgeEvent>> {
        match self.tx.send(event) {
            Ok(count) => {
                tracing::trace!("Published event to {} subscribers", count);
                Ok(count)
            }
            Err(e) => {
                tracing::warn!("Failed to publish event: no active subscribers");
                Err(e)
            }
        }
    }

    /// Subscribe to events
    ///
    /// # Returns
    ///
    /// A receiver that will receive all future events
    pub fn subscribe(&self) -> broadcast::Receiver<ForgeEvent> {
        self.tx.subscribe()
    }

    /// Get the number of active subscribers
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscriber_count", &self.subscriber_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus_publish_subscribe() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        let call_id = CallId::generate();
        let event = ForgeEvent::SessionCreated {
            call_id: call_id.clone(),
            timestamp: Utc::now(),
        };

        // Publish event
        let count = bus.publish(event.clone()).unwrap();
        assert_eq!(count, 1);

        // Receive event
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "session_created");
    }

    #[tokio::test]
    async fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        assert_eq!(bus.subscriber_count(), 2);

        let event = ForgeEvent::EngineStarted {
            timestamp: Utc::now(),
        };

        // Publish to both subscribers
        let count = bus.publish(event).unwrap();
        assert_eq!(count, 2);

        // Both should receive
        let r1 = rx1.recv().await.unwrap();
        let r2 = rx2.recv().await.unwrap();

        assert_eq!(r1.event_type(), "engine_started");
        assert_eq!(r2.event_type(), "engine_started");
    }

    #[test]
    fn test_event_timestamp() {
        let now = Utc::now();
        let event = ForgeEvent::SessionCreated {
            call_id: CallId::generate(),
            timestamp: now,
        };

        assert_eq!(event.timestamp(), now);
    }

    #[test]
    fn test_event_type() {
        let event = ForgeEvent::ParticipantJoined {
            room_id: RoomId::generate(),
            participant_id: ParticipantId::generate(),
            timestamp: Utc::now(),
        };

        assert_eq!(event.event_type(), "participant_joined");
    }
}
