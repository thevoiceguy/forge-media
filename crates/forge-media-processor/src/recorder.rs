//! Audio recording from RTP streams
//!
//! Records RTP audio packets to WAV files

use crate::{AudioFormat, MediaError, Result};
use bytes::Bytes;
use hound::{WavSpec, WavWriter};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tracing::{info, warn};

/// Audio recorder for RTP streams
pub struct AudioRecorder {
    /// Output file path
    path: PathBuf,
    /// Audio format
    format: AudioFormat,
    /// WAV writer (wrapped in Arc<Mutex> for thread safety)
    writer: Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>,
    /// Total samples recorded
    samples_recorded: Arc<Mutex<u64>>,
    /// Recording state
    is_recording: Arc<Mutex<bool>>,
}

impl AudioRecorder {
    /// Create a new audio recorder
    ///
    /// # Arguments
    /// * `path` - Output file path
    /// * `format` - Audio format configuration
    pub async fn new<P: AsRef<Path>>(path: P, format: AudioFormat) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        info!("Creating audio recorder at {:?}", path);

        Ok(Self {
            path,
            format,
            writer: Arc::new(Mutex::new(None)),
            samples_recorded: Arc::new(Mutex::new(0)),
            is_recording: Arc::new(Mutex::new(false)),
        })
    }

    /// Start recording
    pub fn start(&self) -> Result<()> {
        let mut is_recording = self.is_recording.lock();
        if *is_recording {
            warn!("Recording already started");
            return Ok(());
        }

        info!("Starting recording to {:?}", self.path);

        // Create WAV specification
        let spec = WavSpec {
            channels: self.format.channels,
            sample_rate: self.format.sample_rate,
            bits_per_sample: 16, // 16-bit PCM
            sample_format: hound::SampleFormat::Int,
        };

        // Create WAV writer
        let file = std::fs::File::create(&self.path)?;
        let buf_writer = std::io::BufWriter::new(file);
        let wav_writer = WavWriter::new(buf_writer, spec)
            .map_err(|e| MediaError::Internal(format!("Failed to create WAV writer: {}", e)))?;

        *self.writer.lock() = Some(wav_writer);
        *is_recording = true;
        *self.samples_recorded.lock() = 0;

        info!("Recording started successfully");
        Ok(())
    }

    /// Write audio samples to the recording
    ///
    /// # Arguments
    /// * `samples` - PCM audio samples (16-bit signed integers)
    pub fn write_samples(&self, samples: &[i16]) -> Result<()> {
        let mut writer_guard = self.writer.lock();

        if let Some(writer) = writer_guard.as_mut() {
            for &sample in samples {
                writer.write_sample(sample)
                    .map_err(|e| MediaError::Encoding(format!("Failed to write sample: {}", e)))?;
            }

            let mut samples_recorded = self.samples_recorded.lock();
            *samples_recorded += samples.len() as u64;

            Ok(())
        } else {
            Err(MediaError::Internal("Recorder not started".to_string()))
        }
    }

    /// Write raw RTP payload (assumes PCM data)
    ///
    /// # Arguments
    /// * `payload` - Raw RTP payload bytes
    pub fn write_rtp_payload(&self, payload: &Bytes) -> Result<()> {
        // Convert bytes to i16 samples (assuming 16-bit PCM)
        let samples: Vec<i16> = payload
            .chunks_exact(2)
            .map(|chunk| i16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();

        self.write_samples(&samples)
    }

    /// Stop recording and finalize the file
    pub fn stop(&self) -> Result<()> {
        let mut is_recording = self.is_recording.lock();
        if !*is_recording {
            warn!("Recording not started");
            return Ok(());
        }

        info!("Stopping recording");

        let mut writer_guard = self.writer.lock();
        if let Some(writer) = writer_guard.take() {
            writer.finalize()
                .map_err(|e| MediaError::Internal(format!("Failed to finalize WAV file: {}", e)))?;
        }

        *is_recording = false;

        let samples_recorded = *self.samples_recorded.lock();
        let duration_secs = samples_recorded as f64 / self.format.sample_rate as f64;
        info!(
            "Recording stopped. Recorded {} samples ({:.2}s) to {:?}",
            samples_recorded, duration_secs, self.path
        );

        Ok(())
    }

    /// Get the number of samples recorded so far
    pub fn samples_recorded(&self) -> u64 {
        *self.samples_recorded.lock()
    }

    /// Get recording duration in seconds
    pub fn duration_secs(&self) -> f64 {
        let samples = self.samples_recorded();
        samples as f64 / self.format.sample_rate as f64
    }

    /// Check if currently recording
    pub fn is_recording(&self) -> bool {
        *self.is_recording.lock()
    }

    /// Get the output file path
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        // Ensure recording is stopped and file is finalized
        if *self.is_recording.lock() {
            if let Err(e) = self.stop() {
                warn!("Error stopping recording in drop: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_recorder_lifecycle() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.wav");

        let format = AudioFormat::pcm_mono();
        let recorder = AudioRecorder::new(&file_path, format).await.unwrap();

        // Start recording
        recorder.start().unwrap();
        assert!(recorder.is_recording());

        // Write some samples
        let samples: Vec<i16> = (0..48000).map(|i| (i % 1000) as i16).collect();
        recorder.write_samples(&samples).unwrap();

        assert_eq!(recorder.samples_recorded(), 48000);
        assert!((recorder.duration_secs() - 1.0).abs() < 0.01);

        // Stop recording
        recorder.stop().unwrap();
        assert!(!recorder.is_recording());

        // Verify file exists
        assert!(file_path.exists());
    }
}
