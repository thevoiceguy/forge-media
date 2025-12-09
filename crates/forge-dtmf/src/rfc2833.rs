//! RFC 2833 (telephone-event) DTMF implementation
//!
//! RFC 2833 defines RTP payload format for carrying DTMF digits, hookflash,
//! and other telephony events as RTP packets.
//!
//! Packet format (4 bytes):
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |     event     |E|R| volume    |          duration             |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! - event: 0-15 for DTMF digits (0-9, *, #, A-D)
//! - E bit: End of event marker
//! - R bit: Reserved (must be 0)
//! - volume: Power level (0-63 dBm, default 10)
//! - duration: Duration in timestamp units

use crate::detector::{DtmfDetector, DtmfDigit, DtmfEvent, DtmfEventType, DtmfMethod};
use crate::{DtmfError, Result};

/// RFC 2833 telephone-event packet
#[derive(Debug, Clone, PartialEq)]
pub struct Rfc2833Event {
    /// Event code (0-15 for DTMF)
    event: u8,
    /// End of event flag
    end: bool,
    /// Volume/power level (0-63 dBm)
    volume: u8,
    /// Duration in RTP timestamp units
    duration: u16,
}

impl Rfc2833Event {
    /// Minimum packet size (4 bytes)
    pub const MIN_SIZE: usize = 4;

    /// Default volume level (10 dBm)
    pub const DEFAULT_VOLUME: u8 = 10;

    /// Create a new RFC 2833 event
    pub fn new(event: u8, end: bool, volume: u8, duration: u16) -> Result<Self> {
        if event > 15 {
            return Err(DtmfError::InvalidRfc2833(format!(
                "Event code must be 0-15, got {}",
                event
            )));
        }
        if volume > 63 {
            return Err(DtmfError::InvalidRfc2833(format!(
                "Volume must be 0-63, got {}",
                volume
            )));
        }

        Ok(Self {
            event,
            end,
            volume,
            duration,
        })
    }

    /// Create event for DTMF digit
    pub fn for_digit(digit: DtmfDigit, end: bool, duration: u16) -> Self {
        Self {
            event: digit.to_event_code(),
            end,
            volume: Self::DEFAULT_VOLUME,
            duration,
        }
    }

    /// Parse from RTP payload bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < Self::MIN_SIZE {
            return Err(DtmfError::InvalidRfc2833(format!(
                "Payload too short: {} bytes (need at least {})",
                data.len(),
                Self::MIN_SIZE
            )));
        }

        let event = data[0];
        let flags_volume = data[1];
        let end = (flags_volume & 0x80) != 0;
        let volume = flags_volume & 0x3F; // Lower 6 bits
        let duration = u16::from_be_bytes([data[2], data[3]]);

        Self::new(event, end, volume, duration)
    }

    /// Serialize to bytes (4 bytes)
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(4);
        bytes.push(self.event);

        let mut flags_volume = self.volume & 0x3F; // Lower 6 bits
        if self.end {
            flags_volume |= 0x80; // Set end bit
        }
        bytes.push(flags_volume);

        bytes.extend_from_slice(&self.duration.to_be_bytes());
        bytes
    }

    /// Get the DTMF digit if this is a DTMF event (0-15)
    pub fn digit(&self) -> Option<DtmfDigit> {
        DtmfDigit::from_event_code(self.event).ok()
    }

    /// Get event code
    pub fn event(&self) -> u8 {
        self.event
    }

    /// Check if this is the end of the event
    pub fn is_end(&self) -> bool {
        self.end
    }

    /// Get volume level (0-63 dBm)
    pub fn volume(&self) -> u8 {
        self.volume
    }

    /// Get duration in RTP timestamp units
    pub fn duration(&self) -> u16 {
        self.duration
    }

    /// Convert duration to milliseconds for a given sample rate
    pub fn duration_ms(&self, sample_rate: u32) -> u32 {
        (self.duration as u32 * 1000) / sample_rate
    }

    /// Convert to DtmfEvent
    pub fn to_dtmf_event(&self, sample_rate: u32, timestamp: u32) -> Option<DtmfEvent> {
        self.digit().map(|digit| {
            let event_type = if self.end {
                DtmfEventType::End
            } else if self.duration == 0 {
                DtmfEventType::Start
            } else {
                DtmfEventType::Continue
            };

            let mut event = DtmfEvent::new(digit, event_type, DtmfMethod::Rfc2833);
            event.duration_ms = Some(self.duration_ms(sample_rate));
            event.timestamp = Some(timestamp);
            event
        })
    }
}

/// RFC 2833 event generator for creating telephone-event packets
pub struct Rfc2833Generator {
    /// Current event being generated
    current_digit: Option<DtmfDigit>,
    /// Current duration
    current_duration: u16,
    /// Sample rate for timestamp calculations
    sample_rate: u32,
    /// Packet interval in samples (typically 20ms worth)
    packet_interval: u16,
}

impl Rfc2833Generator {
    /// Create a new generator
    ///
    /// # Arguments
    /// * `sample_rate` - RTP sample rate (e.g., 8000 for G.711)
    /// * `packet_interval_ms` - Interval between packets in milliseconds (typically 20ms)
    pub fn new(sample_rate: u32, packet_interval_ms: u32) -> Self {
        let packet_interval = ((sample_rate * packet_interval_ms) / 1000) as u16;
        Self {
            current_digit: None,
            current_duration: 0,
            sample_rate,
            packet_interval,
        }
    }

    /// Start generating a new digit
    ///
    /// Returns the initial RFC 2833 event packet
    pub fn start_digit(&mut self, digit: DtmfDigit) -> Rfc2833Event {
        self.current_digit = Some(digit);
        self.current_duration = 0;
        Rfc2833Event::for_digit(digit, false, 0)
    }

    /// Continue generating the current digit
    ///
    /// Returns the continuation packet or None if no digit is active
    pub fn continue_digit(&mut self) -> Option<Rfc2833Event> {
        self.current_digit.map(|digit| {
            self.current_duration += self.packet_interval;
            Rfc2833Event::for_digit(digit, false, self.current_duration)
        })
    }

    /// End the current digit
    ///
    /// Returns the final packet (3 copies as per RFC) or None if no digit is active
    pub fn end_digit(&mut self) -> Option<Vec<Rfc2833Event>> {
        self.current_digit.take().map(|digit| {
            self.current_duration += self.packet_interval;
            let end_event = Rfc2833Event::for_digit(digit, true, self.current_duration);

            // RFC 2833 recommends sending final packet 3 times for reliability
            vec![end_event.clone(), end_event.clone(), end_event]
        })
    }

    /// Get current digit duration in milliseconds
    pub fn current_duration_ms(&self) -> u32 {
        (self.current_duration as u32 * 1000) / self.sample_rate
    }

    /// Check if currently generating a digit
    pub fn is_active(&self) -> bool {
        self.current_digit.is_some()
    }

    /// Reset generator state
    pub fn reset(&mut self) {
        self.current_digit = None;
        self.current_duration = 0;
    }
}

/// RFC 2833 detector for processing telephone-event RTP payloads
pub struct Rfc2833Detector {
    sample_rate: u32,
    last_event: Option<(DtmfDigit, u16)>, // (digit, duration)
}

impl Rfc2833Detector {
    /// Create a new RFC 2833 detector
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            last_event: None,
        }
    }

    /// Process RFC 2833 payload with RTP timestamp
    pub fn process_with_timestamp(&mut self, data: &[u8], timestamp: u32) -> Result<Vec<DtmfEvent>> {
        let event = Rfc2833Event::from_bytes(data)?;

        let mut events = Vec::new();

        if let Some(digit) = event.digit() {
            // Check if this is a new event
            let is_new = self.last_event.map_or(true, |(last_digit, _)| last_digit != digit);

            if is_new && event.duration() == 0 {
                // Start of new event
                events.push(DtmfEvent::with_timestamp(
                    digit,
                    DtmfEventType::Start,
                    DtmfMethod::Rfc2833,
                    timestamp,
                ));
            } else if event.is_end() {
                // End of event
                let mut end_event = DtmfEvent::with_timestamp(
                    digit,
                    DtmfEventType::End,
                    DtmfMethod::Rfc2833,
                    timestamp,
                );
                end_event.duration_ms = Some(event.duration_ms(self.sample_rate));
                events.push(end_event);
                self.last_event = None;
            } else {
                // Continuation
                events.push(DtmfEvent::with_timestamp(
                    digit,
                    DtmfEventType::Continue,
                    DtmfMethod::Rfc2833,
                    timestamp,
                ));
            }

            if !event.is_end() {
                self.last_event = Some((digit, event.duration()));
            }
        }

        Ok(events)
    }
}

impl DtmfDetector for Rfc2833Detector {
    fn process(&mut self, data: &[u8]) -> Result<Vec<DtmfEvent>> {
        self.process_with_timestamp(data, 0)
    }

    fn reset(&mut self) {
        self.last_event = None;
    }

    fn method(&self) -> DtmfMethod {
        DtmfMethod::Rfc2833
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfc2833_event_parsing() {
        // Event: 5, End: false, Volume: 10, Duration: 160
        let data = vec![0x05, 0x0A, 0x00, 0xA0];
        let event = Rfc2833Event::from_bytes(&data).unwrap();

        assert_eq!(event.event(), 5);
        assert_eq!(event.digit(), Some(DtmfDigit::Five));
        assert!(!event.is_end());
        assert_eq!(event.volume(), 10);
        assert_eq!(event.duration(), 160);
    }

    #[test]
    fn test_rfc2833_event_end_bit() {
        // Event: 5, End: true, Volume: 10, Duration: 960
        let data = vec![0x05, 0x8A, 0x03, 0xC0];
        let event = Rfc2833Event::from_bytes(&data).unwrap();

        assert_eq!(event.digit(), Some(DtmfDigit::Five));
        assert!(event.is_end());
        assert_eq!(event.duration(), 960);
    }

    #[test]
    fn test_rfc2833_event_serialization() {
        let event = Rfc2833Event::for_digit(DtmfDigit::Five, false, 160);
        let bytes = event.to_bytes();

        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes[0], 5); // Event code for '5'
        assert_eq!(bytes[1], 10); // Default volume, no end bit
        assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), 160);

        // Round-trip test
        let parsed = Rfc2833Event::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn test_rfc2833_generator() {
        let mut gen = Rfc2833Generator::new(8000, 20);

        // Start digit
        let start = gen.start_digit(DtmfDigit::Five);
        assert_eq!(start.digit(), Some(DtmfDigit::Five));
        assert!(!start.is_end());
        assert_eq!(start.duration(), 0);
        assert!(gen.is_active());

        // Continue digit
        let cont = gen.continue_digit().unwrap();
        assert_eq!(cont.digit(), Some(DtmfDigit::Five));
        assert!(!cont.is_end());
        assert_eq!(cont.duration(), 160); // 20ms at 8kHz

        // End digit
        let end_packets = gen.end_digit().unwrap();
        assert_eq!(end_packets.len(), 3); // 3 copies as per RFC
        assert!(end_packets[0].is_end());
        assert!(!gen.is_active());
    }

    #[test]
    fn test_rfc2833_detector() {
        let mut detector = Rfc2833Detector::new(8000);

        // Start event
        let start_data = vec![0x05, 0x0A, 0x00, 0x00];
        let events = detector.process_with_timestamp(&start_data, 1000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].digit, DtmfDigit::Five);
        assert_eq!(events[0].event_type, DtmfEventType::Start);

        // Continue event
        let cont_data = vec![0x05, 0x0A, 0x00, 0xA0];
        let events = detector.process_with_timestamp(&cont_data, 1160).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, DtmfEventType::Continue);

        // End event
        let end_data = vec![0x05, 0x8A, 0x03, 0xC0];
        let events = detector.process_with_timestamp(&end_data, 1960).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, DtmfEventType::End);
        assert_eq!(events[0].duration_ms, Some(120)); // 960 samples at 8kHz = 120ms
    }

    #[test]
    fn test_duration_conversion() {
        let event = Rfc2833Event::for_digit(DtmfDigit::Five, false, 800);
        assert_eq!(event.duration_ms(8000), 100); // 800 samples at 8kHz = 100ms
        assert_eq!(event.duration_ms(16000), 50); // 800 samples at 16kHz = 50ms
    }

    #[test]
    fn test_all_dtmf_digits() {
        for digit in [
            DtmfDigit::Zero, DtmfDigit::One, DtmfDigit::Two, DtmfDigit::Three,
            DtmfDigit::Four, DtmfDigit::Five, DtmfDigit::Six, DtmfDigit::Seven,
            DtmfDigit::Eight, DtmfDigit::Nine, DtmfDigit::Star, DtmfDigit::Hash,
            DtmfDigit::A, DtmfDigit::B, DtmfDigit::C, DtmfDigit::D,
        ] {
            let event = Rfc2833Event::for_digit(digit, false, 160);
            assert_eq!(event.digit(), Some(digit));

            // Round-trip
            let bytes = event.to_bytes();
            let parsed = Rfc2833Event::from_bytes(&bytes).unwrap();
            assert_eq!(parsed.digit(), Some(digit));
        }
    }
}
