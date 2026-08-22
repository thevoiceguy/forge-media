//! TURN (Traversal Using Relays around NAT) client — RFC 8656.
//!
//! When both peers sit behind NATs that STUN cannot punch (notably symmetric
//! NAT on both ends), ICE needs a *relay* candidate: a transport address on a
//! TURN server that forwards packets between the two endpoints. This module
//! implements the client side — allocation, refresh, permissions, channels and
//! the ChannelData/Send framing that carries media through the relay.
//!
//! # Flow (RFC 8656 §6–§12)
//!
//! 1. **Allocate** — the first request is unauthenticated; the server answers
//!    `401 Unauthorized` carrying a `REALM` and a `NONCE`. The client derives
//!    the long-term key `MD5(username ":" realm ":" password)` (RFC 8489 §9.2.3)
//!    and retries with `USERNAME`/`REALM`/`NONCE`/`MESSAGE-INTEGRITY`. The
//!    success response carries `XOR-RELAYED-ADDRESS` (the relay candidate) and a
//!    `LIFETIME`.
//! 2. **CreatePermission** / **ChannelBind** — before the relay will forward to
//!    a peer, the client installs a permission for the peer's IP (§9) and,
//!    preferably, binds a 16-bit channel to it (§11) so media uses the compact
//!    4-byte ChannelData header instead of a full Send/Data indication.
//! 3. **Send / recv** — [`TurnClient::send_to`] frames application data as
//!    ChannelData once a channel is bound (falling back to a Send indication
//!    before then); [`TurnClient::recv_from`] unwraps ChannelData and Data
//!    indications back into `(payload, peer)`.
//! 4. **Refresh** — [`TurnClient::refresh`] keeps the allocation alive; call it
//!    at roughly half the lifetime.
//!
//! # Threading
//!
//! [`TurnClient::send_to`] and [`TurnClient::recv_from`] take `&self` and may be
//! used concurrently (tokio `UdpSocket` permits concurrent send/recv). The
//! control operations ([`TurnClient::create_permission`],
//! [`TurnClient::bind_channel`], [`TurnClient::refresh`]) take `&mut self`, so a
//! caller running a concurrent receive loop must serialise them against it —
//! e.g. drive all control traffic from the same task as the media loop. Wiring
//! a shared demultiplexing receive loop into the media transport is the
//! follow-up that lets relay candidates carry live media end to end.

use crate::stun::{MessageType, StunAttribute, StunMessage};
use forge_core::{ForgeError, Result};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::time::timeout;
use tracing::{debug, warn};

/// REQUESTED-TRANSPORT protocol number for UDP (RFC 8656 §14.7 / IANA).
const UDP_TRANSPORT: u8 = 17;
/// Default allocation lifetime requested/assumed when the server omits one.
const DEFAULT_LIFETIME: u32 = 600;
/// Channel numbers live in 0x4000–0x7FFF (RFC 8656 §12). We allocate from the
/// bottom of that range upward, one per distinct peer.
const CHANNEL_MIN: u16 = 0x4000;
const CHANNEL_MAX: u16 = 0x7FFF;
/// Initial retransmission timeout (RFC 8489 §6.2.1); doubled each retry.
const INITIAL_RTO: Duration = Duration::from_millis(500);
/// Number of send attempts before a transaction is declared lost.
const MAX_ATTEMPTS: usize = 5;
/// Bound on 438 Stale-Nonce re-signing to avoid a loop against a hostile server.
const MAX_STALE_NONCE_RETRIES: usize = 3;

/// A TURN server plus the long-term credentials to authenticate against it.
#[derive(Clone, Debug)]
pub struct TurnServer {
    /// `turn:host:port`, `turns:host:port`, or a bare `host:port`.
    pub uri: String,
    /// Long-term-credential username.
    pub username: String,
    /// Long-term-credential password.
    pub password: String,
}

impl TurnServer {
    /// Construct a TURN server descriptor.
    pub fn new(
        uri: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            uri: uri.into(),
            username: username.into(),
            password: password.into(),
        }
    }
}

/// The long-term-credential key: `MD5(username ":" realm ":" password)`
/// (RFC 8489 §9.2.3). This keyed value — not the password — is the HMAC key for
/// MESSAGE-INTEGRITY under the long-term mechanism.
pub fn long_term_key(username: &str, realm: &str, password: &str) -> [u8; 16] {
    md5::compute(format!("{username}:{realm}:{password}").as_bytes()).0
}

/// Is this datagram a ChannelData message (RFC 8656 §12.4) rather than STUN?
///
/// STUN messages start with two zero bits (types 0x0000–0x3FFF); ChannelData
/// starts with a channel number 0x4000–0x7FFF, so the top two bits are `01`.
pub fn is_channel_data(pkt: &[u8]) -> bool {
    !pkt.is_empty() && (pkt[0] & 0xC0) == 0x40
}

/// Frame `data` as a ChannelData message for channel `channel`.
///
/// Layout (RFC 8656 §12.4): channel number (2), length (2), then the data.
/// Over UDP no tail padding is required, so none is added.
pub fn encode_channel_data(channel: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&channel.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// Parse a ChannelData message into `(channel, payload)`; `None` if truncated.
pub fn decode_channel_data(pkt: &[u8]) -> Option<(u16, &[u8])> {
    if pkt.len() < 4 {
        return None;
    }
    let channel = u16::from_be_bytes([pkt[0], pkt[1]]);
    let len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    if 4 + len > pkt.len() {
        return None;
    }
    Some((channel, &pkt[4..4 + len]))
}

/// Resolve a `turn:`/`turns:`/bare `host:port` URI to a socket address.
async fn resolve_turn_server(uri: &str) -> Result<SocketAddr> {
    let hp = uri
        .strip_prefix("turns:")
        .or_else(|| uri.strip_prefix("turn:"))
        .unwrap_or(uri);
    // Drop any `?transport=udp` query the URI form allows (RFC 7065).
    let hp = hp.split('?').next().unwrap_or(hp);
    if let Ok(addr) = hp.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let mut addrs = tokio::net::lookup_host(hp)
        .await
        .map_err(|e| ForgeError::Ice(format!("Failed to resolve TURN server '{uri}': {e}")))?;
    addrs
        .next()
        .ok_or_else(|| ForgeError::Ice(format!("No addresses for TURN server '{uri}'")))
}

/// A live TURN allocation and the framing state to relay through it.
///
/// See the [module docs](self) for the flow and threading model.
pub struct TurnClient {
    socket: UdpSocket,
    server: SocketAddr,
    username: String,
    password: String,
    realm: String,
    nonce: Vec<u8>,
    key: [u8; 16],
    relayed_addr: SocketAddr,
    mapped_addr: Option<SocketAddr>,
    lifetime: u32,
    /// Peer → bound channel number (RFC 8656 §12).
    channels: HashMap<SocketAddr, u16>,
    next_channel: u16,
    /// Media (ChannelData / Data indications) that arrived while a control
    /// transaction was waiting for its response, held so it is not dropped.
    pending_media: Mutex<VecDeque<(SocketAddr, Vec<u8>)>>,
}

impl TurnClient {
    /// Allocate a relayed transport address on `server`.
    ///
    /// Binds a local UDP socket at `local_bind` (use `0.0.0.0:0` to let the OS
    /// choose), performs the unauthenticated→401→authenticated Allocate
    /// handshake, and returns a client holding the resulting allocation.
    pub async fn allocate(local_bind: SocketAddr, server: &TurnServer) -> Result<TurnClient> {
        let server_addr = resolve_turn_server(&server.uri).await?;
        let socket = UdpSocket::bind(local_bind)
            .await
            .map_err(|e| ForgeError::Ice(format!("Failed to bind TURN socket: {e}")))?;

        let mut client = TurnClient {
            socket,
            server: server_addr,
            username: server.username.clone(),
            password: server.password.clone(),
            realm: String::new(),
            nonce: Vec::new(),
            key: [0u8; 16],
            relayed_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            mapped_addr: None,
            lifetime: 0,
            channels: HashMap::new(),
            next_channel: CHANNEL_MIN,
            pending_media: Mutex::new(VecDeque::new()),
        };

        // 1) Unauthenticated Allocate — expect 401 with REALM + NONCE.
        let mut req = StunMessage::new_request(MessageType::AllocateRequest);
        req.attributes
            .push(StunAttribute::RequestedTransport(UDP_TRANSPORT));
        req.add_fingerprint();
        let resp = client.transact(&req).await?;

        let success = if resp.message_type.is_error() {
            let (code, reason) = resp.get_error_code().unwrap_or((0, ""));
            if code != 401 {
                return Err(ForgeError::Ice(format!(
                    "TURN Allocate rejected: {code} {reason}"
                )));
            }
            client.realm = resp
                .get_realm()
                .ok_or_else(|| ForgeError::Ice("401 without REALM".into()))?
                .to_string();
            client.nonce = resp
                .get_nonce()
                .ok_or_else(|| ForgeError::Ice("401 without NONCE".into()))?
                .to_vec();
            client.key = long_term_key(&client.username, &client.realm, &client.password);

            // 2) Authenticated Allocate.
            client
                .transact_authed(
                    MessageType::AllocateRequest,
                    vec![StunAttribute::RequestedTransport(UDP_TRANSPORT)],
                )
                .await?
        } else {
            // Server accepted an unauthenticated allocation (unusual, e.g. an
            // open test server); take it.
            resp
        };

        client.relayed_addr = success.get_xor_relayed_address().ok_or_else(|| {
            ForgeError::Ice("Allocate success without XOR-RELAYED-ADDRESS".into())
        })?;
        client.mapped_addr = success.get_xor_mapped_address();
        client.lifetime = success.get_lifetime().unwrap_or(DEFAULT_LIFETIME);

        debug!(
            "TURN allocation on {}: relayed={} mapped={:?} lifetime={}s",
            client.server, client.relayed_addr, client.mapped_addr, client.lifetime
        );
        Ok(client)
    }

    /// The relayed transport address — this allocation's `relay` candidate.
    pub fn relayed_addr(&self) -> SocketAddr {
        self.relayed_addr
    }

    /// The client's server-reflexive address as the TURN server saw it
    /// (XOR-MAPPED-ADDRESS in the Allocate response), if the server sent one.
    pub fn mapped_addr(&self) -> Option<SocketAddr> {
        self.mapped_addr
    }

    /// The local address of the socket the allocation was made from (the
    /// relay candidate's base).
    pub fn base_addr(&self) -> Result<SocketAddr> {
        self.socket
            .local_addr()
            .map_err(|e| ForgeError::Ice(format!("TURN socket local_addr: {e}")))
    }

    /// The granted allocation lifetime in seconds (refresh at about half).
    pub fn lifetime(&self) -> u32 {
        self.lifetime
    }

    /// Install a permission for `peer`'s IP so the relay will forward its
    /// traffic (RFC 8656 §9). Permissions expire after 300 s; re-call to renew.
    pub async fn create_permission(&mut self, peer: SocketAddr) -> Result<()> {
        self.transact_authed(
            MessageType::CreatePermissionRequest,
            vec![StunAttribute::XorPeerAddress(peer)],
        )
        .await?;
        debug!("TURN permission installed for {peer}");
        Ok(())
    }

    /// Bind a channel to `peer` (RFC 8656 §11) so media uses the 4-byte
    /// ChannelData header. Idempotent: re-binding an existing peer refreshes it.
    /// Returns the channel number. A ChannelBind also installs the permission.
    pub async fn bind_channel(&mut self, peer: SocketAddr) -> Result<u16> {
        let channel = match self.channels.get(&peer).copied() {
            Some(ch) => ch,
            None => {
                if self.next_channel > CHANNEL_MAX {
                    return Err(ForgeError::Ice("TURN channel numbers exhausted".into()));
                }
                self.next_channel
            }
        };
        self.transact_authed(
            MessageType::ChannelBindRequest,
            vec![
                StunAttribute::ChannelNumber(channel),
                StunAttribute::XorPeerAddress(peer),
            ],
        )
        .await?;
        if self.channels.insert(peer, channel).is_none() {
            self.next_channel += 1;
        }
        debug!("TURN channel 0x{channel:04x} bound to {peer}");
        Ok(channel)
    }

    /// Refresh the allocation with a new `lifetime` (0 deletes it). Returns the
    /// lifetime the server granted.
    pub async fn refresh(&mut self, lifetime: u32) -> Result<u32> {
        let resp = self
            .transact_authed(
                MessageType::RefreshRequest,
                vec![StunAttribute::Lifetime(lifetime)],
            )
            .await?;
        let granted = resp.get_lifetime().unwrap_or(lifetime);
        self.lifetime = granted;
        Ok(granted)
    }

    /// Relay `data` to `peer`. Uses a ChannelData frame when a channel is bound
    /// (the efficient path), otherwise a Send indication (RFC 8656 §10).
    pub async fn send_to(&self, peer: SocketAddr, data: &[u8]) -> Result<()> {
        let bytes = if let Some(&channel) = self.channels.get(&peer) {
            encode_channel_data(channel, data)
        } else {
            let mut msg = StunMessage::new_request(MessageType::SendIndication);
            msg.attributes.push(StunAttribute::XorPeerAddress(peer));
            msg.attributes.push(StunAttribute::Data(data.to_vec()));
            msg.serialize()
        };
        self.socket
            .send_to(&bytes, self.server)
            .await
            .map_err(|e| ForgeError::Ice(format!("TURN send to relay: {e}")))?;
        Ok(())
    }

    /// Receive one relayed datagram, returning `(len, peer)` with the peer that
    /// originated it. Unwraps ChannelData and Data indications; STUN traffic
    /// that is not relayed media (e.g. a stray response) is skipped.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        if let Some((peer, data)) = self.pending_media.lock().unwrap().pop_front() {
            let n = data.len().min(buf.len());
            buf[..n].copy_from_slice(&data[..n]);
            return Ok((n, peer));
        }
        let mut rbuf = vec![0u8; 2048];
        loop {
            let (len, from) = self
                .socket
                .recv_from(&mut rbuf)
                .await
                .map_err(|e| ForgeError::Ice(format!("TURN recv: {e}")))?;
            if from != self.server {
                continue;
            }
            let pkt = &rbuf[..len];
            if is_channel_data(pkt) {
                if let Some((channel, data)) = decode_channel_data(pkt) {
                    if let Some(peer) = self.peer_for_channel(channel) {
                        let n = data.len().min(buf.len());
                        buf[..n].copy_from_slice(&data[..n]);
                        return Ok((n, peer));
                    }
                }
                continue;
            }
            match StunMessage::parse(pkt) {
                Ok(m) if m.message_type == MessageType::DataIndication => {
                    if let (Some(peer), Some(data)) = (m.get_xor_peer_address(), m.get_data()) {
                        let n = data.len().min(buf.len());
                        buf[..n].copy_from_slice(&data[..n]);
                        return Ok((n, peer));
                    }
                }
                _ => {} // stray response / unrelated STUN — ignore
            }
        }
    }

    fn peer_for_channel(&self, channel: u16) -> Option<SocketAddr> {
        self.channels
            .iter()
            .find_map(|(peer, ch)| (*ch == channel).then_some(*peer))
    }

    /// Build, sign, and send an authenticated request, verifying the response's
    /// MESSAGE-INTEGRITY and transparently re-signing on `438 Stale Nonce`.
    async fn transact_authed(
        &mut self,
        method: MessageType,
        attrs: Vec<StunAttribute>,
    ) -> Result<StunMessage> {
        for _ in 0..=MAX_STALE_NONCE_RETRIES {
            let mut msg = StunMessage::new_request(method);
            for a in &attrs {
                msg.attributes.push(a.clone());
            }
            self.add_auth(&mut msg)?;
            let resp = self.transact(&msg).await?;

            if resp.message_type.is_error() {
                let (code, reason) = resp.get_error_code().unwrap_or((0, ""));
                // 401/438 carry a fresh NONCE (and possibly REALM) and no
                // MESSAGE-INTEGRITY (RFC 8489 §10.2.2); adopt them and retry.
                if code == 438 || code == 401 {
                    if let Some(n) = resp.get_nonce() {
                        self.nonce = n.to_vec();
                    }
                    if let Some(r) = resp.get_realm() {
                        if r != self.realm {
                            self.realm = r.to_string();
                            self.key = long_term_key(&self.username, &self.realm, &self.password);
                        }
                    }
                    continue;
                }
                return Err(ForgeError::Ice(format!(
                    "TURN {method:?} error: {code} {reason}"
                )));
            }

            // Success responses to authenticated requests are integrity
            // protected with the same long-term key.
            match resp.verify_message_integrity(&self.key) {
                Ok(true) => return Ok(resp),
                _ => {
                    return Err(ForgeError::Ice(format!(
                        "TURN {method:?} response failed MESSAGE-INTEGRITY"
                    )))
                }
            }
        }
        Err(ForgeError::Ice("TURN stale-nonce retries exhausted".into()))
    }

    /// Append USERNAME/REALM/NONCE, then MESSAGE-INTEGRITY (over all of the
    /// above and the method attributes) and FINGERPRINT.
    fn add_auth(&self, msg: &mut StunMessage) -> Result<()> {
        msg.attributes
            .push(StunAttribute::Username(self.username.clone()));
        msg.attributes
            .push(StunAttribute::Realm(self.realm.clone()));
        msg.attributes
            .push(StunAttribute::Nonce(self.nonce.clone()));
        msg.add_message_integrity(&self.key)?;
        msg.add_fingerprint();
        Ok(())
    }

    /// Send `request` and wait for the response with a matching transaction ID,
    /// retransmitting with RFC 8489 §6.2.1 backoff. Relayed media
    /// (ChannelData / Data indications) that arrives while waiting is stashed,
    /// not dropped.
    async fn transact(&self, request: &StunMessage) -> Result<StunMessage> {
        let bytes = request.serialize();
        let txid = request.transaction_id;
        let mut rto = INITIAL_RTO;
        let mut rbuf = vec![0u8; 2048];

        for _ in 0..MAX_ATTEMPTS {
            self.socket
                .send_to(&bytes, self.server)
                .await
                .map_err(|e| ForgeError::Ice(format!("TURN send: {e}")))?;

            let deadline = Instant::now() + rto;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break; // retransmit
                }
                match timeout(remaining, self.socket.recv_from(&mut rbuf)).await {
                    Ok(Ok((len, from))) => {
                        if from != self.server {
                            continue;
                        }
                        let pkt = &rbuf[..len];
                        if is_channel_data(pkt) {
                            if let Some((channel, data)) = decode_channel_data(pkt) {
                                if let Some(peer) = self.peer_for_channel(channel) {
                                    self.pending_media
                                        .lock()
                                        .unwrap()
                                        .push_back((peer, data.to_vec()));
                                }
                            }
                            continue;
                        }
                        match StunMessage::parse(pkt) {
                            Ok(m) if m.transaction_id == txid => return Ok(m),
                            Ok(m) if m.message_type == MessageType::DataIndication => {
                                if let (Some(peer), Some(data)) =
                                    (m.get_xor_peer_address(), m.get_data())
                                {
                                    self.pending_media
                                        .lock()
                                        .unwrap()
                                        .push_back((peer, data.to_vec()));
                                }
                            }
                            _ => {} // stale/other txn — keep waiting
                        }
                    }
                    Ok(Err(e)) => return Err(ForgeError::Ice(format!("TURN recv: {e}"))),
                    Err(_) => break, // timed out → retransmit
                }
            }
            rto *= 2;
        }
        warn!("TURN transaction to {} timed out", self.server);
        Err(ForgeError::Ice("TURN transaction timed out".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn long_term_key_matches_known_md5() {
        // MD5("user:realm:pass") and MD5("alice:forge.test:s3cr3t").
        assert_eq!(
            hex::encode(long_term_key("user", "realm", "pass")),
            "8493fbc53ba582fb4c044c456bdc40eb"
        );
        assert_eq!(
            hex::encode(long_term_key("alice", "forge.test", "s3cr3t")),
            "b5b9010305272a9109b8cf1cee331530"
        );
    }

    #[test]
    fn channel_data_frames_round_trip_and_discriminate() {
        let payload = b"\x80\x60 not-really-rtp but arbitrary bytes";
        let frame = encode_channel_data(0x4002, payload);

        // ChannelData is distinguishable from STUN by the top two bits.
        assert!(is_channel_data(&frame));
        assert!(!is_channel_data(
            &StunMessage::new_binding_request().serialize()
        ));

        let (channel, out) = decode_channel_data(&frame).unwrap();
        assert_eq!(channel, 0x4002);
        assert_eq!(out, payload);

        // Truncated header and a length that overruns the buffer both reject.
        assert!(decode_channel_data(&frame[..3]).is_none());
        let mut bad = frame.clone();
        bad[2] = 0xff;
        bad[3] = 0xff;
        assert!(decode_channel_data(&bad).is_none());
    }

    /// A minimal in-process TURN server: enough of RFC 8656 to drive the client
    /// through allocate → permission → channel-bind → send/recv, relaying a
    /// peer's traffic by echoing whatever the client relays back to it. Signs
    /// every response with the long-term key so the client's response-integrity
    /// check exercises a real signature.
    async fn mock_turn_server(
        socket: UdpSocket,
        username: String,
        realm: String,
        password: String,
    ) {
        let key = long_term_key(&username, &realm, &password);
        let nonce = b"testnonce1234".to_vec();
        let relayed: SocketAddr = "203.0.113.50:60000".parse().unwrap();
        let mut channels: HashMap<u16, SocketAddr> = HashMap::new();
        let mut buf = vec![0u8; 2048];

        loop {
            let (len, from) = match socket.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(_) => return,
            };
            let pkt = &buf[..len];

            if is_channel_data(pkt) {
                // Relay: the peer "replies" with the same bytes on the same channel.
                if let Some((channel, data)) = decode_channel_data(pkt) {
                    let echo = encode_channel_data(channel, data);
                    let _ = socket.send_to(&echo, from).await;
                }
                continue;
            }

            let msg = match StunMessage::parse(pkt) {
                Ok(m) => m,
                Err(_) => continue,
            };
            match msg.message_type {
                MessageType::AllocateRequest => {
                    if msg.get_username().is_none() {
                        // Unauthenticated → 401 challenge with REALM + NONCE.
                        let mut r = StunMessage::new_with_transaction(
                            MessageType::AllocateErrorResponse,
                            msg.transaction_id,
                        );
                        r.attributes
                            .push(StunAttribute::ErrorCode(401, "Unauthorized".into()));
                        r.attributes.push(StunAttribute::Realm(realm.clone()));
                        r.attributes.push(StunAttribute::Nonce(nonce.clone()));
                        let _ = socket.send_to(&r.serialize(), from).await;
                    } else {
                        let mut r = StunMessage::new_with_transaction(
                            MessageType::AllocateResponse,
                            msg.transaction_id,
                        );
                        r.attributes.push(StunAttribute::XorRelayedAddress(relayed));
                        r.attributes.push(StunAttribute::XorMappedAddress(from));
                        r.attributes.push(StunAttribute::Lifetime(600));
                        r.add_message_integrity(&key).unwrap();
                        let _ = socket.send_to(&r.serialize(), from).await;
                    }
                }
                MessageType::CreatePermissionRequest => {
                    let mut r = StunMessage::new_with_transaction(
                        MessageType::CreatePermissionResponse,
                        msg.transaction_id,
                    );
                    r.add_message_integrity(&key).unwrap();
                    let _ = socket.send_to(&r.serialize(), from).await;
                }
                MessageType::ChannelBindRequest => {
                    let channel = msg.attributes.iter().find_map(|a| match a {
                        StunAttribute::ChannelNumber(c) => Some(*c),
                        _ => None,
                    });
                    if let (Some(c), Some(peer)) = (channel, msg.get_xor_peer_address()) {
                        channels.insert(c, peer);
                    }
                    let mut r = StunMessage::new_with_transaction(
                        MessageType::ChannelBindResponse,
                        msg.transaction_id,
                    );
                    r.add_message_integrity(&key).unwrap();
                    let _ = socket.send_to(&r.serialize(), from).await;
                }
                MessageType::RefreshRequest => {
                    let lifetime = msg.get_lifetime().unwrap_or(600);
                    let mut r = StunMessage::new_with_transaction(
                        MessageType::RefreshResponse,
                        msg.transaction_id,
                    );
                    r.attributes.push(StunAttribute::Lifetime(lifetime));
                    r.add_message_integrity(&key).unwrap();
                    let _ = socket.send_to(&r.serialize(), from).await;
                }
                MessageType::SendIndication => {
                    // Relay via a Data indication (the peer "replies" identically).
                    if let (Some(peer), Some(data)) = (msg.get_xor_peer_address(), msg.get_data()) {
                        let mut ind = StunMessage::new_request(MessageType::DataIndication);
                        ind.attributes.push(StunAttribute::XorPeerAddress(peer));
                        ind.attributes.push(StunAttribute::Data(data.to_vec()));
                        let _ = socket.send_to(&ind.serialize(), from).await;
                    }
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn allocate_permission_channel_send_recv_against_mock() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        tokio::spawn(mock_turn_server(
            server,
            "alice".into(),
            "forge.test".into(),
            "s3cr3t".into(),
        ));

        let ts = TurnServer::new(format!("turn:{server_addr}"), "alice", "s3cr3t");
        let mut client = TurnClient::allocate("127.0.0.1:0".parse().unwrap(), &ts)
            .await
            .expect("allocate");

        assert_eq!(
            client.relayed_addr(),
            "203.0.113.50:60000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(client.lifetime(), 600);
        assert!(client.mapped_addr().is_some());

        let peer: SocketAddr = "198.51.100.7:5004".parse().unwrap();
        client.create_permission(peer).await.expect("permission");

        // Before a channel is bound: Send indication ↔ Data indication path.
        client
            .send_to(peer, b"hello-indication")
            .await
            .expect("send");
        let mut buf = [0u8; 256];
        let (n, from) = timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .expect("recv timed out")
            .expect("recv");
        assert_eq!(from, peer);
        assert_eq!(&buf[..n], b"hello-indication");

        // After ChannelBind: ChannelData framing.
        let channel = client.bind_channel(peer).await.expect("channel bind");
        assert!((CHANNEL_MIN..=CHANNEL_MAX).contains(&channel));
        client.send_to(peer, b"hello-channel").await.expect("send");
        let (n, from) = timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .expect("recv timed out")
            .expect("recv");
        assert_eq!(from, peer);
        assert_eq!(&buf[..n], b"hello-channel");

        // Refresh keeps the allocation alive.
        assert_eq!(client.refresh(600).await.expect("refresh"), 600);
    }

    /// Live interop against a real TURN server (e.g. coturn). Ignored by
    /// default; run with credentials in the environment:
    ///
    /// ```text
    /// FORGE_TURN_URI=turn:turn.example.org:3478 \
    /// FORGE_TURN_USER=user FORGE_TURN_PASS=pass \
    ///   cargo test -p forge-ice --ignored live_turn_allocate -- --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires a live TURN server; set FORGE_TURN_URI/USER/PASS"]
    async fn live_turn_allocate() {
        let uri = std::env::var("FORGE_TURN_URI").expect("FORGE_TURN_URI");
        let user = std::env::var("FORGE_TURN_USER").unwrap_or_default();
        let pass = std::env::var("FORGE_TURN_PASS").unwrap_or_default();
        let ts = TurnServer::new(uri, user, pass);

        let mut client = TurnClient::allocate("0.0.0.0:0".parse().unwrap(), &ts)
            .await
            .expect("allocate against live TURN server");
        println!(
            "relayed={} mapped={:?} lifetime={}s",
            client.relayed_addr(),
            client.mapped_addr(),
            client.lifetime()
        );
        assert!(client.lifetime() > 0);
        // Clean up: LIFETIME=0 deletes the allocation.
        let _ = client.refresh(0).await;
    }

    /// Live relay round-trip through a real TURN server: allocate, permit +
    /// channel-bind a loopback peer, then relay bytes both directions through
    /// the server. Ignored by default; needs a server that permits loopback
    /// peers (coturn: `--allow-loopback-peers`).
    #[tokio::test]
    #[ignore = "requires a live TURN server that allows loopback peers"]
    async fn live_turn_relay_roundtrip() {
        let uri = std::env::var("FORGE_TURN_URI").expect("FORGE_TURN_URI");
        let user = std::env::var("FORGE_TURN_USER").unwrap_or_default();
        let pass = std::env::var("FORGE_TURN_PASS").unwrap_or_default();
        let ts = TurnServer::new(uri, user, pass);

        let mut client = TurnClient::allocate("127.0.0.1:0".parse().unwrap(), &ts)
            .await
            .expect("allocate");
        let relayed = client.relayed_addr();
        println!("allocated relay {relayed}");

        // A plain UDP peer on loopback.
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = peer.local_addr().unwrap();

        client
            .create_permission(peer_addr)
            .await
            .expect("permission");
        let channel = client.bind_channel(peer_addr).await.expect("channel bind");
        println!("channel 0x{channel:04x} bound to {peer_addr}");

        // client -> relay -> peer
        client
            .send_to(peer_addr, b"ping-through-turn")
            .await
            .expect("send");
        let mut pbuf = [0u8; 256];
        let (pn, pfrom) = timeout(Duration::from_secs(3), peer.recv_from(&mut pbuf))
            .await
            .expect("peer recv timed out")
            .unwrap();
        assert_eq!(&pbuf[..pn], b"ping-through-turn");
        assert_eq!(
            pfrom, relayed,
            "peer sees the relayed address as the source"
        );

        // peer -> relay -> client
        peer.send_to(b"pong-through-turn", relayed)
            .await
            .expect("peer send");
        let mut cbuf = [0u8; 256];
        let (cn, cfrom) = timeout(Duration::from_secs(3), client.recv_from(&mut cbuf))
            .await
            .expect("client recv timed out")
            .unwrap();
        assert_eq!(&cbuf[..cn], b"pong-through-turn");
        assert_eq!(cfrom, peer_addr, "client recovers the true peer address");

        let _ = client.refresh(0).await;
        println!("relay round-trip through TURN ok");
    }
}
