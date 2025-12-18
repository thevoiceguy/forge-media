//! DTMF relay for converting between detection methods
//!
//! Converts DTMF events between different transport methods:
//! - Inband audio → RFC 2833 packets
//! - RFC 2833 packets → Audio tones (requires tone generator)
//!
//! This is useful for scenarios like:
//! - Converting legacy inband DTMF to RFC 2833 for modern systems
//! - Regenerating DTMF tones from RFC 2833 for systems that only support inband
//! - Transcoding between different RTP streams with different DTMF support

use crate::detector::{DtmfDigit, DtmfEvent, DtmfEventType};
use crate::rfc2833::{Rfc2833Event, Rfc2833Generator};
use crate::Result;
use std::collections::HashMap;

/// DTMF relay configuration
#[derive(Debug, Clone)]
pub struct DtmfRelayConfig {
    /// RTP sample rate (e.g., 8000, 16000)
    pub sample_rate: u32,

    /// Packet interval in milliseconds (typically 20ms)
    pub packet_interval_ms: u32,

    /// Generate RFC 2833 from inband events
    pub inband_to_rfc2833: bool,

    /// Generate tone instructions from RFC 2833 events
    pub rfc2833_to_tone: bool,
}

impl Default for DtmfRelayConfig {
    fn default() -> Self {
        Self {
            sample_rate: 8000,
            packet_interval_ms: 20,
            inband_to_rfc2833: true,
            rfc2833_to_tone: false,
        }
    }
}

/// Output from DTMF relay
#[derive(Debug, Clone)]
pub enum DtmfRelayOutput {
    /// RFC 2833 packet to send
    Rfc2833Packet(Rfc2833Event),

    /// Tone generation instruction (frequency pairs and duration)
    ToneInstruction {
        digit: DtmfDigit,
        low_freq: u32,
        high_freq: u32,
        duration_ms: u32,
        end: bool,
    },

    /// No output (event not relayed)
    None,
}

/// Active digit state for relay
struct ActiveDigitState {
    generator: Rfc2833Generator,
}

/// DTMF relay for converting between detection methods
///
/// # Example
///
/// ```rust,ignore
/// use forge_dtmf::{DtmfRelay, DtmfRelayConfig, DtmfEvent, DtmfDigit, DtmfEventType, DtmfMethod};
///
/// let config = DtmfRelayConfig::default();
/// let mut relay = DtmfRelay::new(config);
///
/// // Convert inband detection to RFC 2833
/// let inband_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
/// let outputs = relay.process_event(inband_event)?;
///
/// for output in outputs {
///     match output {
///         DtmfRelayOutput::Rfc2833Packet(packet) => {
///             // Send as RTP payload type 101
///             send_rtp_packet(101, packet.to_bytes());
///         }
///         _ => {}
///     }
/// }
/// ```
pub struct DtmfRelay {
    config: DtmfRelayConfig,

    /// Active digit states (key: digit, value: generator)
    active_digits: HashMap<DtmfDigit, ActiveDigitState>,
}

impl DtmfRelay {
    /// Create a new DTMF relay
    pub fn new(config: DtmfRelayConfig) -> Self {
        Self {
            config,
            active_digits: HashMap::new(),
        }
    }

    /// Process a DTMF event and generate relay outputs
    ///
    /// Returns a vector of relay outputs (may be empty if event is not relayed)
    pub fn process_event(&mut self, event: DtmfEvent) -> Result<Vec<DtmfRelayOutput>> {
        match event.event_type {
            DtmfEventType::Start => self.handle_start(event),
            DtmfEventType::Continue => self.handle_continue(event),
            DtmfEventType::End => self.handle_end(event),
        }
    }

    /// Handle digit start event
    fn handle_start(&mut self, event: DtmfEvent) -> Result<Vec<DtmfRelayOutput>> {
        let mut outputs = Vec::new();

        // Inband → RFC 2833
        if self.config.inband_to_rfc2833 && event.method == crate::DtmfMethod::Inband {
            // Create generator for this digit
            let mut generator =
                Rfc2833Generator::new(self.config.sample_rate, self.config.packet_interval_ms);

            let start_packet = generator.start_digit(event.digit);
            outputs.push(DtmfRelayOutput::Rfc2833Packet(start_packet));

            // Store generator for continuation
            self.active_digits
                .insert(event.digit, ActiveDigitState { generator });
        }

        // RFC 2833 → Tone
        if self.config.rfc2833_to_tone && event.method == crate::DtmfMethod::Rfc2833 {
            let (low_freq, high_freq) = event.digit.frequencies();
            outputs.push(DtmfRelayOutput::ToneInstruction {
                digit: event.digit,
                low_freq,
                high_freq,
                duration_ms: 0, // Start of tone
                end: false,
            });
        }

        Ok(outputs)
    }

    /// Handle digit continuation event
    fn handle_continue(&mut self, event: DtmfEvent) -> Result<Vec<DtmfRelayOutput>> {
        let mut outputs = Vec::new();

        // Inband → RFC 2833
        if self.config.inband_to_rfc2833 && event.method == crate::DtmfMethod::Inband {
            if let Some(state) = self.active_digits.get_mut(&event.digit) {
                if let Some(cont_packet) = state.generator.continue_digit() {
                    outputs.push(DtmfRelayOutput::Rfc2833Packet(cont_packet));
                }
            }
        }

        // RFC 2833 → Tone (continuation doesn't need new instructions)
        // The tone generator would continue the tone based on start instruction

        Ok(outputs)
    }

    /// Handle digit end event
    fn handle_end(&mut self, event: DtmfEvent) -> Result<Vec<DtmfRelayOutput>> {
        let mut outputs = Vec::new();

        // Inband → RFC 2833
        if self.config.inband_to_rfc2833 && event.method == crate::DtmfMethod::Inband {
            if let Some(mut state) = self.active_digits.remove(&event.digit) {
                if let Some(end_packets) = state.generator.end_digit() {
                    for packet in end_packets {
                        outputs.push(DtmfRelayOutput::Rfc2833Packet(packet));
                    }
                }
            }
        }

        // RFC 2833 → Tone
        if self.config.rfc2833_to_tone && event.method == crate::DtmfMethod::Rfc2833 {
            let (low_freq, high_freq) = event.digit.frequencies();
            let duration = event.duration_ms.unwrap_or(0);
            outputs.push(DtmfRelayOutput::ToneInstruction {
                digit: event.digit,
                low_freq,
                high_freq,
                duration_ms: duration,
                end: true,
            });
        }

        Ok(outputs)
    }

    /// Get current number of active digits being relayed
    pub fn active_count(&self) -> usize {
        self.active_digits.len()
    }

    /// Reset relay state
    pub fn reset(&mut self) {
        self.active_digits.clear();
    }

    /// Get current sample rate
    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DtmfMethod;

    #[test]
    fn test_relay_creation() {
        let config = DtmfRelayConfig::default();
        let relay = DtmfRelay::new(config);
        assert_eq!(relay.active_count(), 0);
    }

    #[test]
    fn test_inband_to_rfc2833() {
        let config = DtmfRelayConfig {
            inband_to_rfc2833: true,
            rfc2833_to_tone: false,
            ..Default::default()
        };
        let mut relay = DtmfRelay::new(config);

        // Start event
        let start_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        let outputs = relay.process_event(start_event).unwrap();
        assert_eq!(outputs.len(), 1);

        match &outputs[0] {
            DtmfRelayOutput::Rfc2833Packet(packet) => {
                assert_eq!(packet.digit(), Some(DtmfDigit::Five));
                assert!(!packet.is_end());
                assert_eq!(packet.duration(), 0);
            }
            _ => panic!("Expected RFC2833 packet"),
        }

        // Active digit should be tracked
        assert_eq!(relay.active_count(), 1);

        // Continue event
        let cont_event =
            DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Continue, DtmfMethod::Inband);
        let outputs = relay.process_event(cont_event).unwrap();
        assert_eq!(outputs.len(), 1);

        match &outputs[0] {
            DtmfRelayOutput::Rfc2833Packet(packet) => {
                assert_eq!(packet.digit(), Some(DtmfDigit::Five));
                assert!(!packet.is_end());
                assert!(packet.duration() > 0);
            }
            _ => panic!("Expected RFC2833 packet"),
        }

        // End event
        let end_event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::End, DtmfMethod::Inband);
        let outputs = relay.process_event(end_event).unwrap();
        assert_eq!(outputs.len(), 3); // 3 copies as per RFC

        for output in outputs {
            match output {
                DtmfRelayOutput::Rfc2833Packet(packet) => {
                    assert_eq!(packet.digit(), Some(DtmfDigit::Five));
                    assert!(packet.is_end());
                }
                _ => panic!("Expected RFC2833 packet"),
            }
        }

        // Active digit should be cleared
        assert_eq!(relay.active_count(), 0);
    }

    #[test]
    fn test_rfc2833_to_tone() {
        let config = DtmfRelayConfig {
            inband_to_rfc2833: false,
            rfc2833_to_tone: true,
            ..Default::default()
        };
        let mut relay = DtmfRelay::new(config);

        // Start event
        let start_event =
            DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Rfc2833);
        let outputs = relay.process_event(start_event).unwrap();
        assert_eq!(outputs.len(), 1);

        match &outputs[0] {
            DtmfRelayOutput::ToneInstruction {
                digit,
                low_freq,
                high_freq,
                duration_ms,
                end,
            } => {
                assert_eq!(*digit, DtmfDigit::Five);
                assert_eq!(*low_freq, 770);
                assert_eq!(*high_freq, 1336);
                assert_eq!(*duration_ms, 0);
                assert!(!end);
            }
            _ => panic!("Expected tone instruction"),
        }

        // End event
        let end_event = DtmfEvent::with_duration(
            DtmfDigit::Five,
            DtmfEventType::End,
            DtmfMethod::Rfc2833,
            120,
        );
        let outputs = relay.process_event(end_event).unwrap();
        assert_eq!(outputs.len(), 1);

        match &outputs[0] {
            DtmfRelayOutput::ToneInstruction {
                digit,
                duration_ms,
                end,
                ..
            } => {
                assert_eq!(*digit, DtmfDigit::Five);
                assert_eq!(*duration_ms, 120);
                assert!(end);
            }
            _ => panic!("Expected tone instruction"),
        }
    }

    #[test]
    fn test_multiple_digits() {
        let config = DtmfRelayConfig {
            inband_to_rfc2833: true,
            ..Default::default()
        };
        let mut relay = DtmfRelay::new(config);

        // Start first digit
        let event1 = DtmfEvent::new(DtmfDigit::One, DtmfEventType::Start, DtmfMethod::Inband);
        relay.process_event(event1).unwrap();

        // Start second digit
        let event2 = DtmfEvent::new(DtmfDigit::Two, DtmfEventType::Start, DtmfMethod::Inband);
        relay.process_event(event2).unwrap();

        assert_eq!(relay.active_count(), 2);

        // End first digit
        let event3 = DtmfEvent::new(DtmfDigit::One, DtmfEventType::End, DtmfMethod::Inband);
        relay.process_event(event3).unwrap();

        assert_eq!(relay.active_count(), 1);

        // End second digit
        let event4 = DtmfEvent::new(DtmfDigit::Two, DtmfEventType::End, DtmfMethod::Inband);
        relay.process_event(event4).unwrap();

        assert_eq!(relay.active_count(), 0);
    }

    #[test]
    fn test_reset() {
        let config = DtmfRelayConfig::default();
        let mut relay = DtmfRelay::new(config);

        let event = DtmfEvent::new(DtmfDigit::Five, DtmfEventType::Start, DtmfMethod::Inband);
        relay.process_event(event).unwrap();
        assert_eq!(relay.active_count(), 1);

        relay.reset();
        assert_eq!(relay.active_count(), 0);
    }
}
