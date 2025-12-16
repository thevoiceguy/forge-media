//! Per-room configuration with global defaults

use crate::config::{ConferenceConfig, DtmfConfig, SecurityConfig};
use crate::dtmf_commands::ConferenceDtmfConfig;
use serde::{Deserialize, Serialize};

/// Configuration for a specific conference room
/// Can override global defaults from ConferenceConfig
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomConfig {
    /// Room-specific guest PIN (overrides global)
    pub guest_pin: Option<String>,

    /// Room-specific host PIN (overrides global)
    pub host_pin: Option<String>,

    /// Require guest PIN for this room (overrides global)
    pub require_guest_pin: Option<bool>,

    /// Maximum PIN attempts for this room (overrides global)
    pub max_pin_attempts: Option<u32>,

    /// Lock this room by default (overrides global)
    pub default_locked: Option<bool>,

    /// Enable DTMF commands for this room (overrides global)
    pub enable_dtmf: Option<bool>,

    /// Room-specific DTMF command bindings (overrides global)
    pub dtmf_commands: Option<DtmfCommandBindings>,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            guest_pin: None,
            host_pin: None,
            require_guest_pin: None,
            max_pin_attempts: None,
            default_locked: None,
            enable_dtmf: None,
            dtmf_commands: None,
        }
    }
}

impl RoomConfig {
    /// Create an empty room config (uses all global defaults)
    pub fn new() -> Self {
        Self::default()
    }

    /// Create room config with custom PINs
    pub fn with_pins(guest_pin: Option<String>, host_pin: Option<String>) -> Self {
        Self {
            guest_pin,
            host_pin,
            ..Default::default()
        }
    }

    /// Merge room config with global config to produce effective configuration
    pub fn merge_with_global(&self, global: &ConferenceConfig) -> EffectiveRoomConfig {
        EffectiveRoomConfig {
            security: SecurityConfig {
                require_guest_pin: self.require_guest_pin
                    .unwrap_or(global.security.require_guest_pin),
                guest_pin: self.guest_pin.clone()
                    .or_else(|| global.security.guest_pin.clone()),
                host_pin: self.host_pin.clone()
                    .or_else(|| global.security.host_pin.clone()),
                pin_requirements: global.security.pin_requirements.clone(),
                max_pin_attempts: self.max_pin_attempts
                    .unwrap_or(global.security.max_pin_attempts),
                lockout_duration_secs: global.security.lockout_duration_secs,
                default_locked: self.default_locked
                    .unwrap_or(global.security.default_locked),
            },
            dtmf_enabled: self.enable_dtmf
                .unwrap_or(global.dtmf.enabled),
            dtmf_config: self.build_dtmf_config(&global.dtmf),
        }
    }

    /// Build ConferenceDtmfConfig from room and global settings
    fn build_dtmf_config(&self, global_dtmf: &DtmfConfig) -> ConferenceDtmfConfig {
        let participant_cmds = if let Some(bindings) = &self.dtmf_commands {
            crate::dtmf_commands::ParticipantCommands {
                mute_toggle: bindings.mute_toggle.clone()
                    .unwrap_or_else(|| global_dtmf.participant_commands.mute_toggle.clone()),
                volume_up: bindings.volume_up.clone()
                    .unwrap_or_else(|| global_dtmf.participant_commands.volume_up.clone()),
                volume_down: bindings.volume_down.clone()
                    .unwrap_or_else(|| global_dtmf.participant_commands.volume_down.clone()),
                info: bindings.info.clone()
                    .unwrap_or_else(|| global_dtmf.participant_commands.info.clone()),
                leave: bindings.leave.clone()
                    .unwrap_or_else(|| global_dtmf.participant_commands.leave.clone()),
            }
        } else {
            crate::dtmf_commands::ParticipantCommands {
                mute_toggle: global_dtmf.participant_commands.mute_toggle.clone(),
                volume_up: global_dtmf.participant_commands.volume_up.clone(),
                volume_down: global_dtmf.participant_commands.volume_down.clone(),
                info: global_dtmf.participant_commands.info.clone(),
                leave: global_dtmf.participant_commands.leave.clone(),
            }
        };

        let host_cmds = if let Some(bindings) = &self.dtmf_commands {
            crate::dtmf_commands::HostCommands {
                kick_prefix: bindings.kick_prefix.clone()
                    .unwrap_or_else(|| global_dtmf.host_commands.kick_prefix.clone()),
                mute_all: bindings.mute_all.clone()
                    .unwrap_or_else(|| global_dtmf.host_commands.mute_all.clone()),
                unmute_all: bindings.unmute_all.clone()
                    .unwrap_or_else(|| global_dtmf.host_commands.unmute_all.clone()),
                lock: bindings.lock.clone()
                    .unwrap_or_else(|| global_dtmf.host_commands.lock.clone()),
                unlock: bindings.unlock.clone()
                    .unwrap_or_else(|| global_dtmf.host_commands.unlock.clone()),
                end_conference: bindings.end_conference.clone()
                    .unwrap_or_else(|| global_dtmf.host_commands.end_conference.clone()),
            }
        } else {
            crate::dtmf_commands::HostCommands {
                kick_prefix: global_dtmf.host_commands.kick_prefix.clone(),
                mute_all: global_dtmf.host_commands.mute_all.clone(),
                unmute_all: global_dtmf.host_commands.unmute_all.clone(),
                lock: global_dtmf.host_commands.lock.clone(),
                unlock: global_dtmf.host_commands.unlock.clone(),
                end_conference: global_dtmf.host_commands.end_conference.clone(),
            }
        };

        ConferenceDtmfConfig {
            enabled: self.enable_dtmf.unwrap_or(global_dtmf.enabled),
            participant_commands: participant_cmds,
            host_commands: host_cmds,
            host_pin: self.host_pin.clone()
                .or_else(|| global_dtmf.enabled.then(|| String::new())), // Will be set from security config
            forward_to_ai: global_dtmf.forward_to_ai,
            sequence_timeout_ms: global_dtmf.sequence_timeout_ms,
            audio_feedback: global_dtmf.audio_feedback,
        }
    }
}

/// Effective configuration for a room after merging with global defaults
#[derive(Debug, Clone)]
pub struct EffectiveRoomConfig {
    pub security: SecurityConfig,
    pub dtmf_enabled: bool,
    pub dtmf_config: ConferenceDtmfConfig,
}

/// Room-specific DTMF command bindings (all optional, uses global defaults if None)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DtmfCommandBindings {
    // Participant commands
    pub mute_toggle: Option<String>,
    pub volume_up: Option<String>,
    pub volume_down: Option<String>,
    pub info: Option<String>,
    pub leave: Option<String>,

    // Host commands
    pub kick_prefix: Option<String>,
    pub mute_all: Option<String>,
    pub unmute_all: Option<String>,
    pub lock: Option<String>,
    pub unlock: Option<String>,
    pub end_conference: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConferenceConfig;

    #[test]
    fn test_empty_room_config_uses_global() {
        let global = ConferenceConfig::default();
        let room = RoomConfig::new();

        let effective = room.merge_with_global(&global);

        // Should use all global settings
        assert_eq!(effective.security.guest_pin, global.security.guest_pin);
        assert_eq!(effective.security.host_pin, global.security.host_pin);
        assert_eq!(effective.security.require_guest_pin, global.security.require_guest_pin);
    }

    #[test]
    fn test_room_pins_override_global() {
        let mut global = ConferenceConfig::default();
        global.security.guest_pin = Some("global123".to_string());
        global.security.host_pin = Some("global999".to_string());

        let room = RoomConfig::with_pins(
            Some("room456".to_string()),
            Some("room888".to_string()),
        );

        let effective = room.merge_with_global(&global);

        // Room-specific PINs should override global
        assert_eq!(effective.security.guest_pin, Some("room456".to_string()));
        assert_eq!(effective.security.host_pin, Some("room888".to_string()));
    }

    #[test]
    fn test_partial_override() {
        let mut global = ConferenceConfig::default();
        global.security.guest_pin = Some("global123".to_string());
        global.security.host_pin = Some("global999".to_string());
        global.security.max_pin_attempts = 3;

        let mut room = RoomConfig::new();
        room.guest_pin = Some("room456".to_string());
        room.max_pin_attempts = Some(5);
        // host_pin not set, should use global

        let effective = room.merge_with_global(&global);

        assert_eq!(effective.security.guest_pin, Some("room456".to_string()));
        assert_eq!(effective.security.host_pin, Some("global999".to_string())); // Global
        assert_eq!(effective.security.max_pin_attempts, 5); // Room override
    }

    #[test]
    fn test_room_can_disable_pin() {
        let mut global = ConferenceConfig::default();
        global.security.require_guest_pin = true;
        global.security.guest_pin = Some("global123".to_string());

        let mut room = RoomConfig::new();
        room.require_guest_pin = Some(false); // Disable for this room

        let effective = room.merge_with_global(&global);

        assert!(!effective.security.require_guest_pin);
    }

    #[test]
    fn test_dtmf_command_override() {
        let global = ConferenceConfig::default();

        let mut room = RoomConfig::new();
        let mut dtmf_bindings = DtmfCommandBindings::default();
        dtmf_bindings.mute_toggle = Some("**6".to_string()); // Different from global
        room.dtmf_commands = Some(dtmf_bindings);

        let effective = room.merge_with_global(&global);

        assert_eq!(effective.dtmf_config.participant_commands.mute_toggle, "**6");
        // Other commands should still use global defaults
        assert_eq!(
            effective.dtmf_config.participant_commands.volume_up,
            global.dtmf.participant_commands.volume_up
        );
    }
}
