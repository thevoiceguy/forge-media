//! DTMF digit buffer with timeouts for IVR applications
//!
//! Collects DTMF digits over time with configurable timeouts for
//! inter-digit gaps and total collection time. Useful for IVR systems
//! that need to collect digit sequences like phone numbers or menu choices.
//!
//! # Example
//!
//! ```rust,ignore
//! use forge_dtmf::{DtmfBuffer, DtmfDigit};
//! use std::time::Duration;
//!
//! let mut buffer = DtmfBuffer::new()
//!     .with_inter_digit_timeout(Duration::from_secs(3))
//!     .with_max_digits(10);
//!
//! // Collect digits
//! buffer.push(DtmfDigit::One);
//! buffer.push(DtmfDigit::Two);
//! buffer.push(DtmfDigit::Three);
//!
//! // Check if timeout expired
//! if buffer.is_inter_digit_timeout() {
//!     let digits = buffer.take_digits();
//!     println!("Collected: {}", digits);
//! }
//! ```

use crate::detector::DtmfDigit;
use std::time::{Duration, Instant};

/// DTMF digit buffer with timeout handling
///
/// Collects DTMF digits over time and handles various timeout scenarios:
/// - Inter-digit timeout: Maximum time between consecutive digits
/// - Total timeout: Maximum total collection time
/// - Max digits: Maximum number of digits to collect
pub struct DtmfBuffer {
    /// Collected digits
    digits: Vec<DtmfDigit>,

    /// Inter-digit timeout (time between digits)
    inter_digit_timeout: Duration,

    /// Total collection timeout
    total_timeout: Option<Duration>,

    /// Maximum number of digits to collect
    max_digits: Option<usize>,

    /// Timestamp of last digit received
    last_digit_time: Option<Instant>,

    /// Timestamp when buffer was created or reset
    start_time: Instant,

    /// Terminator digits that end collection (typically '#')
    terminators: Vec<DtmfDigit>,

    /// Whether collection is complete
    complete: bool,
}

impl DtmfBuffer {
    /// Default inter-digit timeout (3 seconds)
    pub const DEFAULT_INTER_DIGIT_TIMEOUT: Duration = Duration::from_secs(3);

    /// Default total timeout (30 seconds)
    pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

    /// Create a new DTMF buffer with default timeouts
    pub fn new() -> Self {
        Self {
            digits: Vec::new(),
            inter_digit_timeout: Self::DEFAULT_INTER_DIGIT_TIMEOUT,
            total_timeout: Some(Self::DEFAULT_TOTAL_TIMEOUT),
            max_digits: None,
            last_digit_time: None,
            start_time: Instant::now(),
            terminators: vec![DtmfDigit::Hash],
            complete: false,
        }
    }

    /// Set inter-digit timeout
    pub fn with_inter_digit_timeout(mut self, timeout: Duration) -> Self {
        self.inter_digit_timeout = timeout;
        self
    }

    /// Set total collection timeout
    pub fn with_total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = Some(timeout);
        self
    }

    /// Disable total timeout
    pub fn without_total_timeout(mut self) -> Self {
        self.total_timeout = None;
        self
    }

    /// Set maximum number of digits to collect
    pub fn with_max_digits(mut self, max: usize) -> Self {
        self.max_digits = Some(max);
        self
    }

    /// Set terminator digits (default: #)
    pub fn with_terminators(mut self, terminators: Vec<DtmfDigit>) -> Self {
        self.terminators = terminators;
        self
    }

    /// Add a digit to the buffer
    ///
    /// Returns true if the buffer is now complete (reached terminator, max digits, or other condition)
    pub fn push(&mut self, digit: DtmfDigit) -> bool {
        if self.complete {
            return true;
        }

        // Check if this is a terminator
        if self.terminators.contains(&digit) {
            self.complete = true;
            return true;
        }

        // Add digit
        self.digits.push(digit);
        self.last_digit_time = Some(Instant::now());

        // Check if max digits reached
        if let Some(max) = self.max_digits {
            if self.digits.len() >= max {
                self.complete = true;
                return true;
            }
        }

        false
    }

    /// Check if inter-digit timeout has expired
    pub fn is_inter_digit_timeout(&self) -> bool {
        if self.complete {
            return true;
        }

        if let Some(last_time) = self.last_digit_time {
            last_time.elapsed() >= self.inter_digit_timeout
        } else {
            // No digits yet, check total timeout
            if let Some(total) = self.total_timeout {
                self.start_time.elapsed() >= total
            } else {
                false
            }
        }
    }

    /// Check if total timeout has expired
    pub fn is_total_timeout(&self) -> bool {
        if let Some(total) = self.total_timeout {
            self.start_time.elapsed() >= total
        } else {
            false
        }
    }

    /// Check if any timeout has expired
    pub fn is_timeout(&self) -> bool {
        self.is_inter_digit_timeout() || self.is_total_timeout()
    }

    /// Check if collection is complete
    pub fn is_complete(&self) -> bool {
        self.complete || self.is_timeout()
    }

    /// Get the collected digits as a string
    pub fn get_digits(&self) -> String {
        self.digits.iter().map(|d| format!("{}", d)).collect()
    }

    /// Get the collected digits as a vector
    pub fn get_digits_vec(&self) -> &[DtmfDigit] {
        &self.digits
    }

    /// Take the collected digits, consuming them and resetting the buffer
    pub fn take_digits(&mut self) -> String {
        let digits = self.get_digits();
        self.reset();
        digits
    }

    /// Get number of digits collected
    pub fn len(&self) -> usize {
        self.digits.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.digits.is_empty()
    }

    /// Reset the buffer
    pub fn reset(&mut self) {
        self.digits.clear();
        self.last_digit_time = None;
        self.start_time = Instant::now();
        self.complete = false;
    }

    /// Clear only the digits, keeping timeout state
    pub fn clear(&mut self) {
        self.digits.clear();
        self.complete = false;
    }

    /// Get time since last digit (or None if no digits)
    pub fn time_since_last_digit(&self) -> Option<Duration> {
        self.last_digit_time.map(|t| t.elapsed())
    }

    /// Get total collection time
    pub fn total_time(&self) -> Duration {
        self.start_time.elapsed()
    }
}

impl Default for DtmfBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_buffer_creation() {
        let buffer = DtmfBuffer::new();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert!(!buffer.is_complete());
    }

    #[test]
    fn test_push_digits() {
        let mut buffer = DtmfBuffer::new();

        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        buffer.push(DtmfDigit::Three);

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.get_digits(), "123");
    }

    #[test]
    fn test_terminator() {
        let mut buffer = DtmfBuffer::new();

        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        let complete = buffer.push(DtmfDigit::Hash);

        assert!(complete);
        assert!(buffer.is_complete());
        assert_eq!(buffer.get_digits(), "12");
    }

    #[test]
    fn test_max_digits() {
        let mut buffer = DtmfBuffer::new().with_max_digits(3);

        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        let complete = buffer.push(DtmfDigit::Three);

        assert!(complete);
        assert!(buffer.is_complete());
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn test_inter_digit_timeout() {
        let mut buffer = DtmfBuffer::new().with_inter_digit_timeout(Duration::from_millis(50));

        buffer.push(DtmfDigit::One);
        assert!(!buffer.is_inter_digit_timeout());

        thread::sleep(Duration::from_millis(60));
        assert!(buffer.is_inter_digit_timeout());
        assert!(buffer.is_complete());
    }

    #[test]
    fn test_take_digits() {
        let mut buffer = DtmfBuffer::new();

        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        buffer.push(DtmfDigit::Three);

        let digits = buffer.take_digits();
        assert_eq!(digits, "123");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_custom_terminators() {
        let mut buffer = DtmfBuffer::new().with_terminators(vec![DtmfDigit::Star, DtmfDigit::Hash]);

        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        let complete = buffer.push(DtmfDigit::Star);

        assert!(complete);
        assert_eq!(buffer.get_digits(), "12");
    }

    #[test]
    fn test_reset() {
        let mut buffer = DtmfBuffer::new();

        buffer.push(DtmfDigit::One);
        buffer.push(DtmfDigit::Two);
        assert_eq!(buffer.len(), 2);

        buffer.reset();
        assert!(buffer.is_empty());
        assert!(!buffer.is_complete());
    }

    #[test]
    fn test_total_timeout() {
        let mut buffer = DtmfBuffer::new().with_total_timeout(Duration::from_millis(50));

        buffer.push(DtmfDigit::One);
        assert!(!buffer.is_total_timeout());

        thread::sleep(Duration::from_millis(60));
        assert!(buffer.is_total_timeout());
        assert!(buffer.is_complete());
    }
}
