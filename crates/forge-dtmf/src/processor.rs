//! Unified DTMF processor combining all detection methods
//!
//! Provides a high-level API for DTMF detection that combines:
//! - RFC 2833 (telephone-event) detection
//! - Inband (Goertzel) detection
//! - Automatic deduplication with priority handling
//! - Optional digit buffering with timeouts
//!
//! # Example
//!
//! ```rust,ignore
//! use forge_dtmf::{DtmfProcessor, DtmfProcessorConfig};
//!
//! let config = DtmfProcessorConfig {
//!     enable_rfc2833: true,
//!     enable_inband: true,
//!     sample_rate: 8000,
//!     frame_size: 160,
//!     ..Default::default()
//! };
//!
//! let mut processor = DtmfProcessor::new(config);
//!
//! // Process RFC 2833 RTP payload
//! let events = processor.process_rfc2833(&rtp_payload, rtp_timestamp)?;
//! for event in events {
//!     println!("DTMF: {} ({:?})", event.digit, event.event_type);
//! }
//!
//! // Process audio samples for inband detection
//! let events = processor.process_audio(&pcm_samples)?;
//! for event in events {
//!     println!("DTMF: {} ({:?})", event.digit, event.event_type);
//! }
//! ```

use crate::buffer::DtmfBuffer;
use crate::dedup::DtmfDeduplicator;
use crate::detector::{DtmfDetector, DtmfEvent};
use crate::inband::GoertzelDetector;
use crate::rfc2833::Rfc2833Detector;
use crate::Result;
use std::time::Duration;

/// Configuration for unified DTMF processor
#[derive(Debug, Clone)]
pub struct DtmfProcessorConfig {
    /// Enable RFC 2833 detection
    pub enable_rfc2833: bool,

    /// Enable inband detection
    pub enable_inband: bool,

    /// Sample rate (for both RFC 2833 and inband)
    pub sample_rate: u32,

    /// Frame size for inband detection (number of samples, e.g., 160 for 20ms at 8kHz)
    pub frame_size: usize,

    /// Enable automatic deduplication
    pub enable_deduplication: bool,

    /// Deduplication window (default: 100ms)
    pub dedup_window: Duration,

    /// Enable digit buffering
    pub enable_buffering: bool,

    /// Inter-digit timeout for buffer
    pub inter_digit_timeout: Duration,

    /// Total timeout for buffer
    pub total_timeout: Option<Duration>,

    /// Maximum digits to collect in buffer
    pub max_digits: Option<usize>,
}

impl Default for DtmfProcessorConfig {
    fn default() -> Self {
        Self {
            enable_rfc2833: true,
            enable_inband: true,
            sample_rate: 8000,
            frame_size: 160,
            enable_deduplication: true,
            dedup_window: Duration::from_millis(100),
            enable_buffering: false,
            inter_digit_timeout: Duration::from_secs(3),
            total_timeout: Some(Duration::from_secs(30)),
            max_digits: None,
        }
    }
}

/// Unified DTMF processor
///
/// Combines multiple DTMF detection methods with automatic deduplication
/// and optional digit buffering.
pub struct DtmfProcessor {
    config: DtmfProcessorConfig,

    /// RFC 2833 detector
    rfc2833_detector: Option<Rfc2833Detector>,

    /// Inband detector
    inband_detector: Option<GoertzelDetector>,

    /// Deduplicator
    deduplicator: Option<DtmfDeduplicator>,

    /// Digit buffer
    buffer: Option<DtmfBuffer>,
}

impl DtmfProcessor {
    /// Create a new DTMF processor
    pub fn new(config: DtmfProcessorConfig) -> Self {
        let rfc2833_detector = if config.enable_rfc2833 {
            Some(Rfc2833Detector::new(config.sample_rate))
        } else {
            None
        };

        let inband_detector = if config.enable_inband {
            Some(GoertzelDetector::new(
                config.sample_rate,
                config.frame_size,
            ))
        } else {
            None
        };

        let deduplicator = if config.enable_deduplication {
            Some(DtmfDeduplicator::with_window(config.dedup_window))
        } else {
            None
        };

        let buffer = if config.enable_buffering {
            let mut buf = DtmfBuffer::new()
                .with_inter_digit_timeout(config.inter_digit_timeout);

            if let Some(total) = config.total_timeout {
                buf = buf.with_total_timeout(total);
            } else {
                buf = buf.without_total_timeout();
            }

            if let Some(max) = config.max_digits {
                buf = buf.with_max_digits(max);
            }

            Some(buf)
        } else {
            None
        };

        Self {
            config,
            rfc2833_detector,
            inband_detector,
            deduplicator,
            buffer,
        }
    }

    /// Process RFC 2833 RTP payload
    ///
    /// Returns deduplicated events if deduplication is enabled
    pub fn process_rfc2833(&mut self, data: &[u8], timestamp: u32) -> Result<Vec<DtmfEvent>> {
        if let Some(detector) = &mut self.rfc2833_detector {
            let mut events = detector.process_with_timestamp(data, timestamp)?;
            self.filter_and_buffer_events(&mut events);
            Ok(events)
        } else {
            Ok(Vec::new())
        }
    }

    /// Process audio samples for inband detection
    ///
    /// Expects PCM i16 samples. Returns deduplicated events if deduplication is enabled.
    pub fn process_audio(&mut self, samples: &[i16]) -> Result<Vec<DtmfEvent>> {
        if let Some(detector) = &mut self.inband_detector {
            let mut events = detector.process_samples(samples)?;
            self.filter_and_buffer_events(&mut events);
            Ok(events)
        } else {
            Ok(Vec::new())
        }
    }

    /// Process audio bytes (little-endian i16 PCM)
    ///
    /// Convenience method that converts bytes to samples
    pub fn process_audio_bytes(&mut self, data: &[u8]) -> Result<Vec<DtmfEvent>> {
        if let Some(detector) = &mut self.inband_detector {
            let mut events = detector.process(data)?;
            self.filter_and_buffer_events(&mut events);
            Ok(events)
        } else {
            Ok(Vec::new())
        }
    }

    /// Filter events through deduplicator and buffer
    fn filter_and_buffer_events(&mut self, events: &mut Vec<DtmfEvent>) {
        // Apply deduplication
        if let Some(dedup) = &mut self.deduplicator {
            events.retain(|event| dedup.should_publish(event));
        }

        // Add to buffer
        if let Some(buffer) = &mut self.buffer {
            for event in events.iter() {
                // Only buffer Start events (complete digit presses)
                if matches!(event.event_type, crate::DtmfEventType::Start) {
                    buffer.push(event.digit);
                }
            }
        }
    }

    /// Get the digit buffer (if enabled)
    pub fn buffer(&self) -> Option<&DtmfBuffer> {
        self.buffer.as_ref()
    }

    /// Get mutable reference to digit buffer (if enabled)
    pub fn buffer_mut(&mut self) -> Option<&mut DtmfBuffer> {
        self.buffer.as_mut()
    }

    /// Reset all detectors and buffer
    pub fn reset(&mut self) {
        if let Some(detector) = &mut self.rfc2833_detector {
            detector.reset();
        }
        if let Some(detector) = &mut self.inband_detector {
            detector.reset();
        }
        if let Some(dedup) = &mut self.deduplicator {
            dedup.reset();
        }
        if let Some(buffer) = &mut self.buffer {
            buffer.reset();
        }
    }

    /// Get configuration
    pub fn config(&self) -> &DtmfProcessorConfig {
        &self.config
    }

    /// Check if RFC 2833 detection is enabled
    pub fn has_rfc2833(&self) -> bool {
        self.rfc2833_detector.is_some()
    }

    /// Check if inband detection is enabled
    pub fn has_inband(&self) -> bool {
        self.inband_detector.is_some()
    }

    /// Check if deduplication is enabled
    pub fn has_deduplication(&self) -> bool {
        self.deduplicator.is_some()
    }

    /// Check if buffering is enabled
    pub fn has_buffering(&self) -> bool {
        self.buffer.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DtmfDigit, DtmfEventType, Rfc2833Event};

    #[test]
    fn test_processor_creation() {
        let config = DtmfProcessorConfig::default();
        let processor = DtmfProcessor::new(config);

        assert!(processor.has_rfc2833());
        assert!(processor.has_inband());
        assert!(processor.has_deduplication());
        assert!(!processor.has_buffering());
    }

    #[test]
    fn test_rfc2833_processing() {
        let config = DtmfProcessorConfig {
            enable_rfc2833: true,
            enable_inband: false,
            enable_deduplication: false,
            ..Default::default()
        };
        let mut processor = DtmfProcessor::new(config);

        // Start event
        let start_packet = Rfc2833Event::for_digit(DtmfDigit::Five, false, 0);
        let events = processor
            .process_rfc2833(&start_packet.to_bytes(), 1000)
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].digit, DtmfDigit::Five);
        assert_eq!(events[0].event_type, DtmfEventType::Start);
    }

    #[test]
    fn test_deduplication() {
        let config = DtmfProcessorConfig {
            enable_rfc2833: true,
            enable_deduplication: true,
            ..Default::default()
        };
        let mut processor = DtmfProcessor::new(config);

        // First event should pass through
        let packet1 = Rfc2833Event::for_digit(DtmfDigit::Five, false, 0);
        let events = processor
            .process_rfc2833(&packet1.to_bytes(), 1000)
            .unwrap();
        assert_eq!(events.len(), 1);

        // Continue event (same digit, different duration) - should be seen as duplicate Start
        // since duration > 0 but not end, it becomes Continue
        let packet2 = Rfc2833Event::for_digit(DtmfDigit::Five, false, 160);
        let events = processor
            .process_rfc2833(&packet2.to_bytes(), 1160)
            .unwrap();
        // Continue events are also deduplicated within the window
        assert!(events.is_empty() || events[0].event_type == DtmfEventType::Continue);

        // Different digit should pass through
        let packet3 = Rfc2833Event::for_digit(DtmfDigit::Six, false, 0);
        let events = processor
            .process_rfc2833(&packet3.to_bytes(), 2000)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].digit, DtmfDigit::Six);
    }

    #[test]
    fn test_buffering() {
        let config = DtmfProcessorConfig {
            enable_rfc2833: true,
            enable_deduplication: false,
            enable_buffering: true,
            max_digits: Some(3),
            ..Default::default()
        };
        let mut processor = DtmfProcessor::new(config);

        // Send three digits
        for digit in [DtmfDigit::One, DtmfDigit::Two, DtmfDigit::Three] {
            let packet = Rfc2833Event::for_digit(digit, false, 0);
            processor
                .process_rfc2833(&packet.to_bytes(), 1000)
                .unwrap();
        }

        // Check buffer
        let buffer = processor.buffer().unwrap();
        assert_eq!(buffer.get_digits(), "123");
        assert!(buffer.is_complete()); // Max digits reached
    }

    #[test]
    fn test_reset() {
        let config = DtmfProcessorConfig {
            enable_buffering: true,
            ..Default::default()
        };
        let mut processor = DtmfProcessor::new(config);

        // Add a digit
        let packet = Rfc2833Event::for_digit(DtmfDigit::Five, false, 0);
        processor
            .process_rfc2833(&packet.to_bytes(), 1000)
            .unwrap();

        assert_eq!(processor.buffer().unwrap().len(), 1);

        // Reset
        processor.reset();
        assert_eq!(processor.buffer().unwrap().len(), 0);
    }

    #[test]
    fn test_config_flags() {
        let config = DtmfProcessorConfig {
            enable_rfc2833: false,
            enable_inband: false,
            enable_deduplication: false,
            enable_buffering: false,
            ..Default::default()
        };
        let processor = DtmfProcessor::new(config);

        assert!(!processor.has_rfc2833());
        assert!(!processor.has_inband());
        assert!(!processor.has_deduplication());
        assert!(!processor.has_buffering());
    }
}
