//! Forge Core - Common types, traits, and utilities
//!
//! This crate provides the foundational types and interfaces used across
//! the Forge media engine.
//!
//! # Modules
//!
//! - [`types`] - Core type definitions (CallId, RoomId, etc.)
//! - [`error`] - Error types
//! - [`config`] - Configuration structures
//! - [`traits`] - Core traits for codecs and audio processing
//! - [`events`] - Event system for state change notifications

pub mod config;
pub mod error;
pub mod events;
pub mod traits;
pub mod types;

pub use config::*;
pub use error::*;
pub use events::*;
pub use traits::*;
pub use types::*;
