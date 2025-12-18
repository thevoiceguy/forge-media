//! Error types for forge-kernel

use thiserror::Error;

/// Errors that can occur in the kernel module
#[derive(Debug, Error)]
pub enum Error {
    #[error("Aya error: {0}")]
    Aya(#[from] aya::EbpfError),

    #[error("Aya programs error: {0}")]
    AyaPrograms(#[from] aya::programs::ProgramError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("XDP not available: {0}")]
    XdpNotAvailable(String),

    #[error("XDP program load failed: {0}")]
    XdpLoadFailed(String),

    #[error("XDP attach failed: {0}")]
    XdpAttachFailed(String),

    #[error("XDP detach failed: {0}")]
    XdpDetachFailed(String),

    #[error("Map operation failed: {0}")]
    MapOperationFailed(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for kernel operations
pub type Result<T> = std::result::Result<T, Error>;
