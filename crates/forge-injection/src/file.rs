//! File-based audio source using Symphonia

use crate::error::{InjectionError, Result};
use crate::source::AudioSource;
use forge_core::AudioFrame;
use std::path::{Component, Path, PathBuf};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};
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
    decoder: Box<dyn AudioDecoder>,

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

    /// Frames still to discard after an accurate seek: the reader lands on the
    /// packet holding the target, which may start before it
    skip_frames: u64,
}

/// Sandbox for audio-injection file paths.
///
/// Audit finding C16: `FileSource::new` previously accepted any
/// user-supplied path and called `std::fs::File::open`, giving an
/// attacker arbitrary-file-read within the process's UID. All
/// path-accepting entry points now go through an `InjectionSandbox`,
/// which pins a single `base_dir` and rejects paths that:
///   * are absolute and outside `base_dir`,
///   * contain `..` or `.` components (relative),
///   * or would follow a symlink out of the sandbox.
///
/// Construct once at startup (from operator config) and pass it to
/// [`FileSource::open_in_sandbox`] for every injection request.
#[derive(Debug, Clone)]
pub struct InjectionSandbox {
    /// Canonicalised absolute root directory for prompt / announcement
    /// audio files. Any resolved injection path must live under this
    /// directory.
    base_dir: PathBuf,
}

impl InjectionSandbox {
    /// Build a sandbox pinned to `base_dir`. The base is canonicalised at
    /// construction so later relative-path resolution is unambiguous.
    pub fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base = base_dir.as_ref();
        let canonical = base.canonicalize().map_err(|e| {
            InjectionError::PathRejected(format!(
                "sandbox base {} not accessible: {}",
                base.display(),
                e
            ))
        })?;
        Ok(Self {
            base_dir: canonical,
        })
    }

    /// The sandbox's canonicalised root.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Resolve a user-supplied injection path to a concrete filesystem
    /// path, rejecting anything that escapes the sandbox.
    ///
    /// Rules:
    ///   * Relative paths are joined to `base_dir`.
    ///   * Components must all be `Normal` (no `.` / `..` / RootDir /
    ///     Prefix) for the relative part.
    ///   * The resulting path is canonicalised and must still start with
    ///     `base_dir` — which handles symlinks pointing outside the
    ///     sandbox.
    pub fn resolve(&self, requested: impl AsRef<Path>) -> Result<PathBuf> {
        let requested = requested.as_ref();

        // Absolute paths: must already be inside the sandbox (and have no
        // traversal components).
        let joined = if requested.is_absolute() {
            if has_relative_components(requested) {
                return Err(InjectionError::PathRejected(format!(
                    "path contains ./ or ../ components: {}",
                    requested.display()
                )));
            }
            requested.to_path_buf()
        } else {
            // Relative paths: every component must be Normal.
            for comp in requested.components() {
                if !matches!(comp, Component::Normal(_)) {
                    return Err(InjectionError::PathRejected(format!(
                        "path contains non-normal components: {}",
                        requested.display()
                    )));
                }
            }
            self.base_dir.join(requested)
        };

        // Resolve symlinks and normalise, then confirm still inside root.
        let canonical = joined.canonicalize().map_err(|e| {
            InjectionError::PathRejected(format!("cannot resolve path {}: {}", joined.display(), e))
        })?;
        if !canonical.starts_with(&self.base_dir) {
            return Err(InjectionError::PathRejected(format!(
                "path resolves outside sandbox: {} → {}",
                joined.display(),
                canonical.display()
            )));
        }
        Ok(canonical)
    }
}

/// Does this path contain any `.` or `..` components?
fn has_relative_components(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

impl FileSource {
    /// Open an audio file in a sandboxed directory.
    ///
    /// **Preferred constructor for audit finding C16** — resolves
    /// `path` against the given [`InjectionSandbox`] so that user-supplied
    /// filenames cannot escape the configured prompts directory.
    pub fn open_in_sandbox(sandbox: &InjectionSandbox, path: impl AsRef<Path>) -> Result<Self> {
        let resolved = sandbox.resolve(path)?;
        Self::open_trusted(resolved)
    }

    /// Open a file by a fully-trusted, already-validated path.
    ///
    /// **Do not call this with attacker-influenced input.** This is the
    /// raw file-open path — the sandboxing / canonicalisation happens in
    /// [`Self::open_in_sandbox`]. Prefer that constructor.
    pub fn open_trusted(path: impl AsRef<Path>) -> Result<Self> {
        Self::new_inner(path.as_ref())
    }

    /// Create a new file source from a path.
    ///
    /// **Deprecated:** prefer [`Self::open_in_sandbox`]. This method performs
    /// **no path validation** and should only be used for trusted input
    /// (test fixtures, operator-controlled absolute paths). Exists purely
    /// for backward compatibility.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::new_inner(path.as_ref())
    }

    fn new_inner(path: &Path) -> Result<Self> {
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
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| {
                InjectionError::UnsupportedFormat(format!("Failed to probe file format: {}", e))
            })?;

        // Find the first audio track
        let (track, codec_params) = format
            .tracks()
            .iter()
            .find_map(|t| {
                t.codec_params
                    .as_ref()
                    .and_then(|p| p.audio())
                    .map(|p| (t, p))
            })
            .ok_or_else(|| InjectionError::UnsupportedFormat("No audio track found".to_string()))?;

        let track_id = track.id;

        debug!("Audio track found: codec={:?}", codec_params.codec);

        // Get track info
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| InjectionError::InvalidParameters("Unknown sample rate".to_string()))?;

        let channels = codec_params
            .channels
            .as_ref()
            .ok_or_else(|| InjectionError::InvalidParameters("Unknown channel count".to_string()))?
            .count() as u8;

        let time_base = match track.time_base {
            Some(tb) => tb,
            None => TimeBase::try_new(1, sample_rate).ok_or_else(|| {
                InjectionError::InvalidParameters("Sample rate of 0Hz".to_string())
            })?,
        };

        let total_samples = track.num_frames;

        debug!(
            "Audio format: {}Hz, {} channels, duration: {:?} samples",
            sample_rate, channels, total_samples
        );

        // Create a decoder
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(codec_params, &AudioDecoderOptions::default())
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
            skip_frames: 0,
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
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.finished = true;
                    return Err(InjectionError::EndOfFile);
                }
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
            if packet.track_id != self.track_id {
                continue;
            }

            // Decode the packet
            let decoded = self.decoder.decode(&packet)?;

            // Interleave and convert to i16, whatever the decoder's sample format
            self.sample_buffer.clear();
            decoded.copy_to_vec_interleaved(&mut self.sample_buffer);

            if self.skip_frames > 0 {
                let frames = self.samples_to_frames(self.sample_buffer.len());
                let skip = frames.min(self.skip_frames as usize);
                let samples = self.frames_to_samples(skip);
                self.sample_buffer.drain(..samples);
                self.skip_frames -= skip as u64;
                if self.sample_buffer.is_empty() {
                    continue;
                }
            }

            return Ok(());
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

    /// Convert a duration in track time-base units to audio frames
    fn ts_to_frames(&self, ts: i64) -> u64 {
        let ts = ts.max(0) as u128;
        let numer = self.time_base.numer.get() as u128;
        let denom = self.time_base.denom.get() as u128;
        (ts * numer * self.sample_rate as u128 / denom) as u64
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
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::ZERO,
                    track_id: self.track_id,
                },
            )
            .map_err(|e| InjectionError::Internal(format!("Seek failed: {}", e)))?;

        self.decoder.reset();
        self.sample_buffer.clear();
        self.position = 0;
        self.finished = false;
        self.skip_frames = 0;

        Ok(())
    }

    fn seek(&mut self, seconds: f64) -> Result<()> {
        let timestamp = Time::try_from_secs_f64(seconds.max(0.0))
            .and_then(|time| self.time_base.calc_timestamp(time))
            .ok_or_else(|| InjectionError::InvalidParameters(format!("Bad seek: {}s", seconds)))?;

        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: timestamp,
                    track_id: self.track_id,
                },
            )
            .map_err(|e| InjectionError::Internal(format!("Seek failed: {}", e)))?;

        self.decoder.reset();
        self.sample_buffer.clear();
        self.skip_frames = self.ts_to_frames(seeked.required_ts.get() - seeked.actual_ts.get());
        self.position = self.ts_to_frames(seeked.required_ts.get());
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

    // --- C16 sandbox regression tests ---

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn test_sandbox_accepts_relative_within_base() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("prompt.wav"));
        let sb = InjectionSandbox::new(tmp.path()).unwrap();
        let resolved = sb.resolve("prompt.wav").unwrap();
        assert!(resolved.starts_with(sb.base_dir()));
        assert!(resolved.ends_with("prompt.wav"));
    }

    #[test]
    fn test_sandbox_rejects_parent_dir_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let sb = InjectionSandbox::new(tmp.path()).unwrap();
        let err = sb.resolve("../etc/passwd").expect_err("must reject ..");
        assert!(matches!(err, InjectionError::PathRejected(_)));
    }

    #[test]
    fn test_sandbox_rejects_absolute_outside_base() {
        let tmp = tempfile::tempdir().unwrap();
        let sb = InjectionSandbox::new(tmp.path()).unwrap();
        // Absolute to a path outside the sandbox.
        let err = sb
            .resolve("/etc/passwd")
            .expect_err("absolute path outside base must be rejected");
        assert!(matches!(err, InjectionError::PathRejected(_)));
    }

    #[test]
    fn test_sandbox_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();

        // Create a symlink inside the sandbox that points outside.
        let link = tmp.path().join("escape.wav");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        #[cfg(not(unix))]
        {
            // Symlinks are awkward on Windows without privileges — skip.
            return;
        }

        let sb = InjectionSandbox::new(tmp.path()).unwrap();
        let err = sb
            .resolve("escape.wav")
            .expect_err("symlink escape must be rejected");
        assert!(matches!(err, InjectionError::PathRejected(_)));
    }

    #[test]
    fn test_sandbox_rejects_dot_component() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("sub").join("file.wav"));
        let sb = InjectionSandbox::new(tmp.path()).unwrap();
        // "./sub/file.wav" has a CurDir component — rejected.
        let err = sb
            .resolve(Path::new(".").join("sub").join("file.wav"))
            .expect_err("./ prefix must be rejected");
        assert!(matches!(err, InjectionError::PathRejected(_)));
    }

    #[test]
    fn test_sandbox_rejects_nonexistent_path() {
        let tmp = tempfile::tempdir().unwrap();
        let sb = InjectionSandbox::new(tmp.path()).unwrap();
        // Canonicalize fails for nonexistent → PathRejected wraps it.
        let err = sb
            .resolve("does-not-exist.wav")
            .expect_err("nonexistent path must be rejected");
        assert!(matches!(err, InjectionError::PathRejected(_)));
    }

    /// A mono 16-bit PCM WAV whose sample `i` is `i as i16`.
    fn write_ramp_wav(path: &Path, rate: u32, frames: u32) {
        let data_len = frames * 2;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            wav.extend_from_slice(&(i as i16).to_le_bytes());
        }
        std::fs::write(path, wav).unwrap();
    }

    #[test]
    fn test_file_source_decodes_seeks_and_resets() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ramp.wav");
        write_ramp_wav(&path, 8000, 8000);

        let mut source = FileSource::new(&path).unwrap();
        assert_eq!(source.sample_rate(), 8000);
        assert_eq!(source.channels(), 1);
        assert_eq!(source.duration(), Some(1.0));

        let frame = source.read_frame(160).unwrap();
        assert_eq!(frame, (0..160).collect::<Vec<i16>>());

        source.seek(0.5).unwrap();
        assert_eq!(source.position(), 0.5);
        assert_eq!(source.read_frame(4).unwrap(), vec![4000, 4001, 4002, 4003]);

        source.reset().unwrap();
        assert_eq!(source.read_frame(2).unwrap(), vec![0, 1]);

        let mut total = 2;
        while let Ok(frame) = source.read_frame(960) {
            total += frame.len();
        }
        assert_eq!(total, 8000);
        assert!(source.is_finished());
    }
}
