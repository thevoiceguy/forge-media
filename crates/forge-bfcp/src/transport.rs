//! The two transports' bookkeeping.
//!
//! Over TCP a message is a frame in a stream: [`TcpFramer`] cuts them
//! out. Over UDP (RFC 8855 §6.2, §8) each datagram is one message and
//! nothing is reliable: [`UdpPeer`] keeps one client's transactions —
//! the response to a request is cached and replayed when the request is
//! retransmitted, and a server-initiated message (a `FloorRequestStatus`
//! after a decision, a `FloorStatus` to a subscriber) is sent one at a
//! time and retransmitted on timer T1 until the client acknowledges it
//! or it is given up on, which means the client is gone.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::message::{Header, Message, ParseError, ParseErrorKind, COMMON_HEADER_LEN};

/// The initial request retransmission timer (RFC 8855 §8.3.3).
pub const T1: Duration = Duration::from_millis(500);
/// How many times a request is retransmitted before the connection is
/// taken as broken (§8.3.1).
pub const MAX_RETRANSMITS: u32 = 3;
/// How long a response is kept for replay: T2 = T1 × 2⁴ × 1.25 (§8.3.3).
const T2: Duration = Duration::from_millis(10_000);

/// Cuts BFCP messages out of a TCP stream.
#[derive(Debug, Default)]
pub struct TcpFramer {
    buf: Vec<u8>,
}

impl TcpFramer {
    pub fn new() -> TcpFramer {
        TcpFramer::default()
    }

    /// Bytes read from the socket.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next whole message, `Ok(None)` while one is still arriving.
    /// A message that cannot be read is dropped from the stream, with
    /// the error, when its length was readable; one whose header is not
    /// even that leaves the stream unusable, and the caller should close.
    pub fn next_message(&mut self) -> Result<Option<Message>, ParseError> {
        if self.buf.len() < COMMON_HEADER_LEN {
            return Ok(None);
        }
        let header = match Header::parse(&self.buf) {
            Ok(h) => h,
            Err(e) if matches!(e.kind, ParseErrorKind::UnknownPrimitive(_)) => {
                // The length is at the same place whatever the primitive.
                let len = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize * 4;
                let total = COMMON_HEADER_LEN + len;
                if self.buf.len() < total {
                    return Ok(None);
                }
                self.buf.drain(..total);
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let total = header.message_len();
        if self.buf.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = self.buf.drain(..total).collect();
        Message::parse(&frame).map(Some)
    }

    /// Bytes waiting for the rest of their message.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// A server-initiated message in flight.
#[derive(Debug, Clone)]
struct Outstanding {
    transaction_id: u16,
    bytes: Vec<u8>,
    sent_at: Instant,
    /// The next retransmission, and how many have gone.
    due: Instant,
    retransmits: u32,
    /// The timer as it stands: T1, doubled per retransmission.
    interval: Duration,
}

/// What a datagram from the client turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UdpInbound {
    /// A new request: hand it to the floor server, then cache the reply
    /// with [`UdpPeer::cache_response`].
    Request,
    /// A retransmission of a request already answered: send these bytes
    /// again.
    Replay(Vec<u8>),
    /// The acknowledgement of the outstanding server-initiated message;
    /// the next one, if any, is ready to send.
    Acked { transaction_id: u16 },
    /// An acknowledgement of nothing outstanding, or a response with the
    /// R flag that is not one: ignored.
    Stray,
    /// Not a BFCP header at all; the floor server will answer it if it
    /// can.
    Unreadable,
}

/// What a timer tick asks the caller to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tick {
    /// Send these datagrams (retransmissions, or the next queued message).
    pub send: Vec<Vec<u8>>,
    /// The client stopped acknowledging: the connection is broken (§8.3.1).
    pub broken: bool,
}

/// One UDP client's transactions with the floor server.
#[derive(Debug)]
pub struct UdpPeer {
    /// The last request answered, and when: replayed on retransmission
    /// until T2 has passed.
    response: Option<(u16, Vec<u8>, Instant)>,
    outstanding: Option<Outstanding>,
    queue: VecDeque<Vec<u8>>,
    t1: Duration,
}

impl Default for UdpPeer {
    fn default() -> Self {
        UdpPeer::new()
    }
}

impl UdpPeer {
    pub fn new() -> UdpPeer {
        UdpPeer {
            response: None,
            outstanding: None,
            queue: VecDeque::new(),
            t1: T1,
        }
    }

    /// Sort a datagram before the floor server sees it.
    pub fn inbound(&mut self, data: &[u8], now: Instant) -> UdpInbound {
        let Ok(header) = Header::parse(data) else {
            return UdpInbound::Unreadable;
        };
        if header.responder {
            let acked = match &self.outstanding {
                Some(o)
                    if o.transaction_id == header.transaction_id && header.primitive.is_ack() =>
                {
                    // A reply to the first transmission tunes T1; one to a
                    // retransmission cannot be told apart and does not.
                    if o.retransmits == 0 {
                        self.t1 = now.saturating_duration_since(o.sent_at).max(T1);
                    }
                    true
                }
                _ => false,
            };
            if acked {
                let transaction_id = self
                    .outstanding
                    .take()
                    .map(|o| o.transaction_id)
                    .unwrap_or(0);
                return UdpInbound::Acked { transaction_id };
            }
            return UdpInbound::Stray;
        }
        match &self.response {
            Some((txn, bytes, at))
                if *txn == header.transaction_id && now.saturating_duration_since(*at) < T2 =>
            {
                UdpInbound::Replay(bytes.clone())
            }
            _ => UdpInbound::Request,
        }
    }

    /// The response just sent for the client's transaction `transaction_id`,
    /// kept for replay.
    pub fn cache_response(&mut self, transaction_id: u16, bytes: Vec<u8>, now: Instant) {
        self.response = Some((transaction_id, bytes, now));
    }

    /// A server-initiated message for the client. Sent now — the bytes
    /// come back — when none is outstanding; queued behind it otherwise
    /// (§6.2: one outstanding transaction per peer).
    pub fn notify(&mut self, message: &Message, now: Instant) -> Option<Vec<u8>> {
        let bytes = message.to_bytes().ok()?;
        if self.outstanding.is_some() {
            self.queue.push_back(bytes);
            return None;
        }
        Some(self.start(message.transaction_id, bytes, now))
    }

    fn start(&mut self, transaction_id: u16, bytes: Vec<u8>, now: Instant) -> Vec<u8> {
        self.outstanding = Some(Outstanding {
            transaction_id,
            bytes: bytes.clone(),
            sent_at: now,
            due: now + self.t1,
            retransmits: 0,
            interval: self.t1,
        });
        bytes
    }

    /// Time passed: retransmit what is due, send what was queued once
    /// the way is clear, forget an old response.
    pub fn tick(&mut self, now: Instant) -> Tick {
        let mut tick = Tick::default();
        if let Some((_, _, at)) = &self.response {
            if now.saturating_duration_since(*at) >= T2 {
                self.response = None;
            }
        }
        if let Some(o) = &mut self.outstanding {
            if now >= o.due {
                if o.retransmits >= MAX_RETRANSMITS {
                    tick.broken = true;
                    self.outstanding = None;
                    self.queue.clear();
                    return tick;
                }
                o.retransmits += 1;
                o.interval *= 2;
                o.due = now + o.interval;
                tick.send.push(o.bytes.clone());
            }
        } else if let Some(next) = self.queue.pop_front() {
            let txn = Header::parse(&next).map(|h| h.transaction_id).unwrap_or(0);
            tick.send.push(self.start(txn, next, now));
        }
        tick
    }

    /// When the next retransmission is due, for a caller that sleeps.
    pub fn next_due(&self) -> Option<Instant> {
        self.outstanding.as_ref().map(|o| o.due)
    }

    /// Whether a server-initiated message awaits its acknowledgement.
    pub fn is_busy(&self) -> bool {
        self.outstanding.is_some()
    }

    /// Messages waiting their turn.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }
}
