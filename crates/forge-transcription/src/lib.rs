//! forge-transcription - Transcription interfaces and data types.
//!
//! This crate defines the public API for transcription within Forge.

use thiserror::Error;

/// A transcript produced by a transcription engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    text: String,
}

impl Transcript {
    /// Creates a new transcript from UTF-8 text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Returns the transcript text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Errors returned by transcription engines.
#[derive(Debug, Error)]
pub enum TranscriptionError {
    /// The engine has not been implemented yet.
    #[error("transcription engine not implemented")]
    NotImplemented,
}

/// A transcription engine interface for converting audio to text.
pub trait TranscriptionEngine {
    /// Transcribes audio bytes into a transcript.
    fn transcribe(&self, audio: &[u8]) -> Result<Transcript, TranscriptionError>;
}

/// A placeholder engine that always returns `NotImplemented`.
#[derive(Debug, Default)]
pub struct PlaceholderEngine;

impl TranscriptionEngine for PlaceholderEngine {
    fn transcribe(&self, _audio: &[u8]) -> Result<Transcript, TranscriptionError> {
        Err(TranscriptionError::NotImplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_new_and_text_roundtrip() {
        let transcript = Transcript::new("hello");
        assert_eq!(transcript.text(), "hello");
    }

    #[test]
    fn placeholder_engine_returns_not_implemented() {
        let engine = PlaceholderEngine::default();
        let err = engine.transcribe(b"audio").unwrap_err();
        assert!(matches!(err, TranscriptionError::NotImplemented));
    }
}
