//! STUN (Session Traversal Utilities for NAT) client implementation
//! RFC 8489

use bytes::{Buf, BufMut, BytesMut};
use forge_core::{ForgeError, Result};
use rand::Rng;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::net::UdpSocket;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

/// STUN magic cookie (RFC 8489 Section 6)
const MAGIC_COOKIE: u32 = 0x2112A442;

/// STUN message types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum MessageType {
    /// Binding Request (0x0001)
    BindingRequest = 0x0001,
    /// Binding Response (0x0101)
    BindingResponse = 0x0101,
    /// Binding Error Response (0x0111)
    BindingErrorResponse = 0x0111,
}

impl MessageType {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x0001 => Some(MessageType::BindingRequest),
            0x0101 => Some(MessageType::BindingResponse),
            0x0111 => Some(MessageType::BindingErrorResponse),
            _ => None,
        }
    }
}

/// STUN attribute types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum AttributeType {
    /// MAPPED-ADDRESS (0x0001)
    MappedAddress = 0x0001,
    /// XOR-MAPPED-ADDRESS (0x0020) - RFC 8489 Section 14.2
    XorMappedAddress = 0x0020,
    /// MESSAGE-INTEGRITY (0x0008)
    MessageIntegrity = 0x0008,
    /// FINGERPRINT (0x8028)
    Fingerprint = 0x8028,
}

impl AttributeType {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x0001 => Some(AttributeType::MappedAddress),
            0x0020 => Some(AttributeType::XorMappedAddress),
            0x0008 => Some(AttributeType::MessageIntegrity),
            0x8028 => Some(AttributeType::Fingerprint),
            _ => None,
        }
    }
}

/// STUN message
#[derive(Debug, Clone)]
pub struct StunMessage {
    /// Message type
    pub message_type: MessageType,
    /// Transaction ID (96 bits / 12 bytes)
    pub transaction_id: [u8; 12],
    /// Attributes
    pub attributes: Vec<StunAttribute>,
}

/// STUN attribute
#[derive(Debug, Clone)]
pub enum StunAttribute {
    /// XOR-MAPPED-ADDRESS attribute
    XorMappedAddress(SocketAddr),
    /// MAPPED-ADDRESS attribute
    MappedAddress(SocketAddr),
    /// FINGERPRINT attribute (CRC-32)
    Fingerprint(u32),
    /// Unknown attribute
    Unknown { attr_type: u16, value: Vec<u8> },
}

impl StunMessage {
    /// Create a new STUN Binding Request
    pub fn new_binding_request() -> Self {
        let mut rng = rand::thread_rng();
        let mut transaction_id = [0u8; 12];
        rng.fill(&mut transaction_id);

        Self {
            message_type: MessageType::BindingRequest,
            transaction_id,
            attributes: Vec::new(),
        }
    }

    /// Add FINGERPRINT attribute (RFC 8489 Section 14.7)
    pub fn add_fingerprint(&mut self) {
        // Serialize message without fingerprint
        let mut buf = BytesMut::new();
        self.serialize_without_fingerprint(&mut buf);

        // Calculate CRC-32
        let crc = crc32(&buf);
        let fingerprint = crc ^ 0x5354554e; // XOR with "STUN" in ASCII

        self.attributes.push(StunAttribute::Fingerprint(fingerprint));
    }

    /// Serialize message to bytes
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = BytesMut::new();

        // Calculate total message length (excluding 20-byte header)
        let mut attr_length = 0;
        for attr in &self.attributes {
            attr_length += 4 + attr.value_len(); // 4-byte attribute header + value
            // Pad to 4-byte boundary
            let padding = (4 - (attr.value_len() % 4)) % 4;
            attr_length += padding;
        }

        // STUN header (20 bytes)
        buf.put_u16(self.message_type as u16); // Message type
        buf.put_u16(attr_length as u16); // Message length
        buf.put_u32(MAGIC_COOKIE); // Magic cookie
        buf.put_slice(&self.transaction_id); // Transaction ID

        // Attributes
        for attr in &self.attributes {
            attr.serialize(&mut buf, &self.transaction_id);
        }

        buf.to_vec()
    }

    /// Serialize without fingerprint (for fingerprint calculation)
    fn serialize_without_fingerprint(&self, buf: &mut BytesMut) {
        let mut attr_length = 0;
        for attr in &self.attributes {
            if !matches!(attr, StunAttribute::Fingerprint(_)) {
                attr_length += 4 + attr.value_len();
                let padding = (4 - (attr.value_len() % 4)) % 4;
                attr_length += padding;
            }
        }

        // STUN header
        buf.put_u16(self.message_type as u16);
        buf.put_u16(attr_length as u16);
        buf.put_u32(MAGIC_COOKIE);
        buf.put_slice(&self.transaction_id);

        // Attributes (excluding fingerprint)
        for attr in &self.attributes {
            if !matches!(attr, StunAttribute::Fingerprint(_)) {
                attr.serialize(buf, &self.transaction_id);
            }
        }
    }

    /// Parse STUN message from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 20 {
            return Err(ForgeError::Ice("STUN message too short".to_string()));
        }

        let mut cursor = &data[..];

        // Parse header
        let msg_type = cursor.get_u16();
        let msg_length = cursor.get_u16() as usize;
        let magic = cursor.get_u32();
        let mut transaction_id = [0u8; 12];
        cursor.copy_to_slice(&mut transaction_id);

        if magic != MAGIC_COOKIE {
            return Err(ForgeError::Ice(format!(
                "Invalid STUN magic cookie: {:#x}",
                magic
            )));
        }

        let message_type = MessageType::from_u16(msg_type)
            .ok_or_else(|| ForgeError::Ice(format!("Unknown STUN message type: {:#x}", msg_type)))?;

        // Parse attributes
        let mut attributes = Vec::new();
        let attr_data = &data[20..];

        if attr_data.len() < msg_length {
            return Err(ForgeError::Ice("Truncated STUN message".to_string()));
        }

        let mut offset = 0;
        while offset < msg_length {
            if offset + 4 > msg_length {
                break;
            }

            let attr_type = u16::from_be_bytes([attr_data[offset], attr_data[offset + 1]]);
            let attr_len = u16::from_be_bytes([attr_data[offset + 2], attr_data[offset + 3]]) as usize;

            offset += 4;

            if offset + attr_len > msg_length {
                return Err(ForgeError::Ice("Truncated attribute".to_string()));
            }

            let attr_value = &attr_data[offset..offset + attr_len];

            let attribute = match AttributeType::from_u16(attr_type) {
                Some(AttributeType::XorMappedAddress) => {
                    let addr = parse_xor_mapped_address(attr_value, &transaction_id)?;
                    StunAttribute::XorMappedAddress(addr)
                }
                Some(AttributeType::MappedAddress) => {
                    let addr = parse_mapped_address(attr_value)?;
                    StunAttribute::MappedAddress(addr)
                }
                Some(AttributeType::Fingerprint) => {
                    if attr_len == 4 {
                        let fingerprint = u32::from_be_bytes([
                            attr_value[0],
                            attr_value[1],
                            attr_value[2],
                            attr_value[3],
                        ]);
                        StunAttribute::Fingerprint(fingerprint)
                    } else {
                        StunAttribute::Unknown {
                            attr_type,
                            value: attr_value.to_vec(),
                        }
                    }
                }
                _ => StunAttribute::Unknown {
                    attr_type,
                    value: attr_value.to_vec(),
                },
            };

            attributes.push(attribute);

            // Skip padding to 4-byte boundary
            offset += attr_len;
            let padding = (4 - (attr_len % 4)) % 4;
            offset += padding;
        }

        Ok(Self {
            message_type,
            transaction_id,
            attributes,
        })
    }

    /// Get XOR-MAPPED-ADDRESS from response
    pub fn get_xor_mapped_address(&self) -> Option<SocketAddr> {
        for attr in &self.attributes {
            if let StunAttribute::XorMappedAddress(addr) = attr {
                return Some(*addr);
            }
        }
        None
    }
}

impl StunAttribute {
    fn value_len(&self) -> usize {
        match self {
            StunAttribute::XorMappedAddress(addr) | StunAttribute::MappedAddress(addr) => {
                match addr.ip() {
                    IpAddr::V4(_) => 8, // family(2) + port(2) + ipv4(4)
                    IpAddr::V6(_) => 20, // family(2) + port(2) + ipv6(16)
                }
            }
            StunAttribute::Fingerprint(_) => 4,
            StunAttribute::Unknown { value, .. } => value.len(),
        }
    }

    fn serialize(&self, buf: &mut BytesMut, transaction_id: &[u8; 12]) {
        match self {
            StunAttribute::XorMappedAddress(addr) => {
                buf.put_u16(AttributeType::XorMappedAddress as u16);
                serialize_xor_mapped_address(buf, *addr, transaction_id);
            }
            StunAttribute::MappedAddress(addr) => {
                buf.put_u16(AttributeType::MappedAddress as u16);
                serialize_mapped_address(buf, *addr);
            }
            StunAttribute::Fingerprint(crc) => {
                buf.put_u16(AttributeType::Fingerprint as u16);
                buf.put_u16(4); // Length
                buf.put_u32(*crc);
            }
            StunAttribute::Unknown { attr_type, value } => {
                buf.put_u16(*attr_type);
                buf.put_u16(value.len() as u16);
                buf.put_slice(value);
                // Padding
                let padding = (4 - (value.len() % 4)) % 4;
                for _ in 0..padding {
                    buf.put_u8(0);
                }
            }
        }
    }
}

/// Parse XOR-MAPPED-ADDRESS attribute
fn parse_xor_mapped_address(data: &[u8], transaction_id: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(ForgeError::Ice("XOR-MAPPED-ADDRESS too short".to_string()));
    }

    let family = u16::from_be_bytes([data[0], data[1]]);
    let xor_port = u16::from_be_bytes([data[2], data[3]]);

    // XOR port with most significant 16 bits of magic cookie
    let port = xor_port ^ (MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            if data.len() < 8 {
                return Err(ForgeError::Ice("Invalid XOR-MAPPED-ADDRESS length".to_string()));
            }
            let xor_addr = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let addr = xor_addr ^ MAGIC_COOKIE;
            let ip = Ipv4Addr::from(addr);
            Ok(SocketAddr::new(IpAddr::V4(ip), port))
        }
        0x02 => {
            // IPv6
            if data.len() < 20 {
                return Err(ForgeError::Ice("Invalid XOR-MAPPED-ADDRESS length".to_string()));
            }
            let mut xor_addr = [0u8; 16];
            xor_addr.copy_from_slice(&data[4..20]);

            // XOR with magic cookie (first 4 bytes) + transaction ID (12 bytes)
            let mut xor_mask = [0u8; 16];
            xor_mask[0..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
            xor_mask[4..16].copy_from_slice(transaction_id);

            for i in 0..16 {
                xor_addr[i] ^= xor_mask[i];
            }

            let ip = Ipv6Addr::from(xor_addr);
            Ok(SocketAddr::new(IpAddr::V6(ip), port))
        }
        _ => Err(ForgeError::Ice(format!("Unknown address family: {}", family))),
    }
}

/// Parse MAPPED-ADDRESS attribute
fn parse_mapped_address(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(ForgeError::Ice("MAPPED-ADDRESS too short".to_string()));
    }

    let family = u16::from_be_bytes([data[0], data[1]]);
    let port = u16::from_be_bytes([data[2], data[3]]);

    match family {
        0x01 => {
            // IPv4
            let addr = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let ip = Ipv4Addr::from(addr);
            Ok(SocketAddr::new(IpAddr::V4(ip), port))
        }
        0x02 => {
            // IPv6
            if data.len() < 20 {
                return Err(ForgeError::Ice("Invalid MAPPED-ADDRESS length".to_string()));
            }
            let mut addr = [0u8; 16];
            addr.copy_from_slice(&data[4..20]);
            let ip = Ipv6Addr::from(addr);
            Ok(SocketAddr::new(IpAddr::V6(ip), port))
        }
        _ => Err(ForgeError::Ice(format!("Unknown address family: {}", family))),
    }
}

/// Serialize XOR-MAPPED-ADDRESS attribute
fn serialize_xor_mapped_address(buf: &mut BytesMut, addr: SocketAddr, transaction_id: &[u8; 12]) {
    match addr.ip() {
        IpAddr::V4(ipv4) => {
            buf.put_u16(8); // Length
            buf.put_u16(0x01); // Family = IPv4

            // XOR port
            let xor_port = addr.port() ^ (MAGIC_COOKIE >> 16) as u16;
            buf.put_u16(xor_port);

            // XOR address
            let ip_u32 = u32::from(ipv4);
            let xor_addr = ip_u32 ^ MAGIC_COOKIE;
            buf.put_u32(xor_addr);
        }
        IpAddr::V6(ipv6) => {
            buf.put_u16(20); // Length
            buf.put_u16(0x02); // Family = IPv6

            // XOR port
            let xor_port = addr.port() ^ (MAGIC_COOKIE >> 16) as u16;
            buf.put_u16(xor_port);

            // XOR address with magic cookie + transaction ID
            let ip_bytes = ipv6.octets();
            let mut xor_mask = [0u8; 16];
            xor_mask[0..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
            xor_mask[4..16].copy_from_slice(transaction_id);

            for i in 0..16 {
                buf.put_u8(ip_bytes[i] ^ xor_mask[i]);
            }
        }
    }
}

/// Serialize MAPPED-ADDRESS attribute
fn serialize_mapped_address(buf: &mut BytesMut, addr: SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ipv4) => {
            buf.put_u16(8); // Length
            buf.put_u16(0x01); // Family = IPv4
            buf.put_u16(addr.port());
            buf.put_slice(&ipv4.octets());
        }
        IpAddr::V6(ipv6) => {
            buf.put_u16(20); // Length
            buf.put_u16(0x02); // Family = IPv6
            buf.put_u16(addr.port());
            buf.put_slice(&ipv6.octets());
        }
    }
}

/// Calculate CRC-32 for FINGERPRINT
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFFFFFFu32;

    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB88320
            } else {
                crc >> 1
            };
        }
    }

    !crc
}

/// STUN client for performing Binding requests
pub struct StunClient {
    /// UDP socket for STUN communication
    socket: UdpSocket,
    /// Request timeout
    timeout: Duration,
}

impl StunClient {
    /// Create a new STUN client
    pub async fn new(local_addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind(local_addr)
            .await
            .map_err(|e| ForgeError::Ice(format!("Failed to bind STUN socket: {}", e)))?;

        Ok(Self {
            socket,
            timeout: Duration::from_secs(3),
        })
    }

    /// Perform a STUN Binding Request to discover the mapped address
    pub async fn binding_request(&self, server: SocketAddr) -> Result<SocketAddr> {
        // Create Binding Request
        let mut request = StunMessage::new_binding_request();
        request.add_fingerprint();

        let request_bytes = request.serialize();
        let transaction_id = request.transaction_id;

        debug!(
            "Sending STUN Binding Request to {} (txn: {})",
            server,
            hex::encode(&transaction_id)
        );

        // Send request
        self.socket
            .send_to(&request_bytes, server)
            .await
            .map_err(|e| ForgeError::Ice(format!("Failed to send STUN request: {}", e)))?;

        // Receive response with timeout
        let mut buf = vec![0u8; 1500];

        let response_bytes = match timeout(self.timeout, self.socket.recv(&mut buf)).await {
            Ok(Ok(len)) => &buf[..len],
            Ok(Err(e)) => {
                return Err(ForgeError::Ice(format!("Failed to receive STUN response: {}", e)));
            }
            Err(_) => {
                return Err(ForgeError::Ice("STUN request timeout".to_string()));
            }
        };

        // Parse response
        let response = StunMessage::parse(response_bytes)?;

        // Verify transaction ID matches
        if response.transaction_id != transaction_id {
            return Err(ForgeError::Ice("STUN transaction ID mismatch".to_string()));
        }

        // Check message type
        if response.message_type != MessageType::BindingResponse {
            warn!("Received non-success STUN response: {:?}", response.message_type);
            return Err(ForgeError::Ice("STUN binding failed".to_string()));
        }

        // Extract mapped address
        let mapped_addr = response
            .get_xor_mapped_address()
            .ok_or_else(|| ForgeError::Ice("No XOR-MAPPED-ADDRESS in STUN response".to_string()))?;

        debug!("STUN Binding Response: mapped address = {}", mapped_addr);

        Ok(mapped_addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stun_message_serialize_parse() {
        let mut msg = StunMessage::new_binding_request();
        msg.add_fingerprint();

        let bytes = msg.serialize();
        let parsed = StunMessage::parse(&bytes).unwrap();

        assert_eq!(parsed.message_type, MessageType::BindingRequest);
        assert_eq!(parsed.transaction_id, msg.transaction_id);
        assert_eq!(parsed.attributes.len(), 1);
    }

    #[test]
    fn test_xor_mapped_address() {
        let transaction_id = [0u8; 12];
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 50000);

        let mut buf = BytesMut::new();
        serialize_xor_mapped_address(&mut buf, addr, &transaction_id);

        let parsed = parse_xor_mapped_address(&buf[2..], &transaction_id).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn test_crc32() {
        // Test with known CRC-32 value
        let data = b"123456789";
        let crc = crc32(data);
        assert_eq!(crc, 0xCBF43926);
    }
}
