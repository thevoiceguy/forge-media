//! Audio feedback playback system for conference events
//!
//! Plays sound files to participants at appropriate times (join, exit, alone, etc.)

use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

/// Audio feedback player for conference sounds
pub struct AudioFeedbackPlayer {
    /// Sample rate for playback
    sample_rate: u32,
}

impl AudioFeedbackPlayer {
    /// Create a new audio feedback player
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate }
    }

    /// Load and decode an audio file to PCM samples
    ///
    /// Supports WAV, MP3, OGG formats (depending on available decoders)
    ///
    /// # Arguments
    /// * `path` - Path to audio file
    ///
    /// # Returns
    /// PCM samples resampled to the player's sample rate, or None if loading fails
    pub fn load_audio_file<P: AsRef<Path>>(&self, path: P) -> Option<Vec<i16>> {
        let path = path.as_ref();

        // Check if file exists
        if !path.exists() {
            warn!("Audio file not found: {}", path.display());
            return None;
        }

        // Try to load based on file extension
        match path.extension().and_then(|e| e.to_str()) {
            Some("wav") | Some("WAV") => self.load_wav(path),
            Some("mp3") | Some("MP3") => {
                warn!("MP3 support not yet implemented: {}", path.display());
                None
            }
            Some("ogg") | Some("OGG") => {
                warn!("OGG support not yet implemented: {}", path.display());
                None
            }
            _ => {
                warn!("Unsupported audio format: {}", path.display());
                None
            }
        }
    }

    /// Load a WAV file
    fn load_wav<P: AsRef<Path>>(&self, path: P) -> Option<Vec<i16>> {
        let path = path.as_ref();

        // Open WAV file with hound
        let mut reader = match hound::WavReader::open(path) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to open WAV file {}: {}", path.display(), e);
                return None;
            }
        };

        let spec = reader.spec();

        // Only support PCM format
        if spec.sample_format != hound::SampleFormat::Int {
            warn!(
                "Unsupported WAV format in {}: {:?} (only PCM/Int supported)",
                path.display(),
                spec.sample_format
            );
            return None;
        }

        debug!(
            "Loading WAV: {} ({}Hz, {} channels, {} bits)",
            path.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        );

        // Read all samples and convert to i16
        let samples: Vec<i16> = match spec.bits_per_sample {
            16 => {
                // Native i16 samples
                reader.samples::<i16>().filter_map(|s| s.ok()).collect()
            }
            8 => {
                // Convert 8-bit PCM (read as i8) to i16
                reader
                    .samples::<i8>()
                    .filter_map(|s| s.ok())
                    .map(|s| (s as i16) << 8)
                    .collect()
            }
            24 | 32 => {
                // Convert higher bit depth to i16 (scale down)
                let shift = spec.bits_per_sample - 16;
                reader
                    .samples::<i32>()
                    .filter_map(|s| s.ok())
                    .map(|s| (s >> shift) as i16)
                    .collect()
            }
            _ => {
                warn!(
                    "Unsupported bit depth in {}: {} bits",
                    path.display(),
                    spec.bits_per_sample
                );
                return None;
            }
        };

        if samples.is_empty() {
            warn!("No samples read from WAV file: {}", path.display());
            return None;
        }

        // Convert stereo to mono if needed (average channels)
        let mono_samples = if spec.channels == 2 {
            samples
                .chunks_exact(2)
                .map(|chunk| ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16)
                .collect()
        } else if spec.channels == 1 {
            samples
        } else {
            warn!(
                "Unsupported channel count in {}: {} (only mono/stereo supported)",
                path.display(),
                spec.channels
            );
            return None;
        };

        // Resample if needed
        let final_samples = if spec.sample_rate != self.sample_rate {
            debug!(
                "Resampling from {}Hz to {}Hz",
                spec.sample_rate, self.sample_rate
            );
            self.resample(&mono_samples, spec.sample_rate, self.sample_rate)
        } else {
            mono_samples
        };

        debug!(
            "Loaded {} samples from {}",
            final_samples.len(),
            path.display()
        );

        Some(final_samples)
    }

    /// Resample audio from one sample rate to another using linear interpolation
    fn resample(&self, samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
        if from_rate == to_rate {
            return samples.to_vec();
        }

        let ratio = from_rate as f64 / to_rate as f64;
        let output_len = (samples.len() as f64 / ratio) as usize;
        let mut output = Vec::with_capacity(output_len);

        for i in 0..output_len {
            let src_pos = i as f64 * ratio;
            let src_idx = src_pos as usize;

            if src_idx + 1 < samples.len() {
                // Linear interpolation between two samples
                let frac = src_pos - src_idx as f64;
                let s1 = samples[src_idx] as f64;
                let s2 = samples[src_idx + 1] as f64;
                let interpolated = s1 + (s2 - s1) * frac;
                output.push(interpolated as i16);
            } else if src_idx < samples.len() {
                // Last sample, no interpolation
                output.push(samples[src_idx]);
            }
        }

        output
    }

    /// Generate silence samples
    ///
    /// # Arguments
    /// * `duration_ms` - Duration in milliseconds
    ///
    /// # Returns
    /// Vector of silent PCM samples
    pub fn generate_silence(&self, duration_ms: u64) -> Vec<i16> {
        let sample_count = (self.sample_rate as u64 * duration_ms / 1000) as usize;
        vec![0i16; sample_count]
    }
}

/// Pre-loaded conference sounds for efficient playback
#[derive(Clone)]
pub struct ConferenceSounds {
    pub join_sound: Option<Arc<Vec<i16>>>,
    pub exit_sound: Option<Arc<Vec<i16>>>,
    pub alone_sound: Option<Arc<Vec<i16>>>,
    pub invalid_pin_sound: Option<Arc<Vec<i16>>>,
    pub locked_sound: Option<Arc<Vec<i16>>>,
    pub unlocked_sound: Option<Arc<Vec<i16>>>,
    pub kicked_sound: Option<Arc<Vec<i16>>>,
    pub max_members_sound: Option<Arc<Vec<i16>>>,
    pub moh_sound: Option<Arc<Vec<i16>>>,
    pub pin_prompt_sound: Option<Arc<Vec<i16>>>,
    pub recording_started_sound: Option<Arc<Vec<i16>>>,
    pub recording_stopped_sound: Option<Arc<Vec<i16>>>,
}

impl ConferenceSounds {
    /// Load all configured sounds from EffectiveRoomConfig
    pub fn load_from_config(
        config: &crate::room_config::EffectiveRoomConfig,
        player: &AudioFeedbackPlayer,
    ) -> Self {
        Self {
            join_sound: config
                .join_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            exit_sound: config
                .exit_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            alone_sound: config
                .alone_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            invalid_pin_sound: config
                .invalid_pin_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            locked_sound: config
                .locked_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            unlocked_sound: config
                .unlocked_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            kicked_sound: config
                .kicked_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            max_members_sound: config
                .max_members_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            moh_sound: config
                .moh_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            pin_prompt_sound: config
                .pin_prompt_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            recording_started_sound: config
                .recording_started_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
            recording_stopped_sound: config
                .recording_stopped_sound
                .as_ref()
                .and_then(|path| player.load_audio_file(path).map(Arc::new)),
        }
    }

    /// Create empty sounds (all disabled)
    pub fn empty() -> Self {
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
    use tempfile::NamedTempFile;

    #[test]
    fn test_audio_feedback_player_creation() {
        let player = AudioFeedbackPlayer::new(48000);
        assert_eq!(player.sample_rate, 48000);
    }

    #[test]
    fn test_generate_silence() {
        let player = AudioFeedbackPlayer::new(48000);
        let silence = player.generate_silence(100); // 100ms
        assert_eq!(silence.len(), 4800); // 48000 * 0.1
        assert!(silence.iter().all(|&s| s == 0));
    }

    #[test]
    fn test_load_nonexistent_file() {
        let player = AudioFeedbackPlayer::new(48000);
        let result = player.load_audio_file("/nonexistent/path.wav");
        assert!(result.is_none());
    }

    #[test]
    fn test_load_wav_file() {
        // Create a temporary WAV file with test audio
        let temp_file = NamedTempFile::with_suffix(".wav").unwrap();
        let wav_path = temp_file.path().to_path_buf();

        // Create a simple WAV file: 1 second of 440Hz sine wave at 16kHz
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        {
            let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
            for i in 0..16000 {
                let t = i as f32 / 16000.0;
                let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin();
                let amplitude = sample * i16::MAX as f32;
                writer.write_sample(amplitude as i16).unwrap();
            }
            writer.finalize().unwrap();
        }

        // Verify file exists
        assert!(wav_path.exists(), "WAV file should exist");

        // Load the WAV file
        let player = AudioFeedbackPlayer::new(16000);
        let samples = player.load_audio_file(&wav_path);

        assert!(samples.is_some(), "Should load WAV file successfully");
        let samples = samples.unwrap();
        assert_eq!(samples.len(), 16000); // 1 second at 16kHz
        assert!(samples.iter().any(|&s| s != 0)); // Should have non-zero samples

        // Keep temp_file alive until end of test
        drop(temp_file);
    }

    #[test]
    fn test_load_wav_with_resampling() {
        // Create a WAV file at 8kHz
        let temp_file = NamedTempFile::with_suffix(".wav").unwrap();
        let wav_path = temp_file.path().to_path_buf();

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        {
            let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
            for i in 0..8000 {
                let t = i as f32 / 8000.0;
                let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin();
                let amplitude = sample * i16::MAX as f32;
                writer.write_sample(amplitude as i16).unwrap();
            }
            writer.finalize().unwrap();
        }

        // Load and resample to 16kHz
        let player = AudioFeedbackPlayer::new(16000);
        let samples = player.load_audio_file(&wav_path);

        assert!(samples.is_some());
        let samples = samples.unwrap();
        // Should be resampled to 16kHz (approximately 16000 samples)
        assert!(samples.len() >= 15900 && samples.len() <= 16100);
        assert!(samples.iter().any(|&s| s != 0));

        drop(temp_file);
    }

    #[test]
    fn test_load_stereo_wav() {
        // Create a stereo WAV file
        let temp_file = NamedTempFile::with_suffix(".wav").unwrap();
        let wav_path = temp_file.path().to_path_buf();

        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        {
            let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
            for i in 0..16000 {
                let t = i as f32 / 16000.0;
                let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin();
                let amplitude = (sample * i16::MAX as f32) as i16;
                writer.write_sample(amplitude).unwrap(); // Left
                writer.write_sample(amplitude).unwrap(); // Right
            }
            writer.finalize().unwrap();
        }

        // Load stereo file (should be converted to mono)
        let player = AudioFeedbackPlayer::new(16000);
        let samples = player.load_audio_file(&wav_path);

        assert!(samples.is_some());
        let samples = samples.unwrap();
        assert_eq!(samples.len(), 16000); // Should be mono (averaged)
        assert!(samples.iter().any(|&s| s != 0));

        drop(temp_file);
    }

    #[test]
    fn test_resample() {
        let player = AudioFeedbackPlayer::new(48000);

        // Create a simple test signal at 8kHz
        let input: Vec<i16> = (0..8000).map(|i| (i % 1000) as i16).collect();

        // Resample to 16kHz (should double the length)
        let output = player.resample(&input, 8000, 16000);
        assert!(output.len() >= 15900 && output.len() <= 16100);

        // Resample to same rate (should return same length)
        let output_same = player.resample(&input, 8000, 8000);
        assert_eq!(output_same.len(), input.len());
    }

    #[test]
    fn test_conference_sounds_empty() {
        let sounds = ConferenceSounds::empty();
        assert!(sounds.join_sound.is_none());
        assert!(sounds.exit_sound.is_none());
        assert!(sounds.alone_sound.is_none());
    }
}
