//! Audio transcoding library for Forge Media
//!
//! Provides end-to-end audio transcoding between different codecs and sample rates

use thiserror::Error;

mod transcoder;
pub mod rtp;

pub use forge_codecs::AudioFormat;
pub use transcoder::Transcoder;
pub use rtp::{RtpTranscoder, PayloadTypeMap};

/// Transcoder error types
#[derive(Error, Debug)]
pub enum TranscoderError {
    #[error("Audio encoding error: {0}")]
    Encoding(String),

    #[error("Audio decoding error: {0}")]
    Decoding(String),

    #[error("Invalid audio format: {0}")]
    InvalidFormat(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Codec error: {0}")]
    Codec(#[from] forge_codecs::CodecError),

    #[error("Resampler error: {0}")]
    Resampler(#[from] forge_resampler::ResamplerError),
}

/// Result type for transcoder operations
pub type Result<T> = std::result::Result<T, TranscoderError>;
