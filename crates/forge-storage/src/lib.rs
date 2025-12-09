//! Recording storage management library for Forge Media
//!
//! Manages recording metadata, retention policies, and automatic cleanup

use thiserror::Error;

mod storage;

pub use storage::{RecordingInfo, StorageManager};

/// Storage error types
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for storage operations
pub type Result<T> = std::result::Result<T, StorageError>;
