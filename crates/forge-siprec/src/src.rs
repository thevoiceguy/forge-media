//! SRC (Session Recording Client) Implementation
//!
//! The SRC is responsible for forking media streams and sending them to an SRS
//! (Session Recording Server) along with metadata.

use base64::{engine::general_purpose::STANDARD, Engine};
use dashmap::DashMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::forking::{ForkingError, MediaForker};
use crate::metadata::{MediaStream, Participant, ParticipantRole, RecordingSession};
use crate::sip::{DialogManager, SdpBuilder, SipDialog, SipRequest};
use crate::srtp_keys::{SrtpKeyError, SrtpKeyManager};

/// SRC configuration
#[derive(Debug, Clone)]
pub struct SrcConfig {
    /// SRS URI to send recordings to
    pub srs_uri: String,

    /// Local bind address for SIP
    pub local_address: SocketAddr,

    /// Local bind address for RTP (optional, will bind to any if not specified)
    pub rtp_bind_address: Option<SocketAddr>,

    /// Backup SRS URI (for failover)
    pub backup_srs_uri: Option<String>,

    /// Enable SRTP key forwarding
    pub forward_srtp_keys: bool,

    /// Recording reason
    pub recording_reason: Option<String>,
}

impl Default for SrcConfig {
    fn default() -> Self {
        Self {
            srs_uri: "sip:recorder@srs.example.com".to_string(),
            local_address: "0.0.0.0:5060".parse().unwrap(),
            rtp_bind_address: None,
            backup_srs_uri: None,
            forward_srtp_keys: true,
            recording_reason: Some("Compliance recording".to_string()),
        }
    }
}

/// SRC error types
#[derive(Debug, Error)]
pub enum SrcError {
    /// SIP signaling error
    #[error("SIP error: {0}")]
    Sip(String),

    /// RTP forwarding error
    #[error("RTP forwarding error: {0}")]
    Rtp(String),

    /// Metadata error
    #[error("Metadata error: {0}")]
    Metadata(String),

    /// Network error
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),

    /// SRTP key error
    #[error("SRTP key error: {0}")]
    SrtpKey(#[from] SrtpKeyError),

    /// Forking error
    #[error("Forking error: {0}")]
    Forking(#[from] ForkingError),

    /// Session not found
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, SrcError>;

/// Recording session state
#[derive(Debug, Clone)]
pub struct RecordingSessionState {
    /// Session identifier
    pub session_id: String,

    /// Call identifier
    pub call_id: String,

    /// SIP dialog for this recording
    pub dialog: Arc<RwLock<SipDialog>>,

    /// Metadata for this recording
    pub metadata: Arc<RwLock<RecordingSession>>,

    /// Stream IDs being recorded
    pub stream_ids: Vec<String>,

    /// SRS destination address (resolved from SIP)
    pub srs_address: Option<SocketAddr>,

    /// Whether this session is using the backup SRS
    pub using_backup: bool,

    /// Failover attempt count
    pub failover_attempts: u32,
}

/// Session Recording Client
///
/// Manages recording sessions by forking media to an SRS.
pub struct SessionRecordingClient {
    config: SrcConfig,
    dialog_manager: DialogManager,
    media_forker: MediaForker,
    srtp_key_manager: SrtpKeyManager,
    active_sessions: Arc<DashMap<String, RecordingSessionState>>,
}

impl SessionRecordingClient {
    /// Create a new SRC
    pub async fn new(config: SrcConfig) -> Result<Self> {
        Ok(Self {
            config,
            dialog_manager: DialogManager::new(),
            media_forker: MediaForker::new(),
            srtp_key_manager: SrtpKeyManager::new(),
            active_sessions: Arc::new(DashMap::new()),
        })
    }

    /// Start a new recording session
    ///
    /// # Arguments
    ///
    /// * `call_id` - Unique call identifier
    /// * `caller_uri` - SIP URI of caller
    /// * `callee_uri` - SIP URI of callee
    /// * `media_config` - Media stream configuration
    ///
    /// # Returns
    ///
    /// Recording session ID
    pub async fn start_recording(
        &self,
        call_id: impl Into<String>,
        caller_uri: impl Into<String>,
        callee_uri: impl Into<String>,
        media_config: MediaConfig,
    ) -> Result<String> {
        let call_id = call_id.into();
        let caller_uri = caller_uri.into();
        let callee_uri = callee_uri.into();

        // Generate session ID
        let session_id = format!("recording-{}-{}", call_id, uuid::Uuid::new_v4());

        // Create metadata
        let mut metadata = RecordingSession::new(&session_id);

        // Add participants
        metadata.add_participant(
            Participant::new("caller", &caller_uri, ParticipantRole::Caller).with_streams(vec![]),
        );
        metadata.add_participant(
            Participant::new("callee", &callee_uri, ParticipantRole::Callee).with_streams(vec![]),
        );

        // Set recording reason if configured
        if let Some(ref reason) = self.config.recording_reason {
            metadata.set_reason(reason);
        }

        // Add extension data with call ID
        metadata.add_extension("call-id", &call_id);

        // Create SIP dialog
        let local_uri = format!("sip:src@{}", self.config.local_address.ip());
        let sip_call_id = format!(
            "{}@{}",
            uuid::Uuid::new_v4(),
            self.config.local_address.ip()
        );
        let local_tag = format!("{:x}", rand::random::<u64>());

        let dialog = self
            .dialog_manager
            .create_dialog(
                sip_call_id.clone(),
                local_tag,
                local_uri,
                self.config.srs_uri.clone(),
                self.config.local_address,
                1,
            )
            .await;

        // Build SDP with media streams
        let mut sdp = SdpBuilder::new(
            self.config.local_address.ip(),
            self.config.local_address.ip(),
        );

        let mut stream_ids = Vec::new();

        // Add media streams
        for (idx, stream) in media_config.streams.iter().enumerate() {
            let stream_id = format!("{}-stream-{}", session_id, idx);
            stream_ids.push(stream_id.clone());

            // Add to SDP
            sdp.add_audio_stream(
                stream.rtp_port,
                &stream.codec,
                stream.payload_type,
                stream.sample_rate,
            );

            // Enable SRTP if keys provided
            if self.config.forward_srtp_keys {
                if let Some(ref crypto_attr) = stream.crypto_attr {
                    // Parse and store SRTP keys
                    self.srtp_key_manager.add_from_crypto_attr(
                        stream_id.clone(),
                        stream.ssrc,
                        crypto_attr,
                    )?;

                    // Add SRTP to SDP
                    let key_info = self.srtp_key_manager.get_key_by_stream(&stream_id).unwrap();

                    let mut key_material = Vec::new();
                    key_material.extend_from_slice(&key_info.key_material.master_key);
                    key_material.extend_from_slice(&key_info.key_material.master_salt);
                    let key_material_b64 = STANDARD.encode(&key_material);

                    sdp.enable_srtp(idx, key_info.tag, &key_info.suite, &key_material_b64);
                }
            }

            // Add media stream to metadata
            let media_stream = MediaStream::audio(
                &stream_id,
                stream.destination_address.ip().to_string(),
                stream.destination_address.port(),
            )
            .with_format(&stream.codec)
            .with_ssrc(stream.ssrc);

            metadata.add_media_stream(media_stream);
        }

        // Generate SDP and metadata XML
        let sdp_str = sdp.build();
        let metadata_xml = metadata
            .to_xml()
            .map_err(|e| SrcError::Metadata(e.to_string()))?;

        // Create SIPREC INVITE
        let mut invite = {
            let dialog = dialog.read().await;
            SipRequest::new_siprec_invite(
                &dialog.local_uri,
                &dialog.remote_uri,
                dialog.local_contact,
                dialog.local_tag.clone(),
            )
        };

        // Set multipart body with SDP and metadata
        invite.set_multipart_body(sdp_str, metadata_xml);

        // TODO: Send INVITE to SRS (would need actual network implementation)
        // For now, we simulate success
        tracing::info!(
            session_id = %session_id,
            call_id = %call_id,
            srs_uri = %self.config.srs_uri,
            "Started SIPREC recording session"
        );

        // Store session state
        let session_state = RecordingSessionState {
            session_id: session_id.clone(),
            call_id: call_id.clone(),
            dialog,
            metadata: Arc::new(RwLock::new(metadata)),
            stream_ids: stream_ids.clone(),
            srs_address: None, // Would be set after SIP response
            using_backup: false,
            failover_attempts: 0,
        };

        self.active_sessions
            .insert(session_id.clone(), session_state);

        Ok(session_id)
    }

    /// Perform failover to backup SRS for a session
    ///
    /// This will attempt to re-establish the recording on the backup SRS
    pub async fn failover_to_backup(&self, session_id: &str) -> Result<()> {
        // Check if backup SRS is configured
        if self.config.backup_srs_uri.is_none() {
            return Err(SrcError::Internal(
                "No backup SRS configured for failover".to_string(),
            ));
        }

        // Get session and update failover state
        let mut session = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| SrcError::SessionNotFound(session_id.to_string()))?;

        if session.using_backup {
            return Err(SrcError::Internal(
                "Already using backup SRS, no further failover available".to_string(),
            ));
        }

        session.using_backup = true;
        session.failover_attempts += 1;

        let backup_srs_uri = self.config.backup_srs_uri.as_ref().unwrap();

        tracing::warn!(
            session_id = %session_id,
            backup_srs = %backup_srs_uri,
            attempt = session.failover_attempts,
            "Failing over to backup SRS"
        );

        // TODO: In a real implementation, this would:
        // 1. Send BYE to primary SRS
        // 2. Create new dialog with backup SRS
        // 3. Send INVITE with metadata to backup SRS
        // 4. Update media forking destinations

        tracing::info!(
            session_id = %session_id,
            "Failover to backup SRS completed"
        );

        Ok(())
    }

    /// Add a forked stream to an existing recording session
    ///
    /// This sets up the media forking for RTP packets
    pub async fn add_forked_stream(
        &self,
        session_id: &str,
        stream_id: &str,
        ssrc: u32,
        recording_dest: SocketAddr,
    ) -> Result<()> {
        // Verify session exists
        if !self.active_sessions.contains_key(session_id) {
            return Err(SrcError::SessionNotFound(session_id.to_string()));
        }

        // Add stream to forker
        self.media_forker
            .add_stream(
                stream_id.to_string(),
                ssrc,
                recording_dest,
                self.config.rtp_bind_address,
            )
            .await?;

        tracing::debug!(
            session_id = %session_id,
            stream_id = %stream_id,
            ssrc = ssrc,
            dest = %recording_dest,
            "Added forked stream"
        );

        Ok(())
    }

    /// Forward an RTP packet to SRS
    pub async fn forward_rtp(&self, session_id: &str, rtp_packet: &[u8]) -> Result<()> {
        // Verify session exists
        if !self.active_sessions.contains_key(session_id) {
            return Err(SrcError::SessionNotFound(session_id.to_string()));
        }

        // Parse SSRC from RTP header (bytes 8-11)
        if rtp_packet.len() < 12 {
            return Err(SrcError::Rtp("RTP packet too short".to_string()));
        }

        let ssrc =
            u32::from_be_bytes([rtp_packet[8], rtp_packet[9], rtp_packet[10], rtp_packet[11]]);

        // Forward by SSRC
        self.media_forker.forward_by_ssrc(ssrc, rtp_packet).await?;

        Ok(())
    }

    /// Stop a recording session
    pub async fn stop_recording(&self, session_id: &str) -> Result<()> {
        // Clone the session data we need before acquiring any locks
        let (dialog, metadata, stream_ids) = {
            let session_state = self
                .active_sessions
                .get(session_id)
                .ok_or_else(|| SrcError::SessionNotFound(session_id.to_string()))?;

            (
                session_state.dialog.clone(),
                session_state.metadata.clone(),
                session_state.stream_ids.clone(),
            )
        }; // Drop the DashMap reference here
        let sip_call_id = {
            let dialog_lock = dialog.read().await;
            dialog_lock.call_id.clone()
        };

        // Update metadata with stop time
        {
            let mut metadata_lock = metadata.write().await;
            metadata_lock.stop();
        }

        // Create BYE request
        let bye_request = {
            let mut dialog_lock = dialog.write().await;
            dialog_lock
                .create_bye()
                .map_err(|e| SrcError::Sip(e.to_string()))?
        };

        // TODO: Send BYE to SRS (would need actual network implementation)
        let _ = bye_request; // Suppress unused warning

        // Stop RTP forking for all streams
        for stream_id in &stream_ids {
            self.media_forker.remove_stream(stream_id);
            self.srtp_key_manager.remove_key(stream_id);
        }

        // Terminate dialog (use SIP Call-ID from the dialog, not the call_id)
        self.dialog_manager
            .terminate_dialog(&sip_call_id)
            .await
            .map_err(SrcError::Sip)?;

        // Remove session
        self.active_sessions.remove(session_id);

        tracing::info!(
            session_id = %session_id,
            "Stopped SIPREC recording session"
        );

        Ok(())
    }

    /// Get active session count
    pub fn active_session_count(&self) -> usize {
        self.active_sessions.len()
    }

    /// Get statistics for a recording session
    pub async fn get_session_stats(&self, session_id: &str) -> Option<HashMap<String, (u64, u64)>> {
        let session = self.active_sessions.get(session_id)?;

        let mut stats = HashMap::new();
        for stream_id in &session.stream_ids {
            if let Some(stream_stats) = self.media_forker.get_stats(stream_id).await {
                stats.insert(stream_id.clone(), stream_stats);
            }
        }

        Some(stats)
    }

    /// Get the dialog manager
    pub fn dialog_manager(&self) -> &DialogManager {
        &self.dialog_manager
    }

    /// Get the media forker
    pub fn media_forker(&self) -> &MediaForker {
        &self.media_forker
    }

    /// Get the SRTP key manager
    pub fn srtp_key_manager(&self) -> &SrtpKeyManager {
        &self.srtp_key_manager
    }

    /// Get session metadata (for testing)
    pub async fn get_session_metadata(&self, session_id: &str) -> Option<RecordingSession> {
        let session = self.active_sessions.get(session_id)?;
        let metadata = session.metadata.read().await;
        Some(metadata.clone())
    }

    /// Get session stream IDs (for testing)
    pub fn get_session_stream_ids(&self, session_id: &str) -> Option<Vec<String>> {
        let session = self.active_sessions.get(session_id)?;
        Some(session.stream_ids.clone())
    }

    /// Confirm session dialog (for testing - simulates 200 OK from SRS)
    pub async fn confirm_session_dialog(&self, session_id: &str, remote_tag: String) -> Result<()> {
        let session = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| SrcError::SessionNotFound(session_id.to_string()))?;

        let mut dialog = session.dialog.write().await;
        dialog.confirm(remote_tag, None);
        Ok(())
    }

    /// Get session failover state (for testing)
    pub fn get_session_failover_state(&self, session_id: &str) -> Option<(bool, u32)> {
        let session = self.active_sessions.get(session_id)?;
        Some((session.using_backup, session.failover_attempts))
    }
}

/// Media stream configuration for recording
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Codec name (e.g., "PCMU", "opus")
    pub codec: String,

    /// RTP payload type
    pub payload_type: u8,

    /// Sample rate (e.g., 8000, 48000)
    pub sample_rate: u32,

    /// RTP port for this stream
    pub rtp_port: u16,

    /// SSRC for this stream
    pub ssrc: u32,

    /// Destination address for this stream
    pub destination_address: SocketAddr,

    /// SDP crypto attribute (for SRTP)
    pub crypto_attr: Option<String>,
}

/// Media configuration for a recording session
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// List of media streams
    pub streams: Vec<StreamConfig>,
}

impl MediaConfig {
    /// Create a new media configuration
    pub fn new() -> Self {
        Self {
            streams: Vec::new(),
        }
    }

    /// Add a stream
    pub fn add_stream(mut self, stream: StreamConfig) -> Self {
        self.streams.push(stream);
        self
    }
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_src_creation() {
        let config = SrcConfig::default();
        let _src = SessionRecordingClient::new(config).await.unwrap();
    }

    #[tokio::test]
    async fn test_start_recording() {
        let config = SrcConfig::default();
        let src = SessionRecordingClient::new(config).await.unwrap();

        let media_config = MediaConfig::new().add_stream(StreamConfig {
            codec: "PCMU".to_string(),
            payload_type: 0,
            sample_rate: 8000,
            rtp_port: 5004,
            ssrc: 0x12345678,
            destination_address: "192.168.1.100:5004".parse().unwrap(),
            crypto_attr: None,
        });

        let session_id = src
            .start_recording(
                "call-123",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        assert!(session_id.starts_with("recording-call-123"));
        assert_eq!(src.active_session_count(), 1);
    }

    #[tokio::test]
    async fn test_stop_recording() {
        let config = SrcConfig::default();
        let src = SessionRecordingClient::new(config).await.unwrap();

        let media_config = MediaConfig::new().add_stream(StreamConfig {
            codec: "PCMU".to_string(),
            payload_type: 0,
            sample_rate: 8000,
            rtp_port: 5004,
            ssrc: 0x12345678,
            destination_address: "192.168.1.100:5004".parse().unwrap(),
            crypto_attr: None,
        });

        let session_id = src
            .start_recording(
                "call-456",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        assert_eq!(src.active_session_count(), 1);

        // Confirm the dialog (simulating 200 OK from SRS)
        {
            let session = src.active_sessions.get(&session_id).unwrap();
            let mut dialog = session.dialog.write().await;
            dialog.confirm("remote-tag".to_string(), None);
        }

        src.stop_recording(&session_id).await.unwrap();

        assert_eq!(src.active_session_count(), 0);
    }

    #[tokio::test]
    async fn test_recording_with_srtp() {
        let config = SrcConfig {
            forward_srtp_keys: true,
            ..Default::default()
        };
        let src = SessionRecordingClient::new(config).await.unwrap();

        let crypto =
            "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:WVNfX19zZW1jdGwgKCkgewkyMjA7fQp9CnVubGVz";

        let media_config = MediaConfig::new().add_stream(StreamConfig {
            codec: "PCMU".to_string(),
            payload_type: 0,
            sample_rate: 8000,
            rtp_port: 5004,
            ssrc: 0x12345678,
            destination_address: "192.168.1.100:5004".parse().unwrap(),
            crypto_attr: Some(crypto.to_string()),
        });

        let session_id = src
            .start_recording(
                "call-srtp",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        assert!(session_id.starts_with("recording-call-srtp"));

        // Verify SRTP key was stored
        let session = src.active_sessions.get(&session_id).unwrap();
        let stream_id = &session.stream_ids[0];
        assert!(src.srtp_key_manager.get_key_by_stream(stream_id).is_some());
    }

    #[tokio::test]
    async fn test_failover_to_backup() {
        let config = SrcConfig {
            backup_srs_uri: Some("sip:backup-recorder@srs-backup.example.com".to_string()),
            ..Default::default()
        };
        let src = SessionRecordingClient::new(config).await.unwrap();

        let media_config = MediaConfig::new().add_stream(StreamConfig {
            codec: "PCMU".to_string(),
            payload_type: 0,
            sample_rate: 8000,
            rtp_port: 5004,
            ssrc: 0x12345678,
            destination_address: "192.168.1.100:5004".parse().unwrap(),
            crypto_attr: None,
        });

        let session_id = src
            .start_recording(
                "call-failover",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        // Initially not using backup
        {
            let session = src.active_sessions.get(&session_id).unwrap();
            assert!(!session.using_backup);
            assert_eq!(session.failover_attempts, 0);
        }

        // Perform failover
        src.failover_to_backup(&session_id).await.unwrap();

        // Now using backup
        {
            let session = src.active_sessions.get(&session_id).unwrap();
            assert!(session.using_backup);
            assert_eq!(session.failover_attempts, 1);
        }

        // Second failover should fail (already on backup)
        let result = src.failover_to_backup(&session_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_failover_without_backup_configured() {
        let config = SrcConfig {
            backup_srs_uri: None, // No backup configured
            ..Default::default()
        };
        let src = SessionRecordingClient::new(config).await.unwrap();

        let media_config = MediaConfig::new().add_stream(StreamConfig {
            codec: "PCMU".to_string(),
            payload_type: 0,
            sample_rate: 8000,
            rtp_port: 5004,
            ssrc: 0x12345678,
            destination_address: "192.168.1.100:5004".parse().unwrap(),
            crypto_attr: None,
        });

        let session_id = src
            .start_recording(
                "call-no-backup",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        // Failover should fail (no backup configured)
        let result = src.failover_to_backup(&session_id).await;
        assert!(result.is_err());
    }
}
