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

// Pre-existing style lints that would require public-API changes to fix.
// Tracked as technical debt; not blocking correctness.
#![allow(clippy::derivable_impls)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::wrong_self_convention)]
#![allow(clippy::ptr_arg)]

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
