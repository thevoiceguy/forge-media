//! DTMF event deduplication and priority handling
//!
//! When multiple DTMF detection methods are active (RFC 2833, inband, SIP INFO),
//! the same digit press may be detected multiple times. This module implements
//! deduplication logic with priority handling:
//!
//! Priority order:
//! 1. RFC 2833 (telephone-event) - Highest priority, most reliable
//! 2. Inband (Goertzel) - Medium priority, fallback
//! 3. SIP INFO - Lowest priority (when implemented)
//!
//! Deduplication window: 100ms (configurable)

use crate::{DtmfDigit, DtmfEvent, DtmfEventType, DtmfMethod};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Priority value for DTMF detection methods
impl DtmfMethod {
    /// Get priority value (higher = better)
    pub fn priority(&self) -> u8 {
        match self {
            DtmfMethod::Rfc2833 => 3,
            DtmfMethod::Inband => 2,
            DtmfMethod::SipInfo => 1,
        }
    }
}

/// Recent DTMF event record for deduplication
#[derive(Debug, Clone)]
struct RecentEvent {
    digit: DtmfDigit,
    event_type: DtmfEventType,
    method: DtmfMethod,
    timestamp: Instant,
}

/// DTMF event deduplicator
///
/// Filters duplicate DTMF events detected by multiple methods and
/// prefers higher-priority detection methods.
///
/// # Example
///
/// ```rust,ignore
/// use forge_dtmf::{DtmfDeduplicator, DtmfEvent, DtmfMethod, DtmfDigit, DtmfEventType};
///
/// let mut dedup = DtmfDeduplicator::new();
///
/// // RFC 2833 event arrives first
/// let rfc_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
/// assert!(dedup.should_publish(&rfc_event)); // Publish it
///
/// // Inband detection of same digit arrives 30ms later
/// let inband_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
/// assert!(!dedup.should_publish(&inband_event)); // Suppress duplicate
/// ```
pub struct DtmfDeduplicator {
    /// Recent events for deduplication
    recent_events: VecDeque<RecentEvent>,
    /// Deduplication time window
    dedup_window: Duration,
    /// Maximum number of recent events to track
    max_recent: usize,
}

impl DtmfDeduplicator {
    /// Create a new deduplicator with default settings
    ///
    /// Default deduplication window: 100ms
    pub fn new() -> Self {
        Self::with_window(Duration::from_millis(100))
    }

    /// Create a new deduplicator with custom deduplication window
    pub fn with_window(dedup_window: Duration) -> Self {
        Self {
            recent_events: VecDeque::with_capacity(32),
            dedup_window,
            max_recent: 32,
        }
    }

    /// Check if an event should be published or suppressed as duplicate
    ///
    /// Returns `true` if the event should be published, `false` if it's a duplicate.
    ///
    /// Logic:
    /// 1. Clean up expired events (older than dedup window)
    /// 2. Check if same digit+type exists in recent events
    /// 3. If found, compare priority:
    ///    - If new event has higher priority, publish and replace old event
    ///    - If new event has lower/equal priority, suppress
    /// 4. If not found, publish and track event
    pub fn should_publish(&mut self, event: &DtmfEvent) -> bool {
        let now = Instant::now();

        // Clean up expired events
        self.cleanup_expired(now);

        // Look for duplicate in recent events
        if let Some(idx) = self.find_duplicate(event, now) {
            let recent = &self.recent_events[idx];

            // Compare priority
            if event.method.priority() > recent.method.priority() {
                // Higher priority event - replace old one and publish
                tracing::debug!(
                    "DTMF dedup: Replacing {:?} event for {} with higher-priority {:?}",
                    recent.method,
                    event.digit,
                    event.method
                );

                self.recent_events[idx] = RecentEvent {
                    digit: event.digit,
                    event_type: event.event_type,
                    method: event.method,
                    timestamp: now,
                };

                true
            } else {
                // Lower or equal priority - suppress
                tracing::debug!(
                    "DTMF dedup: Suppressing duplicate {:?} event for {} (already have {:?})",
                    event.method,
                    event.digit,
                    recent.method
                );

                false
            }
        } else {
            // Not a duplicate - track and publish
            self.track_event(event, now);
            true
        }
    }

    /// Find duplicate event in recent events
    fn find_duplicate(&self, event: &DtmfEvent, now: Instant) -> Option<usize> {
        self.recent_events
            .iter()
            .position(|recent| {
                recent.digit == event.digit
                    && recent.event_type == event.event_type
                    && now.duration_since(recent.timestamp) < self.dedup_window
            })
    }

    /// Track a new event
    fn track_event(&mut self, event: &DtmfEvent, timestamp: Instant) {
        // Add new event
        self.recent_events.push_back(RecentEvent {
            digit: event.digit,
            event_type: event.event_type,
            method: event.method,
            timestamp,
        });

        // Limit size
        if self.recent_events.len() > self.max_recent {
            self.recent_events.pop_front();
        }
    }

    /// Remove expired events older than deduplication window
    fn cleanup_expired(&mut self, now: Instant) {
        while let Some(front) = self.recent_events.front() {
            if now.duration_since(front.timestamp) >= self.dedup_window {
                self.recent_events.pop_front();
            } else {
                break;
            }
        }
    }

    /// Reset deduplicator state
    pub fn reset(&mut self) {
        self.recent_events.clear();
    }

    /// Get number of tracked recent events
    pub fn recent_count(&self) -> usize {
        self.recent_events.len()
    }
}

impl Default for DtmfDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_same_method() {
        let mut dedup = DtmfDeduplicator::new();

        let event1 = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        let event2 = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);

        // First event should be published
        assert!(dedup.should_publish(&event1));

        // Immediate duplicate should be suppressed
        assert!(!dedup.should_publish(&event2));
    }

    #[test]
    fn test_dedup_priority_rfc2833_wins() {
        let mut dedup = DtmfDeduplicator::new();

        // Inband arrives first
        let inband_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        assert!(dedup.should_publish(&inband_event));

        // RFC 2833 arrives shortly after - should replace and publish
        let rfc_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        assert!(dedup.should_publish(&rfc_event));

        // Another inband should now be suppressed
        let inband_event2 = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        assert!(!dedup.should_publish(&inband_event2));
    }

    #[test]
    fn test_dedup_different_digits() {
        let mut dedup = DtmfDeduplicator::new();

        let event1 = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        let event2 = DtmfEvent::new(DtmfDigit::Six, DtmfEventType::Start, DtmfMethod::Rfc2833);

        // Different digits should both be published
        assert!(dedup.should_publish(&event1));
        assert!(dedup.should_publish(&event2));
    }

    #[test]
    fn test_dedup_different_event_types() {
        let mut dedup = DtmfDeduplicator::new();

        let start_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        let end_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::End, DtmfMethod::Rfc2833);

        // Different event types should both be published
        assert!(dedup.should_publish(&start_event));
        assert!(dedup.should_publish(&end_event));
    }

    #[test]
    fn test_dedup_window_expiry() {
        let mut dedup = DtmfDeduplicator::with_window(Duration::from_millis(50));

        let event1 = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        assert!(dedup.should_publish(&event1));

        // Wait for dedup window to expire
        std::thread::sleep(Duration::from_millis(60));

        // Same event after expiry should be published again
        let event2 = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        assert!(dedup.should_publish(&event2));
    }

    #[test]
    fn test_method_priority() {
        assert!(DtmfMethod::Rfc2833.priority() > DtmfMethod::Inband.priority());
        assert!(DtmfMethod::Inband.priority() > DtmfMethod::SipInfo.priority());
        assert!(DtmfMethod::Rfc2833.priority() > DtmfMethod::SipInfo.priority());
    }

    #[test]
    fn test_reset() {
        let mut dedup = DtmfDeduplicator::new();

        let event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        assert!(dedup.should_publish(&event));
        assert_eq!(dedup.recent_count(), 1);

        dedup.reset();
        assert_eq!(dedup.recent_count(), 0);

        // After reset, same event should be published again
        assert!(dedup.should_publish(&event));
    }
}
