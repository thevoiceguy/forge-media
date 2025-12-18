//! File-based audio source using Symphonia

use crate::error::{InjectionError, Result};
use crate::source::AudioSource;
use forge_core::AudioFrame;
use std::path::Path;
use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::conv::FromSample;
use symphonia::core::sample::Sample;
use symphonia::core::units::{Time, TimeBase};
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::debug;

/// Audio file source using Symphonia for decoding
///
/// Supports a wide variety of audio formats including:
/// - WAV, AIFF, FLAC
/// - MP3, AAC, Vorbis, Opus
/// - WMA, ALAC, APE
/// - and more...
///
/// # Example
///
/// ```rust,ignore
/// use forge_injection::FileSource;
///
/// let mut source = FileSource::new("announcement.wav").await?;
/// println!("Duration: {:.2}s", source.duration().unwrap());
/// println!("Sample rate: {}Hz", source.sample_rate());
///
/// while let Ok(frame) = source.read_frame(960) {
///     // Process audio frame
/// }
/// ```
pub struct FileSource {
    /// Symphonia format reader
    format: Box<dyn FormatReader>,

    /// Symphonia decoder
    decoder: Box<dyn Decoder>,

    /// Track ID being decoded
    track_id: u32,

    /// Track time base
    time_base: TimeBase,

    /// Sample rate in Hz
    sample_rate: u32,

    /// Number of channels
    channels: u8,

    /// Total duration in samples (if known)
    total_samples: Option<u64>,

    /// Current sample position
    position: u64,

    /// Buffer for decoded samples
    sample_buffer: Vec<i16>,

    /// File path for debugging
    file_path: String,

    /// Finished flag
    finished: bool,
}

impl FileSource {
    /// Create a new file source from a path
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the audio file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, the format is unsupported,
    /// or no audio track is found.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file_path = path.display().to_string();

        debug!("Opening audio file: {}", file_path);

        // Open the file
        let file = std::fs::File::open(path)
            .map_err(|_| InjectionError::FileNotFound(file_path.clone()))?;

        // Create the media source
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // Create a hint based on the file extension
        let mut hint = Hint::new();
        if let Some(ext) = path.extension() {
            if let Some(ext_str) = ext.to_str() {
                hint.with_extension(ext_str);
            }
        }

        // Probe the file format
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| {
                InjectionError::UnsupportedFormat(format!("Failed to probe file format: {}", e))
            })?;

        let format = probed.format;

        // Find the first audio track
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or_else(|| InjectionError::UnsupportedFormat("No audio track found".to_string()))?;

        let track_id = track.id;
        let codec_params = &track.codec_params;

        debug!("Audio track found: codec={:?}", codec_params.codec);

        // Get track info
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| InjectionError::InvalidParameters("Unknown sample rate".to_string()))?;

        let channels = codec_params
            .channels
            .ok_or_else(|| InjectionError::InvalidParameters("Unknown channel count".to_string()))?
            .count() as u8;

        let time_base = codec_params
            .time_base
            .unwrap_or_else(|| TimeBase::new(1, sample_rate));

        let total_samples = codec_params.n_frames;

        debug!(
            "Audio format: {}Hz, {} channels, duration: {:?} samples",
            sample_rate, channels, total_samples
        );

        // Create a decoder
        let decoder = symphonia::default::get_codecs()
            .make(codec_params, &DecoderOptions::default())
            .map_err(|e| {
                InjectionError::UnsupportedFormat(format!("Failed to create decoder: {}", e))
            })?;

        Ok(Self {
            format,
            decoder,
            track_id,
            time_base,
            sample_rate,
            channels,
            total_samples,
            position: 0,
            sample_buffer: Vec::new(),
            file_path,
            finished: false,
        })
    }

    /// Set the target sample rate for resampling
    ///
    /// If the file's native sample rate differs from the target, audio will be
    /// resampled on the fly.
    ///
    /// # Arguments
    ///
    /// * `target_rate` - Desired sample rate in Hz
    ///
    /// # Note
    ///
    /// Resampling is not yet implemented. This currently returns an error if
    /// the requested rate differs from the source.
    pub fn with_sample_rate(self, target_rate: u32) -> Result<Self> {
        if target_rate != self.sample_rate {
            return Err(InjectionError::ResamplingError(format!(
                "Requested sample rate {}Hz differs from source {}Hz (resampling not implemented)",
                target_rate, self.sample_rate
            )));
        }

        Ok(self)
    }

    /// Decode the next packet and fill the sample buffer
    fn decode_next_packet(&mut self) -> Result<()> {
        loop {
            // Read the next packet
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.finished = true;
                    return Err(InjectionError::EndOfFile);
                }
                Err(e) => {
                    self.finished = true;
                    return Err(e.into());
                }
            };

            // Skip packets that aren't for our track
            if packet.track_id() != self.track_id {
                continue;
            }

            // Decode the packet
            let decoded = self.decoder.decode(&packet)?;

            // Extract samples (inline to avoid borrow checker issues)
            self.sample_buffer.clear();
            match &decoded {
                AudioBufferRef::S16(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::F32(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::F64(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::U8(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::U16(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::U24(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::U32(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::S8(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::S24(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
                AudioBufferRef::S32(buf) => interleave_to_buffer(&mut self.sample_buffer, buf),
            }

            return Ok(());
        }
    }
}

fn interleave_to_buffer<T>(buffer: &mut Vec<i16>, buf: &AudioBuffer<T>)
where
    T: Sample,
    i16: FromSample<T>,
{
    let channels = buf.spec().channels.count();
    let frames = buf.frames();
    buffer.reserve(frames * channels);

    for frame_idx in 0..frames {
        for ch in 0..channels {
            let sample = buf.chan(ch)[frame_idx];
            buffer.push(i16::from_sample(sample));
        }
    }
}

impl FileSource {
    /// Convert frame count to sample count (frames × channels)
    ///
    /// Audio frames represent time units, while samples are individual channel values.
    /// For stereo audio, 1 frame = 2 samples (left + right).
    fn frames_to_samples(&self, frames: usize) -> usize {
        frames * self.channels as usize
    }

    /// Convert sample count to frame count (samples / channels)
    ///
    /// Divides total sample count by number of channels to get frame count.
    fn samples_to_frames(&self, samples: usize) -> usize {
        samples / self.channels as usize
    }
}

impl AudioSource for FileSource {
    fn read_frame(&mut self, num_samples: usize) -> Result<AudioFrame> {
        if self.finished {
            return Err(InjectionError::EndOfFile);
        }

        let mut frames_needed = num_samples;
        let mut output = Vec::with_capacity(self.frames_to_samples(num_samples));

        while frames_needed > 0 {
            // If buffer is empty, decode next packet
            if self.sample_buffer.is_empty() {
                if let Err(e) = self.decode_next_packet() {
                    if matches!(e, InjectionError::EndOfFile) && !output.is_empty() {
                        // Return partial frame if we have some samples
                        break;
                    }
                    return Err(e);
                }
            }

            // Copy samples from buffer
            let available_frames = self.samples_to_frames(self.sample_buffer.len());
            if available_frames == 0 {
                break;
            }

            let frames_to_copy = frames_needed.min(available_frames);
            let samples_to_copy = self.frames_to_samples(frames_to_copy);

            output.extend_from_slice(&self.sample_buffer[..samples_to_copy]);
            self.sample_buffer.drain(..samples_to_copy);

            self.position += frames_to_copy as u64;
            frames_needed -= frames_to_copy;
        }

        if output.is_empty() {
            Err(InjectionError::EndOfFile)
        } else {
            Ok(output)
        }
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u8 {
        self.channels
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn duration(&self) -> Option<f64> {
        self.total_samples
            .map(|samples| samples as f64 / self.sample_rate as f64)
    }

    fn position(&self) -> f64 {
        self.position as f64 / self.sample_rate as f64
    }

    fn reset(&mut self) -> Result<()> {
        // Seeking to beginning
        self.format
            .seek(
                symphonia::core::formats::SeekMode::Accurate,
                symphonia::core::formats::SeekTo::TimeStamp {
                    ts: 0,
                    track_id: self.track_id,
                },
            )
            .map_err(|e| InjectionError::Internal(format!("Seek failed: {}", e)))?;

        self.decoder.reset();
        self.sample_buffer.clear();
        self.position = 0;
        self.finished = false;

        Ok(())
    }

    fn seek(&mut self, seconds: f64) -> Result<()> {
        let whole_seconds = seconds.trunc();
        let frac = (seconds - whole_seconds).clamp(0.0, 0.999_999_999);
        let timestamp = self
            .time_base
            .calc_timestamp(Time::new(whole_seconds as u64, frac));

        self.format
            .seek(
                symphonia::core::formats::SeekMode::Accurate,
                symphonia::core::formats::SeekTo::TimeStamp {
                    ts: timestamp,
                    track_id: self.track_id,
                },
            )
            .map_err(|e| InjectionError::Internal(format!("Seek failed: {}", e)))?;

        self.sample_buffer.clear();
        self.position = timestamp;
        self.finished = false;

        Ok(())
    }

    fn description(&self) -> String {
        format!(
            "FileSource: {} ({}Hz, {} channels)",
            self.file_path, self.sample_rate, self.channels
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: These tests require actual audio files to be present
    // They are marked with #[ignore] by default

    #[test]
    #[ignore]
    fn test_file_source_wav() {
        let source = FileSource::new("test.wav").unwrap();
        assert!(source.sample_rate() > 0);
        assert!(source.channels() > 0);
    }

    #[test]
    #[ignore]
    fn test_file_source_read_frames() {
        let mut source = FileSource::new("test.wav").unwrap();
        let channels = source.channels() as usize;
        let frame = source.read_frame(960).unwrap();
        assert_eq!(frame.len(), 960 * channels);
    }
}
