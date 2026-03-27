//! Conference configuration from TOML file

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Conference configuration errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse config: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Invalid configuration: {0}")]
    ValidationError(String),
}

/// Main conference configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceConfig {
    /// Security settings
    pub security: SecurityConfig,

    /// DTMF command settings
    pub dtmf: DtmfConfig,

    /// Audio settings
    pub audio: AudioConfig,

    /// Recording settings
    pub recording: RecordingConfig,

    /// Capacity and access control settings
    pub capacity: CapacityConfig,

    /// Meeting requirements
    pub meeting: MeetingRequirementsConfig,

    /// Audio feedback sounds
    pub sounds: AudioFeedbackConfig,
}

impl Default for ConferenceConfig {
    fn default() -> Self {
        Self {
            security: SecurityConfig::default(),
            dtmf: DtmfConfig::default(),
            audio: AudioConfig::default(),
            recording: RecordingConfig::default(),
            capacity: CapacityConfig::default(),
            meeting: MeetingRequirementsConfig::default(),
            sounds: AudioFeedbackConfig::default(),
        }
    }
}

impl ConferenceConfig {
    /// Load configuration from TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.security.validate()?;
        self.dtmf.validate()?;
        Ok(())
    }

    /// Save configuration to TOML file
    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), ConfigError> {
        let contents = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::ValidationError(format!("Failed to serialize: {}", e)))?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}

/// Security and PIN configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Require guest PIN to join conference
    pub require_guest_pin: bool,

    /// Guest PIN (participants must enter this to join)
    /// If None and require_guest_pin is true, no one can join
    pub guest_pin: Option<String>,

    /// Host PIN (elevates participant to host role)
    /// If None, host commands are disabled
    pub host_pin: Option<String>,

    /// PIN complexity requirements
    pub pin_requirements: PinRequirements,

    /// Maximum failed PIN attempts before lockout
    pub max_pin_attempts: u32,

    /// Lockout duration in seconds after max attempts
    pub lockout_duration_secs: u64,

    /// Lock conference by default (prevent joins without unlock)
    pub default_locked: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_guest_pin: false,
            guest_pin: None,
            host_pin: None,
            pin_requirements: PinRequirements::default(),
            max_pin_attempts: 3,
            lockout_duration_secs: 300, // 5 minutes
            default_locked: false,
        }
    }
}

impl SecurityConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        // Validate guest PIN if required
        if let Some(pin) = &self.guest_pin {
            self.pin_requirements.validate_pin(pin, "guest")?;
        }

        // Validate host PIN if provided
        if let Some(pin) = &self.host_pin {
            self.pin_requirements.validate_pin(pin, "host")?;
        }

        // Ensure guest and host PINs are different
        if let (Some(guest), Some(host)) = (&self.guest_pin, &self.host_pin) {
            if guest == host {
                return Err(ConfigError::ValidationError(
                    "Guest PIN and Host PIN must be different".to_string(),
                ));
            }
        }

        Ok(())
    }
}

/// PIN complexity requirements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinRequirements {
    /// Minimum PIN length
    pub min_length: usize,

    /// Maximum PIN length
    pub max_length: usize,

    /// Require at least one digit (0-9)
    pub require_digit: bool,

    /// Require at least one letter (a-z, A-Z)
    pub require_letter: bool,

    /// Require at least one special character (*#)
    pub require_special: bool,

    /// Allow only digits (overrides other requirements)
    pub digits_only: bool,
}

impl Default for PinRequirements {
    fn default() -> Self {
        Self {
            min_length: 4,
            max_length: 12,
            require_digit: true,
            require_letter: false,
            require_special: false,
            digits_only: true, // Default to numeric PINs for phone keypads
        }
    }
}

impl PinRequirements {
    /// Validate a PIN against requirements
    pub fn validate_pin(&self, pin: &str, pin_type: &str) -> Result<(), ConfigError> {
        // Check length
        if pin.len() < self.min_length {
            return Err(ConfigError::ValidationError(format!(
                "{} PIN must be at least {} characters",
                pin_type, self.min_length
            )));
        }

        if pin.len() > self.max_length {
            return Err(ConfigError::ValidationError(format!(
                "{} PIN must be at most {} characters",
                pin_type, self.max_length
            )));
        }

        if self.digits_only {
            // Only allow 0-9, *, #
            if !pin
                .chars()
                .all(|c| c.is_ascii_digit() || c == '*' || c == '#')
            {
                return Err(ConfigError::ValidationError(format!(
                    "{} PIN must contain only digits (0-9), *, or #",
                    pin_type
                )));
            }
        } else {
            // Check complexity requirements
            let has_digit = pin.chars().any(|c| c.is_ascii_digit());
            let has_letter = pin.chars().any(|c| c.is_ascii_alphabetic());
            let has_special = pin.chars().any(|c| matches!(c, '*' | '#'));

            if self.require_digit && !has_digit {
                return Err(ConfigError::ValidationError(format!(
                    "{} PIN must contain at least one digit",
                    pin_type
                )));
            }

            if self.require_letter && !has_letter {
                return Err(ConfigError::ValidationError(format!(
                    "{} PIN must contain at least one letter",
                    pin_type
                )));
            }

            if self.require_special && !has_special {
                return Err(ConfigError::ValidationError(format!(
                    "{} PIN must contain at least one special character (* or #)",
                    pin_type
                )));
            }
        }

        Ok(())
    }
}

/// DTMF command configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtmfConfig {
    /// Enable DTMF command processing
    pub enabled: bool,

    /// Participant commands (available to all)
    pub participant_commands: ParticipantCommandsConfig,

    /// Host commands (require host PIN)
    pub host_commands: HostCommandsConfig,

    /// Forward unhandled DTMF to AI if present
    pub forward_to_ai: bool,

    /// Timeout for multi-digit sequences (milliseconds)
    pub sequence_timeout_ms: u64,

    /// Enable audio feedback (beep tones)
    pub audio_feedback: bool,
}

impl Default for DtmfConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            participant_commands: ParticipantCommandsConfig::default(),
            host_commands: HostCommandsConfig::default(),
            forward_to_ai: true,
            sequence_timeout_ms: 3000,
            audio_feedback: true,
        }
    }
}

impl DtmfConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        // Check for conflicting command bindings
        let mut bindings = std::collections::HashSet::new();

        let cmds = vec![
            &self.participant_commands.mute_toggle,
            &self.participant_commands.volume_up,
            &self.participant_commands.volume_down,
            &self.participant_commands.info,
            &self.participant_commands.leave,
        ];

        for cmd in cmds {
            if !bindings.insert(cmd) {
                return Err(ConfigError::ValidationError(format!(
                    "Duplicate DTMF command binding: {}",
                    cmd
                )));
            }
        }

        Ok(())
    }
}

/// Participant command key bindings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantCommandsConfig {
    /// Mute/unmute toggle
    pub mute_toggle: String,
    /// Volume up
    pub volume_up: String,
    /// Volume down
    pub volume_down: String,
    /// Hear conference info
    pub info: String,
    /// Leave conference
    pub leave: String,
}

impl Default for ParticipantCommandsConfig {
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

/// Host command key bindings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostCommandsConfig {
    /// Kick participant prefix (e.g., "##1" kicks participant 1)
    pub kick_prefix: String,
    /// Mute all participants
    pub mute_all: String,
    /// Unmute all participants
    pub unmute_all: String,
    /// Lock conference
    pub lock: String,
    /// Unlock conference
    pub unlock: String,
    /// End conference for all
    pub end_conference: String,
    /// Dial-out: invite external participant (prefix, then digits, then #)
    pub dial_out: String,
}

impl Default for HostCommandsConfig {
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

/// Audio settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Default sample rate (Hz)
    pub sample_rate: u32,

    /// Default frame size (samples)
    pub frame_size: usize,

    /// Default participant gain (0.0 to 2.0)
    pub default_gain: f32,

    /// Enable voice activity detection
    pub enable_vad: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            frame_size: 960, // 20ms at 48kHz
            default_gain: 1.0,
            enable_vad: true,
        }
    }
}

/// Recording settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    /// Enable automatic recording
    pub auto_record: bool,

    /// Recording codec (pcm, opus)
    pub codec: String,

    /// Recording output directory
    pub output_dir: String,

    /// Include AI in recordings
    pub include_ai: bool,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            auto_record: false,
            codec: "pcm".to_string(),
            output_dir: "./recordings".to_string(),
            include_ai: true,
        }
    }
}

/// Capacity and access control settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityConfig {
    /// Maximum number of participants allowed (0 = unlimited)
    pub max_channels: usize,

    /// Require host to join before participants can join
    /// If true, participants wait in a holding area until host joins
    pub wait_for_moderator: bool,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self {
            max_channels: 0, // Unlimited
            wait_for_moderator: false,
        }
    }
}

/// Meeting requirements configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRequirementsConfig {
    /// Minimum number of users required to start the meeting
    /// If set, conference audio won't be mixed until this many participants join
    pub min_users: usize,

    /// Minimum number of participants required to start recording
    /// Recording will automatically start when this threshold is reached
    pub min_recording_participants: usize,
}

impl Default for MeetingRequirementsConfig {
    fn default() -> Self {
        Self {
            min_users: 1,
            min_recording_participants: 1,
        }
    }
}

/// Audio feedback sound configuration
/// All paths are optional - None means the sound is disabled
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFeedbackConfig {
    /// Sound played when a participant joins the conference
    pub join_sound: Option<String>,

    /// Sound played when a participant exits the conference
    pub exit_sound: Option<String>,

    /// Sound played when you're the only participant in the conference
    pub alone_sound: Option<String>,

    /// Sound played when an invalid PIN is entered
    pub invalid_pin_sound: Option<String>,

    /// Sound played when trying to join a locked conference
    pub locked_sound: Option<String>,

    /// Sound played when the conference is unlocked
    pub unlocked_sound: Option<String>,

    /// Sound played when you are kicked from the conference
    pub kicked_sound: Option<String>,

    /// Sound played when trying to join a full conference (max channels reached)
    pub max_members_sound: Option<String>,

    /// Music on hold - played when only 1 user is in the conference
    /// Loops until another participant joins
    pub moh_sound: Option<String>,

    /// Prompt asking user to enter their PIN
    pub pin_prompt_sound: Option<String>,

    /// Sound played when recording starts
    pub recording_started_sound: Option<String>,

    /// Sound played when recording stops
    pub recording_stopped_sound: Option<String>,
}

impl Default for AudioFeedbackConfig {
    fn default() -> Self {
        Self {
            join_sound: None,
            exit_sound: None,
            alone_sound: None,
            invalid_pin_sound: None,
            locked_sound: None,
            unlocked_sound: None,
            kicked_sound: None,
            max_members_sound: None,
            moh_sound: None,
            pin_prompt_sound: None,
            recording_started_sound: None,
            recording_stopped_sound: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid() {
        let config = ConferenceConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_pin_validation() {
        let reqs = PinRequirements::default();

        // Valid numeric PIN
        assert!(reqs.validate_pin("1234", "test").is_ok());
        assert!(reqs.validate_pin("123456", "test").is_ok());

        // Too short
        assert!(reqs.validate_pin("123", "test").is_err());

        // Too long
        assert!(reqs.validate_pin("1234567890123", "test").is_err());

        // Invalid characters (digits_only = true)
        assert!(reqs.validate_pin("12ab", "test").is_err());

        // Special chars OK for digits_only
        assert!(reqs.validate_pin("12*#", "test").is_ok());
    }

    #[test]
    fn test_pin_complexity() {
        let mut reqs = PinRequirements::default();
        reqs.digits_only = false;
        reqs.require_digit = true;
        reqs.require_letter = true;

        // Valid complex PIN
        assert!(reqs.validate_pin("abc123", "test").is_ok());

        // Missing digit
        assert!(reqs.validate_pin("abcdef", "test").is_err());

        // Missing letter
        assert!(reqs.validate_pin("123456", "test").is_err());
    }

    #[test]
    fn test_guest_host_pins_must_differ() {
        let mut config = SecurityConfig::default();
        config.guest_pin = Some("1234".to_string());
        config.host_pin = Some("1234".to_string());

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_duplicate_dtmf_bindings() {
        let mut config = DtmfConfig::default();
        config.participant_commands.mute_toggle = "*6".to_string();
        config.participant_commands.volume_up = "*6".to_string(); // Duplicate!

        assert!(config.validate().is_err());
    }
}
