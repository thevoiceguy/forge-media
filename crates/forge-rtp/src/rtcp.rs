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
        ntp_secs.saturating_sub(NTP_UNIX_OFFSET)
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

    /// Parse SR from bytes.
    ///
    /// `report_count` is the `RC` field from the RTCP common header
    /// (RFC 3550 §6.4.1) and is the *only* authoritative source for
    /// how many reception report blocks follow the sender info.
    /// Trailing bytes beyond the declared blocks are ignored — they
    /// belong to whatever sub-packet follows in a compound RTCP
    /// packet (SR + SDES + ... — RFC 3550 §6.1 requires every
    /// transmitted RTCP packet to be compounded with at least one
    /// SDES, so this is the common case, not the exception).
    ///
    /// Caller-supplied `data` starts at the SR payload (the SSRC of
    /// the sender), i.e. immediately after the 4-byte RTCP common
    /// header.
    pub fn parse(data: &[u8], report_count: u8) -> Result<Self> {
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

        // Parse exactly `report_count` reception report blocks — no
        // more, no less. Previous behaviour greedily consumed 24-byte
        // chunks until the buffer ran out, which silently treated
        // trailing SDES bytes as bogus RR blocks for every Twilio /
        // FreeSWITCH / Asterisk peer (i.e. nearly everyone, since RFC
        // 3550 §6.1 mandates compound RTCP). The wrong bytes landed
        // in `jitter` / `cumulative_lost` / `last_sr` and corrupted
        // every downstream QoS metric and event.
        let mut report_blocks = Vec::with_capacity(report_count as usize);
        for _ in 0..report_count {
            if cursor.remaining() < 24 {
                return Err(ForgeError::Rtcp(format!(
                    "SR declares RC={report_count} blocks but only {} bytes remain",
                    cursor.remaining()
                )));
            }
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

    /// Parse RR from bytes.
    ///
    /// `report_count` is the `RC` field from the RTCP common header
    /// (RFC 3550 §6.4.2) and bounds the number of reception report
    /// blocks consumed. Trailing bytes beyond the declared blocks
    /// belong to the next sub-packet in a compound RTCP packet and
    /// MUST NOT be parsed as RR blocks — see [`SenderReport::parse`]
    /// for the longer rationale.
    ///
    /// Caller-supplied `data` starts at the RR payload (the SSRC of
    /// the packet sender), i.e. immediately after the 4-byte RTCP
    /// common header.
    pub fn parse(data: &[u8], report_count: u8) -> Result<Self> {
        if data.len() < 4 {
            return Err(ForgeError::Rtcp("RR packet too short".to_string()));
        }

        let mut cursor = std::io::Cursor::new(data);
        let ssrc = cursor.get_u32();

        let mut report_blocks = Vec::with_capacity(report_count as usize);
        for _ in 0..report_count {
            if cursor.remaining() < 24 {
                return Err(ForgeError::Rtcp(format!(
                    "RR declares RC={report_count} blocks but only {} bytes remain",
                    cursor.remaining()
                )));
            }
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
    /// Cumulative number of packets lost (signed 24-bit)
    pub cumulative_lost: i32,
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
        let cumulative_lost_raw = {
            let b1 = cursor.get_u8() as u32;
            let b2 = cursor.get_u8() as u32;
            let b3 = cursor.get_u8() as u32;
            (b1 << 16) | (b2 << 8) | b3
        };
        let cumulative_lost = if (cumulative_lost_raw & 0x0080_0000) != 0 {
            (cumulative_lost_raw | 0xFF00_0000) as i32
        } else {
            cumulative_lost_raw as i32
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
        let cumulative_lost = self.cumulative_lost.clamp(-0x0080_0000, 0x007F_FFFF);
        let cumulative_lost_raw = (cumulative_lost as u32) & 0x00FF_FFFF;

        buf.put_u32(self.ssrc);
        buf.put_u8(self.fraction_lost);
        buf.put_u8((cumulative_lost_raw >> 16) as u8);
        buf.put_u8((cumulative_lost_raw >> 8) as u8);
        buf.put_u8(cumulative_lost_raw as u8);
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
#[derive(Debug, Clone, Default)]
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
        Self::default()
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
                let sr = SenderReport::parse(&data[4..], header.count)?;
                Ok(RtcpPacket::SenderReport(sr))
            }
            RtcpPacketType::RR => {
                let rr = ReceiverReport::parse(&data[4..], header.count)?;
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

    // ─── Compound-packet parsing (RFC 3550 §6.1) ─────────────────────
    //
    // Real SIP / WebRTC peers (Twilio, FreeSWITCH, Asterisk, every
    // WebRTC browser) NEVER send a bare SR or RR — every transmitted
    // RTCP packet MUST be compounded with at least one SDES per §6.1.
    // The parser must therefore stop after exactly `RC` reception
    // report blocks and ignore the trailing SDES (or BYE, or APP)
    // bytes that follow. The pre-fix code greedily consumed 24-byte
    // chunks until the buffer ran out, treating SDES bytes as bogus
    // RR blocks — which corrupted `jitter`, `cumulative_lost`,
    // `last_sr`, etc. by reading the wrong bytes.

    /// Build the canonical 24-byte sender-info chunk (SSRC + NTP +
    /// RTP timestamp + packet/octet counts) used by SR test packets.
    fn sender_info_bytes() -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        buf.extend_from_slice(&0x12345678u32.to_be_bytes()); // SSRC of sender
        buf.extend_from_slice(&0xDEADBEEFu32.to_be_bytes()); // NTP msw
        buf.extend_from_slice(&0xCAFEBABEu32.to_be_bytes()); // NTP lsw
        buf.extend_from_slice(&0x00010000u32.to_be_bytes()); // RTP ts
        buf.extend_from_slice(&100u32.to_be_bytes()); // sender pkt count
        buf.extend_from_slice(&16000u32.to_be_bytes()); // sender octet count
        buf
    }

    /// Build a canonical 4-byte RTCP common header with the given RC,
    /// packet type, and length (in 32-bit words minus one).
    fn rtcp_header(rc: u8, pt: u8, length_words_minus_one: u16) -> Vec<u8> {
        let first = 0x80 | (rc & 0x1F); // V=2, P=0, RC
        let len = length_words_minus_one.to_be_bytes();
        vec![first, pt, len[0], len[1]]
    }

    /// Build a minimal SDES sub-packet with a single CNAME item for
    /// the given SSRC. CNAME content deliberately looks like ASCII
    /// (`"cname@host"`) so a buggy parser would extract a large
    /// integer from its bytes — making the regression test sensitive
    /// to exactly the pre-fix failure mode.
    fn sdes_subpacket() -> Vec<u8> {
        // SDES chunk: SSRC + CNAME item + null terminator + padding to
        // 32-bit boundary.
        let cname = b"alice@example.com"; // 17 bytes
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&0xABCD0001u32.to_be_bytes()); // chunk SSRC
        chunk.push(1); // SDES item type: CNAME
        chunk.push(cname.len() as u8);
        chunk.extend_from_slice(cname);
        chunk.push(0); // SDES item list terminator (type 0)
                       // Pad chunk to multiple of 4 bytes.
        while chunk.len() % 4 != 0 {
            chunk.push(0);
        }

        let length_words_minus_one = ((4 + chunk.len()) / 4 - 1) as u16;
        let mut buf = rtcp_header(
            1,   /* SC=1 chunk */
            202, /* SDES */
            length_words_minus_one,
        );
        buf.extend_from_slice(&chunk);
        buf
    }

    /// Build a single 24-byte reception report block with the given
    /// jitter value. All other fields use distinctive sentinel values
    /// so test assertions can distinguish "parsed from the right
    /// bytes" vs "happened to land on the right offset".
    fn report_block_bytes(jitter: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        buf.extend_from_slice(&0x11111111u32.to_be_bytes()); // SSRC_1
        buf.push(0x22); // fraction_lost
        buf.extend_from_slice(&[0x00, 0x00, 0x03]); // cumulative_lost = 3
        buf.extend_from_slice(&0x44444444u32.to_be_bytes()); // extended highest seq
        buf.extend_from_slice(&jitter.to_be_bytes());
        buf.extend_from_slice(&0x66666666u32.to_be_bytes()); // last_sr
        buf.extend_from_slice(&0x77777777u32.to_be_bytes()); // dlsr
        buf
    }

    #[test]
    fn sender_report_ignores_trailing_compound_bytes() {
        // Compound packet: SR (RC=0, no RR blocks) + SDES.
        // Pre-fix the SR parser ate the SDES bytes as a fake RR block
        // and produced garbage jitter/loss values. With RC=0 honoured,
        // we get zero report blocks.
        let mut packet = rtcp_header(0, 200, 6); // SR with no blocks, len = 6
        packet.extend_from_slice(&sender_info_bytes());
        let sdes_start = packet.len();
        packet.extend_from_slice(&sdes_subpacket());
        assert!(
            packet.len() - sdes_start >= 24,
            "test compound packet must have ≥24 trailing bytes to exercise the pre-fix greedy-consume bug",
        );

        let parsed = RtcpPacket::parse(&packet).expect("compound SR+SDES parses");
        match parsed {
            RtcpPacket::SenderReport(sr) => {
                assert_eq!(sr.ssrc, 0x12345678);
                assert!(
                    sr.report_blocks.is_empty(),
                    "RC=0 SR must have zero report blocks; got {} (trailing SDES misparsed)",
                    sr.report_blocks.len(),
                );
            }
            other => panic!("expected SenderReport, got {:?}", other),
        }
    }

    #[test]
    fn sender_report_parses_exactly_rc_blocks_then_stops() {
        // SR with RC=2 followed by a third 24-byte chunk that is
        // actually SDES bytes. The parser must take the first two
        // RR blocks (recovering the sentinel jitter values) and stop
        // — NOT slurp the SDES bytes as a phantom third block.
        let length_words = ((4 + 24 + 24 * 2 + sdes_subpacket().len()) / 4 - 1) as u16;
        // ^ deliberately wrong length to prove RC, not length, bounds the parse
        let mut packet = rtcp_header(2, 200, length_words);
        packet.extend_from_slice(&sender_info_bytes());
        packet.extend_from_slice(&report_block_bytes(0x0000_0080)); // jitter = 128
        packet.extend_from_slice(&report_block_bytes(0x0000_0140)); // jitter = 320
        packet.extend_from_slice(&sdes_subpacket());

        let parsed = RtcpPacket::parse(&packet).expect("compound SR (RC=2) + SDES parses");
        match parsed {
            RtcpPacket::SenderReport(sr) => {
                assert_eq!(sr.report_blocks.len(), 2, "must stop after RC=2 blocks");
                assert_eq!(sr.report_blocks[0].jitter, 128);
                assert_eq!(sr.report_blocks[1].jitter, 320);
                // Sanity: the sentinel SSRC proves we parsed the right bytes.
                for block in &sr.report_blocks {
                    assert_eq!(block.ssrc, 0x11111111);
                    assert_eq!(block.fraction_lost, 0x22);
                }
            }
            other => panic!("expected SenderReport, got {:?}", other),
        }
    }

    #[test]
    fn receiver_report_ignores_trailing_compound_bytes() {
        // RR with RC=0 + SDES. Same bug, same fix.
        let mut packet = rtcp_header(0, 201, 1); // RR with just SSRC, len = 1
        packet.extend_from_slice(&0x87654321u32.to_be_bytes());
        packet.extend_from_slice(&sdes_subpacket());

        let parsed = RtcpPacket::parse(&packet).expect("compound RR+SDES parses");
        match parsed {
            RtcpPacket::ReceiverReport(rr) => {
                assert_eq!(rr.ssrc, 0x87654321);
                assert!(
                    rr.report_blocks.is_empty(),
                    "RC=0 RR must have zero report blocks; got {} (trailing SDES misparsed)",
                    rr.report_blocks.len(),
                );
            }
            other => panic!("expected ReceiverReport, got {:?}", other),
        }
    }

    #[test]
    fn receiver_report_parses_exactly_rc_blocks_then_stops() {
        let mut packet = rtcp_header(1, 201, 7);
        packet.extend_from_slice(&0x87654321u32.to_be_bytes());
        packet.extend_from_slice(&report_block_bytes(0x0000_0040)); // jitter = 64
        packet.extend_from_slice(&sdes_subpacket());

        let parsed = RtcpPacket::parse(&packet).expect("compound RR (RC=1) + SDES parses");
        match parsed {
            RtcpPacket::ReceiverReport(rr) => {
                assert_eq!(rr.report_blocks.len(), 1, "must stop after RC=1 block");
                assert_eq!(rr.report_blocks[0].jitter, 64);
                assert_eq!(rr.report_blocks[0].ssrc, 0x11111111);
            }
            other => panic!("expected ReceiverReport, got {:?}", other),
        }
    }

    #[test]
    fn sender_report_rejects_rc_count_exceeding_payload() {
        // RC=5 declared, but only one block's worth of bytes provided.
        // Better to surface the malformed packet as an error than to
        // silently truncate and report fewer blocks than declared.
        let mut packet = rtcp_header(5, 200, 18);
        packet.extend_from_slice(&sender_info_bytes());
        packet.extend_from_slice(&report_block_bytes(0));

        let err = RtcpPacket::parse(&packet)
            .expect_err("RC=5 with only 1 block of trailing data must error, not truncate");
        let msg = format!("{err}");
        assert!(
            msg.contains("RC=5") && msg.contains("bytes remain"),
            "error should describe the RC vs available-bytes mismatch; got: {msg}",
        );
    }
}
