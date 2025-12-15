//! SRS (Session Recording Server) Implementation
//!
//! The SRS receives SIPREC sessions from SRCs and stores the recorded media
//! along with metadata.

use std::net::SocketAddr;
use std::path::PathBuf;
use thiserror::Error;

/// SRS configuration
#[derive(Debug, Clone)]
pub struct SrsConfig {
    /// Address to bind SIP server
    pub bind_address: SocketAddr,

    /// Path to store recordings
    pub storage_path: PathBuf,

    /// Maximum concurrent recordings
    pub max_sessions: usize,

    /// Enable metadata storage
    pub store_metadata: bool,
}

impl Default for SrsConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:5060".parse().unwrap(),
            storage_path: "/var/lib/forge/recordings".into(),
            max_sessions: 1000,
            store_metadata: true,
        }
    }
}

/// SRS error types
#[derive(Debug, Error)]
pub enum SrsError {
    /// SIP signaling error
    #[error("SIP error: {0}")]
    Sip(String),

    /// Storage error
    #[error("Storage error: {0}")]
    Storage(String),

    /// Metadata error
    #[error("Metadata error: {0}")]
    Metadata(String),

    /// Network error
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, SrsError>;

/// Session Recording Server
///
/// Receives and stores recording sessions from SRCs.
pub struct SessionRecordingServer {
    #[allow(dead_code)]
    config: SrsConfig,
}

impl SessionRecordingServer {
    /// Create a new SRS
    pub async fn new(config: SrsConfig) -> Result<Self> {
        // TODO: Initialize SIP server
        // TODO: Set up RTP receivers
        // TODO: Initialize storage

        Ok(Self { config })
    }

    /// Start the SRS server
    pub async fn start(&self) -> Result<()> {
        // TODO: Start SIP listener
        // TODO: Accept incoming SIPREC sessions
        // TODO: Handle media reception and storage

        Ok(())
    }

    /// Stop the SRS server
    pub async fn stop(&self) -> Result<()> {
        // TODO: Stop accepting new sessions
        // TODO: Finalize ongoing recordings
        // TODO: Shutdown gracefully

        Ok(())
    }

    /// Get active session count
    pub fn active_sessions(&self) -> usize {
        // TODO: Return actual count
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_srs_creation() {
        let config = SrsConfig::default();
        let srs = SessionRecordingServer::new(config).await.unwrap();
        assert_eq!(srs.active_sessions(), 0);
    }
}
