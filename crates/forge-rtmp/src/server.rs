//! A stand-in ingest for tests: it takes publishers as a live service does
//! (the handshake, `connect`, `createStream`, `publish`), answers them as
//! nginx-rtmp does, and keeps every tag each one sent. It can refuse a
//! stream key, cut every connection, and ping its publishers to see them
//! answer.

use crate::amf0::{self, Value};
use crate::chunk::{kind, u32_payload, ChunkReader, ChunkWriter, Message};
use crate::flv::{Tag, TagKind};
use crate::{handshake, RtmpError};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// One publisher's stream as the stand-in took it.
#[derive(Debug, Clone, Default)]
pub struct Published {
    pub app: String,
    pub tc_url: String,
    pub key: String,
    /// What `@setDataFrame` carried after its name: `onMetaData` and the
    /// array.
    pub metadata: Option<Vec<Value>>,
    pub tags: Vec<Tag>,
    /// Pings answered.
    pub pongs: u32,
    /// It unpublished (`FCUnpublish` or `deleteStream`).
    pub unpublished: bool,
    /// Its connection is gone.
    pub closed: bool,
}

impl Published {
    pub fn video(&self) -> impl Iterator<Item = &Tag> {
        self.tags.iter().filter(|t| t.kind == TagKind::Video)
    }

    pub fn audio(&self) -> impl Iterator<Item = &Tag> {
        self.tags.iter().filter(|t| t.kind == TagKind::Audio)
    }
}

#[derive(Default)]
struct Shared {
    publishes: Mutex<Vec<Published>>,
    refused: Mutex<HashSet<String>>,
    cut: Notify,
    changed: Notify,
}

/// The stand-in, listening on a loopback port.
pub struct StandInServer {
    addr: SocketAddr,
    tls: bool,
    shared: Arc<Shared>,
}

impl StandInServer {
    /// Plain RTMP.
    pub async fn start() -> std::io::Result<Self> {
        Self::listen(None).await
    }

    /// RTMPS, with `config`'s certificate.
    pub async fn start_tls(config: Arc<rustls::ServerConfig>) -> std::io::Result<Self> {
        Self::listen(Some(config)).await
    }

    async fn listen(tls: Option<Arc<rustls::ServerConfig>>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let s = Arc::clone(&shared);
        let acceptor = tls.clone().map(tokio_rustls::TlsAcceptor::from);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let s = Arc::clone(&s);
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = match acceptor {
                        Some(a) => match a.accept(tcp).await {
                            Ok(io) => serve(io, &s).await,
                            Err(e) => Err(RtmpError::Tls(e.to_string())),
                        },
                        None => serve(tcp, &s).await,
                    };
                });
            }
        });
        Ok(Self {
            addr,
            tls: tls.is_some(),
            shared,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL of application `app` here.
    pub fn url(&self, app: &str) -> String {
        format!(
            "{}://127.0.0.1:{}/{app}",
            if self.tls { "rtmps" } else { "rtmp" },
            self.addr.port()
        )
    }

    /// Answer a publish under `key` with `NetStream.Publish.BadName`.
    pub fn refuse_key(&self, key: &str) {
        self.shared.refused.lock().unwrap().insert(key.to_string());
    }

    /// Close every connection there is now.
    pub fn cut(&self) {
        self.shared.cut.notify_waiters();
    }

    /// Every publish so far, in the order they came.
    pub fn publishes(&self) -> Vec<Published> {
        self.shared.publishes.lock().unwrap().clone()
    }

    /// Wait until `ready` holds of the publishes, or `within` passes.
    pub async fn wait_for(
        &self,
        within: Duration,
        ready: impl Fn(&[Published]) -> bool,
    ) -> Result<Vec<Published>, Vec<Published>> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let changed = self.shared.changed.notified();
            let now = self.publishes();
            if ready(&now) {
                return Ok(now);
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                let now = self.publishes();
                return if ready(&now) { Ok(now) } else { Err(now) };
            }
        }
    }
}

fn status(level: &str, code: &str, description: &str) -> Value {
    Value::Object(vec![
        ("level".into(), Value::str(level)),
        ("code".into(), Value::str(code)),
        ("description".into(), Value::str(description)),
    ])
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut io: S,
    shared: &Shared,
) -> Result<(), RtmpError> {
    handshake::server(&mut io).await?;
    let (mut rd, mut wr) = tokio::io::split(io);
    let mut reader = ChunkReader::default();
    let mut writer = ChunkWriter::default();
    let mut buf = Vec::new();
    // Which entry in `publishes` this connection is, once it publishes.
    let mut index: Option<usize> = None;
    let mut connect: (String, String) = Default::default();
    let mark_closed = |index: Option<usize>| {
        if let Some(i) = index {
            shared.publishes.lock().unwrap()[i].closed = true;
            shared.changed.notify_waiters();
        }
    };
    loop {
        let m = tokio::select! {
            m = reader.read_message(&mut rd) => match m {
                Ok(m) => m,
                Err(e) => {
                    mark_closed(index);
                    return Err(e);
                }
            },
            _ = shared.cut.notified() => {
                mark_closed(index);
                let _ = wr.shutdown().await;
                return Ok(());
            }
        };
        let mut replies: Vec<Message> = Vec::new();
        let reply = |csid: u32, stream_id: u32, values: &[Value]| Message {
            csid,
            kind: kind::COMMAND_AMF0,
            timestamp: 0,
            stream_id,
            payload: amf0::encode(values),
        };
        match m.kind {
            kind::COMMAND_AMF0 => {
                let values = amf0::decode(&m.payload)?;
                let name = values.first().and_then(Value::as_str).unwrap_or_default();
                let tx = values.get(1).cloned().unwrap_or(Value::Number(0.0));
                match name {
                    "connect" => {
                        let obj = values.get(2);
                        connect = (
                            obj.and_then(|o| o.get("app"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            obj.and_then(|o| o.get("tcUrl"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                        let ctl = |k: u8, p: Vec<u8>| Message {
                            csid: 2,
                            kind: k,
                            timestamp: 0,
                            stream_id: 0,
                            payload: p,
                        };
                        replies.push(ctl(kind::WINDOW_ACK_SIZE, u32_payload(5_000_000)));
                        let mut bw = u32_payload(5_000_000);
                        bw.push(2);
                        replies.push(ctl(kind::SET_PEER_BANDWIDTH, bw));
                        replies.push(ctl(kind::SET_CHUNK_SIZE, u32_payload(4096)));
                        replies.push(reply(
                            3,
                            0,
                            &[
                                Value::str("_result"),
                                tx,
                                Value::Object(vec![
                                    ("fmsVer".into(), Value::str("FMS/3,0,1,123")),
                                    ("capabilities".into(), Value::Number(31.0)),
                                ]),
                                status(
                                    "status",
                                    "NetConnection.Connect.Success",
                                    "Connection succeeded.",
                                ),
                            ],
                        ));
                    }
                    "createStream" => {
                        replies.push(reply(
                            3,
                            0,
                            &[Value::str("_result"), tx, Value::Null, Value::Number(1.0)],
                        ));
                    }
                    "publish" => {
                        let key = values
                            .get(3)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if shared.refused.lock().unwrap().contains(&key) {
                            replies.push(reply(
                                5,
                                m.stream_id,
                                &[
                                    Value::str("onStatus"),
                                    Value::Number(0.0),
                                    Value::Null,
                                    status(
                                        "error",
                                        "NetStream.Publish.BadName",
                                        "Stream key refused",
                                    ),
                                ],
                            ));
                        } else {
                            let mut all = shared.publishes.lock().unwrap();
                            all.push(Published {
                                app: connect.0.clone(),
                                tc_url: connect.1.clone(),
                                key,
                                ..Default::default()
                            });
                            index = Some(all.len() - 1);
                            drop(all);
                            shared.changed.notify_waiters();
                            replies.push(reply(
                                5,
                                m.stream_id,
                                &[
                                    Value::str("onStatus"),
                                    Value::Number(0.0),
                                    Value::Null,
                                    status("status", "NetStream.Publish.Start", "Publishing"),
                                ],
                            ));
                            // A ping, to be answered.
                            let mut p = vec![0, 6];
                            p.extend_from_slice(&7u32.to_be_bytes());
                            replies.push(Message {
                                csid: 2,
                                kind: kind::USER_CONTROL,
                                timestamp: 0,
                                stream_id: 0,
                                payload: p,
                            });
                        }
                    }
                    "FCUnpublish" | "deleteStream" => {
                        if let Some(i) = index {
                            shared.publishes.lock().unwrap()[i].unpublished = true;
                            shared.changed.notify_waiters();
                        }
                    }
                    _ => {}
                }
            }
            kind::DATA_AMF0 => {
                if let (Some(i), Ok(values)) = (index, amf0::decode(&m.payload)) {
                    if values.first().and_then(Value::as_str) == Some("@setDataFrame") {
                        shared.publishes.lock().unwrap()[i].metadata = Some(values[1..].to_vec());
                        shared.changed.notify_waiters();
                    }
                }
            }
            kind::AUDIO | kind::VIDEO => {
                if let Some(i) = index {
                    let kind = if m.kind == kind::AUDIO {
                        TagKind::Audio
                    } else {
                        TagKind::Video
                    };
                    shared.publishes.lock().unwrap()[i].tags.push(Tag {
                        kind,
                        timestamp: m.timestamp,
                        body: m.payload,
                    });
                    shared.changed.notify_waiters();
                }
            }
            kind::USER_CONTROL if m.payload.get(..2) == Some(&[0, 7]) => {
                if let Some(i) = index {
                    shared.publishes.lock().unwrap()[i].pongs += 1;
                    shared.changed.notify_waiters();
                }
            }
            _ => {}
        }
        for r in replies {
            buf.clear();
            writer.write(&mut buf, &r);
            if r.kind == kind::SET_CHUNK_SIZE {
                writer.set_chunk_size(4096);
            }
            wr.write_all(&buf).await.map_err(RtmpError::from_io)?;
        }
        wr.flush().await.map_err(RtmpError::from_io)?;
    }
}
