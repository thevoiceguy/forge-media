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
#[allow(unused_imports)]
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
#[allow(unused_imports)]
use std::hash::{Hash, Hasher};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tracing::{info, warn};

/// Metadata for Opus recordings
///
/// This metadata is written to the OpusTags packet in the Ogg container.
/// All fields are optional and will be omitted if not provided.
///
/// # Example
/// ```ignore
/// use forge_recorder::OpusMetadata;
///
/// let metadata = OpusMetadata {
///     title: Some("Conference Call".into()),
///     artist: Some("John Doe".into()),
///     album: Some("Business Calls".into()),
///     date: Some("2025-12-18".into()),
///     comment: Some("Monthly review meeting".into()),
/// };
/// ```
#[allow(dead_code)] // TODO: Implement Opus recording support
#[derive(Debug, Clone, Default)]
pub struct OpusMetadata {
    /// Recording title
    pub title: Option<String>,
    /// Artist/Participant name
    pub artist: Option<String>,
    /// Album/Collection name
    pub album: Option<String>,
    /// Recording date (ISO 8601 format recommended: YYYY-MM-DD)
    pub date: Option<String>,
    /// Additional comments
    pub comment: Option<String>,
}

#[allow(dead_code)] // TODO: Implement Opus recording support
impl OpusMetadata {
    /// Create empty metadata
    pub fn new() -> Self {
        Self::default()
    }

    /// Create metadata with title only
    pub fn with_title(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Default::default()
        }
    }

    /// Get all non-empty tags as (key, value) pairs for OpusTags encoding
    fn to_vorbis_comments(&self) -> Vec<(String, String)> {
        let mut comments = Vec::new();

        if let Some(ref title) = self.title {
            comments.push(("TITLE".to_string(), title.clone()));
        }
        if let Some(ref artist) = self.artist {
            comments.push(("ARTIST".to_string(), artist.clone()));
        }
        if let Some(ref album) = self.album {
            comments.push(("ALBUM".to_string(), album.clone()));
        }
        if let Some(ref date) = self.date {
            comments.push(("DATE".to_string(), date.clone()));
        }
        if let Some(ref comment) = self.comment {
            comments.push(("COMMENT".to_string(), comment.clone()));
        }

        comments
    }
}

/// Writer abstraction for different audio formats
enum RecorderWriter {
    /// WAV format writer
    Wav(WavWriter<BufWriter<File>>),
    /// Opus format writer with Ogg container
    #[cfg(feature = "opus")]
    Opus {
        encoder: OpusEncoder,
        frame_buffer: Vec<i16>,
        frame_size: usize,
        ogg_stream: ogg::PacketWriter<BufWriter<File>>,
        stream_serial: u32,
        granule_pos: u64,
        effective_sample_rate: u32,
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
    /// Effective sample rate actually used by the encoder
    effective_sample_rate: Arc<Mutex<u32>>,
    /// Metadata for Opus recordings (ignored for other formats)
    #[cfg(feature = "opus")]
    metadata: Option<OpusMetadata>,
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
            effective_sample_rate: Arc::new(Mutex::new(format.sample_rate)),
            #[cfg(feature = "opus")]
            metadata: None,
        })
    }

    /// Create a new audio recorder with metadata (Opus only)
    ///
    /// # Arguments
    /// * `path` - Output file path
    /// * `format` - Audio format configuration
    /// * `metadata` - Metadata tags (only used for Opus format)
    ///
    /// # Example
    /// ```ignore
    /// use forge_recorder::{AudioRecorder, AudioFormat, OpusMetadata};
    ///
    /// let metadata = OpusMetadata::with_title("Conference Call");
    /// let recorder = AudioRecorder::with_metadata(
    ///     "/tmp/recording.opus",
    ///     AudioFormat::opus_mono(),
    ///     metadata
    /// ).await?;
    /// ```
    #[cfg(feature = "opus")]
    pub async fn with_metadata<P: AsRef<Path>>(
        path: P,
        format: AudioFormat,
        metadata: OpusMetadata,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        info!("Creating audio recorder with metadata at {:?}", path);

        Ok(Self {
            path,
            format,
            writer: Arc::new(Mutex::new(None)),
            samples_recorded: Arc::new(Mutex::new(0)),
            is_recording: Arc::new(Mutex::new(false)),
            effective_sample_rate: Arc::new(Mutex::new(format.sample_rate)),
            metadata: Some(metadata),
        })
    }

    /// Start recording
    pub fn start(&self) -> Result<()> {
        let mut is_recording = self.is_recording.lock();
        if *is_recording {
            return Err(RecorderError::AlreadyRecording);
        }

        info!(
            "Starting recording to {:?} with codec {:?}",
            self.path, self.format.codec
        );

        let writer = match self.format.codec {
            AudioCodec::PCM => {
                *self.effective_sample_rate.lock() = self.format.sample_rate;
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
                let wav_writer = WavWriter::new(buf_writer, spec).map_err(|e| {
                    RecorderError::Internal(format!("Failed to create WAV writer: {}", e))
                })?;

                RecorderWriter::Wav(wav_writer)
            }
            #[cfg(feature = "opus")]
            AudioCodec::Opus => {
                // Determine sample rate for Opus
                let (opus_sample_rate, effective_sample_rate) = match self.format.sample_rate {
                    8000 => (SampleRate::Hz8000, 8000),
                    12000 => (SampleRate::Hz12000, 12000),
                    16000 => (SampleRate::Hz16000, 16000),
                    24000 => (SampleRate::Hz24000, 24000),
                    48000 => (SampleRate::Hz48000, 48000),
                    _ => {
                        warn!(
                            "Unsupported sample rate {} for Opus, using 48000",
                            self.format.sample_rate
                        );
                        (SampleRate::Hz48000, 48000)
                    }
                };
                *self.effective_sample_rate.lock() = effective_sample_rate;

                // Determine channel configuration
                let channels = if self.format.channels == 1 {
                    Channels::Mono
                } else {
                    Channels::Stereo
                };

                // Create Opus encoder
                let encoder = OpusEncoder::new(opus_sample_rate, channels, Application::Voip)
                    .map_err(|e| {
                        RecorderError::Encoding(format!("Failed to create Opus encoder: {:?}", e))
                    })?;

                // Frame size: 20ms of audio (sample_rate / 50)
                let frame_size =
                    (effective_sample_rate / 50) as usize * self.format.channels as usize;

                // Create file for Ogg container
                let file = std::fs::File::create(&self.path)?;
                let buf_writer = std::io::BufWriter::new(file);

                // Create Ogg packet writer
                let mut ogg_stream = ogg::PacketWriter::new(buf_writer);

                // Deterministic stream serial derived from output path
                let stream_serial = {
                    let mut hasher = DefaultHasher::new();
                    self.path.hash(&mut hasher);
                    (hasher.finish() & 0xFFFF_FFFF) as u32
                };

                Self::write_opus_headers(
                    &mut ogg_stream,
                    stream_serial,
                    self.format.channels,
                    effective_sample_rate,
                    self.metadata.as_ref(),
                )?;

                RecorderWriter::Opus {
                    encoder,
                    frame_buffer: Vec::with_capacity(frame_size),
                    frame_size,
                    ogg_stream,
                    stream_serial,
                    granule_pos: 0,
                    effective_sample_rate,
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
                        wav_writer.write_sample(sample).map_err(|e| {
                            RecorderError::Encoding(format!("Failed to write sample: {}", e))
                        })?;
                    }
                }
                #[cfg(feature = "opus")]
                RecorderWriter::Opus {
                    encoder,
                    frame_buffer,
                    frame_size,
                    ogg_stream,
                    stream_serial,
                    granule_pos,
                    effective_sample_rate: _,
                } => {
                    // Buffer samples for Opus frame-based encoding
                    let mut remaining = samples;

                    while !remaining.is_empty() {
                        let space_in_buffer = *frame_size - frame_buffer.len();
                        let to_copy = remaining.len().min(space_in_buffer);

                        frame_buffer.extend_from_slice(&remaining[..to_copy]);
                        remaining = &remaining[to_copy..];

                        // If we have a complete frame, encode it
                        if frame_buffer.len() == *frame_size {
                            self.encode_opus_frame(
                                encoder,
                                frame_buffer,
                                ogg_stream,
                                *stream_serial,
                                granule_pos,
                            )?;
                            frame_buffer.clear();
                        }
                    }
                }
            }

            let mut samples_recorded = self.samples_recorded.lock();
            *samples_recorded += samples.len() as u64;

            Ok(())
        } else {
            Err(RecorderError::NotRecording)
        }
    }

    /// Encode and write a single Opus frame
    #[cfg(feature = "opus")]
    fn encode_opus_frame(
        &self,
        encoder: &mut OpusEncoder,
        frame: &[i16],
        ogg_stream: &mut ogg::PacketWriter<BufWriter<File>>,
        stream_serial: u32,
        granule_pos: &mut u64,
    ) -> Result<()> {
        // Allocate output buffer for encoded data (max Opus packet is 4000 bytes)
        let mut output = vec![0u8; 4000];

        // Encode the frame
        let encoded_size = encoder
            .encode(frame, &mut output)
            .map_err(|e| RecorderError::Encoding(format!("Opus encoding failed: {:?}", e)))?;

        // Truncate to actual encoded size
        output.truncate(encoded_size);

        let channel_count = self.format.channels.max(1) as u64;
        let samples_this_packet = frame.len() as u64 / channel_count;
        let packet_granule = *granule_pos + samples_this_packet;

        // Write as Ogg packet
        ogg_stream
            .write_packet(
                output.into_boxed_slice(),
                stream_serial,
                ogg::PacketWriteEndInfo::NormalPacket,
                packet_granule,
            )
            .map_err(|e| RecorderError::Encoding(format!("Failed to write Ogg packet: {}", e)))?;

        *granule_pos = packet_granule;

        Ok(())
    }

    /// Write raw RTP payload to recording
    ///
    /// # Arguments
    /// * `payload` - Raw audio samples in **native-endian** 16-bit PCM format
    ///
    /// # Format Requirements
    /// - Samples must be 16-bit signed integers
    /// - Byte order must be **little-endian** (native endianness on x86/ARM)
    /// - Length must be even (2 bytes per sample)
    /// - Length must align to frame boundaries (channels × 2 bytes)
    ///
    /// # RTP Integration Note
    /// When receiving RTP packets containing PCM audio (PT 0/8/10/11), note that
    /// RFC 3551 specifies **big-endian** (network byte order) for RTP payloads.
    /// You must convert to native byte order before calling this method:
    ///
    /// ```ignore
    /// use bytes::Bytes;
    ///
    /// // Received RTP payload (big-endian per RFC 3551)
    /// let rtp_payload: &[u8] = /* ... from RTP packet ... */;
    ///
    /// // Convert to native-endian i16 samples
    /// let samples: Vec<i16> = rtp_payload
    ///     .chunks_exact(2)
    ///     .map(|chunk| i16::from_be_bytes([chunk[0], chunk[1]]))
    ///     .collect();
    ///
    /// // Convert back to bytes for this method
    /// let native_bytes: Vec<u8> = samples
    ///     .iter()
    ///     .flat_map(|s| s.to_le_bytes())
    ///     .collect();
    ///
    /// recorder.write_rtp_payload(&Bytes::from(native_bytes))?;
    /// ```
    ///
    /// # Errors
    /// Returns error if:
    /// - Payload length is not even (not 16-bit aligned)
    /// - Payload length doesn't align to frame boundaries
    /// - Recording is not active
    pub fn write_rtp_payload(&self, payload: &Bytes) -> Result<()> {
        let bytes = payload.as_ref();
        if bytes.len() % 2 != 0 {
            return Err(RecorderError::InvalidFormat(
                "RTP payload length must be even (16-bit samples)".into(),
            ));
        }

        let frame_bytes = (self.format.channels as usize).saturating_mul(2);
        if frame_bytes == 0 || bytes.len() % frame_bytes != 0 {
            return Err(RecorderError::InvalidFormat(format!(
                "RTP payload length {} is not aligned to {}-byte frames for {} channels",
                bytes.len(),
                frame_bytes,
                self.format.channels
            )));
        }

        // Convert bytes to i16 samples (little-endian PCM)
        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        self.write_samples(&samples)
    }

    /// Stop recording and finalize the file
    pub fn stop(&self) -> Result<()> {
        let mut is_recording = self.is_recording.lock();
        if !*is_recording {
            return Err(RecorderError::NotRecording);
        }

        info!("Stopping recording");

        let mut writer_guard = self.writer.lock();
        if let Some(writer) = writer_guard.take() {
            match writer {
                RecorderWriter::Wav(wav_writer) => {
                    wav_writer.finalize().map_err(|e| {
                        RecorderError::Internal(format!("Failed to finalize WAV file: {}", e))
                    })?;
                }
                #[cfg(feature = "opus")]
                RecorderWriter::Opus {
                    mut encoder,
                    frame_buffer,
                    frame_size,
                    mut ogg_stream,
                    stream_serial,
                    mut granule_pos,
                    effective_sample_rate,
                } => {
                    *self.effective_sample_rate.lock() = effective_sample_rate;
                    // Encode any remaining buffered samples as a partial frame
                    if !frame_buffer.is_empty() {
                        // Pad with silence to complete the frame
                        let mut padded_frame = frame_buffer;
                        padded_frame.resize(frame_size, 0);
                        self.encode_opus_frame(
                            &mut encoder,
                            &padded_frame,
                            &mut ogg_stream,
                            stream_serial,
                            &mut granule_pos,
                        )?;
                    }

                    // Finalize Ogg stream
                    ogg_stream
                        .write_packet(
                            Vec::new().into_boxed_slice(),
                            stream_serial,
                            ogg::PacketWriteEndInfo::EndStream,
                            granule_pos,
                        )
                        .map_err(|e| {
                            RecorderError::Internal(format!("Failed to finalize Ogg stream: {}", e))
                        })?;
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
        let sample_rate = *self.effective_sample_rate.lock();
        samples as f64 / sample_rate as f64
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

impl AudioRecorder {
    #[cfg(feature = "opus")]
    fn write_opus_headers(
        ogg_stream: &mut ogg::PacketWriter<BufWriter<File>>,
        stream_serial: u32,
        channels: u16,
        sample_rate: u32,
        metadata: Option<&OpusMetadata>,
    ) -> Result<()> {
        if channels == 0 {
            return Err(RecorderError::InvalidFormat(
                "Opus requires at least one channel".into(),
            ));
        }

        // Calculate preskip: Opus codec has 6.5ms algorithmic delay
        // This must be communicated to decoders to discard the correct amount
        let preskip: u16 = match sample_rate {
            8000 => 52,   // 6.5ms @ 8kHz = 52 samples
            12000 => 78,  // 6.5ms @ 12kHz = 78 samples
            16000 => 104, // 6.5ms @ 16kHz = 104 samples
            24000 => 156, // 6.5ms @ 24kHz = 156 samples
            48000 => 312, // 6.5ms @ 48kHz = 312 samples
            _ => {
                warn!(
                    "Unknown sample rate {} for preskip calculation, using 312 (48kHz)",
                    sample_rate
                );
                312
            }
        };

        // OpusHead (RFC 7845 Section 5.1)
        let mut opus_head = Vec::with_capacity(19);
        opus_head.extend_from_slice(b"OpusHead");
        opus_head.push(1); // version
        opus_head.push(channels as u8);
        opus_head.extend_from_slice(&preskip.to_le_bytes());
        opus_head.extend_from_slice(&sample_rate.to_le_bytes());
        opus_head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        opus_head.push(0); // channel mapping (RTP mapping)

        ogg_stream
            .write_packet(
                opus_head.into_boxed_slice(),
                stream_serial,
                ogg::PacketWriteEndInfo::EndPage,
                0,
            )
            .map_err(|e| RecorderError::Encoding(format!("Failed to write OpusHead: {}", e)))?;

        // OpusTags (RFC 7845 Section 5.2) with optional user metadata
        let vendor = b"forge-recorder";
        let user_comments = metadata.map(|m| m.to_vorbis_comments()).unwrap_or_default();

        // Calculate size: magic(8) + vendor_len(4) + vendor + comment_count(4) + comments
        let mut opus_tags_size = 8 + 4 + vendor.len() + 4;
        for (key, value) in &user_comments {
            // Each comment: length(4) + "KEY=value"
            opus_tags_size += 4 + key.len() + 1 + value.len();
        }

        let mut opus_tags = Vec::with_capacity(opus_tags_size);
        opus_tags.extend_from_slice(b"OpusTags");
        opus_tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        opus_tags.extend_from_slice(vendor);
        opus_tags.extend_from_slice(&(user_comments.len() as u32).to_le_bytes());

        // Encode user comments as Vorbis comment format
        for (key, value) in &user_comments {
            let comment = format!("{}={}", key, value);
            opus_tags.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            opus_tags.extend_from_slice(comment.as_bytes());
        }

        ogg_stream
            .write_packet(
                opus_tags.into_boxed_slice(),
                stream_serial,
                ogg::PacketWriteEndInfo::EndPage,
                0,
            )
            .map_err(|e| RecorderError::Encoding(format!("Failed to write OpusTags: {}", e)))?;

        Ok(())
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

    #[cfg(feature = "opus")]
    #[tokio::test]
    async fn test_opus_headers_and_granule_progress() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.opus");

        let format = AudioFormat::opus_mono();
        let recorder = AudioRecorder::new(&file_path, format).await.unwrap();

        recorder.start().unwrap();

        // 3 frames of 20ms audio at 48kHz mono -> 2880 samples
        let samples = vec![1i16; 960 * 3];
        recorder.write_samples(&samples).unwrap();
        recorder.stop().unwrap();

        let file = std::fs::File::open(&file_path).unwrap();
        let mut packets = ogg::PacketReader::new(std::io::BufReader::new(file));

        let head = packets.read_packet().unwrap().unwrap();
        assert!(head.data.starts_with(b"OpusHead"));

        let tags = packets.read_packet().unwrap().unwrap();
        assert!(tags.data.starts_with(b"OpusTags"));

        let first_data = packets.read_packet().unwrap().unwrap();
        assert!(first_data.absgp_page() > 0);
        assert_eq!(head.stream_serial(), first_data.stream_serial());

        let mut last = first_data;
        while let Some(pkt) = packets.read_packet().unwrap() {
            last = pkt;
        }

        assert!(last.last_in_stream());
        assert!(last.absgp_page() >= 960);
    }
}
