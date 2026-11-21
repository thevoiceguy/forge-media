//! FLV and RTMP for pushing a stream out (`docs/WEBINARS.md` in FCP, §2.8):
//! H.264 and AAC in FLV tags, published to a live service's ingest
//! (YouTube Live, Twitch, Facebook, any RTMP server) over `rtmp://` or
//! `rtmps://`.
//!
//! - [`flv`]: the tag bodies and the file around them.
//! - [`amf0`]: the encoding of commands and metadata.
//! - [`chunk`]: the chunk stream.
//! - [`Publisher`]: the client. It connects, shakes hands, publishes under a
//!   stream key and sends tags; it answers the server's pings and
//!   acknowledges what it reads.
//! - [`server`]: a stand-in server that takes one publisher and keeps what
//!   it sent, for tests.

pub mod amf0;
pub mod chunk;
pub mod client;
pub mod flv;
mod handshake;
pub mod server;

pub use client::{Publisher, RtmpUrl};
pub use flv::{AvcTagger, Metadata, Tag, TagKind};

/// What went wrong.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RtmpError {
    #[error("RTMP URL: {0}")]
    Url(String),
    #[error("RTMP connection: {0}")]
    Io(String),
    #[error("RTMP connection closed")]
    Closed,
    #[error("RTMP TLS: {0}")]
    Tls(String),
    #[error("RTMP protocol: {0}")]
    Protocol(String),
    /// The server refused: the status code and its description, such as
    /// `NetStream.Publish.BadName`.
    #[error("RTMP server refused: {code}: {description}")]
    Refused { code: String, description: String },
    #[error("RTMP timed out {0}")]
    Timeout(&'static str),
    #[error("FLV: {0}")]
    Media(String),
}

impl RtmpError {
    pub(crate) fn from_io(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            RtmpError::Closed
        } else {
            RtmpError::Io(e.to_string())
        }
    }
}
