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

pub mod types;
pub mod error;
pub mod config;
pub mod traits;
pub mod events;

pub use types::*;
pub use error::*;
pub use config::*;
pub use traits::*;
pub use events::*;
