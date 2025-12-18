//! RTCP packet handling (RFC 3550)
//!
//! This module implements RTCP (RTP Control Protocol) packets for monitoring
//! RTP data delivery and providing minimal control functionality.

use bytes::{Buf, BufMut, BytesMut};
use forge_core::{ForgeError, Result};
use std::time::{SystemTime, UNIX_EPOCH};

/// NTP timestamp utilities
pub mod ntp {
    use super::*;

    /// Number of seconds between NTP epoch (Jan 1, 1900) and Unix epoch (Jan 1, 1970)
    const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

    /// Get current NTP timestamp as a 64-bit value
    pub fn now() -> u64 {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time before Unix epoch");

        let unix_secs = duration.as_secs();
        let nanos = duration.subsec_nanos();

        // Convert Unix time to NTP time
        let ntp_secs = unix_secs + NTP_UNIX_OFFSET;

        // Convert nanoseconds to NTP fractional seconds (2^32 units per second)
        let ntp_frac = ((nanos as u64) * (1u64 << 32)) / 1_000_000_000;

        (ntp_secs << 32) | ntp_frac
    }

    /// Get current NTP timestamp split into MSW and LSW
    pub fn now_split() -> (u32, u32) {
        let ntp = now();
        let msw = (ntp >> 32) as u32;
        let lsw = (ntp & 0xFFFFFFFF) as u32;
        (msw, lsw)
    }

    /// Convert NTP timestamp (MSW, LSW) to Unix timestamp in seconds
    pub fn to_unix_secs(ntp_msw: u32, _ntp_lsw: u32) -> u64 {
        let ntp_secs = ntp_msw as u64;

        // Convert from NTP epoch to Unix epoch
        if ntp_secs >= NTP_UNIX_OFFSET {
            ntp_secs - NTP_UNIX_OFFSET
        } else {
            0
        }
    }

    /// Convert NTP timestamp to fractional seconds (0.0 - 1.0)
    pub fn fractional_seconds(ntp_lsw: u32) -> f64 {
        (ntp_lsw as f64) / (1u64 << 32) as f64
    }
}

/// RTCP packet types as defined in RFC 3550
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RtcpPacketType {
    /// Sender Report (SR) - 200
    SR = 200,
    /// Receiver Report (RR) - 201
    RR = 201,
    /// Source Description (SDES) - 202
    SDES = 202,
    /// Goodbye (BYE) - 203
    BYE = 203,
    /// Application Defined (APP) - 204
    APP = 204,
}

impl TryFrom<u8> for RtcpPacketType {
    type Error = ForgeError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            200 => Ok(RtcpPacketType::SR),
            201 => Ok(RtcpPacketType::RR),
            202 => Ok(RtcpPacketType::SDES),
            203 => Ok(RtcpPacketType::BYE),
            204 => Ok(RtcpPacketType::APP),
            _ => Err(ForgeError::Rtcp(format!(
                "Unknown RTCP packet type: {}",
                value
            ))),
        }
    }
}

/// RTCP common header (RFC 3550 Section 6.1)
#[derive(Debug, Clone)]
pub struct RtcpHeader {
    /// Version (V): 2 bits, should be 2
    pub version: u8,
    /// Padding (P): 1 bit
    pub padding: bool,
    /// Reception report count (RC) or Source count (SC): 5 bits
    pub count: u8,
    /// Packet type (PT): 8 bits
    pub packet_type: RtcpPacketType,
    /// Length: 16 bits (in 32-bit words minus one)
    pub length: u16,
}

impl RtcpHeader {
    /// Parse RTCP header from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(ForgeError::Rtcp("RTCP header too short".to_string()));
        }

        let first_byte = data[0];
        let version = (first_byte >> 6) & 0x03;
        let padding = (first_byte & 0x20) != 0;
        let count = first_byte & 0x1F;

        let packet_type = RtcpPacketType::try_from(data[1])?;
        let length = u16::from_be_bytes([data[2], data[3]]);

        if version != 2 {
            return Err(ForgeError::Rtcp(format!(
                "Invalid RTCP version: {}",
                version
            )));
        }

        Ok(Self {
            version,
            padding,
            count,
            packet_type,
            length,
        })
    }

    /// Serialize RTCP header to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let first_byte = (self.version << 6) | (if self.padding { 0x20 } else { 0 }) | self.count;

        vec![
            first_byte,
            self.packet_type as u8,
            (self.length >> 8) as u8,
            (self.length & 0xFF) as u8,
        ]
    }
}

/// RTCP Sender Report (SR) packet (RFC 3550 Section 6.4.1)
#[derive(Debug, Clone)]
pub struct SenderReport {
    /// SSRC of sender
    pub ssrc: u32,
    /// NTP timestamp (most significant 32 bits)
    pub ntp_timestamp_msw: u32,
    /// NTP timestamp (least significant 32 bits)
    pub ntp_timestamp_lsw: u32,
    /// RTP timestamp
    pub rtp_timestamp: u32,
    /// Sender's packet count
    pub sender_packet_count: u32,
    /// Sender's octet count
    pub sender_octet_count: u32,
    /// Reception report blocks
    pub report_blocks: Vec<ReceptionReportBlock>,
}

impl SenderReport {
    /// Create a new sender report
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            ntp_timestamp_msw: 0,
            ntp_timestamp_lsw: 0,
            rtp_timestamp: 0,
            sender_packet_count: 0,
            sender_octet_count: 0,
            report_blocks: Vec::new(),
        }
    }

    /// Create a new sender report with current NTP timestamp
    pub fn with_current_time(
        ssrc: u32,
        rtp_timestamp: u32,
        packet_count: u32,
        octet_count: u32,
    ) -> Self {
        let (ntp_msw, ntp_lsw) = ntp::now_split();
        Self {
            ssrc,
            ntp_timestamp_msw: ntp_msw,
            ntp_timestamp_lsw: ntp_lsw,
            rtp_timestamp,
            sender_packet_count: packet_count,
            sender_octet_count: octet_count,
            report_blocks: Vec::new(),
        }
    }

    /// Update NTP timestamp to current time
    pub fn update_timestamp(&mut self) {
        let (ntp_msw, ntp_lsw) = ntp::now_split();
        self.ntp_timestamp_msw = ntp_msw;
        self.ntp_timestamp_lsw = ntp_lsw;
    }

    /// Add a reception report block
    pub fn add_report_block(&mut self, block: ReceptionReportBlock) {
        self.report_blocks.push(block);
    }

    /// Parse SR from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 24 {
            return Err(ForgeError::Rtcp("SR packet too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);

        let ssrc = cursor.get_u32();
        let ntp_timestamp_msw = cursor.get_u32();
        let ntp_timestamp_lsw = cursor.get_u32();
        let rtp_timestamp = cursor.get_u32();
        let sender_packet_count = cursor.get_u32();
        let sender_octet_count = cursor.get_u32();

        // Parse report blocks
        let mut report_blocks = Vec::new();
        while cursor.remaining() >= 24 {
            report_blocks.push(ReceptionReportBlock::parse(
                &data[cursor.position() as usize..],
            )?);
            cursor.set_position(cursor.position() + 24);
        }

        Ok(Self {
            ssrc,
            ntp_timestamp_msw,
            ntp_timestamp_lsw,
            rtp_timestamp,
            sender_packet_count,
            sender_octet_count,
            report_blocks,
        })
    }

    /// Serialize SR to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = BytesMut::with_capacity(28 + self.report_blocks.len() * 24);

        buf.put_u32(self.ssrc);
        buf.put_u32(self.ntp_timestamp_msw);
        buf.put_u32(self.ntp_timestamp_lsw);
        buf.put_u32(self.rtp_timestamp);
        buf.put_u32(self.sender_packet_count);
        buf.put_u32(self.sender_octet_count);

        for block in &self.report_blocks {
            buf.put_slice(&block.to_bytes());
        }

        buf.to_vec()
    }
}

/// RTCP Receiver Report (RR) packet (RFC 3550 Section 6.4.2)
#[derive(Debug, Clone)]
pub struct ReceiverReport {
    /// SSRC of receiver
    pub ssrc: u32,
    /// Reception report blocks
    pub report_blocks: Vec<ReceptionReportBlock>,
}

impl ReceiverReport {
    /// Create a new receiver report
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            report_blocks: Vec::new(),
        }
    }

    /// Add a reception report block
    pub fn add_report_block(&mut self, block: ReceptionReportBlock) {
        self.report_blocks.push(block);
    }

    /// Parse RR from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(ForgeError::Rtcp("RR packet too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);
        let ssrc = cursor.get_u32();

        let mut report_blocks = Vec::new();
        while cursor.remaining() >= 24 {
            report_blocks.push(ReceptionReportBlock::parse(
                &data[cursor.position() as usize..],
            )?);
            cursor.set_position(cursor.position() + 24);
        }

        Ok(Self {
            ssrc,
            report_blocks,
        })
    }

    /// Serialize RR to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = BytesMut::with_capacity(4 + self.report_blocks.len() * 24);

        buf.put_u32(self.ssrc);

        for block in &self.report_blocks {
            buf.put_slice(&block.to_bytes());
        }

        buf.to_vec()
    }
}

/// Reception Report Block (RFC 3550 Section 6.4.1)
#[derive(Debug, Clone)]
pub struct ReceptionReportBlock {
    /// SSRC of source
    pub ssrc: u32,
    /// Fraction lost
    pub fraction_lost: u8,
    /// Cumulative number of packets lost
    pub cumulative_lost: u32, // 24 bits
    /// Extended highest sequence number received
    pub extended_highest_seq: u32,
    /// Interarrival jitter
    pub jitter: u32,
    /// Last SR timestamp
    pub last_sr: u32,
    /// Delay since last SR
    pub delay_since_last_sr: u32,
}

impl ReceptionReportBlock {
    /// Create a new reception report block
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            fraction_lost: 0,
            cumulative_lost: 0,
            extended_highest_seq: 0,
            jitter: 0,
            last_sr: 0,
            delay_since_last_sr: 0,
        }
    }

    /// Parse report block from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 24 {
            return Err(ForgeError::Rtcp("Report block too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);

        let ssrc = cursor.get_u32();
        let fraction_lost = cursor.get_u8();
        // Read 24-bit cumulative_lost (3 bytes)
        let cumulative_lost = {
            let b1 = cursor.get_u8() as u32;
            let b2 = cursor.get_u8() as u32;
            let b3 = cursor.get_u8() as u32;
            (b1 << 16) | (b2 << 8) | b3
        };
        let extended_highest_seq = cursor.get_u32();
        let jitter = cursor.get_u32();
        let last_sr = cursor.get_u32();
        let delay_since_last_sr = cursor.get_u32();

        Ok(Self {
            ssrc,
            fraction_lost,
            cumulative_lost,
            extended_highest_seq,
            jitter,
            last_sr,
            delay_since_last_sr,
        })
    }

    /// Serialize report block to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = BytesMut::with_capacity(24);

        buf.put_u32(self.ssrc);
        buf.put_u8(self.fraction_lost);
        buf.put_uint(self.cumulative_lost as u64, 3);
        buf.put_u32(self.extended_highest_seq);
        buf.put_u32(self.jitter);
        buf.put_u32(self.last_sr);
        buf.put_u32(self.delay_since_last_sr);

        buf.to_vec()
    }
}

/// RTCP Source Description (SDES) item
#[derive(Debug, Clone)]
pub struct SdesItem {
    /// Item type
    pub item_type: u8,
    /// Item text
    pub text: String,
}

impl SdesItem {
    /// Create a new SDES item
    pub fn new(item_type: u8, text: String) -> Self {
        Self { item_type, text }
    }

    /// Parse SDES item from bytes
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        if data.is_empty() {
            return Err(ForgeError::Rtcp("SDES item too short".to_string()));
        }

        let item_type = data[0];

        // END item has no length or text
        if item_type == sdes_type::END {
            return Ok((
                Self {
                    item_type,
                    text: String::new(),
                },
                1,
            ));
        }

        if data.len() < 2 {
            return Err(ForgeError::Rtcp("SDES item missing length".to_string()));
        }

        let length = data[1] as usize;

        if data.len() < 2 + length {
            return Err(ForgeError::Rtcp("SDES item text too short".to_string()));
        }

        let text = String::from_utf8_lossy(&data[2..2 + length]).to_string();

        Ok((Self { item_type, text }, 2 + length))
    }

    /// Serialize SDES item to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.item_type == sdes_type::END {
            return vec![0];
        }

        let text_bytes = self.text.as_bytes();
        let length = text_bytes.len().min(255);

        let mut buf = Vec::with_capacity(2 + length);
        buf.push(self.item_type);
        buf.push(length as u8);
        buf.extend_from_slice(&text_bytes[..length]);

        buf
    }
}

/// SDES item types
pub mod sdes_type {
    pub const END: u8 = 0;
    pub const CNAME: u8 = 1;
    pub const NAME: u8 = 2;
    pub const EMAIL: u8 = 3;
    pub const PHONE: u8 = 4;
    pub const LOC: u8 = 5;
    pub const TOOL: u8 = 6;
    pub const NOTE: u8 = 7;
    pub const PRIV: u8 = 8;
}

/// RTCP Source Description (SDES) packet (RFC 3550 Section 6.5)
#[derive(Debug, Clone)]
pub struct SourceDescription {
    /// Chunks (one per SSRC)
    pub chunks: Vec<SdesChunk>,
}

/// SDES chunk for a single SSRC
#[derive(Debug, Clone)]
pub struct SdesChunk {
    /// SSRC/CSRC
    pub ssrc: u32,
    /// SDES items
    pub items: Vec<SdesItem>,
}

impl SdesChunk {
    /// Create a new SDES chunk
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            items: Vec::new(),
        }
    }

    /// Add an item to this chunk
    pub fn add_item(&mut self, item: SdesItem) {
        self.items.push(item);
    }

    /// Parse SDES chunk from bytes
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        if data.len() < 4 {
            return Err(ForgeError::Rtcp("SDES chunk too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);
        let ssrc = cursor.get_u32();

        let mut items = Vec::new();
        let mut offset = 4;

        // Parse items until END or end of data
        loop {
            if offset >= data.len() {
                break;
            }

            let (item, item_size) = SdesItem::parse(&data[offset..])?;
            offset += item_size;

            if item.item_type == sdes_type::END {
                items.push(item);
                break;
            }

            items.push(item);
        }

        // Round up to 32-bit boundary
        let padded_offset = (offset + 3) & !3;

        Ok((Self { ssrc, items }, padded_offset))
    }

    /// Serialize SDES chunk to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = BytesMut::new();
        buf.put_u32(self.ssrc);

        for item in &self.items {
            buf.put_slice(&item.to_bytes());
        }

        // Add END item if not present
        if self.items.is_empty() || self.items.last().unwrap().item_type != sdes_type::END {
            buf.put_u8(sdes_type::END);
        }

        // Pad to 32-bit boundary
        while buf.len() % 4 != 0 {
            buf.put_u8(0);
        }

        buf.to_vec()
    }
}

impl SourceDescription {
    /// Create new SDES packet
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Add a chunk
    pub fn add_chunk(&mut self, ssrc: u32, items: Vec<SdesItem>) {
        self.chunks.push(SdesChunk { ssrc, items });
    }

    /// Parse SDES packet from bytes
    pub fn parse(data: &[u8], chunk_count: u8) -> Result<Self> {
        let mut chunks = Vec::new();
        let mut offset = 0;

        for _ in 0..chunk_count {
            if offset >= data.len() {
                break;
            }

            let (chunk, chunk_size) = SdesChunk::parse(&data[offset..])?;
            chunks.push(chunk);
            offset += chunk_size;
        }

        Ok(Self { chunks })
    }

    /// Serialize SDES packet to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        for chunk in &self.chunks {
            buf.extend_from_slice(&chunk.to_bytes());
        }

        buf
    }
}

/// RTCP BYE packet (RFC 3550 Section 6.6)
#[derive(Debug, Clone)]
pub struct Bye {
    /// SSRCs leaving
    pub ssrcs: Vec<u32>,
    /// Optional reason for leaving
    pub reason: Option<String>,
}

impl Bye {
    /// Create new BYE packet
    pub fn new(ssrcs: Vec<u32>) -> Self {
        Self {
            ssrcs,
            reason: None,
        }
    }

    /// Create BYE packet with reason
    pub fn with_reason(ssrcs: Vec<u32>, reason: String) -> Self {
        Self {
            ssrcs,
            reason: Some(reason),
        }
    }

    /// Parse BYE packet from bytes
    pub fn parse(data: &[u8], source_count: u8) -> Result<Self> {
        if data.len() < (source_count as usize * 4) {
            return Err(ForgeError::Rtcp("BYE packet too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);
        let mut ssrcs = Vec::new();

        // Parse SSRCs
        for _ in 0..source_count {
            ssrcs.push(cursor.get_u32());
        }

        // Parse optional reason
        let reason = if cursor.remaining() > 0 {
            let length = cursor.get_u8() as usize;

            if cursor.remaining() < length {
                return Err(ForgeError::Rtcp("BYE reason too short".to_string()));
            }

            let mut reason_bytes = vec![0u8; length];
            cursor.copy_to_slice(&mut reason_bytes);

            Some(String::from_utf8_lossy(&reason_bytes).to_string())
        } else {
            None
        };

        Ok(Self { ssrcs, reason })
    }

    /// Serialize BYE packet to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = BytesMut::new();

        // Write SSRCs
        for ssrc in &self.ssrcs {
            buf.put_u32(*ssrc);
        }

        // Write optional reason
        if let Some(reason) = &self.reason {
            let reason_bytes = reason.as_bytes();
            let length = reason_bytes.len().min(255);

            buf.put_u8(length as u8);
            buf.put_slice(&reason_bytes[..length]);

            // Pad to 32-bit boundary
            while buf.len() % 4 != 0 {
                buf.put_u8(0);
            }
        }

        buf.to_vec()
    }
}

/// Compound RTCP packet
#[derive(Debug, Clone)]
pub enum RtcpPacket {
    /// Sender Report
    SenderReport(SenderReport),
    /// Receiver Report
    ReceiverReport(ReceiverReport),
    /// Source Description
    SourceDescription(SourceDescription),
    /// Goodbye
    Bye(Bye),
}

impl RtcpPacket {
    /// Parse RTCP packet from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        let header = RtcpHeader::parse(data)?;

        match header.packet_type {
            RtcpPacketType::SR => {
                let sr = SenderReport::parse(&data[4..])?;
                Ok(RtcpPacket::SenderReport(sr))
            }
            RtcpPacketType::RR => {
                let rr = ReceiverReport::parse(&data[4..])?;
                Ok(RtcpPacket::ReceiverReport(rr))
            }
            RtcpPacketType::SDES => {
                let sdes = SourceDescription::parse(&data[4..], header.count)?;
                Ok(RtcpPacket::SourceDescription(sdes))
            }
            RtcpPacketType::BYE => {
                let bye = Bye::parse(&data[4..], header.count)?;
                Ok(RtcpPacket::Bye(bye))
            }
            RtcpPacketType::APP => Err(ForgeError::Rtcp("APP packets not supported".to_string())),
        }
    }

    /// Serialize RTCP packet to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            RtcpPacket::SenderReport(sr) => {
                let header = RtcpHeader {
                    version: 2,
                    padding: false,
                    count: sr.report_blocks.len() as u8,
                    packet_type: RtcpPacketType::SR,
                    length: ((28 + sr.report_blocks.len() * 24) / 4 - 1) as u16,
                };

                let mut buf = header.to_bytes();
                buf.extend_from_slice(&sr.to_bytes());
                buf
            }
            RtcpPacket::ReceiverReport(rr) => {
                let header = RtcpHeader {
                    version: 2,
                    padding: false,
                    count: rr.report_blocks.len() as u8,
                    packet_type: RtcpPacketType::RR,
                    length: ((4 + rr.report_blocks.len() * 24) / 4 - 1) as u16,
                };

                let mut buf = header.to_bytes();
                buf.extend_from_slice(&rr.to_bytes());
                buf
            }
            RtcpPacket::SourceDescription(sdes) => {
                let payload = sdes.to_bytes();
                let header = RtcpHeader {
                    version: 2,
                    padding: false,
                    count: sdes.chunks.len() as u8,
                    packet_type: RtcpPacketType::SDES,
                    length: ((payload.len() / 4) as u16).saturating_sub(1),
                };

                let mut buf = header.to_bytes();
                buf.extend_from_slice(&payload);
                buf
            }
            RtcpPacket::Bye(bye) => {
                let payload = bye.to_bytes();
                let header = RtcpHeader {
                    version: 2,
                    padding: false,
                    count: bye.ssrcs.len() as u8,
                    packet_type: RtcpPacketType::BYE,
                    length: ((payload.len() / 4) as u16).saturating_sub(1),
                };

                let mut buf = header.to_bytes();
                buf.extend_from_slice(&payload);
                buf
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtcp_header_parse() {
        // V=2, P=0, RC=0, PT=200 (SR), length=6
        let data = vec![0x80, 200, 0, 6];
        let header = RtcpHeader::parse(&data).unwrap();

        assert_eq!(header.version, 2);
        assert!(!header.padding);
        assert_eq!(header.count, 0);
        assert_eq!(header.packet_type, RtcpPacketType::SR);
        assert_eq!(header.length, 6);
    }

    #[test]
    fn test_sender_report_new() {
        let sr = SenderReport::new(0x12345678);
        assert_eq!(sr.ssrc, 0x12345678);
        assert_eq!(sr.report_blocks.len(), 0);
    }

    #[test]
    fn test_receiver_report_new() {
        let rr = ReceiverReport::new(0x87654321);
        assert_eq!(rr.ssrc, 0x87654321);
        assert_eq!(rr.report_blocks.len(), 0);
    }
}
