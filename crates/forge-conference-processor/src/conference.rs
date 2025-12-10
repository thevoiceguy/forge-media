//! Conference bridge management
//!
//! Manages multi-party audio conferences with mixing and recording

use crate::{AudioFormat, ConferenceError, Result};
use bytes::Bytes;
use dashmap::DashMap;
use forge_mixer::AudioMixer;
use forge_recorder::{AudioRecorder, PlaybackSource};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

/// Unique identifier for a conference room
pub type RoomId = String;

/// Conference room with audio mixing and optional recording
pub struct ConferenceRoom {
    /// Room identifier
    id: RoomId,
    /// Audio mixer for combining participant streams
    mixer: Arc<AudioMixer>,
    /// Optional recorder for capturing the conference
    recorder: Arc<RwLock<Option<AudioRecorder>>>,
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
            format,
            _frame_size: frame_size,
        })
    }

    /// Get the room ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Add a participant to the room
    pub fn add_participant<S: Into<String>>(&self, participant_id: S) -> Result<()> {
        let participant_id = participant_id.into();
        info!("Adding participant {} to room {}", participant_id, self.id);
        Ok(self.mixer.add_participant(participant_id, None)?)
    }

    /// Remove a participant from the room
    pub fn remove_participant(&self, participant_id: &str) -> Result<()> {
        info!(
            "Removing participant {} from room {}",
            participant_id, self.id
        );
        Ok(self.mixer.remove_participant(participant_id)?)
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
        let mixed = self.mixer.mix()?;

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
        Ok(())
    }

    /// Stop recording the conference
    pub fn stop_recording(&self) -> Result<()> {
        info!("Stopping recording for room {}", self.id);

        let mut recorder_guard = self.recorder.write();
        if let Some(recorder) = recorder_guard.take() {
            recorder.stop()?;
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
        let path = output_path.as_ref();
        info!(
            "Starting recording for participant {} in room {} to {:?}",
            participant_id, self.id, path
        );

        Ok(self
            .mixer
            .start_participant_recording(participant_id, path)
            .await?)
    }

    /// Stop recording for a specific participant
    pub fn stop_participant_recording(&self, participant_id: &str) -> Result<()> {
        info!(
            "Stopping recording for participant {} in room {}",
            participant_id, self.id
        );
        Ok(self.mixer.stop_participant_recording(participant_id)?)
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
    pub fn add_participant_to_room(&self, room_id: &str, participant_id: &str) -> Result<()> {
        let room = self.get_room(room_id)?;
        room.add_participant(participant_id)
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
        room.add_participant("alice").unwrap();
        room.add_participant("bob").unwrap();

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
        room.add_participant("alice").unwrap();
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
        bridge.add_participant_to_room("room-1", "alice").unwrap();
        bridge.add_participant_to_room("room-1", "bob").unwrap();
        bridge.add_participant_to_room("room-2", "charlie").unwrap();

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

        room.add_participant("alice").unwrap();
        room.add_participant("bob").unwrap();

        // Write audio
        let samples: Vec<i16> = vec![100; 480];
        room.write_audio("alice", &samples).unwrap();
        room.write_audio("bob", &samples).unwrap();

        // Mix excluding alice
        let mixed = room.mix_for_participant("alice").unwrap();
        assert!(mixed.is_some());
    }
}
