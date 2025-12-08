//! RTCP packet handling (RFC 3550)
//!
//! This module implements RTCP (RTP Control Protocol) packets for monitoring
//! RTP data delivery and providing minimal control functionality.

use bytes::{Buf, BufMut, BytesMut};
use forge_core::{ForgeError, Result};

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
            _ => Err(ForgeError::Rtcp(format!("Unknown RTCP packet type: {}", value))),
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
            return Err(ForgeError::Rtcp(format!("Invalid RTCP version: {}", version)));
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
            report_blocks.push(ReceptionReportBlock::parse(&data[cursor.position() as usize..])?);
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

    /// Parse RR from bytes
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(ForgeError::Rtcp("RR packet too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);
        let ssrc = cursor.get_u32();

        let mut report_blocks = Vec::new();
        while cursor.remaining() >= 24 {
            report_blocks.push(ReceptionReportBlock::parse(&data[cursor.position() as usize..])?);
            cursor.set_position(cursor.position() + 24);
        }

        Ok(Self { ssrc, report_blocks })
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

impl SourceDescription {
    /// Create new SDES packet
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Add a chunk
    pub fn add_chunk(&mut self, ssrc: u32, items: Vec<SdesItem>) {
        self.chunks.push(SdesChunk { ssrc, items });
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
                // TODO: Implement SDES parsing
                Err(ForgeError::Rtcp("SDES parsing not yet implemented".to_string()))
            }
            RtcpPacketType::BYE => {
                // TODO: Implement BYE parsing
                Err(ForgeError::Rtcp("BYE parsing not yet implemented".to_string()))
            }
            RtcpPacketType::APP => {
                Err(ForgeError::Rtcp("APP packets not supported".to_string()))
            }
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
            RtcpPacket::SourceDescription(_sdes) => {
                // TODO: Implement SDES serialization
                vec![]
            }
            RtcpPacket::Bye(_bye) => {
                // TODO: Implement BYE serialization
                vec![]
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
