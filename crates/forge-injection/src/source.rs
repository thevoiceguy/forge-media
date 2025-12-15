//! Audio source trait for injection

use crate::error::Result;
use forge_core::AudioFrame;

/// Audio source trait for providing audio samples for injection
///
/// This trait is implemented by all audio sources including file players,
/// tone generators, and TTS engines.
///
/// # Example
///
/// ```rust,ignore
/// use forge_injection::{AudioSource, FileSource};
///
/// let mut source = FileSource::new("announcement.wav").await?;
/// while let Ok(frame) = source.read_frame(960) {
///     // Inject frame into call
/// }
/// ```
pub trait AudioSource: Send + Sync {
    /// Read a frame of audio samples
    ///
    /// # Arguments
    ///
    /// * `num_samples` - Number of samples to read
    ///
    /// # Returns
    ///
    /// Audio frame with up to `num_samples` samples. May return fewer samples
    /// if the end of the source is reached.
    ///
    /// # Errors
    ///
    /// Returns `Err` if reading fails or if the end of file is reached.
    fn read_frame(&mut self, num_samples: usize) -> Result<AudioFrame>;

    /// Get the sample rate of this audio source in Hz
    fn sample_rate(&self) -> u32;

    /// Get the number of audio channels (1 = mono, 2 = stereo)
    fn channels(&self) -> u8;

    /// Check if the source has finished
    fn is_finished(&self) -> bool;

    /// Reset the source to the beginning (if supported)
    ///
    /// Not all sources support seeking (e.g., TTS sources).
    fn reset(&mut self) -> Result<()> {
        Err(crate::error::InjectionError::Internal(
            "Seeking not supported for this source".to_string(),
        ))
    }

    /// Get the total duration in seconds (if known)
    ///
    /// Returns `None` for sources with unknown duration (e.g., streaming sources).
    fn duration(&self) -> Option<f64> {
        None
    }

    /// Get the current playback position in seconds
    fn position(&self) -> f64 {
        0.0
    }

    /// Seek to a specific position in seconds (if supported)
    fn seek(&mut self, _seconds: f64) -> Result<()> {
        Err(crate::error::InjectionError::Internal(
            "Seeking not supported for this source".to_string(),
        ))
    }

    /// Get a human-readable description of this source
    fn description(&self) -> String {
        "Audio source".to_string()
    }
}
