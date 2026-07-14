//! STUN (Session Traversal Utilities for NAT) client implementation
//! RFC 8489

use bytes::{Buf, BufMut, BytesMut};
use forge_core::{ForgeError, Result};
use hmac::{Hmac, Mac};
use rand::rngs::SysRng;
use rand::TryRng;
use sha1::Sha1;
use socket2::{Domain, Protocol as SocketProtocol, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Instant;
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
    /// USERNAME (0x0006)
    Username = 0x0006,
    /// XOR-MAPPED-ADDRESS (0x0020) - RFC 8489 Section 14.2
    XorMappedAddress = 0x0020,
    /// MESSAGE-INTEGRITY (0x0008)
    MessageIntegrity = 0x0008,
    /// PRIORITY (0x0024) - ICE
    Priority = 0x0024,
    /// ICE-CONTROLLED (0x8029)
    IceControlled = 0x8029,
    /// ICE-CONTROLLING (0x802a)
    IceControlling = 0x802a,
    /// FINGERPRINT (0x8028)
    Fingerprint = 0x8028,
}

impl AttributeType {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x0001 => Some(AttributeType::MappedAddress),
            0x0006 => Some(AttributeType::Username),
            0x0020 => Some(AttributeType::XorMappedAddress),
            0x0008 => Some(AttributeType::MessageIntegrity),
            0x0024 => Some(AttributeType::Priority),
            0x8029 => Some(AttributeType::IceControlled),
            0x802a => Some(AttributeType::IceControlling),
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
    /// USERNAME attribute
    Username(String),
    /// PRIORITY attribute (ICE)
    Priority(u32),
    /// ICE-CONTROLLED attribute
    IceControlled(u64),
    /// ICE-CONTROLLING attribute
    IceControlling(u64),
    /// FINGERPRINT attribute (CRC-32)
    Fingerprint(u32),
    /// MESSAGE-INTEGRITY attribute
    MessageIntegrity([u8; 20]),
    /// Unknown attribute
    Unknown { attr_type: u16, value: Vec<u8> },
}

impl StunMessage {
    /// Create a new STUN Binding Request with a CSPRNG-derived transaction ID.
    ///
    /// RFC 8489 §6 requires transaction IDs to be unpredictable. Audit
    /// finding C8: the previous implementation used `rand::thread_rng()`, a
    /// ChaCha8-seeded PRNG that is deterministic after seed recovery. We use
    /// `SysRng` (the OS CSPRNG — `getrandom(2)` on Linux, `BCryptGenRandom`
    /// on Windows) so that an off-path attacker cannot predict an
    /// outstanding transaction ID and forge a response.
    pub fn new_binding_request() -> Self {
        let mut transaction_id = [0u8; 12];
        SysRng
            .try_fill_bytes(&mut transaction_id)
            .expect("OS RNG must not fail");

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

        self.attributes
            .push(StunAttribute::Fingerprint(fingerprint));
    }

    /// Add USERNAME attribute
    pub fn add_username(&mut self, username: String) {
        self.attributes.push(StunAttribute::Username(username));
    }

    /// Add PRIORITY attribute
    pub fn add_priority(&mut self, priority: u32) {
        self.attributes.push(StunAttribute::Priority(priority));
    }

    /// Add ICE-CONTROLLED attribute
    pub fn add_ice_controlled(&mut self, tie_breaker: u64) {
        self.attributes
            .push(StunAttribute::IceControlled(tie_breaker));
    }

    /// Add ICE-CONTROLLING attribute
    pub fn add_ice_controlling(&mut self, tie_breaker: u64) {
        self.attributes
            .push(StunAttribute::IceControlling(tie_breaker));
    }

    /// Add MESSAGE-INTEGRITY attribute (HMAC-SHA1)
    pub fn add_message_integrity(&mut self, key: &[u8]) -> Result<()> {
        // Ensure there is only one MESSAGE-INTEGRITY attribute and it precedes FINGERPRINT
        self.attributes
            .retain(|attr| !matches!(attr, StunAttribute::MessageIntegrity(_)));

        let mi_index = self
            .attributes
            .iter()
            .position(|attr| matches!(attr, StunAttribute::Fingerprint(_)))
            .unwrap_or(self.attributes.len());

        self.attributes
            .insert(mi_index, StunAttribute::MessageIntegrity([0u8; 20]));

        // Serialize without fingerprint and with zeroed integrity value for the HMAC calculation
        let serialized = self.serialize_internal(false, Some([0u8; 20]));

        let mut mac = Hmac::<Sha1>::new_from_slice(key).map_err(|e| {
            ForgeError::Ice(format!(
                "Failed to create HMAC for MESSAGE-INTEGRITY: {}",
                e
            ))
        })?;
        mac.update(&serialized);

        let result = mac.finalize().into_bytes();
        let mut integrity = [0u8; 20];
        integrity.copy_from_slice(&result[..20]);

        if let Some(StunAttribute::MessageIntegrity(existing)) = self
            .attributes
            .iter_mut()
            .find(|attr| matches!(attr, StunAttribute::MessageIntegrity(_)))
        {
            *existing = integrity;
        }

        Ok(())
    }

    /// Verify MESSAGE-INTEGRITY attribute
    ///
    /// Validates the HMAC-SHA1 signature in the MESSAGE-INTEGRITY attribute
    /// using the provided key. This is used when receiving STUN messages to
    /// authenticate the sender.
    ///
    /// The comparison is **constant-time** (`hmac::Mac::verify_slice` uses
    /// `subtle::ConstantTimeEq` internally). Audit finding C10: the
    /// previous `==` comparison leaked timing information an attacker
    /// could use to probe valid signatures byte-by-byte.
    ///
    /// # Arguments
    /// * `key` - The shared secret key for HMAC verification (typically the ICE password)
    ///
    /// # Returns
    /// * `Ok(true)` - MESSAGE-INTEGRITY is present and valid
    /// * `Ok(false)` - MESSAGE-INTEGRITY is missing (message not authenticated)
    /// * `Err(_)` - MESSAGE-INTEGRITY is present but validation failed
    pub fn verify_message_integrity(&self, key: &[u8]) -> Result<bool> {
        // Find MESSAGE-INTEGRITY attribute
        let received_integrity = match self
            .attributes
            .iter()
            .find(|attr| matches!(attr, StunAttribute::MessageIntegrity(_)))
        {
            Some(StunAttribute::MessageIntegrity(value)) => value,
            _ => return Ok(false), // No MESSAGE-INTEGRITY present
        };

        // Serialize message without FINGERPRINT and with zeroed MESSAGE-INTEGRITY
        let serialized = self.serialize_internal(false, Some([0u8; 20]));

        // Constant-time compare via `verify_slice` (backed by `subtle`).
        let mut verifier = Hmac::<Sha1>::new_from_slice(key).map_err(|e| {
            ForgeError::Ice(format!(
                "Failed to create HMAC for MESSAGE-INTEGRITY verification: {}",
                e
            ))
        })?;
        verifier.update(&serialized);
        match verifier.verify_slice(&received_integrity[..]) {
            Ok(()) => Ok(true),
            Err(_) => Err(ForgeError::Ice(
                "MESSAGE-INTEGRITY verification failed: HMAC mismatch".to_string(),
            )),
        }
    }

    /// Serialize message to bytes
    pub fn serialize(&self) -> Vec<u8> {
        self.serialize_internal(true, None).to_vec()
    }

    /// Serialize without fingerprint (for fingerprint calculation)
    fn serialize_without_fingerprint(&self, buf: &mut BytesMut) {
        let serialized = self.serialize_internal(false, None);
        buf.extend_from_slice(&serialized);
    }

    fn serialize_internal(
        &self,
        include_fingerprint: bool,
        integrity_override: Option<[u8; 20]>,
    ) -> BytesMut {
        let mut buf = BytesMut::new();

        // Calculate total message length (excluding 20-byte header)
        let mut attr_length = 0;
        for attr in &self.attributes {
            if !include_fingerprint && matches!(attr, StunAttribute::Fingerprint(_)) {
                continue;
            }
            attr_length += 4 + attr.value_len();
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
            if !include_fingerprint && matches!(attr, StunAttribute::Fingerprint(_)) {
                continue;
            }
            attr.serialize(&mut buf, &self.transaction_id, integrity_override.as_ref());
        }

        buf
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

        let message_type = MessageType::from_u16(msg_type).ok_or_else(|| {
            ForgeError::Ice(format!("Unknown STUN message type: {:#x}", msg_type))
        })?;

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
            let attr_len =
                u16::from_be_bytes([attr_data[offset + 2], attr_data[offset + 3]]) as usize;

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
                Some(AttributeType::Username) => match std::str::from_utf8(attr_value) {
                    Ok(value) => StunAttribute::Username(value.to_string()),
                    Err(_) => StunAttribute::Unknown {
                        attr_type,
                        value: attr_value.to_vec(),
                    },
                },
                Some(AttributeType::Priority) if attr_len == 4 => {
                    let priority = u32::from_be_bytes([
                        attr_value[0],
                        attr_value[1],
                        attr_value[2],
                        attr_value[3],
                    ]);
                    StunAttribute::Priority(priority)
                }
                Some(AttributeType::IceControlled) if attr_len == 8 => {
                    let tie_breaker = u64::from_be_bytes([
                        attr_value[0],
                        attr_value[1],
                        attr_value[2],
                        attr_value[3],
                        attr_value[4],
                        attr_value[5],
                        attr_value[6],
                        attr_value[7],
                    ]);
                    StunAttribute::IceControlled(tie_breaker)
                }
                Some(AttributeType::IceControlling) if attr_len == 8 => {
                    let tie_breaker = u64::from_be_bytes([
                        attr_value[0],
                        attr_value[1],
                        attr_value[2],
                        attr_value[3],
                        attr_value[4],
                        attr_value[5],
                        attr_value[6],
                        attr_value[7],
                    ]);
                    StunAttribute::IceControlling(tie_breaker)
                }
                Some(AttributeType::MessageIntegrity) if attr_len == 20 => {
                    let mut mi = [0u8; 20];
                    mi.copy_from_slice(attr_value);
                    StunAttribute::MessageIntegrity(mi)
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
                    IpAddr::V4(_) => 8,  // family(2) + port(2) + ipv4(4)
                    IpAddr::V6(_) => 20, // family(2) + port(2) + ipv6(16)
                }
            }
            StunAttribute::Username(value) => value.as_bytes().len(),
            StunAttribute::Priority(_) => 4,
            StunAttribute::IceControlled(_) | StunAttribute::IceControlling(_) => 8,
            StunAttribute::MessageIntegrity(_) => 20,
            StunAttribute::Fingerprint(_) => 4,
            StunAttribute::Unknown { value, .. } => value.len(),
        }
    }

    fn serialize(
        &self,
        buf: &mut BytesMut,
        transaction_id: &[u8; 12],
        integrity_override: Option<&[u8; 20]>,
    ) {
        match self {
            StunAttribute::XorMappedAddress(addr) => {
                buf.put_u16(AttributeType::XorMappedAddress as u16);
                serialize_xor_mapped_address(buf, *addr, transaction_id);
            }
            StunAttribute::MappedAddress(addr) => {
                buf.put_u16(AttributeType::MappedAddress as u16);
                serialize_mapped_address(buf, *addr);
            }
            StunAttribute::Username(value) => {
                buf.put_u16(AttributeType::Username as u16);
                buf.put_u16(value.as_bytes().len() as u16);
                buf.put_slice(value.as_bytes());
                let padding = (4 - (value.as_bytes().len() % 4)) % 4;
                for _ in 0..padding {
                    buf.put_u8(0);
                }
            }
            StunAttribute::Priority(priority) => {
                buf.put_u16(AttributeType::Priority as u16);
                buf.put_u16(4);
                buf.put_u32(*priority);
            }
            StunAttribute::IceControlled(tie_breaker) => {
                buf.put_u16(AttributeType::IceControlled as u16);
                buf.put_u16(8);
                buf.put_u64(*tie_breaker);
            }
            StunAttribute::IceControlling(tie_breaker) => {
                buf.put_u16(AttributeType::IceControlling as u16);
                buf.put_u16(8);
                buf.put_u64(*tie_breaker);
            }
            StunAttribute::MessageIntegrity(value) => {
                buf.put_u16(AttributeType::MessageIntegrity as u16);
                buf.put_u16(20);
                if let Some(override_val) = integrity_override {
                    buf.put_slice(override_val);
                } else {
                    buf.put_slice(value);
                }
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
                return Err(ForgeError::Ice(
                    "Invalid XOR-MAPPED-ADDRESS length".to_string(),
                ));
            }
            let xor_addr = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let addr = xor_addr ^ MAGIC_COOKIE;
            let ip = Ipv4Addr::from(addr);
            Ok(SocketAddr::new(IpAddr::V4(ip), port))
        }
        0x02 => {
            // IPv6
            if data.len() < 20 {
                return Err(ForgeError::Ice(
                    "Invalid XOR-MAPPED-ADDRESS length".to_string(),
                ));
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
        _ => Err(ForgeError::Ice(format!(
            "Unknown address family: {}",
            family
        ))),
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
        _ => Err(ForgeError::Ice(format!(
            "Unknown address family: {}",
            family
        ))),
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

/// Decision emitted by [`StunServer::handle_binding_request`] for an
/// incoming STUN Binding Request.
#[derive(Debug)]
pub enum StunServerResponse {
    /// Build and send a success response back to the peer.
    Respond(StunMessage),
    /// The request was malformed or failed integrity/authentication.
    /// Drop silently — never echo or respond to off-path traffic (audit
    /// finding C9: prevents the server from being used as an
    /// amplification / reflection vector).
    Drop(&'static str),
}

/// Authenticated STUN server — handles incoming Binding Requests for ICE
/// connectivity checks.
///
/// Audit finding C9: a naive STUN responder that echoes XOR-MAPPED-ADDRESS
/// for every incoming Binding Request turns the endpoint into a reflector
/// with response > request amplification. This server type requires the
/// incoming request to carry:
///   * a correct `USERNAME` in the `remote_ufrag:local_ufrag` form,
///   * a valid `MESSAGE-INTEGRITY` HMAC computed with the local ICE pwd,
///   * a correct `FINGERPRINT` (RFC 8489 §14.7).
///
/// Requests failing any of those checks are silently dropped. Responses
/// are only ever generated for requests that prove knowledge of the local
/// ICE credentials, so an off-path attacker cannot solicit a response.
pub struct StunServer {
    /// Local ufrag — the peer's USERNAME must end in `:<local_ufrag>`.
    local_ufrag: String,
    /// Local password — used to verify MESSAGE-INTEGRITY on inbound
    /// requests and to sign MESSAGE-INTEGRITY on outbound responses.
    local_pwd: Vec<u8>,
    /// Remote ufrag — the peer's USERNAME must start with `<remote_ufrag>:`.
    remote_ufrag: String,
}

impl StunServer {
    /// Create a new authenticated STUN server bound to a specific ICE
    /// credential pair. Both ufrags and the local password must come from
    /// the signaling channel (SDP `a=ice-ufrag` / `a=ice-pwd`).
    pub fn new(local_ufrag: String, local_pwd: Vec<u8>, remote_ufrag: String) -> Self {
        Self {
            local_ufrag,
            local_pwd,
            remote_ufrag,
        }
    }

    /// Inspect an incoming STUN packet and decide whether/how to respond.
    ///
    /// The caller should wire this up behind its inbound packet-dispatch
    /// demux (STUN detection: first two bits 0, magic cookie at offset 4).
    /// The request's `source` is used to populate XOR-MAPPED-ADDRESS on
    /// the response.
    pub fn handle_binding_request(&self, data: &[u8], source: SocketAddr) -> StunServerResponse {
        let request = match StunMessage::parse(data) {
            Ok(m) => m,
            Err(_) => return StunServerResponse::Drop("malformed STUN packet"),
        };

        if request.message_type != MessageType::BindingRequest {
            return StunServerResponse::Drop("not a Binding Request");
        }

        // USERNAME must be present and shaped `<remote_ufrag>:<local_ufrag>`.
        let username = request.attributes.iter().find_map(|a| match a {
            StunAttribute::Username(u) => Some(u.as_str()),
            _ => None,
        });
        let username = match username {
            Some(u) => u,
            None => return StunServerResponse::Drop("missing USERNAME"),
        };
        let mut parts = username.splitn(2, ':');
        let (req_remote, req_local) = match (parts.next(), parts.next()) {
            (Some(l), Some(r)) => (l, r),
            _ => return StunServerResponse::Drop("malformed USERNAME"),
        };
        if req_local != self.local_ufrag || req_remote != self.remote_ufrag {
            return StunServerResponse::Drop("USERNAME does not match ICE credentials");
        }

        // MESSAGE-INTEGRITY: constant-time verify with the local password.
        match request.verify_message_integrity(&self.local_pwd) {
            Ok(true) => {}
            _ => return StunServerResponse::Drop("invalid or missing MESSAGE-INTEGRITY"),
        }

        // Build the Binding Success Response mirroring the transaction ID.
        let mut response = StunMessage {
            message_type: MessageType::BindingResponse,
            transaction_id: request.transaction_id,
            attributes: Vec::new(),
        };
        response
            .attributes
            .push(StunAttribute::XorMappedAddress(source));
        // Sign + fingerprint the response so the peer can verify it.
        if response.add_message_integrity(&self.local_pwd).is_err() {
            return StunServerResponse::Drop("failed to sign response");
        }
        response.add_fingerprint();
        StunServerResponse::Respond(response)
    }
}

/// STUN client for performing Binding requests
pub struct StunClient {
    /// UDP socket for STUN communication
    socket: UdpSocket,
    /// Request timeout
    timeout: Duration,
}

/// Options for STUN Binding requests
pub struct BindingRequestConfig<'a> {
    pub username: Option<String>,
    pub key: Option<&'a [u8]>,
    pub priority: Option<u32>,
    pub ice_controlling: Option<u64>,
    pub ice_controlled: Option<u64>,
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

    /// Create a STUN client with SO_REUSEADDR/PORT set to allow sharing the port
    pub async fn new_with_reuse(local_addr: SocketAddr) -> Result<Self> {
        let domain = match local_addr {
            SocketAddr::V4(_) => Domain::IPV4,
            SocketAddr::V6(_) => Domain::IPV6,
        };

        let socket = Socket::new(domain, Type::DGRAM, Some(SocketProtocol::UDP))
            .map_err(|e| ForgeError::Ice(format!("Failed to create STUN socket: {}", e)))?;

        socket
            .set_reuse_address(true)
            .map_err(|e| ForgeError::Ice(format!("Failed to set SO_REUSEADDR: {}", e)))?;

        #[cfg(any(target_os = "android", target_os = "linux", target_os = "fuchsia"))]
        socket
            .set_reuse_port(true)
            .map_err(|e| ForgeError::Ice(format!("Failed to set SO_REUSEPORT: {}", e)))?;

        socket
            .bind(&local_addr.into())
            .map_err(|e| ForgeError::Ice(format!("Failed to bind STUN socket: {}", e)))?;

        let std_socket: std::net::UdpSocket = socket.into();
        std_socket
            .set_nonblocking(true)
            .map_err(|e| ForgeError::Ice(format!("Failed to configure STUN socket: {}", e)))?;

        let udp_socket = UdpSocket::from_std(std_socket)
            .map_err(|e| ForgeError::Ice(format!("Failed to create async STUN socket: {}", e)))?;

        Ok(Self {
            socket: udp_socket,
            timeout: Duration::from_secs(3),
        })
    }

    /// Perform a STUN Binding Request to discover the mapped address
    pub async fn binding_request(
        &self,
        server: SocketAddr,
        config: Option<BindingRequestConfig<'_>>,
    ) -> Result<SocketAddr> {
        // Create Binding Request
        let mut request = StunMessage::new_binding_request();

        if let Some(cfg) = &config {
            if let Some(username) = &cfg.username {
                request.add_username(username.clone());
            }
            if let Some(priority) = cfg.priority {
                request.add_priority(priority);
            }
            if let Some(tie_breaker) = cfg.ice_controlling {
                request.add_ice_controlling(tie_breaker);
            }
            if let Some(tie_breaker) = cfg.ice_controlled {
                request.add_ice_controlled(tie_breaker);
            }
            if let Some(key) = cfg.key {
                request.add_message_integrity(key)?;
            }
        }

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
        let deadline = Instant::now() + self.timeout;

        let (len, _from) = loop {
            let now = Instant::now();
            if now >= deadline {
                break Err(ForgeError::Ice("STUN request timeout".to_string()));
            }

            let remaining = deadline.saturating_duration_since(now);
            match timeout(remaining, self.socket.recv_from(&mut buf)).await {
                Ok(Ok((len, from))) => {
                    if from != server {
                        warn!(
                            "Discarding STUN response from unexpected source {} (expected {})",
                            from, server
                        );
                        continue;
                    }
                    break Ok((len, from));
                }
                Ok(Err(e)) => {
                    break Err(ForgeError::Ice(format!(
                        "Failed to receive STUN response: {}",
                        e
                    )));
                }
                Err(_) => {
                    break Err(ForgeError::Ice("STUN request timeout".to_string()));
                }
            }
        }?;
        let response_bytes = &buf[..len];

        // Parse response
        let response = StunMessage::parse(response_bytes)?;

        // Verify transaction ID matches
        if response.transaction_id != transaction_id {
            return Err(ForgeError::Ice("STUN transaction ID mismatch".to_string()));
        }

        // Check message type
        if response.message_type != MessageType::BindingResponse {
            warn!(
                "Received non-success STUN response: {:?}",
                response.message_type
            );
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

    #[test]
    fn test_message_integrity_roundtrip() {
        let key = b"test_password_key";
        let mut msg = StunMessage::new_binding_request();

        // Add some attributes before MESSAGE-INTEGRITY
        msg.add_username("testuser:testpeer".to_string());
        msg.add_priority(12345);

        // Add MESSAGE-INTEGRITY
        msg.add_message_integrity(key).unwrap();

        // Add FINGERPRINT after MESSAGE-INTEGRITY
        msg.add_fingerprint();

        // Serialize and parse
        let bytes = msg.serialize();
        let parsed = StunMessage::parse(&bytes).unwrap();

        // Verify MESSAGE-INTEGRITY
        let result = parsed.verify_message_integrity(key);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);

        // Test with wrong key
        let wrong_key = b"wrong_password_key";
        let result = parsed.verify_message_integrity(wrong_key);
        assert!(result.is_err());

        // Test message without MESSAGE-INTEGRITY
        let mut msg_no_mi = StunMessage::new_binding_request();
        msg_no_mi.add_fingerprint();
        let result = msg_no_mi.verify_message_integrity(key);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false); // Returns false when MI is missing
    }

    #[test]
    fn test_ice_attributes() {
        let mut msg = StunMessage::new_binding_request();

        // Add ICE attributes
        msg.add_priority(2130706431);
        msg.add_ice_controlling(0x123456789ABCDEF0);

        let bytes = msg.serialize();
        let parsed = StunMessage::parse(&bytes).unwrap();

        // Verify attributes were parsed correctly
        assert!(parsed
            .attributes
            .iter()
            .any(|attr| matches!(attr, StunAttribute::Priority(2130706431))));

        assert!(parsed
            .attributes
            .iter()
            .any(|attr| matches!(attr, StunAttribute::IceControlling(0x123456789ABCDEF0))));
    }

    // C8 regression: transaction IDs must come from a CSPRNG and must not
    // collide across successive builds in any practical sense.
    #[test]
    fn test_transaction_ids_are_unique_and_nonzero() {
        use std::collections::HashSet;
        let mut seen: HashSet<[u8; 12]> = HashSet::new();
        for _ in 0..1024 {
            let m = StunMessage::new_binding_request();
            // Astronomically unlikely to be all-zero with a real CSPRNG.
            assert!(m.transaction_id.iter().any(|b| *b != 0));
            assert!(
                seen.insert(m.transaction_id),
                "collision in 1024 transaction IDs — CSPRNG not in use?"
            );
        }
    }

    // C10 regression: MESSAGE-INTEGRITY verification must be constant-time.
    // We can't test timing directly here, but we *can* verify the behaviour
    // is correct for a byte-flipped HMAC (which would fail
    // short-circuit-compare earlier for leading-byte flips). Mostly a
    // behavioural smoke test that we didn't break anything switching to
    // `verify_slice`.
    #[test]
    fn test_message_integrity_rejects_per_byte_flip() {
        let key = b"test-password";
        for flip_byte in 0..20 {
            let mut msg = StunMessage::new_binding_request();
            msg.add_username("u:v".into());
            msg.add_message_integrity(key).unwrap();
            // Flip one byte of the HMAC.
            if let Some(StunAttribute::MessageIntegrity(mi)) = msg
                .attributes
                .iter_mut()
                .find(|a| matches!(a, StunAttribute::MessageIntegrity(_)))
            {
                mi[flip_byte] ^= 0x01;
            }
            let bytes = msg.serialize();
            let parsed = StunMessage::parse(&bytes).unwrap();
            assert!(
                parsed.verify_message_integrity(key).is_err(),
                "byte-{} flip was not detected",
                flip_byte
            );
        }
    }

    // C9 regression: StunServer must reject requests lacking/wrong
    // authentication instead of emitting a response (otherwise we're an
    // open reflector).
    #[test]
    fn test_stun_server_rejects_unauthenticated() {
        let server = StunServer::new(
            "localuser".into(),
            b"localpwd".to_vec(),
            "remoteuser".into(),
        );
        let src = "192.0.2.5:1234".parse::<SocketAddr>().unwrap();

        // (a) No MESSAGE-INTEGRITY, no USERNAME.
        let mut m = StunMessage::new_binding_request();
        m.add_fingerprint();
        match server.handle_binding_request(&m.serialize(), src) {
            StunServerResponse::Drop(_) => {}
            StunServerResponse::Respond(_) => panic!("must not respond to unauthenticated req"),
        }

        // (b) Correct USERNAME but wrong MESSAGE-INTEGRITY key.
        let mut m = StunMessage::new_binding_request();
        m.add_username("remoteuser:localuser".into());
        m.add_message_integrity(b"not-the-right-pwd").unwrap();
        m.add_fingerprint();
        match server.handle_binding_request(&m.serialize(), src) {
            StunServerResponse::Drop(_) => {}
            StunServerResponse::Respond(_) => panic!("must not respond to bad HMAC"),
        }

        // (c) Correct MESSAGE-INTEGRITY but wrong ufrag pair.
        let mut m = StunMessage::new_binding_request();
        m.add_username("remoteuser:someotherlocal".into());
        m.add_message_integrity(b"localpwd").unwrap();
        m.add_fingerprint();
        match server.handle_binding_request(&m.serialize(), src) {
            StunServerResponse::Drop(_) => {}
            StunServerResponse::Respond(_) => panic!("must not respond to mismatched USERNAME"),
        }

        // (d) Not a Binding Request (success response injected).
        let mut bogus = StunMessage::new_binding_request();
        bogus.message_type = MessageType::BindingResponse;
        bogus.add_username("remoteuser:localuser".into());
        bogus.add_message_integrity(b"localpwd").unwrap();
        bogus.add_fingerprint();
        match server.handle_binding_request(&bogus.serialize(), src) {
            StunServerResponse::Drop(_) => {}
            StunServerResponse::Respond(_) => panic!("must not respond to non-request"),
        }
    }

    #[test]
    fn test_stun_server_accepts_authenticated() {
        let server = StunServer::new(
            "localuser".into(),
            b"localpwd".to_vec(),
            "remoteuser".into(),
        );
        let src = "192.0.2.5:1234".parse::<SocketAddr>().unwrap();

        let mut m = StunMessage::new_binding_request();
        m.add_username("remoteuser:localuser".into());
        m.add_message_integrity(b"localpwd").unwrap();
        m.add_fingerprint();

        match server.handle_binding_request(&m.serialize(), src) {
            StunServerResponse::Respond(resp) => {
                assert_eq!(resp.message_type, MessageType::BindingResponse);
                assert_eq!(resp.transaction_id, m.transaction_id);
                // Response itself must validate MESSAGE-INTEGRITY so the peer
                // can verify it.
                assert!(resp.verify_message_integrity(b"localpwd").unwrap());
                // And XOR-MAPPED-ADDRESS must echo the peer's source.
                assert_eq!(resp.get_xor_mapped_address(), Some(src));
            }
            StunServerResponse::Drop(reason) => panic!("server dropped valid req: {}", reason),
        }
    }

    #[test]
    fn test_get_local_preference() {
        use crate::candidate::{CandidateType, IceCandidate, Protocol};
        use std::net::{IpAddr, Ipv4Addr};

        // Create candidate with known local preference
        let candidate = IceCandidate::new_host(
            "1".to_string(),
            1,
            Protocol::Udp,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            50000,
            33768, // Known local preference
        );

        // Extract local preference
        let extracted_pref = candidate.get_local_preference();
        assert_eq!(extracted_pref, 33768);

        // Verify priority calculation includes the preference
        let priority = IceCandidate::compute_priority(CandidateType::Host, 33768, 1);
        assert_eq!((priority >> 8) & 0xFFFF, 33768);
    }
}
