//! DTMF (Dual-Tone Multi-Frequency) detection and generation
//!
//! This crate provides comprehensive DTMF support for VoIP systems:
//! - RFC 2833 (telephone-event) - RTP-based DTMF
//! - Inband DTMF detection using Goertzel algorithm
//! - Event notification system
//!
//! # Examples
//!
//! ## RFC 2833 Parsing
//! ```rust,no_run
//! use forge_dtmf::{Rfc2833Event, DtmfDigit};
//!
//! let rtp_payload = vec![0x05, 0x0A, 0x00, 0xA0]; // Digit '5'
//! let event = Rfc2833Event::from_bytes(&rtp_payload).unwrap();
//! assert_eq!(event.digit(), Some(DtmfDigit::Five));
//! ```

pub mod buffer;
pub mod dedup;
pub mod detector;
pub mod inband;
pub mod processor;
pub mod relay;
pub mod rfc2833;

pub use buffer::DtmfBuffer;
pub use dedup::DtmfDeduplicator;
pub use detector::{DtmfDetector, DtmfDigit, DtmfEvent, DtmfEventType, DtmfMethod};
pub use inband::{GoertzelDetector, InbandDetector};
pub use processor::{DtmfProcessor, DtmfProcessorConfig};
pub use relay::{DtmfRelay, DtmfRelayConfig, DtmfRelayOutput};
pub use rfc2833::{Rfc2833Detector, Rfc2833Event, Rfc2833Generator};

use thiserror::Error;

/// DTMF error types
#[derive(Error, Debug)]
pub enum DtmfError {
    #[error("Invalid DTMF digit: {0}")]
    InvalidDigit(String),

    #[error("Invalid RFC 2833 payload: {0}")]
    InvalidRfc2833(String),

    #[error("Invalid audio format: {0}")]
    InvalidAudioFormat(String),

    #[error("Detection error: {0}")]
    DetectionError(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for DTMF operations
pub type Result<T> = std::result::Result<T, DtmfError>;

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_complete_dtmf_flow_with_processor() {
        // Create processor with all features enabled
        let config = DtmfProcessorConfig {
            enable_rfc2833: true,
            enable_inband: false, // Skip inband for this test
            enable_deduplication: true,
            enable_buffering: true,
            max_digits: Some(4),
            ..Default::default()
        };

        let mut processor = DtmfProcessor::new(config);

        // Simulate collecting a 4-digit PIN
        let digits = vec![
            DtmfDigit::One,
            DtmfDigit::Two,
            DtmfDigit::Three,
            DtmfDigit::Four,
        ];

        for (i, digit) in digits.iter().enumerate() {
            let timestamp = 1000 + (i as u32 * 160);

            // Start event
            let start_packet = Rfc2833Event::for_digit(*digit, false, 0);
            let events = processor
                .process_rfc2833(&start_packet.to_bytes(), timestamp)
                .unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].digit, *digit);

            // Continue event
            let cont_packet = Rfc2833Event::for_digit(*digit, false, 160);
            processor
                .process_rfc2833(&cont_packet.to_bytes(), timestamp + 160)
                .unwrap();

            // End event
            let end_packet = Rfc2833Event::for_digit(*digit, true, 320);
            processor
                .process_rfc2833(&end_packet.to_bytes(), timestamp + 320)
                .unwrap();
        }

        // Check buffer collected all digits
        let buffer = processor.buffer().unwrap();
        assert_eq!(buffer.get_digits(), "1234");
        assert!(buffer.is_complete()); // Max digits reached
    }

    #[test]
    fn test_dtmf_relay_inband_to_rfc2833() {
        let relay_config = DtmfRelayConfig {
            inband_to_rfc2833: true,
            rfc2833_to_tone: false,
            sample_rate: 8000,
            packet_interval_ms: 20,
        };

        let mut relay = DtmfRelay::new(relay_config);

        // Simulate inband detection of digit 5
        let start_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        let outputs = relay.process_event(start_event).unwrap();

        assert_eq!(outputs.len(), 1);
        match &outputs[0] {
            DtmfRelayOutput::Rfc2833Packet(packet) => {
                assert_eq!(packet.digit(), Some(DtmfDigit::Five));
                assert!(!packet.is_end());
            }
            _ => panic!("Expected RFC2833 packet"),
        }

        // Continue
        let cont_event =
            DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Continue, DtmfMethod::Inband);
        let outputs = relay.process_event(cont_event).unwrap();
        assert_eq!(outputs.len(), 1);

        // End
        let end_event =
            DtmfEvent::with_duration(DtmfDigit::Five, DtmfEventType::End, DtmfMethod::Inband, 120);
        let outputs = relay.process_event(end_event).unwrap();
        assert_eq!(outputs.len(), 3); // 3 copies as per RFC
    }

    #[test]
    fn test_deduplicator_with_multiple_methods() {
        let mut dedup = DtmfDeduplicator::new();

        // Inband detection arrives first
        let inband_event =
            DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        assert!(dedup.should_publish(&inband_event));

        // RFC 2833 for same digit arrives shortly after - should replace and publish
        let rfc_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        assert!(dedup.should_publish(&rfc_event)); // Higher priority replaces

        // Another inband should be suppressed
        let inband_event2 =
            DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        assert!(!dedup.should_publish(&inband_event2)); // Suppressed
    }

    #[test]
    fn test_digit_buffer_with_terminator() {
        let mut buffer = DtmfBuffer::new().with_terminators(vec![DtmfDigit::Hash]);

        // Collect digits until #
        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        buffer.push(DtmfDigit::Three);

        let complete = buffer.push(DtmfDigit::Hash); // Terminator

        assert!(complete);
        assert_eq!(buffer.get_digits(), "123"); // Hash not included
    }

    #[test]
    fn test_all_16_dtmf_digits() {
        let all_digits = vec![
            DtmfDigit::Zero,
            DtmfDigit::One,
            DtmfDigit::Two,
            DtmfDigit::Three,
            DtmfDigit::Four,
            DtmfDigit::Five,
            DtmfDigit::Six,
            DtmfDigit::Seven,
            DtmfDigit::Eight,
            DtmfDigit::Nine,
            DtmfDigit::Star,
            DtmfDigit::Hash,
            DtmfDigit::A,
            DtmfDigit::B,
            DtmfDigit::C,
            DtmfDigit::D,
        ];

        for digit in all_digits {
            // Test RFC 2833 encoding
            let event = Rfc2833Event::for_digit(digit, false, 160);
            let bytes = event.to_bytes();
            let parsed = Rfc2833Event::from_bytes(&bytes).unwrap();
            assert_eq!(parsed.digit(), Some(digit));

            // Test frequencies
            let (low, high) = digit.frequencies();
            assert!(low >= 697 && low <= 941);
            assert!(high >= 1209 && high <= 1633);

            // Test event code conversion
            let code = digit.to_event_code();
            let back = DtmfDigit::from_event_code(code).unwrap();
            assert_eq!(back, digit);
        }
    }
}
