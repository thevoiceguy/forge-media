//! Audio recording from RTP streams
//!
//! Records RTP audio packets to WAV or Opus files

use crate::{AudioCodec, AudioFormat, RecorderError, Result};
#[cfg(feature = "opus")]
use audiopus::coder::Encoder as OpusEncoder;
#[cfg(feature = "opus")]
use audiopus::{Application, Channels, SampleRate};
use bytes::Bytes;
use hound::{WavSpec, WavWriter};
use parking_lot::Mutex;
use std::fs::File;
use std::io::BufWriter;
#[cfg(feature = "opus")]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tracing::{info, warn};

/// Writer abstraction for different audio formats
enum RecorderWriter {
    /// WAV format writer
    Wav(WavWriter<BufWriter<File>>),
    /// Opus format writer with Ogg container
    #[cfg(feature = "opus")]
    Opus {
        encoder: OpusEncoder,
        file: BufWriter<File>,
        frame_buffer: Vec<i16>,
        frame_size: usize,
        ogg_stream: ogg::PacketWriter<BufWriter<File>>,
    },
}

/// Audio recorder for RTP streams
pub struct AudioRecorder {
    /// Output file path
    path: PathBuf,
    /// Audio format
    format: AudioFormat,
    /// Format-specific writer (wrapped in Arc<Mutex> for thread safety)
    writer: Arc<Mutex<Option<RecorderWriter>>>,
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

        info!("Starting recording to {:?} with codec {:?}", self.path, self.format.codec);

        let writer = match self.format.codec {
            AudioCodec::PCM => {
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
                    .map_err(|e| RecorderError::Internal(format!("Failed to create WAV writer: {}", e)))?;

                RecorderWriter::Wav(wav_writer)
            }
            #[cfg(feature = "opus")]
            AudioCodec::Opus => {
                // Determine sample rate for Opus
                let opus_sample_rate = match self.format.sample_rate {
                    8000 => SampleRate::Hz8000,
                    12000 => SampleRate::Hz12000,
                    16000 => SampleRate::Hz16000,
                    24000 => SampleRate::Hz24000,
                    48000 => SampleRate::Hz48000,
                    _ => {
                        warn!("Unsupported sample rate {} for Opus, using 48000", self.format.sample_rate);
                        SampleRate::Hz48000
                    }
                };

                // Determine channel configuration
                let channels = if self.format.channels == 1 {
                    Channels::Mono
                } else {
                    Channels::Stereo
                };

                // Create Opus encoder
                let encoder = OpusEncoder::new(opus_sample_rate, channels, Application::Voip)
                    .map_err(|e| RecorderError::Encoding(format!("Failed to create Opus encoder: {:?}", e)))?;

                // Frame size: 20ms of audio (sample_rate / 50)
                let frame_size = (self.format.sample_rate / 50) as usize * self.format.channels as usize;

                // Create file for Ogg container
                let file = std::fs::File::create(&self.path)?;
                let buf_writer = std::io::BufWriter::new(file);

                // Create Ogg packet writer
                let ogg_stream = ogg::PacketWriter::new(buf_writer);

                RecorderWriter::Opus {
                    encoder,
                    file: std::io::BufWriter::new(std::fs::File::create(&self.path)?),
                    frame_buffer: Vec::with_capacity(frame_size),
                    frame_size,
                    ogg_stream,
                }
            }
            _ => {
                return Err(RecorderError::Encoding(format!(
                    "Unsupported codec for recording: {:?}",
                    self.format.codec
                )));
            }
        };

        *self.writer.lock() = Some(writer);
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
            match writer {
                RecorderWriter::Wav(wav_writer) => {
                    // Write directly to WAV
                    for &sample in samples {
                        wav_writer.write_sample(sample)
                            .map_err(|e| RecorderError::Encoding(format!("Failed to write sample: {}", e)))?;
                    }
                }
                #[cfg(feature = "opus")]
                RecorderWriter::Opus { encoder, file, frame_buffer, frame_size, ogg_stream } => {
                    // Buffer samples for Opus frame-based encoding
                    let mut remaining = samples;

                    while !remaining.is_empty() {
                        let space_in_buffer = *frame_size - frame_buffer.len();
                        let to_copy = remaining.len().min(space_in_buffer);

                        frame_buffer.extend_from_slice(&remaining[..to_copy]);
                        remaining = &remaining[to_copy..];

                        // If we have a complete frame, encode it
                        if frame_buffer.len() == *frame_size {
                            self.encode_opus_frame(encoder, file, frame_buffer, ogg_stream)?;
                            frame_buffer.clear();
                        }
                    }
                }
            }

            let mut samples_recorded = self.samples_recorded.lock();
            *samples_recorded += samples.len() as u64;

            Ok(())
        } else {
            Err(RecorderError::Internal("Recorder not started".to_string()))
        }
    }

    /// Encode and write a single Opus frame
    #[cfg(feature = "opus")]
    fn encode_opus_frame(
        &self,
        encoder: &mut OpusEncoder,
        file: &mut BufWriter<File>,
        frame: &[i16],
        ogg_stream: &mut ogg::PacketWriter<BufWriter<File>>,
    ) -> Result<()> {
        // Allocate output buffer for encoded data (max Opus packet is 4000 bytes)
        let mut output = vec![0u8; 4000];

        // Encode the frame
        let encoded_size = encoder.encode(frame, &mut output)
            .map_err(|e| RecorderError::Encoding(format!("Opus encoding failed: {:?}", e)))?;

        // Truncate to actual encoded size
        output.truncate(encoded_size);

        // Write as Ogg packet
        let packet = ogg::Packet {
            data: output.into(),
            granule_pos: 0, // Updated during finalization
            absgp_page: false,
        };

        ogg_stream.write_packet(packet, 0, ogg::PacketWriteEndInfo::NormalPacket, None)
            .map_err(|e| RecorderError::Encoding(format!("Failed to write Ogg packet: {}", e)))?;

        Ok(())
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
            match writer {
                RecorderWriter::Wav(wav_writer) => {
                    wav_writer.finalize()
                        .map_err(|e| RecorderError::Internal(format!("Failed to finalize WAV file: {}", e)))?;
                }
                #[cfg(feature = "opus")]
                RecorderWriter::Opus { mut encoder, mut file, frame_buffer, frame_size, mut ogg_stream } => {
                    // Encode any remaining buffered samples as a partial frame
                    if !frame_buffer.is_empty() {
                        // Pad with silence to complete the frame
                        let mut padded_frame = frame_buffer;
                        padded_frame.resize(frame_size, 0);
                        self.encode_opus_frame(&mut encoder, &mut file, &padded_frame, &mut ogg_stream)?;
                    }

                    // Finalize Ogg stream
                    ogg_stream.write_packet(
                        ogg::Packet {
                            data: vec![].into(),
                            granule_pos: 0,
                            absgp_page: true,
                        },
                        0,
                        ogg::PacketWriteEndInfo::EndStream,
                        None,
                    ).map_err(|e| RecorderError::Internal(format!("Failed to finalize Ogg stream: {}", e)))?;

                    // Flush file buffer
                    file.flush()
                        .map_err(|e| RecorderError::Internal(format!("Failed to flush Opus file: {}", e)))?;
                }
            }
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
