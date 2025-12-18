//! forge-siprec - SIPREC (Session Recording Protocol) Implementation
//!
//! This crate implements RFC 7865 (Session Recording Metadata) and RFC 7866
//! (Session Recording Protocol) for compliance recording.
//!
//! # Features
//!
//! - **SRC (Session Recording Client)**: Send recording streams to SRS
//! - **SRS (Session Recording Server)**: Receive and store recording streams
//! - **Metadata XML**: RFC 7865 compliant metadata generation
//! - **Media Forking**: Duplicate RTP streams to recording server
//! - **SRTP Support**: Forward SRTP keys for encrypted recordings
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐          ┌─────────────┐
//! │   Caller    │◄────────►│   Callee    │
//! └─────────────┘          └─────────────┘
//!        │                        │
//!        │    Media (RTP)         │
//!        └────────┬───────────────┘
//!                 │
//!                 │ Fork
//!                 ▼
//!         ┌───────────────┐
//!         │  SRC (Client) │
//!         └───────────────┘
//!                 │
//!                 │ SIPREC
//!                 │ (SIP + RTP + Metadata XML)
//!                 ▼
//!         ┌───────────────┐
//!         │  SRS (Server) │
//!         └───────────────┘
//!                 │
//!                 ▼
//!           [Recording Storage]
//! ```
//!
//! # Example Usage
//!
//! ## As Session Recording Client (SRC)
//!
//! ```rust,ignore
//! use forge_siprec::{SrcConfig, SessionRecordingClient};
//!
//! let config = SrcConfig {
//!     srs_uri: "sip:recorder@srs.example.com".to_string(),
//!     local_address: "192.168.1.10".parse().unwrap(),
//!     ..Default::default()
//! };
//!
//! let src = SessionRecordingClient::new(config).await?;
//!
//! // Start recording a call
//! let session_id = src.start_recording(
//!     "call-123",
//!     "sip:alice@example.com",
//!     "sip:bob@example.com"
//! ).await?;
//!
//! // Forward RTP packets to SRS
//! src.forward_rtp(session_id, rtp_packet).await?;
//!
//! // Stop recording
//! src.stop_recording(session_id).await?;
//! ```
//!
//! ## As Session Recording Server (SRS)
//!
//! ```rust,ignore
//! use forge_siprec::{SrsConfig, SessionRecordingServer};
//!
//! let config = SrsConfig {
//!     bind_address: "0.0.0.0:5060".parse().unwrap(),
//!     storage_path: "/var/lib/forge/recordings".into(),
//!     ..Default::default()
//! };
//!
//! let srs = SessionRecordingServer::new(config).await?;
//! srs.start().await?;
//! ```

pub mod forking;
pub mod metadata;
pub mod sip;
pub mod src;
pub mod srs;
pub mod srtp_keys;

// Re-export legacy manager for backward compatibility
mod manager;
pub use manager::{
    Result, SiprecConfig, SiprecError, SiprecManager, SiprecManagerConfig, SiprecSession,
};

// Re-export new API
pub use forking::{ForkedStream, ForkingError, MediaForker};
pub use metadata::{
    ExtensionData, MediaStream, MediaType, Participant, ParticipantRole, RecordingSession,
    RtpSession,
};
pub use sip::{
    DialogManager, DialogState, MediaDescription, SdpBuilder, SipDialog, SipMethod, SipRequest,
};
pub use src::{
    MediaConfig, RecordingSessionState, SessionRecordingClient, SrcConfig, SrcError, StreamConfig,
};
pub use srs::{ActiveRecording, SessionRecordingServer, SrsConfig, SrsError};
pub use srtp_keys::{extract_crypto_from_sdp, SrtpKeyError, SrtpKeyManager, StreamKeyInfo};

#[cfg(test)]
mod integration_tests {
    use super::*;
    use src::{MediaConfig, SessionRecordingClient, SrcConfig, StreamConfig};
    use srs::{SessionRecordingServer, SrsConfig};

    #[tokio::test]
    async fn test_end_to_end_recording_flow() {
        // Create SRS
        let srs_config = SrsConfig {
            storage_path: "/tmp/test-e2e-recording".into(),
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(srs_config).await.unwrap();

        // Create SRC
        let src_config = SrcConfig::default();
        let src = SessionRecordingClient::new(src_config).await.unwrap();

        // Start recording on SRC
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
                "e2e-call-1",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        // Get metadata from SRC
        let metadata = src.get_session_metadata(&session_id).await.unwrap();

        // Accept session on SRS
        srs.accept_session(&session_id, metadata, None)
            .await
            .unwrap();

        // Simulate RTP packets
        let rtp_packets = vec![
            vec![
                0x80, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78, 0xAA, 0xBB,
            ],
            vec![
                0x80, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0xA0, 0x12, 0x34, 0x56, 0x78, 0xCC, 0xDD,
            ],
            vec![
                0x80, 0x00, 0x00, 0x03, 0x00, 0x00, 0x01, 0x40, 0x12, 0x34, 0x56, 0x78, 0xEE, 0xFF,
            ],
        ];

        // Forward RTP packets through both SRC and SRS
        for packet in &rtp_packets {
            // SRC would forward to SRS
            // SRS receives and stores
            srs.receive_rtp_packet(&session_id, packet).await.unwrap();
        }

        // Verify statistics
        let (packet_count, byte_count) = srs.get_session_stats(&session_id).await.unwrap();
        assert_eq!(packet_count, 3);
        assert!(byte_count > 0);

        // Confirm dialog on SRC (simulating 200 OK from SRS)
        src.confirm_session_dialog(&session_id, "remote-tag".to_string())
            .await
            .unwrap();

        // Stop recording on SRC
        src.stop_recording(&session_id).await.unwrap();

        // Stop session on SRS
        srs.stop_session(&session_id).await.unwrap();

        // Verify both SRC and SRS cleaned up
        assert_eq!(src.active_session_count(), 0);
        assert_eq!(srs.active_sessions(), 0);
    }

    #[tokio::test]
    async fn test_end_to_end_with_srtp() {
        // Create SRS
        let srs_config = SrsConfig {
            storage_path: "/tmp/test-e2e-srtp".into(),
            ..Default::default()
        };
        let srs = SessionRecordingServer::new(srs_config).await.unwrap();

        // Create SRC with SRTP enabled
        let src_config = SrcConfig {
            forward_srtp_keys: true,
            ..Default::default()
        };
        let src = SessionRecordingClient::new(src_config).await.unwrap();

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
                "e2e-srtp-call",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        // Verify SRTP keys were extracted
        let stream_ids = src.get_session_stream_ids(&session_id).unwrap();
        let stream_id = &stream_ids[0];
        let key_info = src.srtp_key_manager().get_key_by_stream(stream_id);
        assert!(key_info.is_some());

        let key_info = key_info.unwrap();
        assert_eq!(key_info.suite, "AES_CM_128_HMAC_SHA1_80");
        assert_eq!(key_info.tag, 1);

        // Accept session on SRS
        let metadata = src.get_session_metadata(&session_id).await.unwrap();

        srs.accept_session(&session_id, metadata, None)
            .await
            .unwrap();

        // Confirm dialog
        src.confirm_session_dialog(&session_id, "remote-tag".to_string())
            .await
            .unwrap();

        // Clean up
        src.stop_recording(&session_id).await.unwrap();
        srs.stop_session(&session_id).await.unwrap();
    }

    #[tokio::test]
    async fn test_end_to_end_with_failover() {
        // Create primary and backup SRS
        let srs_primary_config = SrsConfig {
            storage_path: "/tmp/test-e2e-failover-primary".into(),
            ..Default::default()
        };
        let srs_primary = SessionRecordingServer::new(srs_primary_config)
            .await
            .unwrap();

        let srs_backup_config = SrsConfig {
            storage_path: "/tmp/test-e2e-failover-backup".into(),
            ..Default::default()
        };
        let srs_backup = SessionRecordingServer::new(srs_backup_config)
            .await
            .unwrap();

        // Create SRC with backup configured
        let src_config = SrcConfig {
            backup_srs_uri: Some("sip:backup-recorder@srs-backup.example.com".to_string()),
            ..Default::default()
        };
        let src = SessionRecordingClient::new(src_config).await.unwrap();

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
                "e2e-failover-call",
                "sip:alice@example.com",
                "sip:bob@example.com",
                media_config,
            )
            .await
            .unwrap();

        // Accept on primary SRS
        let metadata = src.get_session_metadata(&session_id).await.unwrap();

        srs_primary
            .accept_session(&session_id, metadata.clone(), None)
            .await
            .unwrap();

        // Simulate primary SRS failure and failover
        src.failover_to_backup(&session_id).await.unwrap();

        // Verify failover state
        let (using_backup, failover_attempts) =
            src.get_session_failover_state(&session_id).unwrap();
        assert!(using_backup);
        assert_eq!(failover_attempts, 1);

        // Accept on backup SRS
        srs_backup
            .accept_session(&session_id, metadata, None)
            .await
            .unwrap();

        // Clean up
        assert_eq!(srs_primary.active_sessions(), 1);
        assert_eq!(srs_backup.active_sessions(), 1);

        srs_primary.stop_session(&session_id).await.unwrap();
        srs_backup.stop_session(&session_id).await.unwrap();

        assert_eq!(srs_primary.active_sessions(), 0);
        assert_eq!(srs_backup.active_sessions(), 0);
    }
}
