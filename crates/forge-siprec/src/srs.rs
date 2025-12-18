//! SRS (Session Recording Server) Implementation
//!
//! The SRS receives SIPREC sessions from SRCs and stores the recorded media
//! along with metadata.

use dashmap::DashMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use crate::metadata::RecordingSession;
use crate::sip::DialogManager;

/// SRS configuration
#[derive(Debug, Clone)]
pub struct SrsConfig {
    /// Address to bind SIP server
    pub bind_address: SocketAddr,

    /// Address to bind RTP receiver
    pub rtp_bind_address: SocketAddr,

    /// Path to store recordings
    pub storage_path: PathBuf,

    /// Maximum concurrent recordings
    pub max_sessions: usize,

    /// Enable metadata storage
    pub store_metadata: bool,

    /// Enable automatic directory creation
    pub auto_create_dirs: bool,
}

impl Default for SrsConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:5060".parse().unwrap(),
            rtp_bind_address: "0.0.0.0:6000".parse().unwrap(),
            storage_path: "/tmp/forge-recordings".into(),
            max_sessions: 1000,
            store_metadata: true,
            auto_create_dirs: true,
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

    /// Session limit reached
    #[error("Session limit reached: {0}")]
    SessionLimitReached(usize),

    /// Session not found
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, SrsError>;

/// Active recording session in SRS
#[derive(Debug)]
pub struct ActiveRecording {
    /// Session identifier
    pub session_id: String,

    /// Recording metadata
    pub metadata: RecordingSession,

    /// Recording file path
    pub recording_path: PathBuf,

    /// File handle for writing
    file_handle: Option<File>,

    /// Packet count
    pub packet_count: u64,

    /// Byte count
    pub byte_count: u64,

    /// Stream SSRC mappings
    pub ssrc_map: HashMap<u32, String>,
}

impl ActiveRecording {
    /// Create a new active recording
    pub async fn new(
        session_id: String,
        metadata: RecordingSession,
        recording_path: PathBuf,
    ) -> Result<Self> {
        // Create parent directories
        if let Some(parent) = recording_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Open file for writing
        let file_handle = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&recording_path)
            .await?;

        Ok(Self {
            session_id,
            metadata,
            recording_path,
            file_handle: Some(file_handle),
            packet_count: 0,
            byte_count: 0,
            ssrc_map: HashMap::new(),
        })
    }

    /// Write RTP packet to recording file
    pub async fn write_packet(&mut self, packet_data: &[u8]) -> Result<()> {
        if let Some(ref mut file) = self.file_handle {
            // Write packet length (4 bytes) + packet data
            let len = packet_data.len() as u32;
            file.write_all(&len.to_be_bytes()).await?;
            file.write_all(packet_data).await?;

            self.packet_count += 1;
            self.byte_count += packet_data.len() as u64;

            Ok(())
        } else {
            Err(SrsError::Storage("File handle not available".to_string()))
        }
    }

    /// Flush and close the recording file
    pub async fn finalize(&mut self) -> Result<()> {
        if let Some(mut file) = self.file_handle.take() {
            file.flush().await?;
            file.sync_all().await?;
        }
        Ok(())
    }

    /// Get recording statistics
    pub fn stats(&self) -> (u64, u64) {
        (self.packet_count, self.byte_count)
    }
}

/// Session Recording Server
///
/// Receives and stores recording sessions from SRCs.
pub struct SessionRecordingServer {
    config: SrsConfig,
    dialog_manager: DialogManager,
    active_recordings: Arc<DashMap<String, Arc<RwLock<ActiveRecording>>>>,
    rtp_socket: Option<Arc<UdpSocket>>,
}

impl SessionRecordingServer {
    /// Create a new SRS
    pub async fn new(config: SrsConfig) -> Result<Self> {
        // Create storage directory if needed
        if config.auto_create_dirs {
            tokio::fs::create_dir_all(&config.storage_path).await?;
        }

        Ok(Self {
            config,
            dialog_manager: DialogManager::new(),
            active_recordings: Arc::new(DashMap::new()),
            rtp_socket: None,
        })
    }

    /// Start the SRS server
    pub async fn start(&mut self) -> Result<()> {
        // Bind RTP socket
        let rtp_socket = UdpSocket::bind(self.config.rtp_bind_address).await?;
        self.rtp_socket = Some(Arc::new(rtp_socket));

        tracing::info!(
            rtp_addr = %self.config.rtp_bind_address,
            storage_path = %self.config.storage_path.display(),
            "SRS server started"
        );

        // TODO: Start SIP listener (would need actual network implementation)

        Ok(())
    }

    /// Accept a new SIPREC session
    ///
    /// This would normally be called when receiving a SIP INVITE with SIPREC metadata
    pub async fn accept_session(
        &self,
        session_id: impl Into<String>,
        metadata: RecordingSession,
        sdp_body: Option<String>,
    ) -> Result<String> {
        let session_id = session_id.into();

        // Check session limit
        if self.active_recordings.len() >= self.config.max_sessions {
            return Err(SrsError::SessionLimitReached(self.config.max_sessions));
        }

        // Generate recording file path
        let recording_path = self.generate_recording_path(&session_id);

        // Create active recording
        let recording =
            ActiveRecording::new(session_id.clone(), metadata.clone(), recording_path.clone())
                .await?;

        let recording_arc = Arc::new(RwLock::new(recording));
        self.active_recordings
            .insert(session_id.clone(), recording_arc);

        // Store metadata if enabled
        if self.config.store_metadata {
            self.store_metadata(&session_id, &metadata).await?;
        }

        tracing::info!(
            session_id = %session_id,
            recording_path = %recording_path.display(),
            "Accepted SIPREC session"
        );

        // TODO: Parse SDP and set up RTP receivers

        let _ = sdp_body; // Suppress unused warning for now

        Ok(session_id)
    }

    /// Receive and store an RTP packet
    pub async fn receive_rtp_packet(&self, session_id: &str, packet_data: &[u8]) -> Result<()> {
        let recording = self
            .active_recordings
            .get(session_id)
            .ok_or_else(|| SrsError::SessionNotFound(session_id.to_string()))?;

        let mut recording = recording.write().await;
        recording.write_packet(packet_data).await?;

        Ok(())
    }

    /// Stop a recording session
    pub async fn stop_session(&self, session_id: &str) -> Result<()> {
        let recording = self
            .active_recordings
            .remove(session_id)
            .ok_or_else(|| SrsError::SessionNotFound(session_id.to_string()))?;

        let mut recording = recording.1.write().await;
        recording.finalize().await?;

        let (packet_count, byte_count) = recording.stats();

        tracing::info!(
            session_id = %session_id,
            recording_path = %recording.recording_path.display(),
            packets = packet_count,
            bytes = byte_count,
            "Stopped SIPREC session"
        );

        Ok(())
    }

    /// Stop the SRS server
    pub async fn stop(&self) -> Result<()> {
        // Finalize all active recordings
        let session_ids: Vec<String> = self
            .active_recordings
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for session_id in session_ids {
            if let Err(e) = self.stop_session(&session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to stop session during shutdown"
                );
            }
        }

        tracing::info!("SRS server stopped");

        Ok(())
    }

    /// Get active session count
    pub fn active_sessions(&self) -> usize {
        self.active_recordings.len()
    }

    /// Get session statistics
    pub async fn get_session_stats(&self, session_id: &str) -> Option<(u64, u64)> {
        let recording = self.active_recordings.get(session_id)?;
        let recording = recording.read().await;
        Some(recording.stats())
    }

    /// Get all session IDs
    pub fn get_all_session_ids(&self) -> Vec<String> {
        self.active_recordings
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get RTP socket for testing
    pub fn rtp_socket(&self) -> Option<Arc<UdpSocket>> {
        self.rtp_socket.clone()
    }

    /// Generate recording file path
    fn generate_recording_path(&self, session_id: &str) -> PathBuf {
        let filename = format!("{}.rtp", session_id);
        self.config.storage_path.join(filename)
    }

    /// Store metadata to disk
    async fn store_metadata(&self, session_id: &str, metadata: &RecordingSession) -> Result<()> {
        let metadata_path = self.config.storage_path.join(format!("{}.xml", session_id));

        let xml = metadata
            .to_xml()
            .map_err(|e| SrsError::Metadata(e.to_string()))?;

        tokio::fs::write(&metadata_path, xml).await?;

        Ok(())
    }

    /// Load metadata from disk
    pub async fn load_metadata(&self, session_id: &str) -> Result<RecordingSession> {
        let metadata_path = self.config.storage_path.join(format!("{}.xml", session_id));

        let xml = tokio::fs::read_to_string(&metadata_path).await?;

        RecordingSession::from_xml(&xml).map_err(|e| SrsError::Metadata(e.to_string()))
    }

    /// Get the dialog manager
    pub fn dialog_manager(&self) -> &DialogManager {
        &self.dialog_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::Participant;

    #[tokio::test]
    async fn test_srs_creation() {
        let config = SrsConfig {
            storage_path: "/tmp/test-srs-creation".into(),
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(config).await.unwrap();
        assert_eq!(srs.active_sessions(), 0);
    }

    #[tokio::test]
    async fn test_accept_session() {
        let config = SrsConfig {
            storage_path: "/tmp/test-accept-session".into(),
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(config).await.unwrap();

        let mut metadata = RecordingSession::new("test-session-1");
        metadata.add_participant(Participant::caller("sip:alice@example.com"));

        let session_id = srs
            .accept_session("test-session-1", metadata, None)
            .await
            .unwrap();

        assert_eq!(session_id, "test-session-1");
        assert_eq!(srs.active_sessions(), 1);
    }

    #[tokio::test]
    async fn test_stop_session() {
        let config = SrsConfig {
            storage_path: "/tmp/test-stop-session".into(),
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(config).await.unwrap();

        let mut metadata = RecordingSession::new("test-session-2");
        metadata.add_participant(Participant::caller("sip:alice@example.com"));

        srs.accept_session("test-session-2", metadata, None)
            .await
            .unwrap();

        assert_eq!(srs.active_sessions(), 1);

        srs.stop_session("test-session-2").await.unwrap();

        assert_eq!(srs.active_sessions(), 0);
    }

    #[tokio::test]
    async fn test_receive_rtp_packet() {
        let config = SrsConfig {
            storage_path: "/tmp/test-receive-rtp".into(),
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(config).await.unwrap();

        let mut metadata = RecordingSession::new("test-session-3");
        metadata.add_participant(Participant::caller("sip:alice@example.com"));

        srs.accept_session("test-session-3", metadata, None)
            .await
            .unwrap();

        // Simulate RTP packet (12-byte header + payload)
        let rtp_packet = vec![
            0x80, 0x00, 0x00, 0x01, // Version, PT, Seq
            0x00, 0x00, 0x00, 0x00, // Timestamp
            0x12, 0x34, 0x56, 0x78, // SSRC
            0x00, 0x01, 0x02, 0x03, // Payload
        ];

        srs.receive_rtp_packet("test-session-3", &rtp_packet)
            .await
            .unwrap();

        let stats = srs.get_session_stats("test-session-3").await.unwrap();
        assert_eq!(stats.0, 1); // 1 packet
        assert_eq!(stats.1, rtp_packet.len() as u64); // Correct byte count

        srs.stop_session("test-session-3").await.unwrap();
    }

    #[tokio::test]
    async fn test_metadata_storage() {
        use crate::metadata::MediaStream;

        let config = SrsConfig {
            storage_path: "/tmp/test-metadata-storage".into(),
            store_metadata: true,
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(config).await.unwrap();

        let mut metadata = RecordingSession::new("test-session-4");
        metadata.add_participant(Participant::caller("sip:alice@example.com"));
        metadata.add_participant(Participant::callee("sip:bob@example.com"));
        metadata.add_media_stream(MediaStream::audio("stream-1", "192.168.1.100", 5004));

        srs.accept_session("test-session-4", metadata.clone(), None)
            .await
            .unwrap();

        // Load metadata back
        let loaded_metadata = srs.load_metadata("test-session-4").await.unwrap();
        assert_eq!(loaded_metadata.session_id, metadata.session_id);
        assert_eq!(
            loaded_metadata.participants.len(),
            metadata.participants.len()
        );
        assert_eq!(loaded_metadata.streams.len(), metadata.streams.len());

        srs.stop_session("test-session-4").await.unwrap();
    }

    #[tokio::test]
    async fn test_session_limit() {
        let config = SrsConfig {
            storage_path: "/tmp/test-session-limit".into(),
            max_sessions: 2,
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(config).await.unwrap();

        let metadata1 = RecordingSession::new("limit-session-1");
        let metadata2 = RecordingSession::new("limit-session-2");
        let metadata3 = RecordingSession::new("limit-session-3");

        srs.accept_session("limit-session-1", metadata1, None)
            .await
            .unwrap();
        srs.accept_session("limit-session-2", metadata2, None)
            .await
            .unwrap();

        // Third session should fail
        let result = srs.accept_session("limit-session-3", metadata3, None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SrsError::SessionLimitReached(2)
        ));
    }
}
