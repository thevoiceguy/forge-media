//! RTMP's chunk stream (Adobe's RTMP specification 1.0, §5.3): messages cut
//! into chunks for the wire, and chunks put back together into messages.
//!
//! Out, each message's first chunk has a full (type 0) header and the rest
//! type 3. In, every header type is taken, with extended timestamps and the
//! peer's chunk size.

use crate::RtmpError;
use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Message types.
pub mod kind {
    pub const SET_CHUNK_SIZE: u8 = 1;
    pub const ABORT: u8 = 2;
    pub const ACK: u8 = 3;
    pub const USER_CONTROL: u8 = 4;
    pub const WINDOW_ACK_SIZE: u8 = 5;
    pub const SET_PEER_BANDWIDTH: u8 = 6;
    pub const AUDIO: u8 = 8;
    pub const VIDEO: u8 = 9;
    pub const DATA_AMF0: u8 = 18;
    pub const COMMAND_AMF0: u8 = 20;
}

/// A whole message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Which chunk stream it went on.
    pub csid: u32,
    pub kind: u8,
    pub timestamp: u32,
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

/// The largest message this reads: a picture of a few megabytes at most.
const MAX_MESSAGE: usize = 8 * 1024 * 1024;
/// The extended-timestamp marker.
const EXTENDED: u32 = 0xFF_FFFF;

/// Cuts messages into chunks of `chunk_size`.
#[derive(Debug)]
pub struct ChunkWriter {
    chunk_size: usize,
}

impl Default for ChunkWriter {
    fn default() -> Self {
        Self { chunk_size: 128 }
    }
}

impl ChunkWriter {
    /// Chunks of `size` from now on; the peer must be told so first.
    pub fn set_chunk_size(&mut self, size: usize) {
        self.chunk_size = size.clamp(1, 0x7FFF_FFFF);
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    fn basic_header(out: &mut Vec<u8>, fmt: u8, csid: u32) {
        match csid {
            2..=63 => out.push((fmt << 6) | csid as u8),
            64..=319 => {
                out.push(fmt << 6);
                out.push((csid - 64) as u8);
            }
            _ => {
                let v = csid - 64;
                out.push((fmt << 6) | 1);
                out.push(v as u8);
                out.push((v >> 8) as u8);
            }
        }
    }

    /// Append `m` to `out` as chunks.
    pub fn write(&self, out: &mut Vec<u8>, m: &Message) {
        let extended = m.timestamp >= EXTENDED;
        let field = if extended { EXTENDED } else { m.timestamp };
        Self::basic_header(out, 0, m.csid);
        out.extend_from_slice(&field.to_be_bytes()[1..]);
        out.extend_from_slice(&(m.payload.len() as u32).to_be_bytes()[1..]);
        out.push(m.kind);
        out.extend_from_slice(&m.stream_id.to_le_bytes());
        if extended {
            out.extend_from_slice(&m.timestamp.to_be_bytes());
        }
        let mut rest = &m.payload[..];
        loop {
            let n = rest.len().min(self.chunk_size);
            out.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if rest.is_empty() {
                break;
            }
            Self::basic_header(out, 3, m.csid);
            if extended {
                out.extend_from_slice(&m.timestamp.to_be_bytes());
            }
        }
    }
}

/// What one chunk stream last said, for the headers that leave it out.
#[derive(Debug, Default, Clone)]
struct StreamState {
    timestamp: u32,
    delta: u32,
    length: usize,
    kind: u8,
    stream_id: u32,
    extended: bool,
    partial: Vec<u8>,
}

/// Puts chunks back together into messages.
#[derive(Debug)]
pub struct ChunkReader {
    chunk_size: usize,
    streams: HashMap<u32, StreamState>,
    /// Bytes read, for acknowledgements.
    pub bytes_read: u64,
}

impl Default for ChunkReader {
    fn default() -> Self {
        Self {
            chunk_size: 128,
            streams: HashMap::new(),
            bytes_read: 0,
        }
    }
}

impl ChunkReader {
    /// The peer's chunks are `size` from now on.
    pub fn set_chunk_size(&mut self, size: usize) {
        self.chunk_size = size.clamp(1, MAX_MESSAGE);
    }

    async fn read_exact<R: AsyncRead + Unpin>(
        &mut self,
        r: &mut R,
        buf: &mut [u8],
    ) -> Result<(), RtmpError> {
        r.read_exact(buf).await.map_err(RtmpError::from_io)?;
        self.bytes_read += buf.len() as u64;
        Ok(())
    }

    async fn read_u8<R: AsyncRead + Unpin>(&mut self, r: &mut R) -> Result<u8, RtmpError> {
        let mut b = [0u8; 1];
        self.read_exact(r, &mut b).await?;
        Ok(b[0])
    }

    /// The next whole message. A `SetChunkSize` from the peer is applied
    /// here, and returned too.
    pub async fn read_message<R: AsyncRead + Unpin>(
        &mut self,
        r: &mut R,
    ) -> Result<Message, RtmpError> {
        loop {
            let first = self.read_u8(r).await?;
            let fmt = first >> 6;
            let csid = match first & 0x3F {
                0 => 64 + self.read_u8(r).await? as u32,
                1 => {
                    let mut b = [0u8; 2];
                    self.read_exact(r, &mut b).await?;
                    64 + b[0] as u32 + (b[1] as u32) * 256
                }
                n => n as u32,
            };
            let mut header = [0u8; 11];
            let header_len = [11, 7, 3, 0][fmt as usize];
            self.read_exact(r, &mut header[..header_len]).await?;
            let mut state = self.streams.remove(&csid).unwrap_or_default();
            if fmt == 0 && !state.partial.is_empty() {
                return Err(RtmpError::Protocol(format!(
                    "chunk stream {csid}: a new message before the last was whole"
                )));
            }
            let field = if header_len >= 3 {
                u32::from_be_bytes([0, header[0], header[1], header[2]])
            } else {
                0
            };
            if header_len >= 7 {
                state.length = u32::from_be_bytes([0, header[3], header[4], header[5]]) as usize;
                state.kind = header[6];
                if state.length > MAX_MESSAGE {
                    return Err(RtmpError::Protocol(format!(
                        "a message of {} bytes",
                        state.length
                    )));
                }
            }
            if header_len == 11 {
                state.stream_id = u32::from_le_bytes([header[7], header[8], header[9], header[10]]);
            }
            if header_len >= 3 {
                state.extended = field == EXTENDED;
            }
            let ts_field = if state.extended {
                let mut b = [0u8; 4];
                self.read_exact(r, &mut b).await?;
                u32::from_be_bytes(b)
            } else {
                field
            };
            match fmt {
                0 => {
                    state.timestamp = ts_field;
                    state.delta = 0;
                }
                1 | 2 => {
                    state.delta = ts_field;
                    if state.partial.is_empty() {
                        state.timestamp = state.timestamp.wrapping_add(ts_field);
                    }
                }
                _ => {
                    // Type 3 opening a new message repeats the last delta.
                    if state.partial.is_empty() {
                        state.timestamp = state.timestamp.wrapping_add(state.delta);
                    }
                }
            }
            let want = (state.length - state.partial.len()).min(self.chunk_size);
            let start = state.partial.len();
            state.partial.resize(start + want, 0);
            let mut chunk = std::mem::take(&mut state.partial);
            self.read_exact(r, &mut chunk[start..]).await?;
            state.partial = chunk;
            if state.partial.len() < state.length {
                self.streams.insert(csid, state);
                continue;
            }
            let payload = std::mem::take(&mut state.partial);
            let m = Message {
                csid,
                kind: state.kind,
                timestamp: state.timestamp,
                stream_id: state.stream_id,
                payload,
            };
            self.streams.insert(csid, state);
            if m.kind == kind::SET_CHUNK_SIZE && m.payload.len() >= 4 {
                let size = u32::from_be_bytes([
                    m.payload[0] & 0x7F,
                    m.payload[1],
                    m.payload[2],
                    m.payload[3],
                ]);
                self.set_chunk_size(size as usize);
            }
            return Ok(m);
        }
    }
}

/// A protocol control message's payload: one big-endian `u32`.
pub fn u32_payload(v: u32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(csid: u32, len: usize, ts: u32) -> Message {
        Message {
            csid,
            kind: kind::VIDEO,
            timestamp: ts,
            stream_id: 1,
            payload: (0..len).map(|i| i as u8).collect(),
        }
    }

    #[tokio::test]
    async fn messages_cut_into_chunks_come_back_whole() {
        let mut w = ChunkWriter::default();
        let mut wire = Vec::new();
        let small = message(4, 10, 40);
        w.write(&mut wire, &small);
        w.set_chunk_size(100);
        let big = message(6, 1000, 0x0100_0000);
        let far = message(400, 300, 7);
        w.write(&mut wire, &big);
        w.write(&mut wire, &far);

        let mut r = ChunkReader::default();
        let mut cursor = &wire[..];
        assert_eq!(r.read_message(&mut cursor).await.unwrap(), small);
        r.set_chunk_size(100);
        assert_eq!(
            r.read_message(&mut cursor).await.unwrap(),
            big,
            "an extended timestamp on every chunk"
        );
        assert_eq!(
            r.read_message(&mut cursor).await.unwrap(),
            far,
            "a three-byte basic header"
        );
        assert_eq!(r.bytes_read, wire.len() as u64);
    }

    #[tokio::test]
    async fn short_headers_take_what_the_stream_last_said() {
        // A type 0 header, then a type 2 (delta only) and a type 3 that
        // repeats the delta, as a server sends audio.
        let mut wire = vec![
            0x04,
            0,
            0,
            10,
            0,
            0,
            2,
            kind::AUDIO,
            1,
            0,
            0,
            0,
            0xAA,
            0xBB, // ts 10
            0x84,
            0,
            0,
            20,
            0xCC,
            0xDD, // fmt 2: +20
            0xC4,
            0xEE,
            0xFF, // fmt 3: +20 again
        ];
        // A set chunk size of 4096 on the control stream.
        wire.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 4, kind::SET_CHUNK_SIZE, 0, 0, 0, 0]);
        wire.extend_from_slice(&4096u32.to_be_bytes());
        let mut r = ChunkReader::default();
        let mut cursor = &wire[..];
        let ts: Vec<u32> = [
            r.read_message(&mut cursor).await.unwrap(),
            r.read_message(&mut cursor).await.unwrap(),
            r.read_message(&mut cursor).await.unwrap(),
        ]
        .iter()
        .map(|m| m.timestamp)
        .collect();
        assert_eq!(ts, [10, 30, 50]);
        let set = r.read_message(&mut cursor).await.unwrap();
        assert_eq!(set.kind, kind::SET_CHUNK_SIZE);
        assert_eq!(r.chunk_size, 4096);
    }

    #[tokio::test]
    async fn a_message_too_large_is_refused() {
        let wire = [0x04, 0, 0, 0, 0xFF, 0xFF, 0xFF, kind::VIDEO, 1, 0, 0, 0];
        let mut r = ChunkReader::default();
        assert!(r.read_message(&mut &wire[..]).await.is_err());
    }
}
