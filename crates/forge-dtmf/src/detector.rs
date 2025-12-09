//! Core DTMF detection types and unified detector interface

use crate::{DtmfError, Result};
use std::fmt;

/// DTMF digit representation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DtmfDigit {
    Zero,
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Star,      // *
    Hash,      // #
    A,         // Extended digit A
    B,         // Extended digit B
    C,         // Extended digit C
    D,         // Extended digit D
}

impl DtmfDigit {
    /// Convert RFC 2833 event code (0-15) to DTMF digit
    pub fn from_event_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(DtmfDigit::Zero),
            1 => Ok(DtmfDigit::One),
            2 => Ok(DtmfDigit::Two),
            3 => Ok(DtmfDigit::Three),
            4 => Ok(DtmfDigit::Four),
            5 => Ok(DtmfDigit::Five),
            6 => Ok(DtmfDigit::Six),
            7 => Ok(DtmfDigit::Seven),
            8 => Ok(DtmfDigit::Eight),
            9 => Ok(DtmfDigit::Nine),
            10 => Ok(DtmfDigit::Star),
            11 => Ok(DtmfDigit::Hash),
            12 => Ok(DtmfDigit::A),
            13 => Ok(DtmfDigit::B),
            14 => Ok(DtmfDigit::C),
            15 => Ok(DtmfDigit::D),
            _ => Err(DtmfError::InvalidDigit(format!("Invalid event code: {}", code))),
        }
    }

    /// Convert DTMF digit to RFC 2833 event code (0-15)
    pub fn to_event_code(&self) -> u8 {
        match self {
            DtmfDigit::Zero => 0,
            DtmfDigit::One => 1,
            DtmfDigit::Two => 2,
            DtmfDigit::Three => 3,
            DtmfDigit::Four => 4,
            DtmfDigit::Five => 5,
            DtmfDigit::Six => 6,
            DtmfDigit::Seven => 7,
            DtmfDigit::Eight => 8,
            DtmfDigit::Nine => 9,
            DtmfDigit::Star => 10,
            DtmfDigit::Hash => 11,
            DtmfDigit::A => 12,
            DtmfDigit::B => 13,
            DtmfDigit::C => 14,
            DtmfDigit::D => 15,
        }
    }

    /// Get the low and high frequencies for this DTMF digit (in Hz)
    pub fn frequencies(&self) -> (u32, u32) {
        match self {
            DtmfDigit::One => (697, 1209),
            DtmfDigit::Two => (697, 1336),
            DtmfDigit::Three => (697, 1477),
            DtmfDigit::A => (697, 1633),
            DtmfDigit::Four => (770, 1209),
            DtmfDigit::Five => (770, 1336),
            DtmfDigit::Six => (770, 1477),
            DtmfDigit::B => (770, 1633),
            DtmfDigit::Seven => (852, 1209),
            DtmfDigit::Eight => (852, 1336),
            DtmfDigit::Nine => (852, 1477),
            DtmfDigit::C => (852, 1633),
            DtmfDigit::Star => (941, 1209),
            DtmfDigit::Zero => (941, 1336),
            DtmfDigit::Hash => (941, 1477),
            DtmfDigit::D => (941, 1633),
        }
    }

    /// Parse character to DTMF digit
    pub fn from_char(c: char) -> Result<Self> {
        match c {
            '0' => Ok(DtmfDigit::Zero),
            '1' => Ok(DtmfDigit::One),
            '2' => Ok(DtmfDigit::Two),
            '3' => Ok(DtmfDigit::Three),
            '4' => Ok(DtmfDigit::Four),
            '5' => Ok(DtmfDigit::Five),
            '6' => Ok(DtmfDigit::Six),
            '7' => Ok(DtmfDigit::Seven),
            '8' => Ok(DtmfDigit::Eight),
            '9' => Ok(DtmfDigit::Nine),
            '*' => Ok(DtmfDigit::Star),
            '#' => Ok(DtmfDigit::Hash),
            'A' | 'a' => Ok(DtmfDigit::A),
            'B' | 'b' => Ok(DtmfDigit::B),
            'C' | 'c' => Ok(DtmfDigit::C),
            'D' | 'd' => Ok(DtmfDigit::D),
            _ => Err(DtmfError::InvalidDigit(format!("Invalid character: {}", c))),
        }
    }
}

impl fmt::Display for DtmfDigit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = match self {
            DtmfDigit::Zero => '0',
            DtmfDigit::One => '1',
            DtmfDigit::Two => '2',
            DtmfDigit::Three => '3',
            DtmfDigit::Four => '4',
            DtmfDigit::Five => '5',
            DtmfDigit::Six => '6',
            DtmfDigit::Seven => '7',
            DtmfDigit::Eight => '8',
            DtmfDigit::Nine => '9',
            DtmfDigit::Star => '*',
            DtmfDigit::Hash => '#',
            DtmfDigit::A => 'A',
            DtmfDigit::B => 'B',
            DtmfDigit::C => 'C',
            DtmfDigit::D => 'D',
        };
        write!(f, "{}", c)
    }
}

/// DTMF event type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtmfEventType {
    /// Digit press started
    Start,
    /// Digit press continues
    Continue,
    /// Digit press ended
    End,
}

/// DTMF event with timing information
#[derive(Debug, Clone)]
pub struct DtmfEvent {
    /// The detected digit
    pub digit: DtmfDigit,
    /// Event type (start, continue, end)
    pub event_type: DtmfEventType,
    /// Duration in milliseconds (for End events)
    pub duration_ms: Option<u32>,
    /// Detection method
    pub method: DtmfMethod,
    /// RTP timestamp (if from RFC 2833)
    pub timestamp: Option<u32>,
}

/// DTMF detection method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtmfMethod {
    /// RFC 2833 (telephone-event)
    Rfc2833,
    /// Inband audio detection
    Inband,
    /// SIP INFO message
    SipInfo,
}

impl DtmfEvent {
    /// Create a new DTMF event
    pub fn new(digit: DtmfDigit, event_type: DtmfEventType, method: DtmfMethod) -> Self {
        Self {
            digit,
            event_type,
            duration_ms: None,
            method,
            timestamp: None,
        }
    }

    /// Create a new event with duration
    pub fn with_duration(
        digit: DtmfDigit,
        event_type: DtmfEventType,
        method: DtmfMethod,
        duration_ms: u32,
    ) -> Self {
        Self {
            digit,
            event_type,
            duration_ms: Some(duration_ms),
            method,
            timestamp: None,
        }
    }

    /// Create a new event with timestamp
    pub fn with_timestamp(
        digit: DtmfDigit,
        event_type: DtmfEventType,
        method: DtmfMethod,
        timestamp: u32,
    ) -> Self {
        Self {
            digit,
            event_type,
            duration_ms: None,
            method,
            timestamp: Some(timestamp),
        }
    }
}

/// Unified DTMF detector interface
pub trait DtmfDetector {
    /// Process audio samples or RTP payload and return detected events
    fn process(&mut self, data: &[u8]) -> Result<Vec<DtmfEvent>>;

    /// Reset detector state
    fn reset(&mut self);

    /// Get the detection method this detector uses
    fn method(&self) -> DtmfMethod;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dtmf_digit_event_code() {
        assert_eq!(DtmfDigit::Zero.to_event_code(), 0);
        assert_eq!(DtmfDigit::Five.to_event_code(), 5);
        assert_eq!(DtmfDigit::Star.to_event_code(), 10);
        assert_eq!(DtmfDigit::Hash.to_event_code(), 11);
        assert_eq!(DtmfDigit::A.to_event_code(), 12);

        assert_eq!(DtmfDigit::from_event_code(0).unwrap(), DtmfDigit::Zero);
        assert_eq!(DtmfDigit::from_event_code(5).unwrap(), DtmfDigit::Five);
        assert_eq!(DtmfDigit::from_event_code(10).unwrap(), DtmfDigit::Star);
        assert_eq!(DtmfDigit::from_event_code(15).unwrap(), DtmfDigit::D);
    }

    #[test]
    fn test_dtmf_digit_from_char() {
        assert_eq!(DtmfDigit::from_char('0').unwrap(), DtmfDigit::Zero);
        assert_eq!(DtmfDigit::from_char('9').unwrap(), DtmfDigit::Nine);
        assert_eq!(DtmfDigit::from_char('*').unwrap(), DtmfDigit::Star);
        assert_eq!(DtmfDigit::from_char('#').unwrap(), DtmfDigit::Hash);
        assert_eq!(DtmfDigit::from_char('A').unwrap(), DtmfDigit::A);
        assert_eq!(DtmfDigit::from_char('a').unwrap(), DtmfDigit::A);
        assert!(DtmfDigit::from_char('X').is_err());
    }

    #[test]
    fn test_dtmf_digit_frequencies() {
        assert_eq!(DtmfDigit::One.frequencies(), (697, 1209));
        assert_eq!(DtmfDigit::Five.frequencies(), (770, 1336));
        assert_eq!(DtmfDigit::Zero.frequencies(), (941, 1336));
        assert_eq!(DtmfDigit::Star.frequencies(), (941, 1209));
        assert_eq!(DtmfDigit::Hash.frequencies(), (941, 1477));
    }

    #[test]
    fn test_dtmf_digit_display() {
        assert_eq!(format!("{}", DtmfDigit::Zero), "0");
        assert_eq!(format!("{}", DtmfDigit::Five), "5");
        assert_eq!(format!("{}", DtmfDigit::Star), "*");
        assert_eq!(format!("{}", DtmfDigit::Hash), "#");
        assert_eq!(format!("{}", DtmfDigit::A), "A");
    }
}
