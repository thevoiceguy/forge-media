//! Error types for forge-injection

use thiserror::Error;

/// Result type alias for forge-injection operations
pub type Result<T> = std::result::Result<T, InjectionError>;

/// Error types for audio injection operations
#[derive(Debug, Error)]
pub enum InjectionError {
    /// Audio file could not be found or opened
    #[error("Audio file not found: {0}")]
    FileNotFound(String),

    /// Audio file format is not supported
    #[error("Unsupported audio format: {0}")]
    UnsupportedFormat(String),

    /// Audio decoding failed
    #[error("Audio decoding failed: {0}")]
    DecodingError(String),

    /// Audio encoding failed
    #[error("Audio encoding failed: {0}")]
    EncodingError(String),

    /// Sample rate conversion failed
    #[error("Resampling failed: {0}")]
    ResamplingError(String),

    /// Invalid audio parameters (sample rate, channels, etc.)
    #[error("Invalid audio parameters: {0}")]
    InvalidParameters(String),

    /// End of file reached
    #[error("End of file reached")]
    EndOfFile,

    /// TTS service error
    #[error("TTS service error: {0}")]
    TtsError(String),

    /// Invalid tone parameters
    #[error("Invalid tone parameters: {0}")]
    InvalidTone(String),

    /// I/O error
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Convert symphonia errors to InjectionError
impl From<symphonia::core::errors::Error> for InjectionError {
    fn from(err: symphonia::core::errors::Error) -> Self {
        use symphonia::core::errors::Error;
        match err {
            Error::IoError(e) => InjectionError::IoError(e),
            Error::Unsupported(_) => {
                InjectionError::UnsupportedFormat(err.to_string())
            }
            Error::DecodeError(_) => {
                InjectionError::DecodingError(err.to_string())
            }
            _ => InjectionError::Internal(err.to_string()),
        }
    }
}
