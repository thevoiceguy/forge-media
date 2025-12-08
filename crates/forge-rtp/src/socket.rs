//! UDP socket management for RTP/RTCP

use crate::{PortPair, RtpPacket};
use forge_core::{ForgeError, Result};
use std::net::{SocketAddr, IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use bytes::Bytes;

/// Configuration for RTP socket pair
#[derive(Debug, Clone)]
pub struct RtpSocketConfig {
    /// Local bind address (defaults to 0.0.0.0)
    pub bind_addr: IpAddr,
    /// Buffer size for receive operations
    pub recv_buffer_size: usize,
    /// Buffer size for send operations
    pub send_buffer_size: usize,
}

impl Default for RtpSocketConfig {
    fn default() -> Self {
        Self {
            bind_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            recv_buffer_size: 1500,
            send_buffer_size: 1500,
        }
    }
}

/// Remote endpoint information learned through symmetric RTP
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEndpoint {
    /// Remote RTP address
    pub rtp_addr: SocketAddr,
    /// Remote RTCP address (if known)
    pub rtcp_addr: Option<SocketAddr>,
}

/// A pair of UDP sockets for RTP and RTCP
pub struct RtpSocketPair {
    /// RTP socket
    rtp_socket: Arc<UdpSocket>,
    /// RTCP socket
    rtcp_socket: Arc<UdpSocket>,
    /// Local port pair
    ports: PortPair,
    /// Remote endpoint (learned via symmetric RTP)
    remote_endpoint: Arc<RwLock<Option<RemoteEndpoint>>>,
    /// Configuration
    config: RtpSocketConfig,
}

impl RtpSocketPair {
    /// Create a new RTP socket pair
    pub async fn new(ports: PortPair, config: RtpSocketConfig) -> Result<Self> {
        // Bind RTP socket
        let rtp_addr = SocketAddr::new(config.bind_addr, ports.rtp_port);
        let rtp_socket = UdpSocket::bind(rtp_addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to bind RTP socket: {}", e)))?;

        // Bind RTCP socket
        let rtcp_addr = SocketAddr::new(config.bind_addr, ports.rtcp_port);
        let rtcp_socket = UdpSocket::bind(rtcp_addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to bind RTCP socket: {}", e)))?;

        // Note: Socket buffer sizes can be set via setsockopt if needed
        // For now, we rely on system defaults which are usually adequate

        Ok(Self {
            rtp_socket: Arc::new(rtp_socket),
            rtcp_socket: Arc::new(rtcp_socket),
            ports,
            remote_endpoint: Arc::new(RwLock::new(None)),
            config,
        })
    }

    /// Get the local port pair
    pub fn ports(&self) -> PortPair {
        self.ports
    }

    /// Get the local RTP address
    pub fn local_rtp_addr(&self) -> Result<SocketAddr> {
        self.rtp_socket
            .local_addr()
            .map_err(|e| ForgeError::Network(format!("Failed to get local RTP address: {}", e)))
    }

    /// Get the local RTCP address
    pub fn local_rtcp_addr(&self) -> Result<SocketAddr> {
        self.rtcp_socket
            .local_addr()
            .map_err(|e| ForgeError::Network(format!("Failed to get local RTCP address: {}", e)))
    }

    /// Get the remote endpoint (if learned)
    pub async fn remote_endpoint(&self) -> Option<RemoteEndpoint> {
        self.remote_endpoint.read().await.clone()
    }

    /// Set the remote endpoint explicitly
    pub async fn set_remote_endpoint(&self, endpoint: RemoteEndpoint) {
        *self.remote_endpoint.write().await = Some(endpoint);
    }

    /// Learn remote endpoint from received packet (symmetric RTP)
    async fn learn_remote_endpoint(&self, addr: SocketAddr) {
        let mut remote = self.remote_endpoint.write().await;

        if let Some(ref mut endpoint) = *remote {
            // Already have RTP endpoint, check if this is RTCP
            if endpoint.rtp_addr.port() + 1 == addr.port() && endpoint.rtp_addr.ip() == addr.ip() {
                endpoint.rtcp_addr = Some(addr);
                tracing::debug!("Learned RTCP endpoint: {}", addr);
            }
        } else {
            // First packet, assume RTP
            *remote = Some(RemoteEndpoint {
                rtp_addr: addr,
                rtcp_addr: None,
            });
            tracing::debug!("Learned RTP endpoint: {}", addr);
        }
    }

    /// Receive an RTP packet
    ///
    /// This method will:
    /// 1. Receive data from the RTP socket
    /// 2. Learn the remote endpoint if not already known (symmetric RTP)
    /// 3. Parse the RTP packet
    /// 4. Return the packet and source address
    pub async fn recv_rtp(&self) -> Result<(RtpPacket, SocketAddr)> {
        let mut buf = vec![0u8; self.config.recv_buffer_size];
        let (len, addr) = self
            .rtp_socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to receive RTP packet: {}", e)))?;

        // Learn remote endpoint (symmetric RTP)
        self.learn_remote_endpoint(addr).await;

        // Parse RTP packet
        buf.truncate(len);
        let packet = RtpPacket::parse(Bytes::from(buf))?;

        Ok((packet, addr))
    }

    /// Send an RTP packet to the remote endpoint
    ///
    /// If no remote endpoint is set, this will return an error.
    pub async fn send_rtp(&self, packet: &RtpPacket) -> Result<usize> {
        let remote = self.remote_endpoint.read().await;
        let endpoint = remote
            .as_ref()
            .ok_or_else(|| ForgeError::Network("No remote endpoint set".to_string()))?;

        let data = packet.to_bytes();
        self.rtp_socket
            .send_to(&data, endpoint.rtp_addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to send RTP packet: {}", e)))
    }

    /// Send raw RTP data to the remote endpoint
    pub async fn send_rtp_raw(&self, data: &[u8]) -> Result<usize> {
        let remote = self.remote_endpoint.read().await;
        let endpoint = remote
            .as_ref()
            .ok_or_else(|| ForgeError::Network("No remote endpoint set".to_string()))?;

        self.rtp_socket
            .send_to(data, endpoint.rtp_addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to send RTP data: {}", e)))
    }

    /// Send raw RTP data to a specific address (bypassing remote endpoint)
    pub async fn send_rtp_to(&self, data: &[u8], addr: SocketAddr) -> Result<usize> {
        self.rtp_socket
            .send_to(data, addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to send RTP data: {}", e)))
    }

    /// Receive RTCP data
    pub async fn recv_rtcp(&self) -> Result<(Bytes, SocketAddr)> {
        let mut buf = vec![0u8; self.config.recv_buffer_size];
        let (len, addr) = self
            .rtcp_socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to receive RTCP packet: {}", e)))?;

        buf.truncate(len);
        Ok((Bytes::from(buf), addr))
    }

    /// Send RTCP data to the remote endpoint
    pub async fn send_rtcp(&self, data: &[u8]) -> Result<usize> {
        let remote = self.remote_endpoint.read().await;
        let endpoint = remote
            .as_ref()
            .ok_or_else(|| ForgeError::Network("No remote endpoint set".to_string()))?;

        let rtcp_addr = endpoint
            .rtcp_addr
            .ok_or_else(|| ForgeError::Network("No remote RTCP endpoint set".to_string()))?;

        self.rtcp_socket
            .send_to(data, rtcp_addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to send RTCP data: {}", e)))
    }

    /// Send RTCP data to a specific address (bypassing remote endpoint)
    pub async fn send_rtcp_to(&self, data: &[u8], addr: SocketAddr) -> Result<usize> {
        self.rtcp_socket
            .send_to(data, addr)
            .await
            .map_err(|e| ForgeError::Network(format!("Failed to send RTCP data: {}", e)))
    }

    /// Get a clone of the RTP socket for external use
    pub fn rtp_socket(&self) -> Arc<UdpSocket> {
        Arc::clone(&self.rtp_socket)
    }

    /// Get a clone of the RTCP socket for external use
    pub fn rtcp_socket(&self) -> Arc<UdpSocket> {
        Arc::clone(&self.rtcp_socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PortPair;

    #[tokio::test]
    async fn test_socket_pair_creation() {
        let config = RtpSocketConfig::default();
        let ports = PortPair::new(10000).unwrap();

        let socket_pair = RtpSocketPair::new(ports, config).await.unwrap();

        assert_eq!(socket_pair.ports(), ports);
        assert!(socket_pair.local_rtp_addr().is_ok());
        assert!(socket_pair.local_rtcp_addr().is_ok());
    }

    #[tokio::test]
    async fn test_remote_endpoint() {
        let config = RtpSocketConfig::default();
        let ports = PortPair::new(10002).unwrap();

        let socket_pair = RtpSocketPair::new(ports, config).await.unwrap();

        // Initially no remote endpoint
        assert!(socket_pair.remote_endpoint().await.is_none());

        // Set remote endpoint
        let remote = RemoteEndpoint {
            rtp_addr: "127.0.0.1:20000".parse().unwrap(),
            rtcp_addr: Some("127.0.0.1:20001".parse().unwrap()),
        };
        socket_pair.set_remote_endpoint(remote.clone()).await;

        // Verify it's set
        let retrieved = socket_pair.remote_endpoint().await.unwrap();
        assert_eq!(retrieved, remote);
    }

    #[tokio::test]
    async fn test_symmetric_rtp_learning() {
        // Use 127.0.0.1 for testing so addresses match
        let config = RtpSocketConfig {
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ..Default::default()
        };
        let ports1 = PortPair::new(10004).unwrap();
        let ports2 = PortPair::new(10006).unwrap();

        let socket1 = RtpSocketPair::new(ports1, config.clone()).await.unwrap();
        let socket2 = RtpSocketPair::new(ports2, config).await.unwrap();

        // Set socket2's remote to socket1
        let addr1 = socket1.local_rtp_addr().unwrap();
        socket2.set_remote_endpoint(RemoteEndpoint {
            rtp_addr: addr1,
            rtcp_addr: None,
        }).await;

        // Send packet from socket2 to socket1
        let test_packet = RtpPacket::build(0, 1234, 5678, 0xABCDEF12, Bytes::from_static(b"test"), false);
        socket2.send_rtp(&test_packet).await.unwrap();

        // Receive on socket1 - should learn socket2's address
        tokio::time::timeout(std::time::Duration::from_millis(100), socket1.recv_rtp())
            .await
            .unwrap()
            .unwrap();

        // Verify socket1 learned socket2's address
        let learned = socket1.remote_endpoint().await.unwrap();
        let addr2 = socket2.local_rtp_addr().unwrap();
        assert_eq!(learned.rtp_addr, addr2);
    }
}
