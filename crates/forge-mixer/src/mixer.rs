//! Audio mixing for multi-party conferences
//!
//! Combines multiple audio streams into a single mixed output

use crate::{AudioFormat, MixerError, Result};
use bytes::Bytes;
use dashmap::DashMap;
use forge_recorder::AudioRecorder;
use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Unique identifier for a participant in the mixer
pub type ParticipantId = String;

/// Audio buffer for a single participant
struct ParticipantBuffer {
    /// Buffered audio samples (16-bit PCM)
    samples: VecDeque<i16>,
    /// Gain level (0.0 to 1.0)
    gain: f32,
    /// Optional recorder for this participant
    recorder: Arc<Mutex<Option<AudioRecorder>>>,
}

impl ParticipantBuffer {
    fn new(gain: f32) -> Self {
        Self {
            samples: VecDeque::new(),
            gain: gain.clamp(0.0, 1.0),
            recorder: Arc::new(Mutex::new(None)),
        }
    }

    fn push_samples(&mut self, samples: &[i16]) {
        self.samples.extend(samples);

        // Write to recorder if recording
        if let Some(recorder) = self.recorder.lock().as_ref() {
            if recorder.is_recording() {
                if let Err(e) = recorder.write_samples(samples) {
                    warn!("Failed to write samples to participant recorder: {}", e);
                }
            }
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

    async fn start_recording<P: AsRef<Path>>(&self, path: P, format: AudioFormat) -> Result<()> {
        let recorder = AudioRecorder::new(path, format).await?;
        recorder.start()?;
        *self.recorder.lock() = Some(recorder);
        Ok(())
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
}

impl AudioMixer {
    /// Create a new audio mixer
    ///
    /// # Arguments
    /// * `format` - Audio format for all streams (must match)
    /// * `frame_size` - Frame size in samples for mixing operations
    pub fn new(format: AudioFormat, frame_size: usize) -> Result<Self> {
        if frame_size == 0 {
            return Err(MixerError::InvalidFormat(
                "Frame size must be > 0".to_string(),
            ));
        }

        info!(
            "Creating audio mixer with format: {:?}, frame_size: {}",
            format, frame_size
        );

        Ok(Self {
            participants: Arc::new(DashMap::new()),
            format: Arc::new(RwLock::new(format)),
            frame_size,
            auto_gain: true,
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

        self.participants.insert(id, ParticipantBuffer::new(gain));
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

        // Check if any participant has enough samples
        let has_data = self
            .participants
            .iter()
            .any(|p| p.available_samples() >= self.frame_size);

        if !has_data {
            return Ok(None);
        }

        let num_participants = self.participants.len();
        let mut mixed = vec![0i32; self.frame_size];

        // Mix all participants
        for mut participant in self.participants.iter_mut() {
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
        let output: Vec<i16> = if self.auto_gain && num_participants > 1 {
            let gain = 1.0 / (num_participants as f32).sqrt();
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
            num_participants
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

        // Check if any other participant has enough samples
        let has_data = self
            .participants
            .iter()
            .filter(|p| p.key() != exclude_id)
            .any(|p| p.available_samples() >= self.frame_size);

        if !has_data {
            return Ok(None);
        }

        let mut num_mixed = 0;
        let mut mixed = vec![0i32; self.frame_size];

        // Mix all participants except the excluded one
        for mut participant in self.participants.iter_mut() {
            if participant.key() == exclude_id {
                continue;
            }

            let samples = participant.drain_samples(self.frame_size);
            num_mixed += 1;

            for (i, &sample) in samples.iter().enumerate() {
                if i >= self.frame_size {
                    break;
                }

                let gained = (sample as f32 * participant.gain) as i32;
                mixed[i] += gained;
            }
        }

        if num_mixed == 0 {
            return Ok(None);
        }

        // Apply auto-gain
        let output: Vec<i16> = if self.auto_gain && num_mixed > 1 {
            let gain = 1.0 / (num_mixed as f32).sqrt();
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
            num_mixed,
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
        let participant = self
            .participants
            .get(id)
            .ok_or_else(|| MixerError::Internal(format!("Participant {} not found", id)))?;

        // Copy format before await to ensure RwLockReadGuard is dropped
        let format = *self.format.read();

        info!(
            "Starting recording for participant {} to {:?}",
            id,
            path.as_ref()
        );
        participant.start_recording(path, format).await
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
}
