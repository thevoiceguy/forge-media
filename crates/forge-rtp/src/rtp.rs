//! RTP packet handling

use bytes::{Buf, BufMut, Bytes, BytesMut};
use forge_core::ForgeError;

/// RTP packet header (12 bytes minimum)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct RtpHeader {
    pub version_flags: u8,       // V(2), P(1), X(1), CC(4)
    pub marker_payload_type: u8, // M(1), PT(7)
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl RtpHeader {
    pub const SIZE: usize = 12;

    /// RTP version (should be 2)
    pub fn version(&self) -> u8 {
        (self.version_flags >> 6) & 0x03
    }

    /// Padding bit
    pub fn padding(&self) -> bool {
        (self.version_flags & 0x20) != 0
    }

    /// Extension bit
    pub fn extension(&self) -> bool {
        (self.version_flags & 0x10) != 0
    }

    /// CSRC count
    pub fn csrc_count(&self) -> u8 {
        self.version_flags & 0x0F
    }

    /// Marker bit
    pub fn marker(&self) -> bool {
        (self.marker_payload_type & 0x80) != 0
    }

    /// Payload type
    pub fn payload_type(&self) -> u8 {
        self.marker_payload_type & 0x7F
    }

    /// Parse RTP header from bytes
    pub fn parse(data: &[u8]) -> Result<Self, ForgeError> {
        if data.len() < Self::SIZE {
            return Err(ForgeError::Rtp("Packet too short".into()));
        }

        let mut buf = &data[..Self::SIZE];
        Ok(Self {
            version_flags: buf.get_u8(),
            marker_payload_type: buf.get_u8(),
            sequence_number: buf.get_u16(),
            timestamp: buf.get_u32(),
            ssrc: buf.get_u32(),
        })
    }

    /// Write RTP header to buffer
    pub fn write(&self, buf: &mut BytesMut) {
        buf.put_u8(self.version_flags);
        buf.put_u8(self.marker_payload_type);
        buf.put_u16(self.sequence_number);
        buf.put_u32(self.timestamp);
        buf.put_u32(self.ssrc);
    }
}

/// Complete RTP packet
#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub header: RtpHeader,
    pub csrc_list: Vec<u32>,
    pub extension: Option<RtpExtension>,
    pub payload: Bytes,
    pub padding_len: u8,
}

impl RtpPacket {
    /// Parse an RTP packet from bytes
    pub fn parse(data: Bytes) -> Result<Self, ForgeError> {
        if data.len() < RtpHeader::SIZE {
            return Err(ForgeError::Rtp("Packet too short".into()));
        }

        let header = RtpHeader::parse(&data)?;

        if header.version() != 2 {
            return Err(ForgeError::Rtp(format!(
                "Invalid RTP version: {}",
                header.version()
            )));
        }

        let mut offset = RtpHeader::SIZE;

        // Parse CSRC list
        let csrc_count = header.csrc_count() as usize;
        let mut csrc_list = Vec::with_capacity(csrc_count);
        for _ in 0..csrc_count {
            if offset + 4 > data.len() {
                return Err(ForgeError::Rtp("Invalid CSRC list".into()));
            }
            let csrc = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            csrc_list.push(csrc);
            offset += 4;
        }

        // Parse extension if present
        let extension = if header.extension() {
            let ext = RtpExtension::parse(&data[offset..])?;
            offset += ext.total_size();
            Some(ext)
        } else {
            None
        };

        // Determine padding length
        let padding_len = if header.padding() {
            if data.is_empty() {
                return Err(ForgeError::Rtp("Missing padding length".into()));
            }
            let len = data[data.len() - 1] as usize;
            if len == 0 {
                return Err(ForgeError::Rtp("Invalid padding length".into()));
            }
            let payload_end = data
                .len()
                .checked_sub(len)
                .ok_or_else(|| ForgeError::Rtp("Invalid padding length".into()))?;
            if payload_end < offset {
                return Err(ForgeError::Rtp("Invalid padding length".into()));
            }
            len as u8
        } else {
            0
        };

        // Extract payload
        let payload_end = data
            .len()
            .checked_sub(padding_len as usize)
            .ok_or_else(|| ForgeError::Rtp("Invalid padding length".into()))?;
        if offset > payload_end {
            return Err(ForgeError::Rtp("Invalid payload length".into()));
        }
        let payload = data.slice(offset..payload_end);

        Ok(Self {
            header,
            csrc_list,
            extension,
            payload,
            padding_len,
        })
    }

    /// Build an RTP packet
    pub fn build(
        payload_type: u8,
        sequence_number: u16,
        timestamp: u32,
        ssrc: u32,
        payload: Bytes,
        marker: bool,
    ) -> Self {
        let header = RtpHeader {
            version_flags: 0x80, // Version 2, no padding, no extension, no CSRC
            marker_payload_type: if marker {
                0x80 | payload_type
            } else {
                payload_type
            },
            sequence_number,
            timestamp,
            ssrc,
        };

        Self {
            header,
            csrc_list: vec![],
            extension: None,
            payload,
            padding_len: 0,
        }
    }

    /// Serialize packet to bytes
    pub fn to_bytes(&self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(
            RtpHeader::SIZE
                + self.csrc_list.len() * 4
                + self.extension.as_ref().map_or(0, |e| e.total_size())
                + self.payload.len()
                + self.padding_len as usize,
        );

        self.header.write(&mut buf);

        for csrc in &self.csrc_list {
            buf.put_u32(*csrc);
        }

        if let Some(ext) = &self.extension {
            ext.write(&mut buf);
        }

        buf.put(self.payload.clone());

        if self.padding_len > 0 {
            let target_len = buf.len() + self.padding_len as usize;
            buf.resize(target_len, 0);
            let len = buf.len();
            buf[len - 1] = self.padding_len;
        }

        buf
    }
}

/// RTP header extension
#[derive(Debug, Clone)]
pub struct RtpExtension {
    pub profile: u16,
    pub data: Bytes,
}

impl RtpExtension {
    fn parse(data: &[u8]) -> Result<Self, ForgeError> {
        if data.len() < 4 {
            return Err(ForgeError::Rtp("Extension too short".into()));
        }

        let profile = u16::from_be_bytes([data[0], data[1]]);
        let length_words = u16::from_be_bytes([data[2], data[3]]) as usize;
        let length = length_words
            .checked_mul(4)
            .ok_or_else(|| ForgeError::Rtp("Extension length overflow".into()))?;

        let total = 4usize
            .checked_add(length)
            .ok_or_else(|| ForgeError::Rtp("Extension length overflow".into()))?;
        if data.len() < total {
            return Err(ForgeError::Rtp("Extension data truncated".into()));
        }

        Ok(Self {
            profile,
            data: Bytes::copy_from_slice(&data[4..total]),
        })
    }

    fn write(&self, buf: &mut BytesMut) {
        let padded_len = (self.data.len() + 3) & !3;
        buf.put_u16(self.profile);
        buf.put_u16((padded_len / 4) as u16);
        buf.put(self.data.clone());
        if padded_len > self.data.len() {
            buf.resize(buf.len() + (padded_len - self.data.len()), 0);
        }
    }

    fn total_size(&self) -> usize {
        4 + self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtp_header_parsing() {
        let data = vec![
            0x80, 0x00, // V=2, PT=0
            0x12, 0x34, // Sequence
            0x00, 0x00, 0x00, 0x01, // Timestamp
            0xAB, 0xCD, 0xEF, 0x12, // SSRC
        ];

        let header = RtpHeader::parse(&data).unwrap();
        assert_eq!(header.version(), 2);
        assert_eq!(header.payload_type(), 0);
        // Copy fields to avoid unaligned reference to packed struct
        let seq = header.sequence_number;
        let ts = header.timestamp;
        let ssrc = header.ssrc;
        assert_eq!(seq, 0x1234);
        assert_eq!(ts, 1);
        assert_eq!(ssrc, 0xABCDEF12);
    }

    // C1 regression: malicious extension length must not bypass bounds check
    // via arithmetic overflow. A length field of 0xFFFF means
    // 0xFFFF * 4 = 262140 bytes of extension payload, which cannot exist in a
    // short packet. Parser must reject this cleanly (no panic, no OOB read).
    #[test]
    fn test_rtp_extension_length_overflow_rejected() {
        // Only the 4-byte extension header — no payload — with length field = 0xFFFF.
        let bogus = [0x00, 0x00, 0xFF, 0xFF];
        let res = RtpExtension::parse(&bogus);
        assert!(res.is_err(), "malicious ext length must be rejected");
    }

    #[test]
    fn test_rtp_extension_truncated_rejected() {
        // Claim 1 word (4 bytes) of payload but provide none.
        let bogus = [0x00, 0x00, 0x00, 0x01];
        let res = RtpExtension::parse(&bogus);
        assert!(res.is_err(), "truncated ext payload must be rejected");
    }

    #[test]
    fn test_rtp_extension_valid_roundtrip() {
        // Valid: profile=0xBEDE (one-byte RFC 8285 header), one word of payload.
        let data = [0xBE, 0xDE, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD];
        let ext = RtpExtension::parse(&data).expect("valid extension must parse");
        assert_eq!(ext.profile, 0xBEDE);
        assert_eq!(ext.data.len(), 4);
        assert_eq!(&ext.data[..], &[0xAA, 0xBB, 0xCC, 0xDD]);
    }
}
