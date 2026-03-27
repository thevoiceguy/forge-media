//! DTMF command handler for conference control
//!
//! Allows participants to control their conference experience via DTMF tones:
//! - Mute/unmute themselves
//! - Adjust volume
//! - Host commands (with PIN authentication)

use crate::RoomId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Configuration for DTMF commands in a conference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceDtmfConfig {
    /// Enable DTMF command processing
    pub enabled: bool,

    /// Participant commands (any participant can use these)
    pub participant_commands: ParticipantCommands,

    /// Host commands (require PIN authentication)
    pub host_commands: HostCommands,

    /// Host PIN (if None, host commands are disabled)
    pub host_pin: Option<String>,

    /// Forward unhandled DTMF to AI (if AI is attached)
    pub forward_to_ai: bool,

    /// Timeout for multi-digit sequences (milliseconds)
    pub sequence_timeout_ms: u64,

    /// Enable audio feedback (beep tones for confirmation)
    pub audio_feedback: bool,
}

impl Default for ConferenceDtmfConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            participant_commands: ParticipantCommands::default(),
            host_commands: HostCommands::default(),
            host_pin: None,
            forward_to_ai: true,
            sequence_timeout_ms: 3000, // 3 seconds
            audio_feedback: true,
        }
    }
}

/// Participant commands (available to all)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantCommands {
    /// Mute/unmute toggle
    pub mute_toggle: String, // default: "*6"

    /// Volume up (increase gain)
    pub volume_up: String, // default: "*7"

    /// Volume down (decrease gain)
    pub volume_down: String, // default: "*8"

    /// Hear conference info (participant count, duration)
    pub info: String, // default: "*0"

    /// Leave conference
    pub leave: String, // default: "*9"
}

impl Default for ParticipantCommands {
    fn default() -> Self {
        Self {
            mute_toggle: "*6".to_string(),
            volume_up: "*7".to_string(),
            volume_down: "*8".to_string(),
            info: "*0".to_string(),
            leave: "*9".to_string(),
        }
    }
}

/// Host commands (require PIN authentication)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostCommands {
    /// Kick participant (format: <prefix><participant_number>)
    /// Example: "##1" kicks participant 1
    pub kick_prefix: String, // default: "##"

    /// Mute all participants
    pub mute_all: String, // default: "*91"

    /// Unmute all participants
    pub unmute_all: String, // default: "*92"

    /// Lock conference (prevent new joins)
    pub lock: String, // default: "*93"

    /// Unlock conference
    pub unlock: String, // default: "*94"

    /// End conference for all
    pub end_conference: String, // default: "*99"

    /// Dial-out prefix: starts collecting an extension/number.
    /// After entering the prefix the caller types digits then #.
    pub dial_out: String, // default: "*5"
}

impl Default for HostCommands {
    fn default() -> Self {
        Self {
            kick_prefix: "##".to_string(),
            mute_all: "*91".to_string(),
            unmute_all: "*92".to_string(),
            lock: "*93".to_string(),
            unlock: "*94".to_string(),
            end_conference: "*99".to_string(),
            dial_out: "*5".to_string(),
        }
    }
}

/// DTMF command that was parsed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DtmfCommand {
    // Participant commands
    MuteToggle,
    VolumeUp,
    VolumeDown,
    Info,
    Leave,

    // Host commands
    KickParticipant(usize), // participant number
    MuteAll,
    UnmuteAll,
    LockConference,
    UnlockConference,
    EndConference,

    // Host: dial-out
    DialOut(String), // destination extension/number

    // Special
    EnterHostPin,    // Waiting for PIN entry
    EnterDialOut,    // Waiting for extension digits
    InvalidCommand,
    Incomplete, // Need more digits
}

/// Role of the participant
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantRole {
    Participant,
    Host,
}

/// State of DTMF sequence collection for a participant
#[derive(Debug, Clone)]
struct SequenceState {
    /// Accumulated digits
    digits: String,
    /// When the sequence started
    started_at: Instant,
    /// Is this participant authenticated as host?
    is_host: bool,
    /// Is participant entering host PIN?
    entering_pin: bool,
    /// Is participant entering a dial-out destination?
    entering_dialout: bool,
    /// Last digit received (for dedup)
    last_digit: Option<char>,
    /// When the last digit was received
    last_digit_time: Instant,
}

impl SequenceState {
    fn new() -> Self {
        Self {
            digits: String::new(),
            started_at: Instant::now(),
            is_host: false,
            entering_pin: false,
            entering_dialout: false,
            last_digit: None,
            last_digit_time: Instant::now(),
        }
    }

    fn add_digit(&mut self, digit: char) -> bool {
        // Dedup: ignore duplicate digit within 200ms (RFC 2833 retransmissions)
        if self.last_digit == Some(digit) && self.last_digit_time.elapsed() < Duration::from_millis(200) {
            return false; // duplicate, ignore
        }
        self.last_digit = Some(digit);
        self.last_digit_time = Instant::now();
        self.digits.push(digit);
        self.started_at = Instant::now(); // Reset timeout
        true
    }

    fn clear(&mut self) {
        self.digits.clear();
        self.started_at = Instant::now();
        self.entering_pin = false;
        self.entering_dialout = false;
    }

    fn is_expired(&self, timeout: Duration) -> bool {
        self.started_at.elapsed() > timeout
    }
}

/// DTMF command handler for a conference room
pub struct DtmfCommandHandler {
    /// Room ID this handler is for
    room_id: RoomId,

    /// Configuration
    config: ConferenceDtmfConfig,

    /// Active sequences per participant
    sequences: Arc<parking_lot::RwLock<HashMap<String, SequenceState>>>,
}

impl DtmfCommandHandler {
    /// Create a new DTMF command handler
    pub fn new(room_id: impl Into<RoomId>, config: ConferenceDtmfConfig) -> Self {
        Self {
            room_id: room_id.into(),
            config,
            sequences: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        }
    }

    /// Process a DTMF digit from a participant
    ///
    /// Returns the command if a complete sequence was recognized, or None if:
    /// - More digits are needed
    /// - The sequence expired
    /// - DTMF processing is disabled
    pub fn process_digit(
        &self,
        participant_id: &str,
        digit: char,
    ) -> Option<(DtmfCommand, ParticipantRole)> {
        if !self.config.enabled {
            return None;
        }

        // Get or create sequence state
        let mut sequences = self.sequences.write();
        let sequence = sequences
            .entry(participant_id.to_string())
            .or_insert_with(SequenceState::new);

        // Check if sequence expired
        let timeout = Duration::from_millis(self.config.sequence_timeout_ms);
        if sequence.is_expired(timeout) {
            debug!(
                "DTMF sequence expired for participant {} in room {}",
                participant_id, self.room_id
            );
            sequence.clear();
        }

        // Add digit to sequence (dedup RFC 2833 retransmissions)
        if !sequence.add_digit(digit) {
            return None; // duplicate digit, ignore
        }

        // Check if entering PIN (inline to avoid deadlock from nested lock)
        if sequence.entering_pin {
            let expected_pin = match &self.config.host_pin {
                Some(pin) => pin,
                None => {
                    sequence.clear();
                    return Some((DtmfCommand::InvalidCommand, ParticipantRole::Participant));
                }
            };

            let digits = sequence.digits.clone();

            if &digits == expected_pin {
                // PIN correct, grant host privileges
                sequence.is_host = true;
                sequence.clear();
                info!(
                    "Participant {} authenticated as host in room {}",
                    participant_id, self.room_id
                );
                return Some((DtmfCommand::EnterHostPin, ParticipantRole::Host));
            } else if expected_pin.starts_with(&digits) {
                // Still entering PIN
                return None;
            } else {
                // Wrong PIN
                sequence.clear();
                warn!(
                    "Incorrect host PIN attempt from {} in room {}",
                    participant_id, self.room_id
                );
                return Some((DtmfCommand::InvalidCommand, ParticipantRole::Participant));
            }
        }

        // Check if entering dial-out destination digits
        if sequence.entering_dialout {
            let digits = sequence.digits.clone();
            if digit == '#' {
                // # terminates the destination entry
                let destination = digits.trim_end_matches('#').to_string();
                sequence.clear();
                if destination.is_empty() {
                    warn!(
                        "Empty dial-out destination from {} in room {}",
                        participant_id, self.room_id
                    );
                    return Some((DtmfCommand::InvalidCommand, ParticipantRole::Host));
                }
                info!(
                    "DTMF dial-out from {} in room {}: destination={}",
                    participant_id, self.room_id, destination
                );
                return Some((DtmfCommand::DialOut(destination), ParticipantRole::Host));
            }
            // Still collecting digits — keep accumulating
            return None;
        }

        // Try to match command
        let role = if sequence.is_host {
            ParticipantRole::Host
        } else {
            ParticipantRole::Participant
        };

        let result = self.match_command(&sequence.digits, role);

        match result {
            Some(DtmfCommand::Incomplete) => {
                // Need more digits, keep accumulating
                None
            }
            Some(DtmfCommand::EnterHostPin) => {
                // Start PIN entry mode
                sequence.entering_pin = true;
                sequence.digits.clear();
                info!(
                    "Participant {} entering host PIN in room {}",
                    participant_id, self.room_id
                );
                None
            }
            Some(DtmfCommand::EnterDialOut) => {
                // Start dial-out digit collection mode
                sequence.entering_dialout = true;
                sequence.digits.clear();
                info!(
                    "Host {} entering dial-out destination in room {}",
                    participant_id, self.room_id
                );
                None
            }
            Some(cmd) => {
                // Complete command recognized
                info!(
                    "DTMF command {:?} from {} in room {}",
                    cmd, participant_id, self.room_id
                );
                sequence.clear();
                Some((cmd, role))
            }
            None => {
                // Invalid sequence, clear and ignore
                warn!(
                    "Invalid DTMF sequence '{}' from {} in room {}",
                    sequence.digits, participant_id, self.room_id
                );
                sequence.clear();
                Some((DtmfCommand::InvalidCommand, role))
            }
        }
    }

    /// Match accumulated digits to a command
    fn match_command(&self, digits: &str, role: ParticipantRole) -> Option<DtmfCommand> {
        // Check participant commands (available to all)
        // But first check if this could also be a prefix of a host command
        // to avoid premature command execution
        let mut participant_command = None;

        if digits == self.config.participant_commands.mute_toggle {
            participant_command = Some(DtmfCommand::MuteToggle);
        } else if digits == self.config.participant_commands.volume_up {
            participant_command = Some(DtmfCommand::VolumeUp);
        } else if digits == self.config.participant_commands.volume_down {
            participant_command = Some(DtmfCommand::VolumeDown);
        } else if digits == self.config.participant_commands.info {
            participant_command = Some(DtmfCommand::Info);
        } else if digits == self.config.participant_commands.leave {
            participant_command = Some(DtmfCommand::Leave);
        }

        // If we found a participant command, check if digits could also be a host command prefix
        if let Some(cmd) = participant_command {
            // Check if this is also a prefix of a longer host command
            if self.is_host_command_prefix(digits)
                && digits.len() < self.longest_host_command_length()
            {
                // Ambiguous - could be complete participant command or prefix of host command
                // Return Incomplete to wait for more digits
                return Some(DtmfCommand::Incomplete);
            }
            return Some(cmd);
        }

        // Check if digits match a complete host command
        let is_mute_all = digits == self.config.host_commands.mute_all;
        let is_unmute_all = digits == self.config.host_commands.unmute_all;
        let is_lock = digits == self.config.host_commands.lock;
        let is_unlock = digits == self.config.host_commands.unlock;
        let is_end_conference = digits == self.config.host_commands.end_conference;
        let is_kick = digits.starts_with(&self.config.host_commands.kick_prefix)
            && digits.len() > self.config.host_commands.kick_prefix.len();
        let is_dial_out = digits == self.config.host_commands.dial_out;

        let is_host_command =
            is_mute_all || is_unmute_all || is_lock || is_unlock || is_end_conference || is_kick || is_dial_out;

        if is_host_command {
            if role == ParticipantRole::Host {
                // Authenticated host - execute command
                if is_mute_all {
                    return Some(DtmfCommand::MuteAll);
                }
                if is_unmute_all {
                    return Some(DtmfCommand::UnmuteAll);
                }
                if is_lock {
                    return Some(DtmfCommand::LockConference);
                }
                if is_unlock {
                    return Some(DtmfCommand::UnlockConference);
                }
                if is_end_conference {
                    return Some(DtmfCommand::EndConference);
                }
                if is_kick {
                    let number_part = &digits[self.config.host_commands.kick_prefix.len()..];
                    if let Ok(participant_num) = number_part.parse::<usize>() {
                        return Some(DtmfCommand::KickParticipant(participant_num));
                    }
                }
                if is_dial_out {
                    // Enter dial-out digit collection mode
                    return Some(DtmfCommand::EnterDialOut);
                }
            } else if self.config.host_pin.is_some() {
                // Non-authenticated participant trying to use host command - prompt for PIN
                return Some(DtmfCommand::EnterHostPin);
            }
        }

        // Check if this could be a partial match for any command
        if self.is_partial_match(digits, role) {
            return Some(DtmfCommand::Incomplete);
        }

        // No match
        None
    }

    /// Check if digits could be a prefix of any host command
    fn is_host_command_prefix(&self, digits: &str) -> bool {
        let cmds = &self.config.host_commands;
        cmds.mute_all.starts_with(digits)
            || cmds.unmute_all.starts_with(digits)
            || cmds.lock.starts_with(digits)
            || cmds.unlock.starts_with(digits)
            || cmds.end_conference.starts_with(digits)
            || cmds.kick_prefix.starts_with(digits)
            || cmds.dial_out.starts_with(digits)
    }

    /// Get the length of the longest host command
    fn longest_host_command_length(&self) -> usize {
        let cmds = &self.config.host_commands;
        [
            cmds.mute_all.len(),
            cmds.unmute_all.len(),
            cmds.lock.len(),
            cmds.unlock.len(),
            cmds.end_conference.len(),
            cmds.kick_prefix.len() + 2, // Account for kick prefix + at least 2 digits
            cmds.dial_out.len(),
        ]
        .iter()
        .max()
        .copied()
        .unwrap_or(0)
    }

    /// Check if digits could be a partial match for any command
    fn is_partial_match(&self, digits: &str, _role: ParticipantRole) -> bool {
        let part_cmds = &self.config.participant_commands;

        // Check participant commands
        if part_cmds.mute_toggle.starts_with(digits)
            || part_cmds.volume_up.starts_with(digits)
            || part_cmds.volume_down.starts_with(digits)
            || part_cmds.info.starts_with(digits)
            || part_cmds.leave.starts_with(digits)
        {
            return true;
        }

        // Check host commands (for both participants and hosts)
        // Participants need this to return Incomplete while entering host commands
        if self.is_host_command_prefix(digits) {
            return true;
        }

        false
    }

    /// Set a participant's host status (call when they join as host via PIN auth)
    pub fn set_host(&self, participant_id: &str, is_host: bool) {
        let mut sequences = self.sequences.write();
        let sequence = sequences
            .entry(participant_id.to_string())
            .or_insert_with(SequenceState::new);
        sequence.is_host = is_host;
    }

    /// Clear sequence for a participant (call when they leave)
    pub fn clear_participant(&self, participant_id: &str) {
        self.sequences.write().remove(participant_id);
    }

    /// Get current role of a participant
    pub fn get_role(&self, participant_id: &str) -> ParticipantRole {
        self.sequences
            .read()
            .get(participant_id)
            .map(|s| {
                if s.is_host {
                    ParticipantRole::Host
                } else {
                    ParticipantRole::Participant
                }
            })
            .unwrap_or(ParticipantRole::Participant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_participant_commands() {
        let config = ConferenceDtmfConfig::default();
        let handler = DtmfCommandHandler::new("test-room", config);

        // Test mute toggle (*6)
        assert_eq!(handler.process_digit("alice", '*'), None); // Incomplete
        let result = handler.process_digit("alice", '6');
        assert_eq!(
            result,
            Some((DtmfCommand::MuteToggle, ParticipantRole::Participant))
        );

        // Test volume up (*7)
        assert_eq!(handler.process_digit("bob", '*'), None);
        let result = handler.process_digit("bob", '7');
        assert_eq!(
            result,
            Some((DtmfCommand::VolumeUp, ParticipantRole::Participant))
        );
    }

    #[test]
    fn test_host_commands_require_pin() {
        let mut config = ConferenceDtmfConfig::default();
        config.host_pin = Some("1234".to_string());
        let handler = DtmfCommandHandler::new("test-room", config);

        // Try to use host command without PIN
        assert_eq!(handler.process_digit("alice", '*'), None);
        assert_eq!(handler.process_digit("alice", '9'), None);
        let result = handler.process_digit("alice", '1'); // *91 = mute all
                                                          // Should prompt for PIN
        assert_eq!(result, None); // Entering PIN mode

        // Enter correct PIN
        handler.process_digit("alice", '1');
        handler.process_digit("alice", '2');
        handler.process_digit("alice", '3');
        let result = handler.process_digit("alice", '4');
        assert_eq!(
            result,
            Some((DtmfCommand::EnterHostPin, ParticipantRole::Host))
        );

        // Now can use host commands
        handler.process_digit("alice", '*');
        handler.process_digit("alice", '9');
        let result = handler.process_digit("alice", '1');
        assert_eq!(result, Some((DtmfCommand::MuteAll, ParticipantRole::Host)));
    }

    #[test]
    fn test_invalid_sequence() {
        let config = ConferenceDtmfConfig::default();
        let handler = DtmfCommandHandler::new("test-room", config);

        handler.process_digit("alice", '*');
        handler.process_digit("alice", '5'); // *5 is not a valid command
        let result = handler.process_digit("alice", '5');
        assert_eq!(
            result,
            Some((DtmfCommand::InvalidCommand, ParticipantRole::Participant))
        );
    }

    #[test]
    fn test_sequence_timeout() {
        let mut config = ConferenceDtmfConfig::default();
        config.sequence_timeout_ms = 100; // 100ms timeout
        let handler = DtmfCommandHandler::new("test-room", config);

        handler.process_digit("alice", '*');
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Sequence should have expired, so this starts a new sequence
        let result = handler.process_digit("alice", '7'); // Just '7' is not valid
        assert_eq!(
            result,
            Some((DtmfCommand::InvalidCommand, ParticipantRole::Participant))
        );
    }
}
