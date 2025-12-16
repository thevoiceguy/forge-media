//! AI conference participant management
//!
//! Integrates AI (via OpenAI Realtime API) as a virtual participant in conference rooms

use crate::{Result, RoomId};
use forge_core::CallId;
use forge_engine::ai_integration::{AISessionManager, AISessionState};
use forge_mixer::AudioFormat;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// Special participant ID for AI in the mixer
pub const AI_PARTICIPANT_ID: &str = "__ai__";

/// Default audio sample rate for AI (16kHz is standard for speech)
pub const AI_SAMPLE_RATE: u32 = 16000;

/// Default frame size for AI audio chunks (20ms @ 16kHz = 320 samples)
pub const AI_FRAME_SIZE: usize = 320;

/// Default polling interval for AI responses
pub const AI_POLL_INTERVAL_MS: u64 = 100;

/// Sleep duration for audio routing task
pub const AUDIO_TASK_SLEEP_MS: u64 = 20;

/// Audio routing mode for AI participant
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioMode {
    /// AI hears all participants mixed together (default)
    /// - Lower CPU usage
    /// - Simpler implementation
    /// - Good for conversation and Q&A
    /// - **Status**: ✅ Fully implemented
    Mixed,

    /// AI receives separate labeled audio streams per participant
    /// - Higher CPU usage (scales with participant count)
    /// - Better speaker identification
    /// - Required for accurate transcription with speaker attribution
    /// - **Status**: ⚠️ Not yet implemented (requires mixer enhancements)
    ///
    /// To implement this mode, the mixer needs a method to extract per-participant
    /// audio buffers before mixing. This is planned for a future release.
    Individual,
}

impl Default for AudioMode {
    fn default() -> Self {
        Self::Mixed
    }
}

/// Configuration for AI conference participant
#[derive(Debug, Clone)]
pub struct ConferenceAIConfig {
    /// Participant ID for AI in the mixer
    pub participant_id: String,

    /// Audio sample rate for AI
    pub sample_rate: u32,

    /// Frame size for audio chunks (samples)
    pub frame_size: usize,

    /// Polling interval for AI responses
    pub poll_interval: Duration,

    /// Enable transcription of all participants
    pub enable_transcription: bool,

    /// Audio routing mode
    pub audio_mode: AudioMode,

    /// Conference audio format
    pub conference_format: AudioFormat,
}

impl ConferenceAIConfig {
    /// Create default configuration with conference format
    pub fn new(conference_format: AudioFormat) -> Self {
        Self {
            participant_id: AI_PARTICIPANT_ID.to_string(),
            sample_rate: AI_SAMPLE_RATE,
            frame_size: AI_FRAME_SIZE,
            poll_interval: Duration::from_millis(AI_POLL_INTERVAL_MS),
            enable_transcription: false,
            audio_mode: AudioMode::Mixed,
            conference_format,
        }
    }

    /// Set audio mode
    pub fn with_audio_mode(mut self, mode: AudioMode) -> Self {
        self.audio_mode = mode;
        self
    }

    /// Enable transcription
    pub fn with_transcription(mut self, enabled: bool) -> Self {
        self.enable_transcription = enabled;
        self
    }
}

/// State of AI participant in conference
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConferenceAIState {
    /// AI is connecting to the service
    Connecting,

    /// AI is active and listening
    Active,

    /// AI is currently speaking
    Speaking,

    /// AI is transcribing participants
    Transcribing,

    /// AI is terminating
    Terminating,

    /// AI has terminated
    Terminated,
}

impl From<AISessionState> for ConferenceAIState {
    fn from(state: AISessionState) -> Self {
        match state {
            AISessionState::Connecting => Self::Connecting,
            AISessionState::Active => Self::Active,
            AISessionState::Speaking => Self::Speaking,
            AISessionState::Disconnecting => Self::Terminating,
            AISessionState::Terminated => Self::Terminated,
        }
    }
}

/// Manager for AI participant in a conference room
pub struct ConferenceAIManager {
    /// Room ID this manager is attached to
    room_id: RoomId,

    /// Call ID for the AI session
    call_id: CallId,

    /// AI session manager from forge-engine
    ai_session_manager: Arc<AISessionManager>,

    /// Audio processing task handle
    audio_task: Arc<RwLock<Option<JoinHandle<()>>>>,

    /// AI response polling task handle
    response_task: Arc<RwLock<Option<JoinHandle<()>>>>,

    /// DTMF forwarding task handle
    dtmf_task: Arc<RwLock<Option<JoinHandle<()>>>>,

    /// Configuration
    config: ConferenceAIConfig,

    /// Current state
    state: Arc<RwLock<ConferenceAIState>>,
}

impl ConferenceAIManager {
    /// Create a new AI manager for a conference room
    ///
    /// # Arguments
    /// * `room_id` - The conference room ID
    /// * `call_id` - The call ID for the AI session
    /// * `ai_session_manager` - The AI session manager
    /// * `config` - Configuration for AI integration
    pub fn new(
        room_id: impl Into<RoomId>,
        call_id: CallId,
        ai_session_manager: Arc<AISessionManager>,
        config: ConferenceAIConfig,
    ) -> Result<Self> {
        let room_id = room_id.into();

        info!(
            "Creating AI manager for room {} with mode {:?}",
            room_id, config.audio_mode
        );

        Ok(Self {
            room_id,
            call_id,
            ai_session_manager,
            audio_task: Arc::new(RwLock::new(None)),
            response_task: Arc::new(RwLock::new(None)),
            dtmf_task: Arc::new(RwLock::new(None)),
            config,
            state: Arc::new(RwLock::new(ConferenceAIState::Connecting)),
        })
    }

    /// Get the room ID
    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    /// Get the AI participant ID
    pub fn participant_id(&self) -> &str {
        &self.config.participant_id
    }

    /// Get current state
    pub fn state(&self) -> ConferenceAIState {
        *self.state.read()
    }

    /// Get the audio mode
    pub fn audio_mode(&self) -> AudioMode {
        self.config.audio_mode
    }

    /// Check if transcription is enabled
    pub fn transcription_enabled(&self) -> bool {
        self.config.enable_transcription
    }

    /// Start AI participant in the conference
    ///
    /// This will:
    /// 1. Spawn audio routing task (conference → AI)
    /// 2. Spawn AI response polling task (AI → conference)
    /// 3. Spawn DTMF forwarding task (conference → AI) if event bus provided
    ///
    /// The AI participant must be added to the mixer before calling this.
    ///
    /// # Arguments
    /// * `room` - Reference to the conference room
    /// * `event_bus` - Optional event bus for DTMF forwarding
    ///
    /// # Supported Audio Modes
    /// - Mixed: AI hears combined audio from all participants (lower CPU)
    /// - Individual: AI receives labeled per-participant streams (better speaker ID)
    pub async fn start(
        &self,
        room: Arc<crate::conference::ConferenceRoom>,
        event_bus: Option<Arc<forge_core::EventBus>>,
    ) -> Result<()> {
        info!(
            "Starting AI manager for room {} in {:?} mode",
            self.room_id, self.config.audio_mode
        );

        // Update state
        *self.state.write() = ConferenceAIState::Active;

        // Spawn audio routing task (conference → AI)
        // Different implementation based on audio mode
        let audio_task = self.spawn_audio_routing_task(room.clone()).await;
        *self.audio_task.write() = Some(audio_task);

        // Spawn AI response polling task (AI → conference)
        let response_task = self.spawn_response_polling_task(room.clone()).await;
        *self.response_task.write() = Some(response_task);

        // Spawn DTMF forwarding task if event bus is provided
        if let Some(event_bus) = event_bus {
            info!("Enabling DTMF forwarding for AI in room {}", self.room_id);
            let dtmf_task = self.spawn_dtmf_forwarding_task(event_bus).await;
            *self.dtmf_task.write() = Some(dtmf_task);
        }

        info!("AI manager started for room {}", self.room_id);

        Ok(())
    }

    /// Spawn task to route conference audio to AI
    ///
    /// Handles both Mixed and Individual audio modes
    async fn spawn_audio_routing_task(
        &self,
        room: Arc<crate::conference::ConferenceRoom>,
    ) -> JoinHandle<()> {
        let ai_session_manager = Arc::clone(&self.ai_session_manager);
        let call_id = self.call_id.clone();
        let participant_id = self.config.participant_id.clone();
        let ai_sample_rate = self.config.sample_rate;
        let conference_sample_rate = self.config.conference_format.sample_rate;
        let room_id = self.room_id.clone();
        let audio_mode = self.config.audio_mode;
        let frame_size = self.config.frame_size;

        tokio::spawn(async move {
            debug!("Audio routing task started for room {} in {:?} mode", room_id, audio_mode);

            loop {
                tokio::time::sleep(Duration::from_millis(AUDIO_TASK_SLEEP_MS)).await;

                match audio_mode {
                    AudioMode::Mixed => {
                        // Get conference mix (excluding AI's own voice)
                        let mixed = match room.mix_for_participant(&participant_id) {
                            Ok(Some(samples)) => samples,
                            Ok(None) => continue, // Not enough audio yet
                            Err(e) => {
                                debug!("Failed to get mix for AI in room {}: {}", room_id, e);
                                continue;
                            }
                        };

                        // Resample if needed
                        let samples_to_send = if conference_sample_rate != ai_sample_rate {
                            Self::resample_audio(&mixed, conference_sample_rate, ai_sample_rate)
                        } else {
                            mixed
                        };

                        // Send to AI via session manager
                        if let Err(e) = ai_session_manager.send_audio(&call_id, &samples_to_send).await {
                            debug!("Failed to send audio to AI in room {}: {}", room_id, e);
                        }
                    }
                    AudioMode::Individual => {
                        // Get audio from each participant individually
                        let participant_audio = room.get_all_participant_audio(frame_size, Some(&participant_id));

                        if participant_audio.is_empty() {
                            continue; // No participants have audio yet
                        }

                        // Send each participant's audio with their label
                        for (pid, samples) in participant_audio {
                            // Resample if needed
                            let samples_to_send = if conference_sample_rate != ai_sample_rate {
                                Self::resample_audio(&samples, conference_sample_rate, ai_sample_rate)
                            } else {
                                samples
                            };

                            // Send labeled audio to AI
                            if let Err(e) = ai_session_manager
                                .send_labeled_audio(&call_id, &pid, &samples_to_send)
                                .await
                            {
                                debug!(
                                    "Failed to send labeled audio from {} to AI in room {}: {}",
                                    pid, room_id, e
                                );
                            }
                        }
                    }
                }
            }
        })
    }

    /// Spawn task to poll AI responses and inject into conference
    async fn spawn_response_polling_task(
        &self,
        room: Arc<crate::conference::ConferenceRoom>,
    ) -> JoinHandle<()> {
        let ai_session_manager = Arc::clone(&self.ai_session_manager);
        let call_id = self.call_id.clone();
        let participant_id = self.config.participant_id.clone();
        let conference_sample_rate = self.config.conference_format.sample_rate;
        let poll_interval = self.config.poll_interval;
        let room_id = self.room_id.clone();

        tokio::spawn(async move {
            debug!("AI response polling task started for room {}", room_id);

            loop {
                tokio::time::sleep(poll_interval).await;

                // Poll for AI audio responses via session manager
                let response = match ai_session_manager.try_recv_audio_response(&call_id).await {
                    Some(resp) => resp,
                    None => continue, // No response yet
                };

                // Resample if needed
                let samples_to_inject = if response.sample_rate != conference_sample_rate {
                    Self::resample_audio(&response.samples, response.sample_rate, conference_sample_rate)
                } else {
                    response.samples.clone()
                };

                // Write to AI participant buffer in mixer
                if let Err(e) = room.write_audio(&participant_id, &samples_to_inject) {
                    debug!("Failed to inject AI audio in room {}: {}", room_id, e);
                }
            }
        })
    }

    /// Spawn task to forward DTMF events to AI
    async fn spawn_dtmf_forwarding_task(
        &self,
        event_bus: Arc<forge_core::EventBus>,
    ) -> JoinHandle<()> {
        let ai_session_manager = Arc::clone(&self.ai_session_manager);
        let call_id = self.call_id.clone();
        let room_id = self.room_id.clone();

        tokio::spawn(async move {
            let mut rx = event_bus.subscribe();
            debug!("DTMF forwarding task started for AI in room {}", room_id);

            loop {
                match rx.recv().await {
                    Ok(event) => {
                        // Filter for DTMF events
                        if let forge_core::ForgeEvent::DtmfDigitDetected {
                            call_id: event_call_id,
                            digit,
                            method,
                            event_type,
                            ..
                        } = event
                        {
                            // Only process "End" events (complete digit presses)
                            // to avoid sending duplicate/partial events
                            if event_type != forge_core::DtmfEventKind::End {
                                continue;
                            }

                            // For conference DTMF, the call_id will be the participant's call_id
                            // We want to forward all conference participant DTMF to the AI
                            debug!(
                                "Forwarding DTMF '{}' from {} to AI in room {}",
                                digit, event_call_id.0, room_id
                            );

                            // Forward to AI
                            if let Err(e) = ai_session_manager
                                .send_dtmf_event(&call_id, digit, method)
                                .await
                            {
                                debug!("Failed to send DTMF to AI in room {}: {}", room_id, e);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            "DTMF event receiver lagged for AI in room {}, skipped {} events",
                            room_id,
                            skipped
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        debug!("Event bus closed for AI in room {}", room_id);
                        break;
                    }
                }
            }

            debug!("DTMF forwarding task stopped for AI in room {}", room_id);
        })
    }

    /// Resample audio using linear interpolation
    ///
    /// This is a simple resampler - for production, consider using a higher-quality
    /// resampling algorithm (e.g., from `rubato` or `samplerate` crates).
    fn resample_audio(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
        if from_rate == to_rate {
            return samples.to_vec();
        }

        let ratio = from_rate as f64 / to_rate as f64;
        let output_len = (samples.len() as f64 / ratio).ceil() as usize;
        let mut output = Vec::with_capacity(output_len);

        for i in 0..output_len {
            let src_index = (i as f64) * ratio;
            let src_index_floor = src_index.floor() as usize;
            let src_index_ceil = (src_index_floor + 1).min(samples.len() - 1);
            let frac = src_index - src_index_floor as f64;

            let sample_a = samples[src_index_floor] as f64;
            let sample_b = samples[src_index_ceil] as f64;
            let interpolated = sample_a + (sample_b - sample_a) * frac;
            output.push(interpolated.round() as i16);
        }

        output
    }

    /// Stop AI participant and cleanup resources
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping AI manager for room {}", self.room_id);

        // Update state
        *self.state.write() = ConferenceAIState::Terminating;

        // Abort audio task
        if let Some(handle) = self.audio_task.write().take() {
            handle.abort();
            debug!("Audio task aborted for room {}", self.room_id);
        }

        // Abort response task
        if let Some(handle) = self.response_task.write().take() {
            handle.abort();
            debug!("Response task aborted for room {}", self.room_id);
        }

        // Abort DTMF task
        if let Some(handle) = self.dtmf_task.write().take() {
            handle.abort();
            debug!("DTMF task aborted for room {}", self.room_id);
        }

        // Detach AI session via manager
        if let Err(e) = self.ai_session_manager.detach_ai(&self.call_id).await {
            debug!("Failed to detach AI session for room {}: {}", self.room_id, e);
        }

        // Update state
        *self.state.write() = ConferenceAIState::Terminated;

        info!("AI manager stopped for room {}", self.room_id);
        Ok(())
    }

    /// Check if the AI manager is active
    pub fn is_active(&self) -> bool {
        matches!(
            *self.state.read(),
            ConferenceAIState::Active | ConferenceAIState::Speaking | ConferenceAIState::Transcribing
        )
    }
}

impl Drop for ConferenceAIManager {
    fn drop(&mut self) {
        debug!("Dropping AI manager for room {}", self.room_id);

        // Abort any running tasks
        if let Some(handle) = self.audio_task.write().take() {
            handle.abort();
        }
        if let Some(handle) = self.response_task.write().take() {
            handle.abort();
        }
        if let Some(handle) = self.dtmf_task.write().take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_mode_default() {
        assert_eq!(AudioMode::default(), AudioMode::Mixed);
    }

    #[test]
    fn test_conference_ai_config_new() {
        let format = AudioFormat::pcm_mono();
        let config = ConferenceAIConfig::new(format);

        assert_eq!(config.participant_id, AI_PARTICIPANT_ID);
        assert_eq!(config.sample_rate, AI_SAMPLE_RATE);
        assert_eq!(config.frame_size, AI_FRAME_SIZE);
        assert_eq!(config.audio_mode, AudioMode::Mixed);
        assert!(!config.enable_transcription);
    }

    #[test]
    fn test_conference_ai_config_builder() {
        let format = AudioFormat::pcm_mono();
        let config = ConferenceAIConfig::new(format)
            .with_audio_mode(AudioMode::Individual)
            .with_transcription(true);

        assert_eq!(config.audio_mode, AudioMode::Individual);
        assert!(config.enable_transcription);
    }

    #[test]
    fn test_state_conversion_from_ai_session_state() {
        assert_eq!(
            ConferenceAIState::from(AISessionState::Connecting),
            ConferenceAIState::Connecting
        );
        assert_eq!(
            ConferenceAIState::from(AISessionState::Active),
            ConferenceAIState::Active
        );
        assert_eq!(
            ConferenceAIState::from(AISessionState::Speaking),
            ConferenceAIState::Speaking
        );
        assert_eq!(
            ConferenceAIState::from(AISessionState::Disconnecting),
            ConferenceAIState::Terminating
        );
        assert_eq!(
            ConferenceAIState::from(AISessionState::Terminated),
            ConferenceAIState::Terminated
        );
    }
}
