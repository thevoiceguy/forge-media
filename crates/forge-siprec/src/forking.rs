//! RTP Media Forking for SIPREC
//!
//! This module implements media forking to duplicate RTP streams to a recording server.
//! Media forking allows call audio/video to be sent to both the original destination
//! and the recording server simultaneously.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use thiserror::Error;
use dashmap::DashMap;

use forge_rtp::RtpPacket;

/// Media forking error types
#[derive(Debug, Error)]
pub enum ForkingError {
    /// Socket error
    #[error("Socket error: {0}")]
    Socket(#[from] std::io::Error),

    /// Stream not found
    #[error("Stream not found: {0}")]
    StreamNotFound(String),

    /// Invalid destination
    #[error("Invalid destination: {0}")]
    InvalidDestination(String),

    /// Encoding error
    #[error("Encoding error: {0}")]
    Encoding(String),
}

pub type Result<T> = std::result::Result<T, ForkingError>;

/// Forked stream configuration
#[derive(Debug, Clone)]
pub struct ForkedStream {
    /// Stream identifier
    pub stream_id: String,

    /// Original SSRC
    pub ssrc: u32,

    /// Recording server destination
    pub recording_dest: SocketAddr,

    /// Socket for sending forked packets
    socket: Arc<UdpSocket>,

    /// Packet count for statistics
    pub packet_count: u64,

    /// Byte count for statistics
    pub byte_count: u64,
}

impl ForkedStream {
    /// Create a new forked stream
    pub async fn new(
        stream_id: String,
        ssrc: u32,
        recording_dest: SocketAddr,
        local_bind: Option<SocketAddr>,
    ) -> Result<Self> {
        // Bind to a local address
        let bind_addr = local_bind.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());
        let socket = UdpSocket::bind(bind_addr).await?;

        Ok(Self {
            stream_id,
            ssrc,
            recording_dest,
            socket: Arc::new(socket),
            packet_count: 0,
            byte_count: 0,
        })
    }

    /// Forward an RTP packet to the recording server
    pub async fn forward_packet(&mut self, packet_data: &[u8]) -> Result<()> {
        let bytes_sent = self
            .socket
            .send_to(packet_data, self.recording_dest)
            .await?;

        self.packet_count += 1;
        self.byte_count += bytes_sent as u64;

        Ok(())
    }

    /// Get statistics
    pub fn stats(&self) -> (u64, u64) {
        (self.packet_count, self.byte_count)
    }
}

/// Media Forking Manager
///
/// Manages multiple forked media streams for SIPREC recording.
#[derive(Debug, Clone)]
pub struct MediaForker {
    /// Active forked streams
    streams: Arc<DashMap<String, Arc<RwLock<ForkedStream>>>>,

    /// SSRC to stream mapping
    ssrc_map: Arc<DashMap<u32, String>>,
}

impl MediaForker {
    /// Create a new media forker
    pub fn new() -> Self {
        Self {
            streams: Arc::new(DashMap::new()),
            ssrc_map: Arc::new(DashMap::new()),
        }
    }

    /// Add a stream to fork
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Unique stream identifier
    /// * `ssrc` - RTP SSRC for this stream
    /// * `recording_dest` - Destination address for forked packets
    /// * `local_bind` - Optional local bind address
    pub async fn add_stream(
        &self,
        stream_id: String,
        ssrc: u32,
        recording_dest: SocketAddr,
        local_bind: Option<SocketAddr>,
    ) -> Result<()> {
        let stream = ForkedStream::new(
            stream_id.clone(),
            ssrc,
            recording_dest,
            local_bind,
        )
        .await?;

        let stream_arc = Arc::new(RwLock::new(stream));
        self.streams.insert(stream_id.clone(), stream_arc);
        self.ssrc_map.insert(ssrc, stream_id);

        Ok(())
    }

    /// Remove a forked stream
    pub fn remove_stream(&self, stream_id: &str) -> Option<Arc<RwLock<ForkedStream>>> {
        if let Some((_, stream)) = self.streams.remove(stream_id) {
            // Remove from SSRC map
            let ssrc = {
                let stream_lock = stream.try_read().ok()?;
                stream_lock.ssrc
            };
            self.ssrc_map.remove(&ssrc);
            Some(stream)
        } else {
            None
        }
    }

    /// Forward an RTP packet by stream ID
    pub async fn forward_by_stream_id(
        &self,
        stream_id: &str,
        packet_data: &[u8],
    ) -> Result<()> {
        if let Some(stream) = self.streams.get(stream_id) {
            let mut stream = stream.write().await;
            stream.forward_packet(packet_data).await
        } else {
            Err(ForkingError::StreamNotFound(stream_id.to_string()))
        }
    }

    /// Forward an RTP packet by SSRC
    pub async fn forward_by_ssrc(&self, ssrc: u32, packet_data: &[u8]) -> Result<()> {
        if let Some(stream_id) = self.ssrc_map.get(&ssrc) {
            let stream_id = stream_id.clone();
            drop(stream_id); // Drop the reference before calling forward_by_stream_id
            if let Some(stream_id_entry) = self.ssrc_map.get(&ssrc) {
                let id = stream_id_entry.value().clone();
                self.forward_by_stream_id(&id, packet_data).await
            } else {
                Err(ForkingError::StreamNotFound(format!("SSRC {}", ssrc)))
            }
        } else {
            Err(ForkingError::StreamNotFound(format!("SSRC {}", ssrc)))
        }
    }

    /// Forward a parsed RTP packet
    pub async fn forward_packet(&self, packet: &RtpPacket) -> Result<()> {
        // Serialize the packet back to bytes using RtpPacket's built-in method
        let packet_bytes = packet.to_bytes();
        self.forward_by_ssrc(packet.header.ssrc, &packet_bytes)
            .await
    }

    /// Get stream count
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Get statistics for a stream
    pub async fn get_stats(&self, stream_id: &str) -> Option<(u64, u64)> {
        if let Some(stream) = self.streams.get(stream_id) {
            let stream = stream.read().await;
            Some(stream.stats())
        } else {
            None
        }
    }

    /// Get statistics for all streams
    pub async fn get_all_stats(&self) -> HashMap<String, (u64, u64)> {
        let mut stats = HashMap::new();
        for entry in self.streams.iter() {
            let stream_id = entry.key().clone();
            let stream = entry.value().read().await;
            stats.insert(stream_id, stream.stats());
        }
        stats
    }
}

impl Default for MediaForker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use forge_rtp::{RtpHeader, RtpPacket};

    #[tokio::test]
    async fn test_forked_stream_creation() {
        let dest: SocketAddr = "127.0.0.1:10000".parse().unwrap();
        let stream = ForkedStream::new("stream-1".to_string(), 12345, dest, None)
            .await
            .unwrap();

        assert_eq!(stream.stream_id, "stream-1");
        assert_eq!(stream.ssrc, 12345);
        assert_eq!(stream.packet_count, 0);
    }

    #[tokio::test]
    async fn test_media_forker_add_stream() {
        let forker = MediaForker::new();
        let dest: SocketAddr = "127.0.0.1:10000".parse().unwrap();

        forker
            .add_stream("stream-1".to_string(), 12345, dest, None)
            .await
            .unwrap();

        assert_eq!(forker.stream_count(), 1);
    }

    #[tokio::test]
    async fn test_media_forker_remove_stream() {
        let forker = MediaForker::new();
        let dest: SocketAddr = "127.0.0.1:10000".parse().unwrap();

        forker
            .add_stream("stream-1".to_string(), 12345, dest, None)
            .await
            .unwrap();

        assert_eq!(forker.stream_count(), 1);

        forker.remove_stream("stream-1");

        assert_eq!(forker.stream_count(), 0);
    }

    #[tokio::test]
    async fn test_rtp_packet_serialization() {
        // V(2)=2, P(1)=0, X(1)=0, CC(4)=0 = 0b10000000 = 0x80
        let version_flags = 0x80;
        // M(1)=0, PT(7)=0 = 0b00000000 = 0x00
        let marker_payload_type = 0x00;

        let packet = RtpPacket {
            header: RtpHeader {
                version_flags,
                marker_payload_type,
                sequence_number: 1234,
                timestamp: 160000,
                ssrc: 0x12345678,
            },
            csrc_list: vec![],
            extension: None,
            payload: Bytes::from_static(b"test payload"),
            padding_len: 0,
        };

        let serialized = packet.to_bytes();

        // Check header is correct size (12 bytes + payload)
        assert_eq!(serialized.len(), 12 + b"test payload".len());

        // Check version bits (should be 2)
        assert_eq!(serialized[0] >> 6, 2);
    }

    #[tokio::test]
    async fn test_forker_stats() {
        let forker = MediaForker::new();
        let dest: SocketAddr = "127.0.0.1:10000".parse().unwrap();

        forker
            .add_stream("stream-1".to_string(), 12345, dest, None)
            .await
            .unwrap();

        let stats = forker.get_stats("stream-1").await.unwrap();
        assert_eq!(stats, (0, 0)); // Initially zero
    }
}
