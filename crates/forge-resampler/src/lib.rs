//! Audio resampling library for Forge Media
//!
//! Provides sample rate conversion for audio streams, necessary when transcoding
//! between codecs with different sample rates.

use thiserror::Error;

mod resampler;

pub use resampler::{calculate_output_length, Resampler};

/// Resampler error types
#[derive(Error, Debug)]
pub enum ResamplerError {
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
}

/// Result type for resampler operations
pub type Result<T> = std::result::Result<T, ResamplerError>;
