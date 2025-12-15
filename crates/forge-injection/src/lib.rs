//! forge-injection - Audio Injection System
//!
//! This crate provides audio injection capabilities for the Forge media engine,
//! including file playback, tone generation, and text-to-speech (TTS) integration.
//!
//! # Features
//!
//! - **File Playback**: Play audio files in various formats (WAV, MP3, FLAC, OGG, etc.) using Symphonia
//! - **Tone Generation**: Generate DTMF tones, comfort noise, and custom waveforms
//! - **Text-to-Speech**: Integrate with Google TTS, AWS Polly, and Azure TTS
//! - **Mix Modes**: Support Mix, Replace, and Duck modes for audio injection
//! - **Fade Effects**: Smooth fade-in and fade-out transitions
//!
//! # Example
//!
//! ```rust,ignore
//! use forge_injection::{FileSource, MixMode};
//!
//! // Play an audio file
//! let source = FileSource::new("announcement.wav").await?;
//! let frames = source.read_frames(960)?;
//!
//! // Generate a DTMF tone
//! let tone = ToneGenerator::dtmf('5', 8000);
//! let samples = tone.generate(160)?;
//! ```

pub mod error;
pub mod file;
pub mod mix;
pub mod source;
pub mod tone;

#[cfg(any(feature = "tts-google", feature = "tts-aws", feature = "tts-azure"))]
pub mod tts;

pub use error::*;
pub use file::*;
pub use mix::*;
pub use source::*;
pub use tone::*;

#[cfg(any(feature = "tts-google", feature = "tts-aws", feature = "tts-azure"))]
pub use tts::*;
