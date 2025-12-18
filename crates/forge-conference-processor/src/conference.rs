//! Conference bridge management
//!
//! Manages multi-party audio conferences with mixing and recording

use crate::dtmf_commands::{
    ConferenceDtmfConfig, DtmfCommand, DtmfCommandHandler, ParticipantRole,
};
use crate::pin_auth::PinAuthenticator;
use crate::room_config::EffectiveRoomConfig;
use crate::{AudioFormat, ConferenceError, Result};
use bytes::Bytes;
use dashmap::DashMap;
use forge_core::{CallId, DtmfEventKind, EventBus, ForgeEvent};
use forge_mixer::AudioMixer;
use forge_recorder::{AudioRecorder, PlaybackSource};
use metrics::{counter, gauge, histogram};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Unique identifier for a conference room
pub type RoomId = String;

/// Constant participant ID for audio feedback sounds
const AUDIO_FEEDBACK_PARTICIPANT_ID: &str = "__audio_feedback__";

/// Conference room with audio mixing and optional recording
pub struct ConferenceRoom {
    /// Room identifier
    id: RoomId,
    /// Audio mixer for combining participant streams
    mixer: Arc<AudioMixer>,
    /// Optional recorder for capturing the conference
    recorder: Arc<RwLock<Option<AudioRecorder>>>,
    /// Optional AI manager for this room
    ai_manager: Arc<RwLock<Option<crate::ai_manager::ConferenceAIManager>>>,
    /// Optional DTMF command handler
    dtmf_handler: Arc<RwLock<Option<DtmfCommandHandler>>>,
    /// DTMF event processing task
    dtmf_task: Arc<RwLock<Option<JoinHandle<()>>>>,
    /// PIN authenticator for this room
    pin_auth: Arc<RwLock<Option<PinAuthenticator>>>,
    /// Room-specific configuration
    room_config: Arc<RwLock<Option<EffectiveRoomConfig>>>,
    /// Mapping from call_id to participant_id
    call_id_map: Arc<DashMap<CallId, String>>,
    /// Conference is locked (no new participants allowed)
    is_locked: Arc<RwLock<bool>>,
    /// Set of host participant IDs
    hosts: Arc<DashMap<String, ()>>,
    /// Set of participants waiting for moderator (participant_id)
    waiting_participants: Arc<DashMap<String, ()>>,
    /// Audio feedback player for playing sounds
    audio_feedback_player: Arc<RwLock<Option<crate::audio_feedback::AudioFeedbackPlayer>>>,
    /// Pre-loaded conference sounds
    conference_sounds: Arc<RwLock<Option<crate::audio_feedback::ConferenceSounds>>>,
    /// Audio format
    format: AudioFormat,
    /// Frame size for mixing operations
    _frame_size: usize,
}

impl ConferenceRoom {
    /// Create a new conference room
    ///
    /// # Arguments
    /// * `id` - Unique room identifier
    /// * `format` - Audio format for all streams
    /// * `frame_size` - Frame size for mixing operations (e.g., 480 for 10ms at 48kHz)
    pub fn new<S: Into<String>>(id: S, format: AudioFormat, frame_size: usize) -> Result<Self> {
        let id = id.into();
        info!("Creating conference room: {}", id);

        let mixer = AudioMixer::new(format, frame_size)?;

        Ok(Self {
            id,
            mixer: Arc::new(mixer),
            recorder: Arc::new(RwLock::new(None)),
            ai_manager: Arc::new(RwLock::new(None)),
            dtmf_handler: Arc::new(RwLock::new(None)),
            dtmf_task: Arc::new(RwLock::new(None)),
            pin_auth: Arc::new(RwLock::new(None)),
            room_config: Arc::new(RwLock::new(None)),
            call_id_map: Arc::new(DashMap::new()),
            is_locked: Arc::new(RwLock::new(false)),
            hosts: Arc::new(DashMap::new()),
            waiting_participants: Arc::new(DashMap::new()),
            audio_feedback_player: Arc::new(RwLock::new(None)),
            conference_sounds: Arc::new(RwLock::new(None)),
            format,
            _frame_size: frame_size,
        })
    }

    /// Configure room with PIN authentication and room-specific settings
    ///
    /// # Arguments
    /// * `room_config` - Room-specific configuration (overrides global defaults)
    /// * `global_config` - Global conference configuration (provides defaults)
    pub fn configure(
        &self,
        room_config: crate::room_config::RoomConfig,
        global_config: &crate::config::ConferenceConfig,
    ) -> Result<()> {
        info!("Configuring room {} with custom settings", self.id);

        // Merge room config with global defaults
        let effective_config = room_config.merge_with_global(global_config);

        // Create PIN authenticator from effective security config
        let pin_auth = PinAuthenticator::new(effective_config.security.clone());

        // Store configuration
        *self.pin_auth.write() = Some(pin_auth);
        *self.room_config.write() = Some(effective_config.clone());

        // Set initial lock state
        *self.is_locked.write() = effective_config.security.default_locked;

        // Initialize audio feedback if any sounds are configured
        let has_sounds = effective_config.join_sound.is_some()
            || effective_config.exit_sound.is_some()
            || effective_config.alone_sound.is_some()
            || effective_config.invalid_pin_sound.is_some()
            || effective_config.locked_sound.is_some()
            || effective_config.unlocked_sound.is_some()
            || effective_config.kicked_sound.is_some()
            || effective_config.max_members_sound.is_some()
            || effective_config.moh_sound.is_some()
            || effective_config.pin_prompt_sound.is_some()
            || effective_config.recording_started_sound.is_some()
            || effective_config.recording_stopped_sound.is_some();

        if has_sounds {
            // Create audio feedback player with room's sample rate
            let player = crate::audio_feedback::AudioFeedbackPlayer::new(self.format.sample_rate);

            // Load all configured sounds
            let sounds = crate::audio_feedback::ConferenceSounds::load_from_config(
                &effective_config,
                &player,
            );

            *self.audio_feedback_player.write() = Some(player);
            *self.conference_sounds.write() = Some(sounds);

            // Add audio feedback as a virtual participant to the mixer
            self.mixer
                .add_participant(AUDIO_FEEDBACK_PARTICIPANT_ID, None)?;

            info!("Audio feedback enabled for room {}", self.id);
        }

        debug!(
            "Room {} configured: require_guest_pin={}, default_locked={}, audio_feedback={}",
            self.id,
            effective_config.security.require_guest_pin,
            effective_config.security.default_locked,
            has_sounds
        );

        Ok(())
    }

    /// Authenticate a participant with a PIN
    ///
    /// # Arguments
    /// * `participant_id` - Unique identifier for the participant
    /// * `pin` - The PIN entered by the participant
    ///
    /// # Returns
    /// `PinAuthResult` indicating success, failure, or lockout
    pub fn authenticate_participant(
        &self,
        participant_id: &str,
        pin: &str,
    ) -> crate::pin_auth::PinAuthResult {
        let guard = self.pin_auth.read();
        match guard.as_ref() {
            Some(pin_auth) => {
                let result = pin_auth.authenticate(participant_id, pin);
                debug!(
                    "PIN authentication for participant {} in room {}: {:?}",
                    participant_id, self.id, result
                );
                result
            }
            None => {
                // No PIN authenticator configured, allow without PIN
                debug!(
                    "No PIN authentication configured for room {}, allowing participant {}",
                    self.id, participant_id
                );
                crate::pin_auth::PinAuthResult::NoPinRequired
            }
        }
    }

    /// Check if a participant is locked out due to failed PIN attempts
    pub fn is_participant_locked_out(&self, participant_id: &str) -> bool {
        let guard = self.pin_auth.read();
        match guard.as_ref() {
            Some(pin_auth) => pin_auth.is_locked_out(participant_id),
            None => false,
        }
    }

    /// Clear lockout for a participant (admin override)
    pub fn clear_participant_lockout(&self, participant_id: &str) {
        let guard = self.pin_auth.read();
        if let Some(pin_auth) = guard.as_ref() {
            pin_auth.clear_lockout(participant_id);
            info!(
                "Cleared lockout for participant {} in room {}",
                self.id, participant_id
            );
        }
    }

    /// Get the room's effective configuration
    pub fn room_config(&self) -> Option<EffectiveRoomConfig> {
        self.room_config.read().clone()
    }

    /// Get the room ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Check if a participant is a host
    pub fn is_host(&self, participant_id: &str) -> bool {
        self.hosts.contains_key(participant_id)
    }

    /// Get the number of hosts in the room
    pub fn host_count(&self) -> usize {
        self.hosts.len()
    }

    /// Get the number of participants waiting for moderator
    pub fn waiting_count(&self) -> usize {
        self.waiting_participants.len()
    }

    /// Get the list of waiting participant IDs
    pub fn waiting_participants(&self) -> Vec<String> {
        self.waiting_participants
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get the effective room configuration
    pub fn get_effective_config(&self) -> Option<crate::room_config::EffectiveRoomConfig> {
        self.room_config.read().clone()
    }

    /// Promote a participant to host
    pub fn promote_to_host(&self, participant_id: &str) -> Result<()> {
        // Check if participant exists in the room
        if !self.participants().contains(&participant_id.to_string()) {
            return Err(ConferenceError::Internal(format!(
                "Participant {} not found in room {}",
                participant_id, self.id
            )));
        }

        // Add to hosts
        self.hosts.insert(participant_id.to_string(), ());

        info!(
            "Promoted participant {} to host in room {}",
            participant_id, self.id
        );

        Ok(())
    }

    /// Check if the minimum user requirement is met
    pub fn meets_min_users_requirement(&self) -> bool {
        let config = self.room_config.read();
        if let Some(cfg) = config.as_ref() {
            self.mixer.participant_count() >= cfg.min_users
        } else {
            true // No requirement configured
        }
    }

    /// Check if conference is at capacity
    pub fn is_at_capacity(&self) -> bool {
        let config = self.room_config.read();
        if let Some(cfg) = config.as_ref() {
            if cfg.max_channels > 0 {
                let current_count =
                    self.mixer.participant_count() + self.waiting_participants.len();
                return current_count >= cfg.max_channels;
            }
        }
        false
    }

    /// Add a participant to the room
    ///
    /// This method enforces:
    /// - Capacity limits (max_channels)
    /// - Wait-for-moderator requirement
    /// - Conference lock status
    ///
    /// # Arguments
    /// * `participant_id` - Unique identifier for the participant
    /// * `is_host` - Whether this participant is joining as a host/moderator
    pub fn add_participant<S: Into<String>>(&self, participant_id: S, is_host: bool) -> Result<()> {
        let participant_id = participant_id.into();

        // Check if conference is locked
        if *self.is_locked.read() {
            warn!(
                "Participant {} denied entry - room {} is locked",
                participant_id, self.id
            );
            return Err(ConferenceError::ConferenceLocked);
        }

        // Get effective configuration
        let config = self.room_config.read();
        let config = config.as_ref();

        // Check capacity limits (if configured)
        if let Some(cfg) = config {
            if cfg.max_channels > 0 {
                let current_count =
                    self.mixer.participant_count() + self.waiting_participants.len();
                if current_count >= cfg.max_channels {
                    warn!(
                        "Participant {} denied entry - room {} is full ({}/{})",
                        participant_id, self.id, current_count, cfg.max_channels
                    );
                    return Err(ConferenceError::ConferenceFull(cfg.max_channels));
                }
            }

            // Check wait-for-moderator requirement
            if cfg.wait_for_moderator && !is_host && self.hosts.is_empty() {
                info!(
                    "Participant {} waiting for moderator in room {}",
                    participant_id, self.id
                );
                self.waiting_participants.insert(participant_id.clone(), ());
                return Err(ConferenceError::WaitingForModerator);
            }
        }

        // If this is a host, track them
        if is_host {
            info!("Adding host {} to room {}", participant_id, self.id);
            self.hosts.insert(participant_id.clone(), ());

            // Release waiting participants when first host joins
            self.release_waiting_participants();
        }

        info!("Adding participant {} to room {}", participant_id, self.id);
        self.mixer.add_participant(&participant_id, None)?;

        // Play join sound
        self.play_join_sound();

        // Update metrics
        counter!("forge_conference_participants_joined_total", 1, "room_id" => self.id.clone());
        gauge!("forge_conference_participants_active", self.mixer.participant_count() as f64, "room_id" => self.id.clone());

        // Check if we should auto-start recording
        self.check_auto_start_recording();

        Ok(())
    }

    /// Release all waiting participants (called when host joins)
    fn release_waiting_participants(&self) {
        if self.waiting_participants.is_empty() {
            return;
        }

        let waiting: Vec<String> = self
            .waiting_participants
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for participant_id in waiting {
            info!(
                "Releasing participant {} from waiting in room {}",
                participant_id, self.id
            );

            // Add to mixer
            if let Err(e) = self.mixer.add_participant(&participant_id, None) {
                warn!(
                    "Failed to add waiting participant {} to room {}: {}",
                    participant_id, self.id, e
                );
            } else {
                self.waiting_participants.remove(&participant_id);
                counter!("forge_conference_participants_joined_total", 1, "room_id" => self.id.clone());
            }
        }

        gauge!("forge_conference_participants_active", self.mixer.participant_count() as f64, "room_id" => self.id.clone());
    }

    /// Check if recording should auto-start based on participant count
    fn check_auto_start_recording(&self) {
        let config = self.room_config.read();
        if let Some(cfg) = config.as_ref() {
            let participant_count = self.mixer.participant_count();

            // Check if we've reached the threshold to start recording
            if participant_count >= cfg.min_recording_participants {
                // Check if recording is not already active
                if self.recorder.read().is_none() {
                    info!(
                        "Auto-starting recording for room {} ({} participants reached threshold of {})",
                        self.id, participant_count, cfg.min_recording_participants
                    );
                    // TODO: Start recording automatically
                    // This will be implemented when we add the recording API
                }
            }
        }
    }

    /// Remove a participant from the room
    pub fn remove_participant(&self, participant_id: &str) -> Result<()> {
        info!(
            "Removing participant {} from room {}",
            participant_id, self.id
        );

        // Remove from hosts if they were a host
        let was_host = self.hosts.remove(participant_id).is_some();

        // Remove from waiting participants if they were waiting
        self.waiting_participants.remove(participant_id);

        // Remove from mixer
        self.mixer.remove_participant(participant_id)?;

        // Play exit sound
        self.play_exit_sound();

        // Check if only one participant is left (alone sound)
        // Count actual participants (excluding audio feedback participant)
        let remaining_count = self.mixer.participant_count();
        if remaining_count == 2 {
            // 1 real participant + 1 audio feedback participant = 2 total
            self.play_alone_sound();
        }

        // Update metrics
        counter!("forge_conference_participants_left_total", 1, "room_id" => self.id.clone());
        gauge!("forge_conference_participants_active", self.mixer.participant_count() as f64, "room_id" => self.id.clone());

        // If the last host left and wait_for_moderator is enabled,
        // move active participants to waiting
        if was_host && self.hosts.is_empty() {
            self.move_participants_to_waiting();
        }

        Ok(())
    }

    /// Move all active participants to waiting (when last host leaves)
    fn move_participants_to_waiting(&self) {
        let config = self.room_config.read();
        if let Some(cfg) = config.as_ref() {
            if cfg.wait_for_moderator {
                // Get list of participant IDs from mixer
                let participants = self.mixer.participants();

                for participant_id in participants {
                    info!(
                        "Moving participant {} to waiting (no moderator) in room {}",
                        participant_id, self.id
                    );

                    // Remove from mixer
                    if let Err(e) = self.mixer.remove_participant(&participant_id) {
                        warn!(
                            "Failed to remove participant {} from mixer: {}",
                            participant_id, e
                        );
                        continue;
                    }

                    // Add to waiting
                    self.waiting_participants.insert(participant_id, ());
                }
            }
        }
    }

    /// Write audio samples from a participant
    pub fn write_audio(&self, participant_id: &str, samples: &[i16]) -> Result<()> {
        Ok(self.mixer.write_samples(participant_id, samples)?)
    }

    /// Write RTP payload from a participant
    pub fn write_rtp_payload(&self, participant_id: &str, payload: &Bytes) -> Result<()> {
        Ok(self.mixer.write_rtp_payload(participant_id, payload)?)
    }

    /// Mix audio for all participants
    ///
    /// Returns the mixed audio if enough data is available
    pub fn mix(&self) -> Result<Option<Vec<i16>>> {
        // Start timing mixing operation
        let mix_start = std::time::Instant::now();

        let mixed = self.mixer.mix()?;

        // Record mixing duration if mixing occurred
        if mixed.is_some() {
            let mix_duration = mix_start.elapsed();
            histogram!("forge_conference_mixing_duration_seconds", mix_duration.as_secs_f64(), "room_id" => self.id.clone());
            counter!("forge_conference_mix_operations_total", 1, "room_id" => self.id.clone());
        }

        // Write to recorder if recording
        if let Some(ref samples) = mixed {
            if let Some(recorder) = self.recorder.read().as_ref() {
                if recorder.is_recording() {
                    recorder.write_samples(samples)?;
                }
            }
        }

        Ok(mixed)
    }

    /// Mix audio for a specific participant (excluding their own audio)
    pub fn mix_for_participant(&self, participant_id: &str) -> Result<Option<Vec<i16>>> {
        Ok(self.mixer.mix_excluding(participant_id)?)
    }

    /// Get audio from all participants individually (for AI Individual mode)
    ///
    /// Returns a map of participant IDs to their audio samples.
    /// Only returns participants that have enough samples available.
    ///
    /// # Arguments
    /// * `count` - Number of samples to retrieve per participant
    /// * `exclude_id` - Optional participant ID to exclude (e.g., "__ai__")
    pub fn get_all_participant_audio(
        &self,
        count: usize,
        exclude_id: Option<&str>,
    ) -> std::collections::HashMap<String, Vec<i16>> {
        self.mixer.get_all_participant_audio(count, exclude_id)
    }

    /// Start recording the conference
    ///
    /// # Arguments
    /// * `output_path` - Path where the recording will be saved
    /// * `format` - Optional format override (uses room's format if None)
    pub async fn start_recording<P: AsRef<Path>>(
        &self,
        output_path: P,
        format: Option<AudioFormat>,
    ) -> Result<()> {
        let path = output_path.as_ref();
        let recording_format = format.unwrap_or(self.format);

        info!(
            "Starting recording for room {} to {:?} with codec {:?}",
            self.id, path, recording_format.codec
        );

        let recorder = AudioRecorder::new(path, recording_format).await?;
        recorder.start()?;

        *self.recorder.write() = Some(recorder);

        // Play recording started sound
        self.play_recording_started_sound();

        // Update metrics
        counter!("forge_conference_recordings_started_total", 1, "room_id" => self.id.clone());
        gauge!("forge_conference_recordings_active", 1.0, "room_id" => self.id.clone());

        Ok(())
    }

    /// Stop recording the conference
    pub fn stop_recording(&self) -> Result<()> {
        info!("Stopping recording for room {}", self.id);

        let mut recorder_guard = self.recorder.write();
        if let Some(recorder) = recorder_guard.take() {
            recorder.stop()?;

            // Play recording stopped sound
            self.play_recording_stopped_sound();

            // Update metrics
            counter!("forge_conference_recordings_stopped_total", 1, "room_id" => self.id.clone());
            gauge!("forge_conference_recordings_active", 0.0, "room_id" => self.id.clone());

            Ok(())
        } else {
            Err(ConferenceError::RecordingNotFound(format!(
                "No active recording for room {}",
                self.id
            )))
        }
    }

    /// Check if the room is currently recording
    pub fn is_recording(&self) -> bool {
        self.recorder
            .read()
            .as_ref()
            .map(|r| r.is_recording())
            .unwrap_or(false)
    }

    /// Get the list of participants in the room
    pub fn participants(&self) -> Vec<String> {
        self.mixer.participants()
    }

    /// Get the number of participants in the room
    pub fn participant_count(&self) -> usize {
        self.mixer.participant_count()
    }

    /// Set gain for a specific participant
    pub fn set_participant_gain(&self, participant_id: &str, gain: f32) -> Result<()> {
        Ok(self.mixer.set_gain(participant_id, gain)?)
    }

    /// Get the audio format
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Start recording for a specific participant
    ///
    /// # Arguments
    /// * `participant_id` - Participant identifier
    /// * `output_path` - Path where the recording will be saved
    pub async fn start_participant_recording<P: AsRef<Path>>(
        &self,
        participant_id: &str,
        output_path: P,
    ) -> Result<()> {
        // Convert to owned PathBuf to ensure Send
        let path = output_path.as_ref().to_path_buf();
        info!(
            "Starting recording for participant {} in room {} to {:?}",
            participant_id, self.id, path
        );

        self.mixer
            .start_participant_recording(participant_id, path)
            .await?;

        // Update metrics
        counter!("forge_conference_participant_recordings_started_total", 1,
            "room_id" => self.id.clone(),
            "participant_id" => participant_id.to_string());

        Ok(())
    }

    /// Stop recording for a specific participant
    pub fn stop_participant_recording(&self, participant_id: &str) -> Result<()> {
        info!(
            "Stopping recording for participant {} in room {}",
            participant_id, self.id
        );
        self.mixer.stop_participant_recording(participant_id)?;

        // Update metrics
        counter!("forge_conference_participant_recordings_stopped_total", 1,
            "room_id" => self.id.clone(),
            "participant_id" => participant_id.to_string());

        Ok(())
    }

    /// Check if a participant is currently recording
    pub fn is_participant_recording(&self, participant_id: &str) -> Result<bool> {
        Ok(self.mixer.is_participant_recording(participant_id)?)
    }

    /// Play an announcement/IVR prompt into the conference.
    ///
    /// This loads a local audio file and injects it as a temporary participant so it
    /// is included in the mixed output.
    pub async fn play_announcement<P: AsRef<Path>>(&self, audio_path: P) -> Result<()> {
        const ANNOUNCER_ID: &str = "__forge_announcement__";

        // Ensure announcer participant exists
        let _ = self
            .mixer
            .add_participant(ANNOUNCER_ID.to_string(), Some(1.0));

        let mut playback = PlaybackSource::open(audio_path)
            .map_err(|e| ConferenceError::RecordingNotFound(format!("Playback failed: {}", e)))?;

        while let Some(chunk) = playback.next_samples(self._frame_size).map_err(|e| {
            ConferenceError::RecordingNotFound(format!("Playback read failed: {}", e))
        })? {
            self.mixer
                .write_samples(ANNOUNCER_ID, &chunk)
                .map_err(|e| {
                    ConferenceError::RecordingNotFound(format!("Playback injection failed: {}", e))
                })?;
            // Trigger mixing so recorder (if active) captures the injected audio
            let _ = self.mix();
        }

        // Cleanup announcer track
        let _ = self.mixer.remove_participant(ANNOUNCER_ID);
        Ok(())
    }

    /// Get metadata for a specific participant
    pub fn get_participant_metadata(
        &self,
        participant_id: &str,
    ) -> Result<forge_mixer::ParticipantMetadata> {
        Ok(self.mixer.get_participant_metadata(participant_id)?)
    }

    /// Set the state for a specific participant
    pub fn set_participant_state(
        &self,
        participant_id: &str,
        state: forge_mixer::ParticipantState,
    ) -> Result<()> {
        Ok(self.mixer.set_participant_state(participant_id, state)?)
    }

    /// Get metadata for all participants in the room
    pub fn get_all_participant_metadata(&self) -> Vec<forge_mixer::ParticipantMetadata> {
        self.mixer.get_all_participant_metadata()
    }

    /// Attach AI to this conference room
    ///
    /// # Arguments
    /// * `ai_manager` - The AI manager to attach
    /// * `event_bus` - Optional event bus for DTMF forwarding
    pub async fn attach_ai(
        self: &Arc<Self>,
        ai_manager: crate::ai_manager::ConferenceAIManager,
        event_bus: Option<Arc<EventBus>>,
    ) -> Result<()> {
        // Check if AI already attached
        {
            let guard = self.ai_manager.read();
            if guard.is_some() {
                return Err(ConferenceError::AlreadyHasAI(self.id.clone()));
            }
        } // read guard dropped here

        // Add AI participant to mixer
        self.mixer
            .add_participant(ai_manager.participant_id(), None)?;

        // Start AI manager (spawns audio routing tasks) - no lock held
        if let Err(err) = ai_manager.start(Arc::clone(self), event_bus).await {
            // Roll back the mixer participant on failure so we don't leak a track
            let _ = self.mixer.remove_participant(ai_manager.participant_id());
            return Err(err);
        }

        info!("Attached AI to conference room {}", self.id);

        // Store the AI manager after successful start
        {
            let mut guard = self.ai_manager.write();
            *guard = Some(ai_manager);
        }

        Ok(())
    }

    /// Detach AI from this conference room
    pub async fn detach_ai(&self) -> Result<()> {
        // Extract ai_manager in a scoped block to ensure the write guard is dropped
        let ai_manager = {
            let mut guard = self.ai_manager.write();
            guard
                .take()
                .ok_or_else(|| ConferenceError::NoAIAttached(self.id.clone()))?
        }; // write guard dropped here

        // Stop AI manager (this will abort tasks)
        ai_manager.stop().await?;

        // Remove AI participant from mixer
        self.mixer.remove_participant(ai_manager.participant_id())?;

        info!("Detached AI from conference room {}", self.id);

        Ok(())
    }

    /// Check if AI is attached to this room
    pub fn has_ai(&self) -> bool {
        self.ai_manager.read().is_some()
    }

    /// Get AI state if attached
    pub fn ai_state(&self) -> Option<crate::ai_manager::ConferenceAIState> {
        self.ai_manager.read().as_ref().map(|ai| ai.state())
    }

    /// Get AI configuration if attached
    pub fn ai_config(&self) -> Option<crate::ai_manager::ConferenceAIConfig> {
        self.ai_manager
            .read()
            .as_ref()
            .map(|ai| ai.config().clone())
    }

    // ============================================================================
    // DTMF Command Management
    // ============================================================================

    /// Enable DTMF command processing for this conference room
    ///
    /// # Arguments
    /// * `self_ref` - Arc reference to self (needed for spawning tasks)
    /// * `event_bus` - EventBus to subscribe to DTMF events
    /// * `config` - DTMF command configuration
    pub async fn enable_dtmf_commands(
        self: &Arc<Self>,
        event_bus: Arc<EventBus>,
        config: ConferenceDtmfConfig,
    ) -> Result<()> {
        info!("Enabling DTMF commands for room {}", self.id);

        // Create DTMF command handler
        let handler = DtmfCommandHandler::new(self.id.clone(), config);

        // Store handler
        *self.dtmf_handler.write() = Some(handler);

        // Spawn DTMF event processing task
        let task = self.spawn_dtmf_processing_task(event_bus).await;
        *self.dtmf_task.write() = Some(task);

        info!("DTMF commands enabled for room {}", self.id);
        Ok(())
    }

    /// Disable DTMF command processing
    pub async fn disable_dtmf_commands(&self) -> Result<()> {
        info!("Disabling DTMF commands for room {}", self.id);

        // Abort DTMF task
        if let Some(handle) = self.dtmf_task.write().take() {
            handle.abort();
        }

        // Remove handler
        *self.dtmf_handler.write() = None;

        info!("DTMF commands disabled for room {}", self.id);
        Ok(())
    }

    /// Register a participant's call_id for DTMF event routing
    ///
    /// Call this when a participant joins with their SIP call_id
    pub fn register_participant_call_id(&self, participant_id: String, call_id: CallId) {
        debug!(
            "Registering call_id {:?} for participant {} in room {}",
            call_id, participant_id, self.id
        );
        self.call_id_map.insert(call_id, participant_id);
    }

    /// Look up participant_id for a given call_id (used for DTMF scoping)
    pub(crate) fn participant_id_for_call_id(&self, call_id: &CallId) -> Option<String> {
        self.call_id_map
            .get(call_id)
            .map(|entry| entry.value().clone())
    }

    /// Unregister a participant's call_id
    pub fn unregister_participant_call_id(&self, call_id: &CallId) {
        if let Some((_, participant_id)) = self.call_id_map.remove(call_id) {
            debug!(
                "Unregistered call_id {:?} for participant {} in room {}",
                call_id, participant_id, self.id
            );

            // Clear DTMF sequence state for this participant
            if let Some(handler) = self.dtmf_handler.read().as_ref() {
                handler.clear_participant(&participant_id);
            }
        }
    }

    /// Check if conference is locked
    pub fn is_locked(&self) -> bool {
        *self.is_locked.read()
    }

    /// Lock the conference (prevent new participants)
    pub fn lock(&self) {
        *self.is_locked.write() = true;
        info!("Conference {} locked", self.id);
    }

    /// Unlock the conference
    pub fn unlock(&self) {
        *self.is_locked.write() = false;
        info!("Conference {} unlocked", self.id);
    }

    /// Spawn task to process DTMF events from EventBus
    async fn spawn_dtmf_processing_task(
        self: &Arc<Self>,
        event_bus: Arc<EventBus>,
    ) -> JoinHandle<()> {
        let room = Arc::clone(self);
        let room_id = self.id.clone();

        tokio::spawn(async move {
            let mut rx = event_bus.subscribe();
            debug!("DTMF processing task started for room {}", room_id);

            loop {
                match rx.recv().await {
                    Ok(event) => {
                        // Filter for DTMF events
                        if let ForgeEvent::DtmfDigitDetected {
                            call_id,
                            digit,
                            event_type,
                            ..
                        } = event
                        {
                            // Only process End events to avoid duplicates
                            if event_type != DtmfEventKind::End {
                                continue;
                            }

                            // Check if this call_id belongs to our room
                            if let Some(entry) = room.call_id_map.get(&call_id) {
                                let participant_id = entry.value().clone();
                                drop(entry); // Release the reference

                                debug!(
                                    "DTMF digit '{}' from participant {} in room {}",
                                    digit, participant_id, room_id
                                );

                                // Process the digit
                                room.process_dtmf_digit(&participant_id, digit).await;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Error receiving event in room {}: {}", room_id, e);
                        break;
                    }
                }
            }

            debug!("DTMF processing task ended for room {}", room_id);
        })
    }

    /// Process a DTMF digit from a participant
    async fn process_dtmf_digit(&self, participant_id: &str, digit: char) {
        // Process digit through handler (lock held briefly)
        let command_result = {
            let guard = self.dtmf_handler.read();
            match guard.as_ref() {
                Some(handler) => handler.process_digit(participant_id, digit),
                None => return, // DTMF not enabled
            }
        }; // Lock released here

        if let Some((command, role)) = command_result {
            info!(
                "Executing DTMF command {:?} from {} (role: {:?}) in room {}",
                command, participant_id, role, self.id
            );

            // Execute the command
            if let Err(e) = self
                .execute_dtmf_command(participant_id, command, role)
                .await
            {
                warn!(
                    "Failed to execute DTMF command for {} in room {}: {}",
                    participant_id, self.id, e
                );
            }
        }
    }

    /// Execute a DTMF command
    async fn execute_dtmf_command(
        &self,
        participant_id: &str,
        command: DtmfCommand,
        role: ParticipantRole,
    ) -> Result<()> {
        use forge_mixer::ParticipantState;

        match command {
            // Participant commands
            DtmfCommand::MuteToggle => {
                // Toggle mute state
                let metadata = self.mixer.get_participant_metadata(participant_id)?;
                let new_state = match metadata.state {
                    ParticipantState::Active => ParticipantState::Muted,
                    ParticipantState::Muted => ParticipantState::Active,
                    other => other, // Keep OnHold as is
                };
                self.mixer
                    .set_participant_state(participant_id, new_state)?;
                info!(
                    "Toggled mute for {} in room {}: {:?}",
                    participant_id, self.id, new_state
                );
            }

            DtmfCommand::VolumeUp => {
                // TODO: Add set_participant_gain() method to AudioMixer
                // For now, just log the request
                info!(
                    "Volume up requested for {} in room {} (not yet implemented)",
                    participant_id, self.id
                );
            }

            DtmfCommand::VolumeDown => {
                // TODO: Add set_participant_gain() method to AudioMixer
                // For now, just log the request
                info!(
                    "Volume down requested for {} in room {} (not yet implemented)",
                    participant_id, self.id
                );
            }

            DtmfCommand::Info => {
                // Log conference info (in future, could play audio announcement)
                let participant_count = self.mixer.participant_count();
                info!(
                    "Conference info requested by {} in room {}: {} participants",
                    participant_id, self.id, participant_count
                );
                // TODO: Play audio announcement with conference info
            }

            DtmfCommand::Leave => {
                // Participant wants to leave
                info!("Participant {} leaving room {}", participant_id, self.id);
                self.remove_participant(participant_id)?;
            }

            // Host commands (require host role)
            DtmfCommand::MuteAll => {
                if role != ParticipantRole::Host {
                    warn!(
                        "Non-host {} tried to use MuteAll in room {}",
                        participant_id, self.id
                    );
                    return Ok(());
                }

                // Mute all participants except the host
                let participants = self.participants();
                for pid in participants {
                    if pid != participant_id {
                        self.mixer
                            .set_participant_state(&pid, ParticipantState::Muted)?;
                    }
                }
                info!(
                    "Host {} muted all participants in room {}",
                    participant_id, self.id
                );
            }

            DtmfCommand::UnmuteAll => {
                if role != ParticipantRole::Host {
                    warn!(
                        "Non-host {} tried to use UnmuteAll in room {}",
                        participant_id, self.id
                    );
                    return Ok(());
                }

                // Unmute all participants
                let participants = self.participants();
                for pid in participants {
                    let metadata = self.mixer.get_participant_metadata(&pid)?;
                    if metadata.state == ParticipantState::Muted {
                        self.mixer
                            .set_participant_state(&pid, ParticipantState::Active)?;
                    }
                }
                info!(
                    "Host {} unmuted all participants in room {}",
                    participant_id, self.id
                );
            }

            DtmfCommand::LockConference => {
                if role != ParticipantRole::Host {
                    warn!("Non-host {} tried to lock room {}", participant_id, self.id);
                    return Ok(());
                }

                self.lock();
            }

            DtmfCommand::UnlockConference => {
                if role != ParticipantRole::Host {
                    warn!(
                        "Non-host {} tried to unlock room {}",
                        participant_id, self.id
                    );
                    return Ok(());
                }

                self.unlock();
            }

            DtmfCommand::KickParticipant(participant_num) => {
                if role != ParticipantRole::Host {
                    warn!(
                        "Non-host {} tried to kick participant in room {}",
                        participant_id, self.id
                    );
                    return Ok(());
                }

                // Get participants list and kick by index
                let participants = self.participants();
                if participant_num > 0 && participant_num <= participants.len() {
                    let target = &participants[participant_num - 1]; // 1-indexed
                    info!(
                        "Host {} kicking participant {} from room {}",
                        participant_id, target, self.id
                    );
                    self.remove_participant(target)?;
                } else {
                    warn!(
                        "Invalid participant number {} in room {} (total: {})",
                        participant_num,
                        self.id,
                        participants.len()
                    );
                }
            }

            DtmfCommand::EndConference => {
                if role != ParticipantRole::Host {
                    warn!(
                        "Non-host {} tried to end conference {}",
                        participant_id, self.id
                    );
                    return Ok(());
                }

                info!("Host {} ending conference {}", participant_id, self.id);
                // Remove all participants
                let participants = self.participants();
                for pid in participants {
                    self.remove_participant(&pid)?;
                }
            }

            DtmfCommand::InvalidCommand => {
                debug!(
                    "Invalid DTMF command from {} in room {}",
                    participant_id, self.id
                );
                // TODO: Play error tone
            }

            DtmfCommand::EnterHostPin => {
                debug!(
                    "Host PIN accepted for {} in room {}",
                    participant_id, self.id
                );
                // TODO: Play success tone
            }

            DtmfCommand::Incomplete => {
                // Waiting for more digits, do nothing
            }
        }

        Ok(())
    }

    /// Play a sound to all participants
    ///
    /// # Arguments
    /// * `samples` - PCM audio samples to play
    fn play_sound_to_all(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }

        // Write samples to the audio feedback participant
        if let Err(e) = self
            .mixer
            .write_samples(AUDIO_FEEDBACK_PARTICIPANT_ID, samples)
        {
            warn!("Failed to play sound in room {}: {}", self.id, e);
        }
    }

    /// Play the join sound
    fn play_join_sound(&self) {
        let sounds = self.conference_sounds.read();
        if let Some(sounds) = sounds.as_ref() {
            if let Some(samples) = sounds.join_sound.as_ref() {
                debug!("Playing join sound in room {}", self.id);
                self.play_sound_to_all(samples);
            }
        }
    }

    /// Play the exit sound
    fn play_exit_sound(&self) {
        let sounds = self.conference_sounds.read();
        if let Some(sounds) = sounds.as_ref() {
            if let Some(samples) = sounds.exit_sound.as_ref() {
                debug!("Playing exit sound in room {}", self.id);
                self.play_sound_to_all(samples);
            }
        }
    }

    /// Play the alone sound (last participant remaining)
    fn play_alone_sound(&self) {
        let sounds = self.conference_sounds.read();
        if let Some(sounds) = sounds.as_ref() {
            if let Some(samples) = sounds.alone_sound.as_ref() {
                debug!("Playing alone sound in room {}", self.id);
                self.play_sound_to_all(samples);
            }
        }
    }

    /// Play the recording started sound
    fn play_recording_started_sound(&self) {
        let sounds = self.conference_sounds.read();
        if let Some(sounds) = sounds.as_ref() {
            if let Some(samples) = sounds.recording_started_sound.as_ref() {
                debug!("Playing recording started sound in room {}", self.id);
                self.play_sound_to_all(samples);
            }
        }
    }

    /// Play the recording stopped sound
    fn play_recording_stopped_sound(&self) {
        let sounds = self.conference_sounds.read();
        if let Some(sounds) = sounds.as_ref() {
            if let Some(samples) = sounds.recording_stopped_sound.as_ref() {
                debug!("Playing recording stopped sound in room {}", self.id);
                self.play_sound_to_all(samples);
            }
        }
    }
}

/// Conference bridge for managing multiple conference rooms
pub struct ConferenceBridge {
    /// Map of active conference rooms
    rooms: Arc<DashMap<RoomId, Arc<ConferenceRoom>>>,
    /// Default audio format for new rooms
    default_format: AudioFormat,
    /// Default frame size for mixing
    default_frame_size: usize,
}

impl ConferenceBridge {
    /// Create a new conference bridge
    ///
    /// # Arguments
    /// * `default_format` - Default audio format for new rooms
    /// * `default_frame_size` - Default frame size for mixing (e.g., 480 for 10ms at 48kHz)
    pub fn new(default_format: AudioFormat, default_frame_size: usize) -> Result<Self> {
        info!("Creating conference bridge");

        Ok(Self {
            rooms: Arc::new(DashMap::new()),
            default_format,
            default_frame_size,
        })
    }

    /// Create a new conference room
    ///
    /// # Arguments
    /// * `room_id` - Unique room identifier
    /// * `format` - Optional audio format (uses default if not specified)
    pub fn create_room<S: Into<String>>(
        &self,
        room_id: S,
        format: Option<AudioFormat>,
    ) -> Result<Arc<ConferenceRoom>> {
        let room_id = room_id.into();

        if self.rooms.contains_key(&room_id) {
            return Err(ConferenceError::Internal(format!(
                "Room {} already exists",
                room_id
            )));
        }

        let format = format.unwrap_or(self.default_format);
        let room = Arc::new(ConferenceRoom::new(
            &room_id,
            format,
            self.default_frame_size,
        )?);

        self.rooms.insert(room_id.clone(), room.clone());
        info!("Created conference room: {}", room_id);

        // Update metrics
        counter!("forge_conference_rooms_created_total", 1);
        gauge!("forge_conference_rooms_active", self.rooms.len() as f64);

        Ok(room)
    }

    /// Get a conference room by ID
    pub fn get_room(&self, room_id: &str) -> Result<Arc<ConferenceRoom>> {
        self.rooms
            .get(room_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| {
                ConferenceError::ConferenceNotFound(format!("Room {} not found", room_id))
            })
    }

    /// Delete a conference room
    pub fn delete_room(&self, room_id: &str) -> Result<()> {
        info!("Deleting conference room: {}", room_id);

        let (_, room) = self.rooms.remove(room_id).ok_or_else(|| {
            ConferenceError::ConferenceNotFound(format!("Room {} not found", room_id))
        })?;

        // Stop recording if active
        if room.is_recording() {
            if let Err(e) = room.stop_recording() {
                warn!("Error stopping recording for room {}: {}", room_id, e);
            }
        }

        // Update metrics
        counter!("forge_conference_rooms_deleted_total", 1);
        gauge!("forge_conference_rooms_active", self.rooms.len() as f64);

        Ok(())
    }

    /// List all active room IDs
    pub fn list_rooms(&self) -> Vec<String> {
        self.rooms.iter().map(|r| r.key().clone()).collect()
    }

    /// Get the number of active rooms
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Get or create a conference room
    ///
    /// If the room exists, returns it. Otherwise, creates a new room.
    pub fn get_or_create_room<S: Into<String>>(
        &self,
        room_id: S,
        format: Option<AudioFormat>,
    ) -> Result<Arc<ConferenceRoom>> {
        let room_id = room_id.into();

        if let Some(room) = self.rooms.get(&room_id) {
            Ok(room.value().clone())
        } else {
            self.create_room(room_id, format)
        }
    }

    /// Add a participant to a room
    pub fn add_participant_to_room(
        &self,
        room_id: &str,
        participant_id: &str,
        is_host: bool,
    ) -> Result<()> {
        let room = self.get_room(room_id)?;
        room.add_participant(participant_id, is_host)
    }

    /// Remove a participant from a room
    pub fn remove_participant_from_room(&self, room_id: &str, participant_id: &str) -> Result<()> {
        let room = self.get_room(room_id)?;
        room.remove_participant(participant_id)
    }

    /// Get total number of participants across all rooms
    pub fn total_participants(&self) -> usize {
        self.rooms
            .iter()
            .map(|r| r.value().participant_count())
            .sum()
    }
}

impl Default for ConferenceBridge {
    fn default() -> Self {
        Self::new(AudioFormat::pcm_mono(), 480).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_conference_room_lifecycle() {
        let room = ConferenceRoom::new("test-room", AudioFormat::pcm_mono(), 480).unwrap();

        // Add participants
        room.add_participant("alice", false).unwrap();
        room.add_participant("bob", false).unwrap();

        assert_eq!(room.participant_count(), 2);
        assert!(room.participants().contains(&"alice".to_string()));

        // Remove participant
        room.remove_participant("alice").unwrap();
        assert_eq!(room.participant_count(), 1);
    }

    #[tokio::test]
    async fn test_room_recording() {
        let temp_dir = TempDir::new().unwrap();
        let recording_path = temp_dir.path().join("conference.wav");

        let room = ConferenceRoom::new("test-room", AudioFormat::pcm_mono(), 480).unwrap();

        // Start recording
        room.start_recording(&recording_path, None).await.unwrap();
        assert!(room.is_recording());

        // Add participant and write audio
        room.add_participant("alice", false).unwrap();
        let samples: Vec<i16> = vec![100; 480];
        room.write_audio("alice", &samples).unwrap();

        // Mix audio (should write to recorder)
        let mixed = room.mix().unwrap();
        assert!(mixed.is_some());

        // Stop recording
        room.stop_recording().unwrap();
        assert!(!room.is_recording());

        // Verify file exists
        assert!(recording_path.exists());
    }

    #[test]
    fn test_conference_bridge() {
        let bridge = ConferenceBridge::default();

        // Create rooms
        let room1 = bridge.create_room("room-1", None).unwrap();
        let room2 = bridge.create_room("room-2", None).unwrap();

        assert_eq!(bridge.room_count(), 2);
        assert!(bridge.list_rooms().contains(&"room-1".to_string()));

        // Add participants
        bridge
            .add_participant_to_room("room-1", "alice", false)
            .unwrap();
        bridge
            .add_participant_to_room("room-1", "bob", false)
            .unwrap();
        bridge
            .add_participant_to_room("room-2", "charlie", false)
            .unwrap();

        assert_eq!(bridge.total_participants(), 3);
        assert_eq!(room1.participant_count(), 2);
        assert_eq!(room2.participant_count(), 1);

        // Delete room
        bridge.delete_room("room-1").unwrap();
        assert_eq!(bridge.room_count(), 1);
    }

    #[test]
    fn test_get_or_create_room() {
        let bridge = ConferenceBridge::default();

        // Create new room
        let room1 = bridge.get_or_create_room("test-room", None).unwrap();
        assert_eq!(room1.id(), "test-room");

        // Get existing room
        let room2 = bridge.get_or_create_room("test-room", None).unwrap();
        assert_eq!(room1.id(), room2.id());

        // Should be the same instance
        assert_eq!(bridge.room_count(), 1);
    }

    #[test]
    fn test_mix_for_participant() {
        let room = ConferenceRoom::new("test-room", AudioFormat::pcm_mono(), 480).unwrap();

        room.add_participant("alice", false).unwrap();
        room.add_participant("bob", false).unwrap();

        // Write audio
        let samples: Vec<i16> = vec![100; 480];
        room.write_audio("alice", &samples).unwrap();
        room.write_audio("bob", &samples).unwrap();

        // Mix excluding alice
        let mixed = room.mix_for_participant("alice").unwrap();
        assert!(mixed.is_some());
    }
}
