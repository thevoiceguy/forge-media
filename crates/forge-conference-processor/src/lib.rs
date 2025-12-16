//! Conference processing library for Forge Media
//!
//! Manages multi-party audio conferences with mixing and recording

use thiserror::Error;

mod conference;
mod ai_manager;
mod dtmf_commands;
mod config;
mod pin_auth;
mod room_config;
mod audio_feedback;

pub use conference::{ConferenceBridge, ConferenceRoom, RoomId};
pub use ai_manager::{
    AudioMode, ConferenceAIConfig, ConferenceAIManager, ConferenceAIState, AI_PARTICIPANT_ID,
};
pub use dtmf_commands::{
    ConferenceDtmfConfig, DtmfCommand, DtmfCommandHandler, HostCommands, ParticipantCommands,
    ParticipantRole,
};
pub use config::{
    AudioConfig, ConferenceConfig, ConfigError, DtmfConfig, HostCommandsConfig,
    ParticipantCommandsConfig, PinRequirements, RecordingConfig, SecurityConfig,
};
pub use pin_auth::{PinAuthResult, PinAuthenticator};
pub use room_config::{DtmfCommandBindings, EffectiveRoomConfig, RoomConfig};
pub use audio_feedback::{AudioFeedbackPlayer, ConferenceSounds};
pub use forge_core::AudioFormat;
pub use forge_mixer::{ParticipantMetadata, ParticipantState};

/// Conference error types
#[derive(Error, Debug)]
pub enum ConferenceError {
    #[error("Conference not found: {0}")]
    ConferenceNotFound(String),

    #[error("Room not found: {0}")]
    RoomNotFound(String),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Mixer error: {0}")]
    Mixer(#[from] forge_mixer::MixerError),

    #[error("Recorder error: {0}")]
    Recorder(#[from] forge_recorder::RecorderError),

    #[error("AI already attached to room: {0}")]
    AlreadyHasAI(String),

    #[error("No AI attached to room: {0}")]
    NoAIAttached(String),

    #[error("AI session failed: {0}")]
    AISessionFailed(String),

    #[error("Audio routing failed: {0}")]
    AudioRoutingFailed(String),

    #[error("Conference is full (max {0} participants)")]
    ConferenceFull(usize),

    #[error("Waiting for moderator to join")]
    WaitingForModerator,

    #[error("Conference is locked")]
    ConferenceLocked,
}

/// Result type for conference operations
pub type Result<T> = std::result::Result<T, ConferenceError>;
