//! PIN authentication for conference access control

use crate::config::SecurityConfig;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Result of PIN authentication attempt
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinAuthResult {
    /// Guest PIN accepted - participant can join
    GuestAuthenticated,

    /// Host PIN accepted - participant has host privileges
    HostAuthenticated,

    /// Incorrect PIN
    IncorrectPin { attempts_remaining: u32 },

    /// Too many failed attempts - participant is locked out
    LockedOut { retry_after_secs: u64 },

    /// No PIN required
    NoPinRequired,
}

/// Tracks failed authentication attempts per participant
#[derive(Debug, Clone)]
struct AttemptTracker {
    failed_attempts: u32,
    lockout_until: Option<Instant>,
}

impl AttemptTracker {
    fn new() -> Self {
        Self {
            failed_attempts: 0,
            lockout_until: None,
        }
    }

    fn is_locked_out(&self) -> bool {
        if let Some(lockout_until) = self.lockout_until {
            Instant::now() < lockout_until
        } else {
            false
        }
    }

    fn seconds_until_unlock(&self) -> Option<u64> {
        if let Some(lockout_until) = self.lockout_until {
            let now = Instant::now();
            if now < lockout_until {
                Some((lockout_until - now).as_secs())
            } else {
                None
            }
        } else {
            None
        }
    }

    fn record_failure(&mut self, max_attempts: u32, lockout_duration: Duration) {
        self.failed_attempts += 1;

        if self.failed_attempts >= max_attempts {
            self.lockout_until = Some(Instant::now() + lockout_duration);
            warn!(
                "Participant locked out after {} failed PIN attempts",
                self.failed_attempts
            );
        }
    }

    fn reset(&mut self) {
        self.failed_attempts = 0;
        self.lockout_until = None;
    }
}

/// PIN authentication manager
pub struct PinAuthenticator {
    config: SecurityConfig,
    attempts: Arc<DashMap<String, AttemptTracker>>,
}

impl PinAuthenticator {
    /// Create a new PIN authenticator
    pub fn new(config: SecurityConfig) -> Self {
        Self {
            config,
            attempts: Arc::new(DashMap::new()),
        }
    }

    /// Authenticate a participant's PIN
    ///
    /// # Arguments
    /// * `participant_id` - Unique identifier for the participant
    /// * `pin` - The PIN entered by the participant
    ///
    /// # Returns
    /// `PinAuthResult` indicating success, failure, or lockout
    pub fn authenticate(&self, participant_id: &str, pin: &str) -> PinAuthResult {
        // Check if no PIN is required
        if !self.config.require_guest_pin {
            return PinAuthResult::NoPinRequired;
        }

        // Check lockout status
        let mut tracker = self.attempts.entry(participant_id.to_string())
            .or_insert_with(AttemptTracker::new);

        if tracker.is_locked_out() {
            let secs = tracker.seconds_until_unlock().unwrap_or(0);
            debug!(
                "Participant {} is locked out for {} more seconds",
                participant_id, secs
            );
            return PinAuthResult::LockedOut { retry_after_secs: secs };
        }

        // Check host PIN first (takes priority)
        if let Some(host_pin) = &self.config.host_pin {
            if pin == host_pin {
                tracker.reset();
                debug!("Participant {} authenticated as host", participant_id);
                return PinAuthResult::HostAuthenticated;
            }
        }

        // Check guest PIN
        if let Some(guest_pin) = &self.config.guest_pin {
            if pin == guest_pin {
                tracker.reset();
                debug!("Participant {} authenticated as guest", participant_id);
                return PinAuthResult::GuestAuthenticated;
            }
        }

        // PIN incorrect
        let lockout_duration = Duration::from_secs(self.config.lockout_duration_secs);
        tracker.record_failure(self.config.max_pin_attempts, lockout_duration);

        let attempts_remaining = self.config.max_pin_attempts
            .saturating_sub(tracker.failed_attempts);

        warn!(
            "Incorrect PIN for participant {} ({} attempts remaining)",
            participant_id, attempts_remaining
        );

        if attempts_remaining == 0 {
            PinAuthResult::LockedOut {
                retry_after_secs: self.config.lockout_duration_secs,
            }
        } else {
            PinAuthResult::IncorrectPin { attempts_remaining }
        }
    }

    /// Check if a participant is currently locked out
    pub fn is_locked_out(&self, participant_id: &str) -> bool {
        self.attempts
            .get(participant_id)
            .map(|tracker| tracker.is_locked_out())
            .unwrap_or(false)
    }

    /// Clear lockout for a participant (admin override)
    pub fn clear_lockout(&self, participant_id: &str) {
        if let Some(mut tracker) = self.attempts.get_mut(participant_id) {
            tracker.reset();
            debug!("Cleared lockout for participant {}", participant_id);
        }
    }

    /// Get failed attempt count for a participant
    pub fn failed_attempts(&self, participant_id: &str) -> u32 {
        self.attempts
            .get(participant_id)
            .map(|tracker| tracker.failed_attempts)
            .unwrap_or(0)
    }

    /// Update security configuration
    pub fn update_config(&mut self, config: SecurityConfig) {
        self.config = config;
    }

    /// Get current security configuration
    pub fn config(&self) -> &SecurityConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PinRequirements;

    fn test_config() -> SecurityConfig {
        SecurityConfig {
            require_guest_pin: true,
            guest_pin: Some("1234".to_string()),
            host_pin: Some("9876".to_string()),
            pin_requirements: PinRequirements::default(),
            max_pin_attempts: 3,
            lockout_duration_secs: 1, // Short for testing
            default_locked: false,
        }
    }

    #[test]
    fn test_guest_pin_authentication() {
        let auth = PinAuthenticator::new(test_config());

        let result = auth.authenticate("alice", "1234");
        assert_eq!(result, PinAuthResult::GuestAuthenticated);
    }

    #[test]
    fn test_host_pin_authentication() {
        let auth = PinAuthenticator::new(test_config());

        let result = auth.authenticate("alice", "9876");
        assert_eq!(result, PinAuthResult::HostAuthenticated);
    }

    #[test]
    fn test_incorrect_pin() {
        let auth = PinAuthenticator::new(test_config());

        let result = auth.authenticate("alice", "wrong");
        assert!(matches!(result, PinAuthResult::IncorrectPin { attempts_remaining: 2 }));
    }

    #[test]
    fn test_lockout_after_max_attempts() {
        let auth = PinAuthenticator::new(test_config());

        // First two failures
        auth.authenticate("alice", "wrong");
        auth.authenticate("alice", "wrong");

        // Third failure triggers lockout
        let result = auth.authenticate("alice", "wrong");
        assert!(matches!(result, PinAuthResult::LockedOut { .. }));

        // Further attempts are blocked
        let result = auth.authenticate("alice", "1234"); // Even correct PIN!
        assert!(matches!(result, PinAuthResult::LockedOut { .. }));
    }

    #[test]
    fn test_lockout_expires() {
        let auth = PinAuthenticator::new(test_config());

        // Trigger lockout
        auth.authenticate("alice", "wrong");
        auth.authenticate("alice", "wrong");
        auth.authenticate("alice", "wrong");

        assert!(auth.is_locked_out("alice"));

        // Wait for lockout to expire
        std::thread::sleep(Duration::from_secs(2));

        assert!(!auth.is_locked_out("alice"));

        // Should be able to authenticate again
        let result = auth.authenticate("alice", "1234");
        assert_eq!(result, PinAuthResult::GuestAuthenticated);
    }

    #[test]
    fn test_clear_lockout() {
        let auth = PinAuthenticator::new(test_config());

        // Trigger lockout
        auth.authenticate("alice", "wrong");
        auth.authenticate("alice", "wrong");
        auth.authenticate("alice", "wrong");

        assert!(auth.is_locked_out("alice"));

        // Admin clears lockout
        auth.clear_lockout("alice");

        assert!(!auth.is_locked_out("alice"));

        // Should be able to authenticate immediately
        let result = auth.authenticate("alice", "1234");
        assert_eq!(result, PinAuthResult::GuestAuthenticated);
    }

    #[test]
    fn test_no_pin_required() {
        let mut config = test_config();
        config.require_guest_pin = false;

        let auth = PinAuthenticator::new(config);

        let result = auth.authenticate("alice", "anything");
        assert_eq!(result, PinAuthResult::NoPinRequired);
    }

    #[test]
    fn test_host_pin_takes_priority() {
        let auth = PinAuthenticator::new(test_config());

        // Host PIN should be checked first
        let result = auth.authenticate("alice", "9876");
        assert_eq!(result, PinAuthResult::HostAuthenticated);

        // Not guest authenticated
        assert!(result != PinAuthResult::GuestAuthenticated);
    }
}
