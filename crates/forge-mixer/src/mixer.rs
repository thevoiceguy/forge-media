//! Audio mixing for multi-party conferences
//!
//! Combines multiple audio streams into a single mixed output

use crate::{AudioFormat, MixerError, Result};
use bytes::Bytes;
use dashmap::DashMap;
use forge_recorder::AudioRecorder;
use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Configurable options for the mixer
///
/// # Security Configuration
///
/// For production deployments, configure both base and jail to prevent
/// directory traversal and symlink escape attacks:
///
/// ```rust
/// use forge_mixer::MixerOptions;
/// use std::path::PathBuf;
///
/// let options = MixerOptions {
///     max_buffer_frames: 50,
///     recording_base_dir: Some("/var/forge/sessions/current".into()),
///     recording_root_jail: Some("/var/forge".into()),
/// };
/// ```
///
/// This configuration:
/// - Keeps recordings organized under `/var/forge/sessions/current`
/// - Prevents recordings from escaping the `/var/forge` jail
/// - Protects against symlink attacks via canonicalization
#[derive(Debug, Clone)]
pub struct MixerOptions {
    /// Number of frames to buffer per participant before dropping oldest data
    pub max_buffer_frames: usize,
    /// Base directory for participant recordings
    pub recording_base_dir: Option<PathBuf>,
    /// Root jail for participant recordings
    pub recording_root_jail: Option<PathBuf>,
}

impl Default for MixerOptions {
    fn default() -> Self {
        Self {
            max_buffer_frames: 50,
            recording_base_dir: None,
            recording_root_jail: None,
        }
    }
}

/// Unique identifier for a participant in the mixer
pub type ParticipantId = String;

/// Participant state in the conference
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantState {
    /// Participant is active and can send/receive audio
    Active,
    /// Participant is muted (not sending audio)
    Muted,
    /// Participant is on hold (not sending or receiving audio)
    OnHold,
}

/// Metadata about a participant in the mixer
#[derive(Debug, Clone)]
pub struct ParticipantMetadata {
    /// Participant ID
    pub id: String,
    /// Time when participant joined
    pub join_time: Instant,
    /// Current participant state
    pub state: ParticipantState,
    /// Audio gain level (0.0 to 1.0)
    pub gain: f32,
    /// Whether participant is currently being recorded
    pub is_recording: bool,
    /// Number of audio packets received from this participant
    pub packets_received: u64,
    /// Whether participant is currently speaking (VAD detected)
    pub is_speaking: bool,
    /// Last time speech was detected (None if never)
    pub last_speech_detected: Option<Instant>,
}

/// Audio buffer for a single participant
struct ParticipantBuffer {
    /// Buffered audio samples (16-bit PCM)
    samples: VecDeque<i16>,
    /// Gain level (0.0 to 1.0)
    gain: f32,
    /// Optional recorder for this participant
    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    /// Time when participant joined
    join_time: Instant,
    /// Current participant state
    state: ParticipantState,
    /// Number of packets received from this participant
    packets_received: u64,
    /// Voice Activity Detection state
    is_speaking: bool,
    /// Last time speech was detected
    last_speech_detected: Option<Instant>,
    /// Consecutive frames above speech threshold
    speech_frames: u32,
    /// Consecutive frames below silence threshold
    silence_frames: u32,
    /// Maximum buffered samples before dropping oldest data
    max_buffer_samples: usize,
}

impl ParticipantBuffer {
    fn new(gain: f32, max_buffer_samples: usize) -> Self {
        Self {
            samples: VecDeque::new(),
            gain: gain.clamp(0.0, 1.0),
            recorder: Arc::new(Mutex::new(None)),
            join_time: Instant::now(),
            state: ParticipantState::Active,
            packets_received: 0,
            is_speaking: false,
            last_speech_detected: None,
            speech_frames: 0,
            silence_frames: 0,
            max_buffer_samples,
        }
    }

    fn push_samples(&mut self, samples: &[i16]) {
        self.samples.extend(samples);
        if self.samples.len() > self.max_buffer_samples {
            let overflow = self.samples.len() - self.max_buffer_samples;
            self.samples.drain(..overflow);
            warn!(
                "Dropped {} samples due to buffer limit (max {})",
                overflow, self.max_buffer_samples
            );
        }
        self.packets_received += 1;

        // Perform Voice Activity Detection
        self.update_vad(samples);

        // Write to recorder if recording
        if let Some(recorder) = self.recorder.lock().as_ref() {
            if recorder.is_recording() {
                if let Err(e) = recorder.write_samples(samples) {
                    warn!("Failed to write samples to participant recorder: {}", e);
                }
            }
        }
    }

    /// Update Voice Activity Detection state based on audio samples
    fn update_vad(&mut self, samples: &[i16]) {
        // Calculate RMS (Root Mean Square) energy
        let energy = if !samples.is_empty() {
            let sum_squares: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
            (sum_squares / samples.len() as f64).sqrt()
        } else {
            0.0
        };

        // VAD thresholds (tuned for 16-bit PCM)
        const SPEECH_THRESHOLD: f64 = 300.0; // Energy above this = potential speech
        const SILENCE_THRESHOLD: f64 = 200.0; // Energy below this = silence
        const SPEECH_START_FRAMES: u32 = 3; // Consecutive frames to start speaking
        const SPEECH_STOP_FRAMES: u32 = 5; // Consecutive frames to stop speaking

        if energy > SPEECH_THRESHOLD {
            // High energy detected
            self.speech_frames += 1;
            self.silence_frames = 0;

            if !self.is_speaking && self.speech_frames >= SPEECH_START_FRAMES {
                // Transition to speaking
                self.is_speaking = true;
                self.last_speech_detected = Some(Instant::now());
                debug!("Participant started speaking (energy: {:.0})", energy);
            } else if self.is_speaking {
                // Update last speech time
                self.last_speech_detected = Some(Instant::now());
            }
        } else if energy < SILENCE_THRESHOLD {
            // Low energy detected
            self.silence_frames += 1;
            self.speech_frames = 0;

            if self.is_speaking && self.silence_frames >= SPEECH_STOP_FRAMES {
                // Transition to silence
                self.is_speaking = false;
                debug!("Participant stopped speaking (energy: {:.0})", energy);
            }
        } else {
            // Energy in middle range - maintain current state but reset counters partially
            self.speech_frames = self.speech_frames.saturating_sub(1);
            self.silence_frames = self.silence_frames.saturating_sub(1);
        }
    }

    fn drain_samples(&mut self, count: usize) -> Vec<i16> {
        self.samples
            .drain(..count.min(self.samples.len()))
            .collect()
    }

    fn available_samples(&self) -> usize {
        self.samples.len()
    }

    fn stop_recording(&self) -> Result<()> {
        let mut recorder_guard = self.recorder.lock();
        if let Some(recorder) = recorder_guard.take() {
            recorder.stop()?;
            Ok(())
        } else {
            Err(MixerError::RecordingNotFound(
                "No active recording for participant".to_string(),
            ))
        }
    }

    fn is_recording(&self) -> bool {
        self.recorder
            .lock()
            .as_ref()
            .map(|r| r.is_recording())
            .unwrap_or(false)
    }
}

/// Audio mixer for combining multiple audio streams
pub struct AudioMixer {
    /// Participant audio buffers
    participants: Arc<DashMap<ParticipantId, ParticipantBuffer>>,
    /// Audio format for all streams
    format: Arc<RwLock<AudioFormat>>,
    /// Frame size in samples for mixing
    frame_size: usize,
    /// Auto-gain control enabled
    auto_gain: bool,
    /// Maximum buffered samples per participant
    max_buffer_samples: usize,
    /// Optional base directory for recordings
    recording_base_dir: Option<PathBuf>,
    /// Optional root jail that recordings must stay within
    recording_root_jail: Option<PathBuf>,
}

impl AudioMixer {
    /// Create a new audio mixer
    ///
    /// # Arguments
    /// * `format` - Audio format for all streams (must match)
    /// * `frame_size` - Frame size in samples for mixing operations
    pub fn new(format: AudioFormat, frame_size: usize) -> Result<Self> {
        Self::with_options(format, frame_size, MixerOptions::default())
    }

    /// Create a new audio mixer with custom options
    ///
    /// # Security
    ///
    /// Canonicalizes recording base and jail directories during creation to prevent
    /// TOCTOU (Time-Of-Check-Time-Of-Use) attacks. This ensures that symlinks cannot
    /// be modified after the mixer is created to escape the configured boundaries.
    pub fn with_options(
        format: AudioFormat,
        frame_size: usize,
        options: MixerOptions,
    ) -> Result<Self> {
        if frame_size == 0 {
            return Err(MixerError::InvalidFormat(
                "Frame size must be > 0".to_string(),
            ));
        }
        if options.max_buffer_frames == 0 {
            return Err(MixerError::InvalidFormat(
                "max_buffer_frames must be > 0".to_string(),
            ));
        }

        // Canonicalize base and jail directories ONCE during creation to prevent TOCTOU
        let recording_base_canon = if let Some(base) = &options.recording_base_dir {
            let canon = std::fs::canonicalize(base).map_err(|e| {
                MixerError::InvalidFormat(format!(
                    "Invalid recording base directory {:?}: {}",
                    base, e
                ))
            })?;
            Some(canon)
        } else {
            None
        };

        let recording_jail_canon = if let Some(jail) = &options.recording_root_jail {
            let canon = std::fs::canonicalize(jail).map_err(|e| {
                MixerError::InvalidFormat(format!(
                    "Invalid recording root jail {:?}: {}",
                    jail, e
                ))
            })?;
            Some(canon)
        } else {
            None
        };

        info!(
            "Creating audio mixer with format: {:?}, frame_size: {}, base: {:?}, jail: {:?}",
            format, frame_size, recording_base_canon, recording_jail_canon
        );

        Ok(Self {
            participants: Arc::new(DashMap::new()),
            format: Arc::new(RwLock::new(format)),
            frame_size,
            auto_gain: true,
            max_buffer_samples: frame_size * options.max_buffer_frames,
            recording_base_dir: recording_base_canon,
            recording_root_jail: recording_jail_canon,
        })
    }

    /// Add a participant to the mixer
    ///
    /// # Arguments
    /// * `id` - Unique identifier for the participant
    /// * `gain` - Gain level (0.0 to 1.0, default 1.0)
    pub fn add_participant<S: Into<String>>(&self, id: S, gain: Option<f32>) -> Result<()> {
        let id = id.into();
        let gain = gain.unwrap_or(1.0);

        info!("Adding participant {} with gain {}", id, gain);

        self.participants
            .insert(id, ParticipantBuffer::new(gain, self.max_buffer_samples));
        Ok(())
    }

    /// Remove a participant from the mixer
    pub fn remove_participant(&self, id: &str) -> Result<()> {
        info!("Removing participant {}", id);

        self.participants
            .remove(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;
        Ok(())
    }

    /// Write audio samples for a specific participant
    ///
    /// # Arguments
    /// * `id` - Participant identifier
    /// * `samples` - PCM audio samples (16-bit signed integers)
    pub fn write_samples(&self, id: &str, samples: &[i16]) -> Result<()> {
        let mut participant = self
            .participants
            .get_mut(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        if participant.state != ParticipantState::Active {
            debug!(
                "Ignoring samples for participant {} due to state {:?}",
                id, participant.state
            );
            return Ok(());
        }

        participant.push_samples(samples);
        debug!("Wrote {} samples for participant {}", samples.len(), id);
        Ok(())
    }

    /// Write raw RTP payload for a participant (assumes PCM data)
    ///
    /// # Arguments
    /// * `id` - Participant identifier
    /// * `payload` - Raw RTP payload bytes
    pub fn write_rtp_payload(&self, id: &str, payload: &Bytes) -> Result<()> {
        if payload.len() % 2 != 0 {
            return Err(MixerError::InvalidFormat(format!(
                "RTP payload length {} is not 16-bit aligned",
                payload.len()
            )));
        }

        let format = *self.format.read();
        if format.channels != 1 || !matches!(format.codec, forge_core::AudioCodec::PCM) {
            return Err(MixerError::InvalidFormat(format!(
                "Unsupported RTP format for mixer: {:?} channels {}",
                format.codec, format.channels
            )));
        }

        // Convert bytes to i16 samples (assuming big-endian 16-bit PCM)
        let samples: Vec<i16> = payload
            .chunks_exact(2)
            .map(|chunk| i16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();

        self.write_samples(id, &samples)
    }

    /// Mix audio from all participants into a single output
    ///
    /// Returns mixed samples if enough data is available from at least one participant.
    /// The frame size determines how many samples are mixed.
    pub fn mix(&self) -> Result<Option<Vec<i16>>> {
        if self.participants.is_empty() {
            return Ok(None);
        }

        // Collect only active participants that can contribute a full frame
        let mut ready_participants = Vec::new();
        for participant in self.participants.iter_mut() {
            if participant.state != ParticipantState::Active {
                continue;
            }
            if participant.available_samples() >= self.frame_size {
                ready_participants.push(participant);
            }
        }

        if ready_participants.is_empty() {
            return Ok(None);
        }

        let contributors = ready_participants.len();
        let mut mixed = vec![0i32; self.frame_size];

        // Mix all participants
        for mut participant in ready_participants {
            let samples = participant.drain_samples(self.frame_size);

            for (i, &sample) in samples.iter().enumerate() {
                if i >= self.frame_size {
                    break;
                }

                // Apply participant gain and accumulate
                let gained = (sample as f32 * participant.gain) as i32;
                mixed[i] += gained;
            }
        }

        // Apply auto-gain to prevent clipping
        let output: Vec<i16> = if self.auto_gain && contributors > 1 {
            let gain = 1.0 / (contributors as f32).sqrt();
            mixed
                .iter()
                .map(|&s| ((s as f32 * gain).clamp(-32768.0, 32767.0)) as i16)
                .collect()
        } else {
            // Just clamp without gain adjustment
            mixed
                .iter()
                .map(|&s| s.clamp(-32768, 32767) as i16)
                .collect()
        };

        debug!(
            "Mixed {} samples from {} participants",
            output.len(),
            contributors
        );
        Ok(Some(output))
    }

    /// Create a mixed output for a specific participant (excluding their own audio)
    ///
    /// This is useful for conference calls where you don't want to hear your own voice.
    pub fn mix_excluding(&self, exclude_id: &str) -> Result<Option<Vec<i16>>> {
        if self.participants.len() <= 1 {
            return Ok(None);
        }

        // Collect only active, non-excluded participants that can contribute a full frame
        let mut ready_participants = Vec::new();
        for participant in self.participants.iter_mut() {
            if participant.key() == exclude_id || participant.state != ParticipantState::Active {
                continue;
            }
            if participant.available_samples() >= self.frame_size {
                ready_participants.push(participant);
            }
        }

        if ready_participants.is_empty() {
            return Ok(None);
        }

        let mut mixed = vec![0i32; self.frame_size];

        // Mix all participants except the excluded one
        for participant in ready_participants.iter_mut() {
            let samples = participant.drain_samples(self.frame_size);

            for (i, &sample) in samples.iter().enumerate() {
                if i >= self.frame_size {
                    break;
                }

                let gained = (sample as f32 * participant.gain) as i32;
                mixed[i] += gained;
            }
        }

        // Apply auto-gain
        let contributors = ready_participants.len();
        let output: Vec<i16> = if self.auto_gain && contributors > 1 {
            let gain = 1.0 / (contributors as f32).sqrt();
            mixed
                .iter()
                .map(|&s| ((s as f32 * gain).clamp(-32768.0, 32767.0)) as i16)
                .collect()
        } else {
            mixed
                .iter()
                .map(|&s| s.clamp(-32768, 32767) as i16)
                .collect()
        };

        debug!(
            "Mixed {} samples from {} participants (excluding {})",
            output.len(),
            contributors,
            exclude_id
        );
        Ok(Some(output))
    }

    /// Set gain for a specific participant
    pub fn set_gain(&self, id: &str, gain: f32) -> Result<()> {
        let mut participant = self
            .participants
            .get_mut(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        participant.gain = gain.clamp(0.0, 1.0);
        debug!("Set gain for participant {} to {}", id, participant.gain);
        Ok(())
    }

    /// Enable or disable automatic gain control
    pub fn set_auto_gain(&mut self, enabled: bool) {
        self.auto_gain = enabled;
        info!("Auto-gain {}", if enabled { "enabled" } else { "disabled" });
    }

    /// Get number of participants
    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    /// Get list of participant IDs
    pub fn participants(&self) -> Vec<String> {
        self.participants.iter().map(|p| p.key().clone()).collect()
    }

    /// Clear all buffered audio for a participant
    pub fn clear_buffer(&self, id: &str) -> Result<()> {
        let mut participant = self
            .participants
            .get_mut(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        participant.samples.clear();
        debug!("Cleared buffer for participant {}", id);
        Ok(())
    }

    /// Get the current audio format
    pub fn format(&self) -> AudioFormat {
        *self.format.read()
    }

    /// Start recording for a specific participant
    ///
    /// # Arguments
    /// * `id` - Participant identifier
    /// * `path` - Output file path for the recording
    pub async fn start_participant_recording<P: AsRef<Path>>(
        &self,
        id: &str,
        path: P,
    ) -> Result<()> {
        let path = path.as_ref();

        let participant_recorder = self
            .participants
            .get(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?
            .recorder
            .clone();

        // Copy format before await to ensure RwLockReadGuard is dropped
        let format = *self.format.read();
        if format.channels != 1 || !matches!(format.codec, forge_core::AudioCodec::PCM) {
            return Err(MixerError::InvalidFormat(format!(
                "Recording only supports mono PCM, got codec {:?} channels {}",
                format.codec, format.channels
            )));
        }

        let expected_ext = format.codec.file_extension();
        if let Some(ext) = path.extension() {
            if ext != expected_ext {
                warn!(
                    "Recording path {:?} extension {:?} does not match expected {:?}",
                    path,
                    ext,
                    expected_ext
                );
            }
        }

        info!(
            "Starting recording for participant {} to {:?}",
            id,
            path
        );
        let target_path = self.validate_recording_path(path)?;
        let recorder = AudioRecorder::new(&target_path, format).await?;
        recorder.start()?;
        *participant_recorder.lock() = Some(recorder);
        Ok(())
    }

    /// Validate and resolve the recording path against configured jail/base.
    ///
    /// # Security
    ///
    /// This method performs the following security checks in order:
    /// 1. Resolves relative paths against the pre-canonicalized base directory
    /// 2. Canonicalizes the target parent directory (creating it temporarily if needed)
    /// 3. Validates against the pre-canonicalized base directory
    /// 4. Validates against the pre-canonicalized jail directory
    /// 5. Creates the directory structure AFTER all validation passes
    ///
    /// This ordering prevents:
    /// - Directory creation side effects before validation
    /// - TOCTOU attacks (base/jail pre-canonicalized at mixer creation)
    /// - Symlink escape attacks (canonicalization resolves symlinks)
    /// - Directory traversal attacks (parent directory validation)
    fn validate_recording_path(&self, path: &Path) -> Result<PathBuf> {
        // Require a configured base when a relative path is provided
        let target = if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(base) = &self.recording_base_dir {
            base.join(path)
        } else {
            return Err(MixerError::InvalidFormat(
                "Relative recording paths require a configured base directory".to_string(),
            ));
        };

        // Get the parent directory that will contain the recording file
        // Root path has no parent; use root itself for validation (safe - cannot escape root)
        let target_parent = target
            .parent()
            .unwrap_or_else(|| Path::new(std::path::MAIN_SEPARATOR_STR));

        // Canonicalize parent directory to resolve symlinks and normalize path
        // We need to create it temporarily if it doesn't exist for canonicalization to work
        let parent_exists = target_parent.exists();
        if !parent_exists {
            std::fs::create_dir_all(target_parent).map_err(|e| {
                MixerError::InvalidFormat(format!(
                    "Failed to prepare recording directory {:?} for validation: {}",
                    target_parent, e
                ))
            })?;
        }

        let canon_parent = std::fs::canonicalize(target_parent).map_err(|e| {
            // Clean up directory if we created it
            if !parent_exists {
                let _ = std::fs::remove_dir_all(target_parent);
            }
            MixerError::InvalidFormat(format!(
                "Failed to canonicalize recording path {:?}: {}",
                target, e
            ))
        })?;

        // Validate against base directory (uses pre-canonicalized base from mixer creation)
        if let Some(canon_base) = &self.recording_base_dir {
            if !canon_parent.starts_with(canon_base) {
                // Clean up directory if we created it and validation failed
                if !parent_exists {
                    let _ = std::fs::remove_dir_all(target_parent);
                }
                return Err(MixerError::InvalidFormat(format!(
                    "Recording path {:?} (canonicalized: {:?}) escapes base directory {:?}",
                    target, canon_parent, canon_base
                )));
            }
        }

        // Validate against jail directory (uses pre-canonicalized jail from mixer creation)
        if let Some(canon_jail) = &self.recording_root_jail {
            if !canon_parent.starts_with(canon_jail) {
                // Clean up directory if we created it and validation failed
                if !parent_exists {
                    let _ = std::fs::remove_dir_all(target_parent);
                }
                return Err(MixerError::InvalidFormat(format!(
                    "Recording path {:?} (canonicalized: {:?}) escapes recording jail {:?}",
                    target, canon_parent, canon_jail
                )));
            }
        }

        // Validation passed! Directory already exists from validation above, so we're good
        Ok(target)
    }

    /// Stop recording for a specific participant
    pub fn stop_participant_recording(&self, id: &str) -> Result<()> {
        let participant = self
            .participants
            .get(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        info!("Stopping recording for participant {}", id);
        participant.stop_recording()
    }

    /// Check if a participant is currently recording
    pub fn is_participant_recording(&self, id: &str) -> Result<bool> {
        let participant = self
            .participants
            .get(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        Ok(participant.is_recording())
    }

    /// Get metadata for a specific participant
    ///
    /// # Arguments
    /// * `id` - Participant identifier
    ///
    /// # Returns
    /// ParticipantMetadata containing all tracking information
    pub fn get_participant_metadata(&self, id: &str) -> Result<ParticipantMetadata> {
        let participant = self
            .participants
            .get(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        Ok(ParticipantMetadata {
            id: id.to_string(),
            join_time: participant.join_time,
            state: participant.state,
            gain: participant.gain,
            is_recording: participant.is_recording(),
            packets_received: participant.packets_received,
            is_speaking: participant.is_speaking,
            last_speech_detected: participant.last_speech_detected,
        })
    }

    /// Set the state for a specific participant
    ///
    /// # Arguments
    /// * `id` - Participant identifier
    /// * `state` - New state (Active, Muted, or OnHold)
    pub fn set_participant_state(&self, id: &str, state: ParticipantState) -> Result<()> {
        let mut participant = self
            .participants
            .get_mut(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        participant.state = state;
        info!("Set participant {} state to {:?}", id, state);
        Ok(())
    }

    /// Get metadata for all participants
    ///
    /// # Returns
    /// Vec of ParticipantMetadata for all active participants
    pub fn get_all_participant_metadata(&self) -> Vec<ParticipantMetadata> {
        self.participants
            .iter()
            .map(|entry| ParticipantMetadata {
                id: entry.key().clone(),
                join_time: entry.value().join_time,
                state: entry.value().state,
                gain: entry.value().gain,
                is_recording: entry.value().is_recording(),
                packets_received: entry.value().packets_received,
                is_speaking: entry.value().is_speaking,
                last_speech_detected: entry.value().last_speech_detected,
            })
            .collect()
    }

    /// Get audio samples for a specific participant without mixing
    ///
    /// This is useful for AI Individual mode where each participant's audio
    /// needs to be sent separately with speaker labels.
    ///
    /// # Arguments
    /// * `id` - Participant identifier
    /// * `count` - Number of samples to retrieve (typically frame_size)
    ///
    /// # Returns
    /// Option containing the samples if available, with participant gain applied
    pub fn get_participant_audio(&self, id: &str, count: usize) -> Result<Option<Vec<i16>>> {
        let mut participant = self
            .participants
            .get_mut(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        if participant.state != ParticipantState::Active {
            return Ok(None);
        }

        // Check if participant has enough samples
        if participant.available_samples() < count {
            return Ok(None);
        }

        // Drain samples from this participant
        let samples = participant.drain_samples(count);

        // Apply participant gain
        let output: Vec<i16> = samples
            .iter()
            .map(|&s| ((s as f32 * participant.gain).clamp(-32768.0, 32767.0)) as i16)
            .collect();

        debug!(
            "Retrieved {} samples for participant {} (individual mode)",
            output.len(),
            id
        );

        Ok(Some(output))
    }

    /// Get audio samples for all participants individually (for AI Individual mode)
    ///
    /// Returns a map of participant IDs to their audio samples.
    /// Only returns participants that have enough samples available.
    ///
    /// # Arguments
    /// * `count` - Number of samples to retrieve per participant
    /// * `exclude_id` - Optional participant ID to exclude (e.g., "__ai__")
    ///
    /// # Returns
    /// Map of participant ID to audio samples
    pub fn get_all_participant_audio(
        &self,
        count: usize,
        exclude_id: Option<&str>,
    ) -> std::collections::HashMap<String, Vec<i16>> {
        let mut result = std::collections::HashMap::new();

        for mut participant in self.participants.iter_mut() {
            // Clone the key first to avoid borrow conflicts
            let id = participant.key().clone();

            // Skip excluded participant
            if let Some(exclude) = exclude_id {
                if id == exclude {
                    continue;
                }
            }

            // Skip if not active
            if participant.state != ParticipantState::Active {
                continue;
            }

            // Check if participant has enough samples
            if participant.available_samples() >= count {
                let samples = participant.drain_samples(count);

                // Apply participant gain
                let output: Vec<i16> = samples
                    .iter()
                    .map(|&s| ((s as f32 * participant.gain).clamp(-32768.0, 32767.0)) as i16)
                    .collect();

                result.insert(id, output);
            }
        }

        debug!(
            "Retrieved audio for {} participants (individual mode)",
            result.len()
        );

        result
    }
}

impl Default for AudioMixer {
    fn default() -> Self {
        Self::new(AudioFormat::pcm_mono(), 480).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixer_lifecycle() {
        let format = AudioFormat::pcm_mono();
        let mixer = AudioMixer::new(format, 480).unwrap();

        // Add participants
        mixer.add_participant("alice", None).unwrap();
        mixer.add_participant("bob", None).unwrap();

        assert_eq!(mixer.participant_count(), 2);
        assert!(mixer.participants().contains(&"alice".to_string()));
        assert!(mixer.participants().contains(&"bob".to_string()));

        // Remove participant
        mixer.remove_participant("bob").unwrap();
        assert_eq!(mixer.participant_count(), 1);
    }

    #[test]
    fn test_mixing() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 480).unwrap();

        mixer.add_participant("alice", None).unwrap();
        mixer.add_participant("bob", None).unwrap();

        // Write samples for both participants
        let alice_samples: Vec<i16> = (0..480).map(|i| (i % 100) as i16).collect();
        let bob_samples: Vec<i16> = (0..480).map(|i| -(i % 100) as i16).collect();

        mixer.write_samples("alice", &alice_samples).unwrap();
        mixer.write_samples("bob", &bob_samples).unwrap();

        // Mix audio
        let mixed = mixer.mix().unwrap();
        assert!(mixed.is_some());

        let mixed = mixed.unwrap();
        assert_eq!(mixed.len(), 480);
    }

    #[test]
    fn test_mix_excluding() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 480).unwrap();

        mixer.add_participant("alice", None).unwrap();
        mixer.add_participant("bob", None).unwrap();
        mixer.add_participant("charlie", None).unwrap();

        // Write samples
        let samples: Vec<i16> = vec![100; 480];
        mixer.write_samples("alice", &samples).unwrap();
        mixer.write_samples("bob", &samples).unwrap();
        mixer.write_samples("charlie", &samples).unwrap();

        // Mix excluding alice
        let mixed = mixer.mix_excluding("alice").unwrap();
        assert!(mixed.is_some());

        let mixed = mixed.unwrap();
        assert_eq!(mixed.len(), 480);
    }

    #[test]
    fn test_gain_control() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 480).unwrap();

        mixer.add_participant("alice", Some(0.5)).unwrap();
        mixer.set_gain("alice", 0.8).unwrap();

        // Verify gain is applied
        let samples: Vec<i16> = vec![1000; 480];
        mixer.write_samples("alice", &samples).unwrap();

        let mixed = mixer.mix().unwrap();
        assert!(mixed.is_some());
    }

    #[test]
    fn test_muted_participant_ignored() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 4).unwrap();

        mixer.add_participant("alice", None).unwrap();
        mixer.add_participant("bob", None).unwrap();
        mixer
            .set_participant_state("bob", ParticipantState::Muted)
            .unwrap();

        let alice_samples: Vec<i16> = vec![500; 4];
        let bob_samples: Vec<i16> = vec![1000; 4];

        mixer.write_samples("alice", &alice_samples).unwrap();
        mixer.write_samples("bob", &bob_samples).unwrap(); // ignored

        let mixed = mixer.mix().unwrap().unwrap();
        // Only alice should contribute, so output equals alice samples (no auto-gain)
        assert_eq!(mixed, alice_samples);
    }

    #[test]
    fn test_autogain_counts_only_contributors() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 4).unwrap();

        mixer.add_participant("alice", None).unwrap();
        mixer.add_participant("bob", None).unwrap();
        mixer.add_participant("charlie", None).unwrap();

        // Charlie provides no samples; should not affect gain
        let alice_samples: Vec<i16> = vec![1000; 4];
        let bob_samples: Vec<i16> = vec![1000; 4];

        mixer.write_samples("alice", &alice_samples).unwrap();
        mixer.write_samples("bob", &bob_samples).unwrap();

        let mixed = mixer.mix().unwrap().unwrap();
        // Two contributors -> divided by sqrt(2)
        let expected: Vec<i16> = vec![(2000f32 / 2f32.sqrt()) as i16; 4];
        assert_eq!(mixed, expected);
    }

    #[test]
    fn test_rtp_payload_validation() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 4).unwrap();
        mixer.add_participant("alice", None).unwrap();

        let bad_payload = Bytes::from_static(&[0x00, 0x01, 0x02]); // odd length
        let err = mixer.write_rtp_payload("alice", &bad_payload).unwrap_err();
        assert!(matches!(err, MixerError::InvalidFormat(_)));
    }

    #[test]
    fn test_buffer_limit_enforced() {
        let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 2).unwrap();
        mixer.add_participant("alice", None).unwrap();

        // Default buffer: frame_size * 50 = 100 samples. Push 120 samples.
        let samples: Vec<i16> = (0..120).map(|i| i as i16).collect();
        mixer.write_samples("alice", &samples).unwrap();

        // Request more than limit should return None
        assert!(mixer.get_participant_audio("alice", 120).unwrap().is_none());

        // Exactly the capped amount should succeed
        let audio = mixer.get_participant_audio("alice", 100).unwrap();
        assert!(audio.is_some());
        assert_eq!(audio.unwrap().len(), 100);
    }

    // Security tests for recording path validation
    #[cfg(test)]
    mod security_tests {
        use super::*;
        use std::fs;
        use std::os::unix::fs as unix_fs;

        #[test]
        fn test_recording_path_traversal_blocked() {
            // Create test directories
            let test_dir = std::env::temp_dir().join(format!("mixer-test-{}", uuid::Uuid::new_v4()));
            let jail = test_dir.join("jail");
            let base = jail.join("recordings");
            fs::create_dir_all(&base).unwrap();

            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: Some(base.clone()),
                recording_root_jail: Some(jail.clone()),
            };

            let mixer = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options).unwrap();

            // Attempt directory traversal
            let result = mixer.validate_recording_path(Path::new("../../etc/passwd"));
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("escapes"));

            // Cleanup
            fs::remove_dir_all(&test_dir).ok();
        }

        #[tokio::test]
        async fn test_recording_symlink_escape_blocked() {
            // Create test directories
            let test_dir = std::env::temp_dir().join(format!("mixer-test-{}", uuid::Uuid::new_v4()));
            let jail = test_dir.join("jail");
            let base = jail.join("recordings");
            let evil_target = test_dir.join("evil");
            fs::create_dir_all(&base).unwrap();
            fs::create_dir_all(&evil_target).unwrap();

            // Create symlink inside base that points outside jail
            let symlink_path = base.join("escape");
            unix_fs::symlink(&evil_target, &symlink_path).unwrap();

            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: Some(base.clone()),
                recording_root_jail: Some(jail.clone()),
            };

            let mixer = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options).unwrap();
            mixer.add_participant("alice", None).unwrap();

            // Attempt to record through symlink that escapes jail
            let result = mixer.start_participant_recording("alice", symlink_path.join("recording.wav")).await;
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("escapes"));

            // Cleanup
            fs::remove_dir_all(&test_dir).ok();
        }

        #[test]
        fn test_recording_absolute_path_within_jail() {
            // Create test directories
            let test_dir = std::env::temp_dir().join(format!("mixer-test-{}", uuid::Uuid::new_v4()));
            let jail = test_dir.join("jail");
            let base = jail.join("recordings");
            fs::create_dir_all(&base).unwrap();

            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: Some(base.clone()),
                recording_root_jail: Some(jail.clone()),
            };

            let mixer = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options).unwrap();

            // Absolute path within jail should be allowed
            let valid_path = base.join("session-1").join("alice.wav");
            let result = mixer.validate_recording_path(&valid_path);
            assert!(result.is_ok());

            // Cleanup
            fs::remove_dir_all(&test_dir).ok();
        }

        #[test]
        fn test_recording_absolute_path_escapes_jail() {
            // Create test directories
            let test_dir = std::env::temp_dir().join(format!("mixer-test-{}", uuid::Uuid::new_v4()));
            let jail = test_dir.join("jail");
            let base = jail.join("recordings");
            fs::create_dir_all(&base).unwrap();

            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: Some(base.clone()),
                recording_root_jail: Some(jail.clone()),
            };

            let mixer = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options).unwrap();

            // Absolute path outside jail should be blocked
            let evil_path = test_dir.join("outside-jail.wav");
            let result = mixer.validate_recording_path(&evil_path);
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("escapes"));

            // Cleanup
            fs::remove_dir_all(&test_dir).ok();
        }

        #[test]
        fn test_recording_relative_path_requires_base() {
            // No base directory configured
            let mixer = AudioMixer::new(AudioFormat::pcm_mono(), 480).unwrap();

            // Relative path should fail without base
            let result = mixer.validate_recording_path(Path::new("alice.wav"));
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("require"));
        }

        #[test]
        fn test_recording_base_escape_blocked() {
            // Create test directories
            let test_dir = std::env::temp_dir().join(format!("mixer-test-{}", uuid::Uuid::new_v4()));
            let jail = test_dir.join("jail");
            let base = jail.join("recordings").join("session-1");
            let sibling = jail.join("recordings").join("session-2");
            fs::create_dir_all(&base).unwrap();
            fs::create_dir_all(&sibling).unwrap();

            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: Some(base.clone()),
                recording_root_jail: Some(jail.clone()),
            };

            let mixer = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options).unwrap();

            // Attempt to escape base (but stay in jail)
            let result = mixer.validate_recording_path(Path::new("../session-2/alice.wav"));
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("escapes base"));

            // Cleanup
            fs::remove_dir_all(&test_dir).ok();
        }

        #[test]
        fn test_recording_invalid_jail_config() {
            // Attempt to create mixer with non-existent jail
            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: None,
                recording_root_jail: Some(PathBuf::from("/nonexistent/jail")),
            };

            let result = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options);
            assert!(result.is_err());
            if let Err(e) = result {
                assert!(e.to_string().contains("Invalid recording root jail"));
            }
        }

        #[test]
        fn test_recording_invalid_base_config() {
            // Attempt to create mixer with non-existent base
            let options = MixerOptions {
                max_buffer_frames: 50,
                recording_base_dir: Some(PathBuf::from("/nonexistent/base")),
                recording_root_jail: None,
            };

            let result = AudioMixer::with_options(AudioFormat::pcm_mono(), 480, options);
            assert!(result.is_err());
            if let Err(e) = result {
                assert!(e.to_string().contains("Invalid recording base directory"));
            }
        }
    }
}
