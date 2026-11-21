//! The publisher: one stream pushed to an ingest.
//!
//! ```text
//! TCP or TLS → handshake → set chunk size 4096 → connect(app, tcUrl)
//!   → releaseStream(key), FCPublish(key), createStream → publish(key, "live")
//!   → NetStream.Publish.Start → @setDataFrame onMetaData, then audio and video
//! ```
//!
//! While it publishes, a task reads what the server sends: it answers pings,
//! acknowledges a window's worth of bytes, and turns an error status or a
//! closed connection into the error the next send returns.

use crate::amf0::{self, Value};
use crate::chunk::{kind, u32_payload, ChunkReader, ChunkWriter, Message};
use crate::flv::Metadata;
use crate::{handshake, RtmpError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Chunks this size out, as encoders send them.
const CHUNK_SIZE: usize = 4096;

const CSID_CONTROL: u32 = 2;
const CSID_COMMAND: u32 = 3;
const CSID_AUDIO: u32 = 4;
const CSID_DATA: u32 = 5;
const CSID_VIDEO: u32 = 6;
const CSID_STREAM_COMMAND: u32 = 8;

/// Where to publish: `rtmp://host[:port]/app[/…]` or `rtmps://…`. The
/// stream key is apart from it, as services hand them out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtmpUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// The application: the path after the host.
    pub app: String,
    /// The URL `connect` names, without any key.
    pub tc_url: String,
}

impl RtmpUrl {
    pub fn parse(url: &str) -> Result<Self, RtmpError> {
        let bad = |why: &str| RtmpError::Url(format!("{why}: {url}"));
        let (tls, rest) = if let Some(r) = url.strip_prefix("rtmps://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("rtmp://") {
            (false, r)
        } else {
            return Err(bad("not rtmp:// or rtmps://"));
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.is_empty() || authority.contains('@') {
            return Err(bad("no host, or credentials in it"));
        }
        let default_port = if tls { 443 } else { 1935 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, after) = v6
                .split_once(']')
                .ok_or_else(|| bad("an unclosed IPv6 address"))?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse::<u16>().map_err(|_| bad("a bad port"))?,
                None if after.is_empty() => default_port,
                None => return Err(bad("text after the IPv6 address")),
            };
            (h.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    p.parse::<u16>().map_err(|_| bad("a bad port"))?,
                ),
                None => (authority.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return Err(bad("no host"));
        }
        let app = path.trim_matches('/').to_string();
        if app.is_empty() {
            return Err(bad("no application in the path"));
        }
        let tc_url = format!(
            "{}://{}/{}",
            if tls { "rtmps" } else { "rtmp" },
            authority,
            app
        );
        Ok(Self {
            tls,
            host,
            port,
            app,
            tc_url,
        })
    }
}

/// How to connect.
#[derive(Clone)]
pub struct Options {
    /// For `rtmps://`: the TLS configuration, the platform's roots unless
    /// set.
    pub tls: Option<Arc<rustls::ClientConfig>>,
    /// Connecting and publishing may take this long.
    pub timeout: Duration,
    /// What `connect`'s `flashVer` says.
    pub flash_ver: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tls: None,
            timeout: Duration::from_secs(10),
            flash_ver: "FMLE/3.0 (compatible; forge-rtmp)".to_string(),
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("tls", &self.tls.is_some())
            .field("timeout", &self.timeout)
            .finish()
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Stream = Box<dyn Io>;

/// The platform's roots, on ring.
pub fn default_tls_config() -> Result<Arc<rustls::ClientConfig>, RtmpError> {
    let mut roots = rustls::RootCertStore::empty();
    let found = rustls_native_certs::load_native_certs();
    for cert in found.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(RtmpError::Tls(
            "no trusted root certificates on this host".into(),
        ));
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| RtmpError::Tls(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

async fn open(url: &RtmpUrl, options: &Options) -> Result<Stream, RtmpError> {
    let tcp = tokio::net::TcpStream::connect((url.host.as_str(), url.port))
        .await
        .map_err(|e| RtmpError::Io(format!("{}:{}: {e}", url.host, url.port)))?;
    let _ = tcp.set_nodelay(true);
    if !url.tls {
        return Ok(Box::new(tcp));
    }
    let config = match &options.tls {
        Some(c) => Arc::clone(c),
        None => default_tls_config()?,
    };
    let name = rustls::pki_types::ServerName::try_from(url.host.clone())
        .map_err(|e| RtmpError::Tls(format!("{}: {e}", url.host)))?;
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(|e| RtmpError::Tls(e.to_string()))?;
    Ok(Box::new(tls))
}

/// The writing half and what it has sent.
struct Out {
    io: WriteHalf<Stream>,
    chunks: ChunkWriter,
    buf: Vec<u8>,
}

impl Out {
    async fn send(&mut self, m: &Message, sent: &AtomicU64) -> Result<(), RtmpError> {
        self.buf.clear();
        self.chunks.write(&mut self.buf, m);
        self.io
            .write_all(&self.buf)
            .await
            .map_err(RtmpError::from_io)?;
        self.io.flush().await.map_err(RtmpError::from_io)?;
        sent.fetch_add(self.buf.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}

fn control(kind: u8, payload: Vec<u8>) -> Message {
    Message {
        csid: CSID_CONTROL,
        kind,
        timestamp: 0,
        stream_id: 0,
        payload,
    }
}

fn command(csid: u32, stream_id: u32, values: &[Value]) -> Message {
    Message {
        csid,
        kind: kind::COMMAND_AMF0,
        timestamp: 0,
        stream_id,
        payload: amf0::encode(values),
    }
}

/// The reading half, answering the server's control messages itself.
struct Inbound {
    io: ReadHalf<Stream>,
    chunks: ChunkReader,
    window: u64,
    acked: u64,
}

impl Inbound {
    /// The next command or data message; control messages are answered.
    async fn next(
        &mut self,
        out: &tokio::sync::Mutex<Out>,
        sent: &AtomicU64,
    ) -> Result<Message, RtmpError> {
        loop {
            let m = self.chunks.read_message(&mut self.io).await?;
            if self.window > 0 && self.chunks.bytes_read - self.acked >= self.window {
                self.acked = self.chunks.bytes_read;
                let ack = control(kind::ACK, u32_payload(self.acked as u32));
                out.lock().await.send(&ack, sent).await?;
            }
            match m.kind {
                kind::WINDOW_ACK_SIZE if m.payload.len() >= 4 => {
                    self.window =
                        u32::from_be_bytes(m.payload[..4].try_into().expect("four")) as u64;
                }
                kind::SET_PEER_BANDWIDTH if m.payload.len() >= 4 => {
                    // Told how much it may send unacknowledged: say the same
                    // window back, as encoders do.
                    let size = control(kind::WINDOW_ACK_SIZE, m.payload[..4].to_vec());
                    out.lock().await.send(&size, sent).await?;
                }
                kind::USER_CONTROL if m.payload.len() >= 6 => {
                    let event = u16::from_be_bytes([m.payload[0], m.payload[1]]);
                    if event == 6 {
                        // A ping: answer with its timestamp.
                        let mut p = vec![0, 7];
                        p.extend_from_slice(&m.payload[2..6]);
                        out.lock()
                            .await
                            .send(&control(kind::USER_CONTROL, p), sent)
                            .await?;
                    }
                }
                kind::SET_CHUNK_SIZE | kind::ACK | kind::ABORT | kind::USER_CONTROL => {}
                kind::WINDOW_ACK_SIZE | kind::SET_PEER_BANDWIDTH => {}
                kind::COMMAND_AMF0 | kind::DATA_AMF0 => return Ok(m),
                other => debug!(kind = other, "RTMP: a message a publisher ignores"),
            }
        }
    }
}

/// An `onStatus` or `_error`'s code and description, when it is an error.
fn status_error(values: &[Value]) -> Option<RtmpError> {
    let info = values.iter().find(|v| v.get("code").is_some())?;
    let code = info.get("code").and_then(Value::as_str).unwrap_or_default();
    let level = info
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let failed = level == "error"
        || code.contains("Failed")
        || code.contains("BadName")
        || code.contains("Rejected");
    failed.then(|| RtmpError::Refused {
        code: code.to_string(),
        description: info
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// The `_result` of transaction `tx`, or its `_error` as an error.
async fn wait_result(
    inbound: &mut Inbound,
    out: &tokio::sync::Mutex<Out>,
    sent: &AtomicU64,
    tx: f64,
) -> Result<Vec<Value>, RtmpError> {
    loop {
        let m = inbound.next(out, sent).await?;
        if m.kind != kind::COMMAND_AMF0 {
            continue;
        }
        let values = amf0::decode(&m.payload)?;
        let name = values.first().and_then(Value::as_str).unwrap_or_default();
        let id = values.get(1).and_then(Value::as_number);
        match name {
            "_result" if id == Some(tx) => return Ok(values),
            "_error" if id == Some(tx) => {
                return Err(status_error(&values).unwrap_or(RtmpError::Refused {
                    code: "_error".into(),
                    description: String::new(),
                }))
            }
            _ => debug!(command = name, "RTMP: while waiting"),
        }
    }
}

/// A stream being published.
pub struct Publisher {
    out: Arc<tokio::sync::Mutex<Out>>,
    sent: Arc<AtomicU64>,
    failed: Arc<Mutex<Option<RtmpError>>>,
    stream_id: u32,
    key: String,
    reader: JoinHandle<()>,
    next_tx: f64,
}

impl std::fmt::Debug for Publisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publisher")
            .field("stream_id", &self.stream_id)
            .field("sent", &self.bytes_sent())
            .finish_non_exhaustive()
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Publisher {
    /// Connect to `url` and publish under `key`, with the platform's roots.
    pub async fn connect(url: &RtmpUrl, key: &str) -> Result<Self, RtmpError> {
        Self::connect_with(url, key, &Options::default()).await
    }

    pub async fn connect_with(
        url: &RtmpUrl,
        key: &str,
        options: &Options,
    ) -> Result<Self, RtmpError> {
        tokio::time::timeout(options.timeout, Self::setup(url, key, options))
            .await
            .map_err(|_| RtmpError::Timeout("connecting and publishing"))?
    }

    async fn setup(url: &RtmpUrl, key: &str, options: &Options) -> Result<Self, RtmpError> {
        let mut io = open(url, options).await?;
        handshake::client(&mut io).await?;
        let (rd, wr) = tokio::io::split(io);
        let out = Arc::new(tokio::sync::Mutex::new(Out {
            io: wr,
            chunks: ChunkWriter::default(),
            buf: Vec::with_capacity(CHUNK_SIZE * 2),
        }));
        let sent = Arc::new(AtomicU64::new(0));
        let mut inbound = Inbound {
            io: rd,
            chunks: ChunkReader::default(),
            window: 0,
            acked: 0,
        };
        {
            let mut o = out.lock().await;
            o.send(
                &control(kind::SET_CHUNK_SIZE, u32_payload(CHUNK_SIZE as u32)),
                &sent,
            )
            .await?;
            o.chunks.set_chunk_size(CHUNK_SIZE);
            let connect = command(
                CSID_COMMAND,
                0,
                &[
                    Value::str("connect"),
                    Value::Number(1.0),
                    Value::Object(vec![
                        ("app".into(), Value::str(url.app.clone())),
                        ("type".into(), Value::str("nonprivate")),
                        ("flashVer".into(), Value::str(options.flash_ver.clone())),
                        ("tcUrl".into(), Value::str(url.tc_url.clone())),
                    ]),
                ],
            );
            o.send(&connect, &sent).await?;
        }
        let connected = wait_result(&mut inbound, &out, &sent, 1.0).await?;
        if let Some(e) = status_error(&connected) {
            return Err(e);
        }
        {
            let mut o = out.lock().await;
            for (name, tx) in [("releaseStream", 2.0), ("FCPublish", 3.0)] {
                let m = command(
                    CSID_COMMAND,
                    0,
                    &[
                        Value::str(name),
                        Value::Number(tx),
                        Value::Null,
                        Value::str(key),
                    ],
                );
                o.send(&m, &sent).await?;
            }
            let create = command(
                CSID_COMMAND,
                0,
                &[Value::str("createStream"), Value::Number(4.0), Value::Null],
            );
            o.send(&create, &sent).await?;
        }
        let created = wait_result(&mut inbound, &out, &sent, 4.0).await?;
        let stream_id = created
            .get(3)
            .and_then(Value::as_number)
            .ok_or_else(|| RtmpError::Protocol("createStream gave no stream id".into()))?
            as u32;
        let publish = command(
            CSID_STREAM_COMMAND,
            stream_id,
            &[
                Value::str("publish"),
                Value::Number(5.0),
                Value::Null,
                Value::str(key),
                Value::str("live"),
            ],
        );
        out.lock().await.send(&publish, &sent).await?;
        loop {
            let m = inbound.next(&out, &sent).await?;
            if m.kind != kind::COMMAND_AMF0 {
                continue;
            }
            let values = amf0::decode(&m.payload)?;
            let name = values.first().and_then(Value::as_str).unwrap_or_default();
            if let Some(e) = status_error(&values) {
                return Err(e);
            }
            let code = values
                .iter()
                .find_map(|v| v.get("code").and_then(Value::as_str))
                .unwrap_or_default();
            if (name == "onStatus" || name == "onFCPublish") && code == "NetStream.Publish.Start" {
                break;
            }
        }

        let failed = Arc::new(Mutex::new(None));
        let reader = {
            let out = Arc::clone(&out);
            let sent = Arc::clone(&sent);
            let failed = Arc::clone(&failed);
            tokio::spawn(async move {
                loop {
                    match inbound.next(&out, &sent).await {
                        Ok(m) if m.kind == kind::COMMAND_AMF0 => {
                            let Ok(values) = amf0::decode(&m.payload) else {
                                continue;
                            };
                            if let Some(e) = status_error(&values) {
                                warn!(error = %e, "RTMP: the server stopped the stream");
                                *failed.lock().unwrap() = Some(e);
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            *failed.lock().unwrap() = Some(e);
                            return;
                        }
                    }
                }
            })
        };
        Ok(Self {
            out,
            sent,
            failed,
            stream_id,
            key: key.to_string(),
            reader,
            next_tx: 6.0,
        })
    }

    /// What has gone wrong since, if anything has.
    pub fn error(&self) -> Option<RtmpError> {
        self.failed.lock().unwrap().clone()
    }

    /// Bytes written to the connection.
    pub fn bytes_sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    async fn send(&self, m: Message) -> Result<(), RtmpError> {
        if let Some(e) = self.error() {
            return Err(e);
        }
        let result = self.out.lock().await.send(&m, &self.sent).await;
        if let Err(e) = &result {
            *self.failed.lock().unwrap() = Some(e.clone());
        }
        result
    }

    /// `@setDataFrame onMetaData`: what the stream is.
    pub async fn send_metadata(&self, meta: &Metadata) -> Result<(), RtmpError> {
        let payload = amf0::encode(&[
            Value::str("@setDataFrame"),
            Value::str("onMetaData"),
            meta.to_amf(),
        ]);
        self.send(Message {
            csid: CSID_DATA,
            kind: kind::DATA_AMF0,
            timestamp: 0,
            stream_id: self.stream_id,
            payload,
        })
        .await
    }

    /// A video tag's body (see [`crate::flv`]) at `timestamp` ms.
    pub async fn send_video(&self, timestamp: u32, body: Vec<u8>) -> Result<(), RtmpError> {
        self.send(Message {
            csid: CSID_VIDEO,
            kind: kind::VIDEO,
            timestamp,
            stream_id: self.stream_id,
            payload: body,
        })
        .await
    }

    /// An audio tag's body at `timestamp` ms.
    pub async fn send_audio(&self, timestamp: u32, body: Vec<u8>) -> Result<(), RtmpError> {
        self.send(Message {
            csid: CSID_AUDIO,
            kind: kind::AUDIO,
            timestamp,
            stream_id: self.stream_id,
            payload: body,
        })
        .await
    }

    /// Stop publishing and close the connection.
    pub async fn close(mut self) {
        let tx = self.next_tx;
        self.next_tx += 2.0;
        let unpublish = command(
            CSID_COMMAND,
            0,
            &[
                Value::str("FCUnpublish"),
                Value::Number(tx),
                Value::Null,
                Value::str(self.key.clone()),
            ],
        );
        let delete = command(
            CSID_STREAM_COMMAND,
            self.stream_id,
            &[
                Value::str("deleteStream"),
                Value::Number(tx + 1.0),
                Value::Null,
                Value::Number(self.stream_id as f64),
            ],
        );
        if self.error().is_none() {
            let _ = self.send(unpublish).await;
            let _ = self.send(delete).await;
        }
        self.reader.abort();
        let _ = self.out.lock().await.io.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_name_their_host_port_and_application() {
        let u = RtmpUrl::parse("rtmp://a.rtmp.youtube.com/live2").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.app.as_str()),
            (false, "a.rtmp.youtube.com", 1935, "live2")
        );
        assert_eq!(u.tc_url, "rtmp://a.rtmp.youtube.com/live2");
        let u = RtmpUrl::parse("rtmps://live-api-s.facebook.com:443/rtmp/").unwrap();
        assert_eq!((u.tls, u.port, u.app.as_str()), (true, 443, "rtmp"));
        let u = RtmpUrl::parse("rtmps://ingest.example/app/sub").unwrap();
        assert_eq!((u.port, u.app.as_str()), (443, "app/sub"));
        let u = RtmpUrl::parse("rtmp://[::1]:1936/live").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 1936));
        for bad in [
            "http://x/live",
            "rtmp://x",
            "rtmp:///live",
            "rtmp://u:p@x/live",
            "rtmp://x:99999/live",
        ] {
            assert!(RtmpUrl::parse(bad).is_err(), "{bad}");
        }
    }
}
