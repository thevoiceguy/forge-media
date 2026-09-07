//! The media transport behind a peer connection: one UDP socket carrying
//! STUN (ICE checks and keepalives), DTLS (key exchange) and SRTP/SRTCP,
//! demultiplexed by first byte (RFC 7983).
//!
//! Design: a single owner of the socket. One task reads the socket and hands
//! every datagram to [`Inner::handle_packet`]; one task ticks
//! [`Inner::tick`] for pacing, retransmission, nomination, DTLS timers and
//! keepalives. Both produce "bytes to send" lists that are flushed after the
//! lock is released, so no lock is ever held across socket I/O. Connectivity
//! checks are sent from the same socket the media uses, which is what lets
//! STUN responses, the peer's DTLS flights and SRTP all arrive in one place —
//! the previous implementation opened a second `SO_REUSEPORT` socket per
//! check and could lose the peer's packets to it.
//!
//! All local candidates share the one socket, so the checklist is keyed by
//! remote transport address rather than by (local, remote) pair.
//!
//! Media: one SRTP context serves every stream on the socket (BUNDLE), so
//! audio and video share keys but keep their own SSRC, sequence space and
//! rollover counter. Inbound RTP is sorted into audio or video by payload
//! type from the negotiated map; inbound RTCP is parsed. The transport
//! keeps RFC 3550 reception statistics per remote SSRC and sends a
//! compound SR/RR + SDES on [`TransportConfig::rtcp_interval`], and
//! prefixes the same report to any feedback the owner sends, so every
//! RTCP packet on the wire is a proper compound (RFC 3550 §6.1).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use forge_ice::candidate::{CandidatePair, CandidateType, PairState};
use forge_ice::stun::{MessageType, StunMessage, StunServer, StunServerResponse};
use forge_ice::{IceAgent, IceCandidate, Protocol, TurnClient, TurnInbound, TurnServer};
use forge_rtp::dtls::{DtlsCertificate, DtlsConnection, DtlsContext, DtlsRole, DtlsState};
use forge_rtp::{
    ReceiverReport, RtcpPacket, RtpPacket, SdesItem, SenderStats, SourceDescription, SourceStats,
    SrtpContext,
};
use parking_lot::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, trace, warn};

use crate::{ConnectionState, Result, WebRtcError};

/// Events the transport reports to its owner.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// A local candidate became available (trickle it to the peer).
    LocalCandidate(IceCandidate),
    /// Gathering finished; no more `LocalCandidate` events will follow.
    GatheringComplete,
    /// ICE nominated a pair.
    IceConnected {
        /// Local socket address.
        local: SocketAddr,
        /// Remote transport address of the nominated pair.
        remote: SocketAddr,
    },
    /// DTLS completed and SRTP keys are installed: media can flow.
    Connected,
    /// An authenticated, decrypted inbound audio RTP packet (or one whose
    /// payload type the negotiated map does not know).
    Rtp(RtpPacket),
    /// An authenticated, decrypted inbound video RTP packet.
    VideoRtp(RtpPacket),
    /// The sub-packets of an authenticated, decrypted inbound RTCP
    /// compound packet, in order. Sender reports have already updated the
    /// reception statistics; feedback is the owner's to act on.
    Rtcp(Vec<RtcpPacket>),
    /// The transport failed; no recovery is attempted (ICE restart is
    /// deliberately unsupported in this version).
    Failed(String),
    /// The transport was closed locally.
    Closed,
}

/// Which stream a payload type belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// The audio section.
    Audio,
    /// The video section.
    Video,
}

/// One negotiated payload type: which stream it belongs to and its RTP
/// clock rate, for sorting inbound packets and measuring jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadMapping {
    /// The payload type.
    pub payload_type: u8,
    /// The stream it belongs to.
    pub kind: MediaKind,
    /// Its RTP clock rate in Hz.
    pub clock_rate: u32,
}

/// The most remote SSRCs one transport keeps statistics for. A peer has
/// two (audio and video); the bound stops a misbehaving one from growing
/// the map.
const MAX_SOURCES: usize = 32;

/// ICE role (RFC 8445 §6.1.1): the offerer controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceRole {
    /// Controlling: runs nomination.
    Controlling,
    /// Controlled: follows the peer's nomination.
    Controlled,
}

/// Transport tunables.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// STUN servers for server-reflexive candidates (`stun:host:port`).
    pub stun_servers: Vec<String>,
    /// TURN servers for relay candidates — the fallback when STUN cannot punch
    /// a path (symmetric NAT on both ends). Empty = no relay candidates.
    pub turn_servers: Vec<TurnServer>,
    /// Tick period; at most one new connectivity check is sent per tick
    /// (RFC 8445 §6.1.4.2 Ta pacing).
    pub check_interval: Duration,
    /// STUN retransmission timeout.
    pub rto: Duration,
    /// Retransmissions before a check fails (RFC 8489 §6.2.1 Rc).
    pub max_attempts: u8,
    /// Time allowed from the first remote description to a nominated pair.
    pub ice_timeout: Duration,
    /// Time allowed for the DTLS handshake once ICE is nominated.
    pub dtls_timeout: Duration,
    /// Keepalive / consent-freshness interval on the nominated pair.
    pub keepalive: Duration,
    /// Capacity of the event channel.
    pub event_capacity: usize,
    /// How often a compound SR/RR + SDES goes out once media can flow.
    /// One second suits a two-party session with a couple of streams
    /// (RFC 3550 §6.2 would allow more); browsers use our RR for loss-based
    /// rate control and our SR for lip sync, so it must not be rare.
    pub rtcp_interval: Duration,
    /// UDP port to bind the ICE socket to; `0` lets the OS choose.
    ///
    /// A WebRTC connection needs exactly one socket (BUNDLE plus
    /// `a=rtcp-mux`), and by default it lands wherever the OS puts it.
    /// That is fine for a browser, but a *server* usually has a media
    /// port range its operator has opened in a firewall and sized as a
    /// capacity budget. Pinning the port lets that server draw the
    /// WebRTC socket from the same range and the same accounting as
    /// its RTP sessions instead of quietly using an ephemeral port
    /// outside both.
    ///
    /// The caller owns the choice, including its failure: if the port
    /// is already in use, [`Transport::new`] fails rather than falling
    /// back to an ephemeral one, because silently landing outside the
    /// range would defeat the reason for asking.
    ///
    /// This binds the *host* socket only. A TURN allocation gathers
    /// through its own socket (see [`TransportConfig::turn_servers`]),
    /// which the OS still assigns; with no TURN servers configured
    /// there is exactly one socket and this is its port.
    pub local_port: u16,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            stun_servers: vec![],
            turn_servers: vec![],
            check_interval: Duration::from_millis(20),
            rto: Duration::from_millis(500),
            max_attempts: 7,
            ice_timeout: Duration::from_secs(30),
            dtls_timeout: Duration::from_secs(15),
            keepalive: Duration::from_millis(2500),
            event_capacity: 512,
            rtcp_interval: Duration::from_secs(1),
            local_port: 0,
        }
    }
}

type Outgoing = Vec<(Vec<u8>, SocketAddr)>;

/// A command to a TURN actor task (the task owns the `TurnClient` and is the
/// sole reader of its socket, so control ops and relayed media never race two
/// reads).
enum TurnCmd {
    /// Relay `bytes` to `peer` (a check, DTLS flight, keepalive or media).
    Send(SocketAddr, Vec<u8>),
    /// Install a permission so the relay will forward this peer both ways.
    Permission(SocketAddr),
}

/// Outer-side handle to one TURN allocation/actor.
struct TurnHandle {
    tx: mpsc::Sender<TurnCmd>,
    /// The relayed transport address — this allocation's `relay` candidate.
    relayed_addr: SocketAddr,
}

/// Per-remote-address checklist entry.
struct RemoteEntry {
    cand: IceCandidate,
    addr: SocketAddr,
    priority: u64,
    state: PairState,
    /// A triggered check is due (peer's request reached us on this address).
    triggered: bool,
    attempts: u8,
    last_sent: Option<Instant>,
    tx_id: Option<[u8; 12]>,
    last_request: Vec<u8>,
    /// Our outstanding check carries USE-CANDIDATE.
    nominating: bool,
    /// The pair is nominated (by us, or by the controlling peer).
    nominated: bool,
}

struct Inner {
    cfg: TransportConfig,
    agent: IceAgent,
    local_addr: SocketAddr,
    local_candidates: Vec<IceCandidate>,
    gathering_complete: bool,
    /// Outstanding plain Binding Requests to STUN servers (srflx gathering).
    stun_pending: HashMap<[u8; 12], (SocketAddr, Instant)>,

    role: IceRole,
    remote_creds: Option<(String, String)>,
    stun_server: Option<StunServer>,
    remotes: Vec<RemoteEntry>,
    selected: Option<SocketAddr>,
    ice_started_at: Option<Instant>,
    last_keepalive: Option<Instant>,

    cert: Arc<DtlsCertificate>,
    dtls_role: Option<DtlsRole>,
    remote_fingerprint: Option<String>,
    dtls: Option<DtlsConnection>,
    dtls_peer: Option<SocketAddr>,
    dtls_started_at: Option<Instant>,
    last_dtls_timer: Instant,

    srtp: Option<SrtpContext>,
    /// Our audio SSRC (the one an RR is sent from).
    ssrc: u32,
    /// Our video SSRC.
    video_ssrc: u32,
    /// Audio sequence number (video packets arrive with their own).
    seq: u16,
    cname: String,
    /// Negotiated payload types → stream and clock.
    payload_map: HashMap<u8, (MediaKind, u32)>,
    /// Reception statistics per remote SSRC.
    sources: HashMap<u32, SourceStats>,
    /// Transmission statistics: `[audio, video]`.
    senders: [SenderStats; 2],
    last_rtcp: Option<Instant>,

    state: Arc<Mutex<ConnectionState>>,
    events: mpsc::Sender<TransportEvent>,
    closed: bool,
    rtp_dropped: u64,
}

impl Inner {
    fn emit(&mut self, ev: TransportEvent) {
        let is_media = matches!(
            ev,
            TransportEvent::Rtp(_) | TransportEvent::VideoRtp(_) | TransportEvent::Rtcp(_)
        );
        if self.events.try_send(ev).is_err() && is_media {
            self.rtp_dropped += 1;
            if self.rtp_dropped == 1 || self.rtp_dropped % 1000 == 0 {
                warn!(
                    "media event channel full: {} inbound packets dropped",
                    self.rtp_dropped
                );
            }
        }
    }

    fn state(&self) -> ConnectionState {
        *self.state.lock()
    }

    fn set_state(&self, s: ConnectionState) {
        *self.state.lock() = s;
    }

    fn fail(&mut self, why: impl Into<String>) {
        let why = why.into();
        if matches!(
            self.state(),
            ConnectionState::Failed | ConnectionState::Closed
        ) {
            return;
        }
        warn!("transport failed: {why}");
        self.set_state(ConnectionState::Failed);
        self.emit(TransportEvent::Failed(why));
    }

    fn best_local(&self) -> Option<&IceCandidate> {
        self.local_candidates.iter().max_by_key(|c| c.priority)
    }

    fn local_priority(&self) -> u32 {
        self.best_local()
            .map(|c| c.priority)
            .unwrap_or_else(|| IceCandidate::compute_priority(CandidateType::Host, 65_535, 1))
    }

    // ------------------------------------------------------------ candidates

    fn add_local_candidate(&mut self, cand: IceCandidate) {
        if self
            .local_candidates
            .iter()
            .any(|c| c.ip == cand.ip && c.port == cand.port && c.typ == cand.typ)
        {
            return;
        }
        debug!("local candidate {cand}");
        self.local_candidates.push(cand.clone());
        self.emit(TransportEvent::LocalCandidate(cand));
    }

    fn add_remote_candidate(&mut self, cand: IceCandidate) {
        if cand.protocol != Protocol::Udp || cand.component != 1 {
            return;
        }
        if cand.ip.is_ipv4() != self.local_addr.is_ipv4() {
            return;
        }
        let addr = SocketAddr::new(cand.ip, cand.port);
        if let Some(e) = self.remotes.iter_mut().find(|e| e.addr == addr) {
            // A signalled candidate replaces a peer-reflexive placeholder.
            if e.cand.typ == CandidateType::PeerReflexive
                && cand.typ != CandidateType::PeerReflexive
            {
                e.cand = cand;
            }
            return;
        }
        let local = self.best_local().cloned().unwrap_or_else(|| {
            IceCandidate::new_host(
                "0".into(),
                1,
                Protocol::Udp,
                self.local_addr.ip(),
                self.local_addr.port(),
                65_535,
            )
        });
        let priority = CandidatePair::new(local, cand.clone()).priority;
        debug!("remote candidate {cand}");
        self.remotes.push(RemoteEntry {
            cand,
            addr,
            priority,
            state: PairState::Waiting,
            triggered: false,
            attempts: 0,
            last_sent: None,
            tx_id: None,
            last_request: Vec::new(),
            nominating: false,
            nominated: false,
        });
        self.remotes.sort_by_key(|e| std::cmp::Reverse(e.priority));
    }

    fn peer_reflexive(&mut self, addr: SocketAddr, priority: Option<u32>) {
        if self.remotes.iter().any(|e| e.addr == addr) {
            return;
        }
        let mut cand = IceCandidate::new_host(
            format!("prflx{}", self.remotes.len()),
            1,
            Protocol::Udp,
            addr.ip(),
            addr.port(),
            0,
        );
        cand.typ = CandidateType::PeerReflexive;
        cand.priority = priority
            .unwrap_or_else(|| IceCandidate::compute_priority(CandidateType::PeerReflexive, 0, 1));
        info!("learned peer-reflexive remote candidate {addr}");
        self.add_remote_candidate(cand);
    }

    // ------------------------------------------------------------ checks

    fn build_check(&self, entry: &RemoteEntry, nominate: bool) -> Option<StunMessage> {
        let (remote_ufrag, remote_pwd) = self.remote_creds.as_ref()?;
        let (local_ufrag, _) = self.agent.get_local_credentials();
        let mut msg = StunMessage::new_binding_request();
        msg.add_username(format!("{remote_ufrag}:{local_ufrag}"));
        // PRIORITY: what a peer-reflexive candidate learned from this check
        // would have (RFC 8445 §7.1.1).
        let _ = entry;
        msg.add_priority(IceCandidate::compute_priority(
            CandidateType::PeerReflexive,
            (self.local_priority() >> 8) as u16,
            1,
        ));
        match self.role {
            IceRole::Controlling => {
                msg.add_ice_controlling(self.agent.tie_breaker());
                if nominate {
                    msg.add_use_candidate();
                }
            }
            IceRole::Controlled => msg.add_ice_controlled(self.agent.tie_breaker()),
        }
        msg.add_message_integrity(remote_pwd.as_bytes()).ok()?;
        msg.add_fingerprint();
        Some(msg)
    }

    fn send_check(&mut self, idx: usize, nominate: bool, now: Instant, out: &mut Outgoing) {
        let Some(msg) = self.build_check(&self.remotes[idx], nominate) else {
            return;
        };
        let bytes = msg.serialize();
        let e = &mut self.remotes[idx];
        e.tx_id = Some(msg.transaction_id);
        e.last_request = bytes.clone();
        e.last_sent = Some(now);
        e.attempts = 1;
        e.state = PairState::InProgress;
        e.triggered = false;
        e.nominating = nominate;
        trace!("check → {} (nominate={nominate}, attempt 1)", e.addr);
        out.push((bytes, e.addr));
    }

    fn select(&mut self, addr: SocketAddr, now: Instant, out: &mut Outgoing) {
        if self.selected.is_some() {
            return;
        }
        self.selected = Some(addr);
        self.last_keepalive = Some(now);
        for e in &mut self.remotes {
            if e.addr == addr {
                e.nominated = true;
            }
        }
        info!("ICE nominated {} ↔ {}", self.local_addr, addr);
        self.emit(TransportEvent::IceConnected {
            local: self.local_addr,
            remote: addr,
        });
        if let Err(e) = self.start_dtls(addr, now, out) {
            self.fail(format!("DTLS start: {e}"));
        }
    }

    fn tick(&mut self, now: Instant) -> Outgoing {
        let mut out = Outgoing::new();
        if self.closed {
            return out;
        }

        // --- server-reflexive gathering timeouts
        if !self.stun_pending.is_empty() {
            self.stun_pending.retain(|_, (server, sent)| {
                let keep = now.duration_since(*sent) < Duration::from_secs(3);
                if !keep {
                    debug!("STUN server {server} did not answer");
                }
                keep
            });
            if self.stun_pending.is_empty() {
                self.finish_gathering();
            }
        }

        // --- ICE
        if self.remote_creds.is_some() && self.state() != ConnectionState::Failed {
            if self.selected.is_none() {
                // Retransmit or fail in-progress checks.
                for i in 0..self.remotes.len() {
                    let e = &self.remotes[i];
                    if e.state != PairState::InProgress {
                        continue;
                    }
                    let due = e
                        .last_sent
                        .map(|t| now.duration_since(t) >= self.cfg.rto)
                        .unwrap_or(true);
                    if !due {
                        continue;
                    }
                    let e = &mut self.remotes[i];
                    if e.attempts >= self.cfg.max_attempts {
                        debug!("check to {} failed after {} attempts", e.addr, e.attempts);
                        e.state = PairState::Failed;
                        e.tx_id = None;
                        e.nominating = false;
                    } else {
                        e.attempts += 1;
                        e.last_sent = Some(now);
                        trace!("check → {} (retransmit {})", e.addr, e.attempts);
                        out.push((e.last_request.clone(), e.addr));
                    }
                }
                // One new check per tick: triggered first, then by priority.
                let next = self
                    .remotes
                    .iter()
                    .position(|e| e.triggered && e.state != PairState::InProgress)
                    .or_else(|| {
                        self.remotes
                            .iter()
                            .position(|e| matches!(e.state, PairState::Waiting | PairState::Frozen))
                    });
                if let Some(i) = next {
                    self.send_check(i, false, now, &mut out);
                }
                if let Some(started) = self.ice_started_at {
                    if now.duration_since(started) > self.cfg.ice_timeout {
                        self.fail("ICE timeout: no candidate pair was nominated");
                    }
                }
            } else if let Some(addr) = self.selected {
                let due = self
                    .last_keepalive
                    .map(|t| now.duration_since(t) >= self.cfg.keepalive)
                    .unwrap_or(true);
                if due {
                    self.last_keepalive = Some(now);
                    if let Some(i) = self.remotes.iter().position(|e| e.addr == addr) {
                        if let Some(msg) = self.build_check(&self.remotes[i], false) {
                            self.remotes[i].tx_id = Some(msg.transaction_id);
                            out.push((msg.serialize(), addr));
                        }
                    }
                }
            }
        }

        // --- DTLS timers
        if let Some(dtls) = self.dtls.as_mut() {
            if dtls.state() == DtlsState::Handshaking {
                if now.duration_since(self.last_dtls_timer) >= Duration::from_millis(100) {
                    self.last_dtls_timer = now;
                    match dtls.handle_timeout() {
                        Ok(bytes) if !bytes.is_empty() => {
                            if let Some(peer) = self.dtls_peer {
                                out.push((bytes, peer));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => self.fail(format!("DTLS: {e}")),
                    }
                }
                if let Some(started) = self.dtls_started_at {
                    if now.duration_since(started) > self.cfg.dtls_timeout {
                        self.fail("DTLS handshake timeout");
                    }
                }
            }
        }

        // --- RTCP reports
        if self.srtp.is_some() && self.state() == ConnectionState::Connected {
            let due = self
                .last_rtcp
                .map(|t| now.duration_since(t) >= self.cfg.rtcp_interval)
                .unwrap_or(true);
            if due {
                self.last_rtcp = Some(now);
                match self.protect_rtcp(&[], now) {
                    Ok(pkt) => out.push(pkt),
                    Err(e) => debug!("RTCP report: {e}"),
                }
            }
        }
        out
    }

    fn finish_gathering(&mut self) {
        if !self.gathering_complete {
            self.gathering_complete = true;
            debug!(
                "gathering complete: {} local candidates",
                self.local_candidates.len()
            );
            self.emit(TransportEvent::GatheringComplete);
        }
    }

    // ------------------------------------------------------------ inbound

    fn handle_packet(&mut self, data: &[u8], from: SocketAddr, now: Instant) -> Outgoing {
        let mut out = Outgoing::new();
        if self.closed || data.is_empty() {
            return out;
        }
        match data[0] {
            0..=3 if data.len() >= 20 => self.handle_stun(data, from, now, &mut out),
            20..=63 => self.handle_dtls(data, from, now, &mut out),
            128..=191 => self.handle_srtp(data, from),
            b => trace!("ignoring packet from {from} with first byte {b}"),
        }
        out
    }

    fn handle_stun(&mut self, data: &[u8], from: SocketAddr, now: Instant, out: &mut Outgoing) {
        let Ok(msg) = StunMessage::parse(data) else {
            return;
        };
        match msg.message_type {
            MessageType::BindingRequest => {
                let Some(server) = self.stun_server.as_ref() else {
                    trace!("check from {from} before remote credentials; dropped");
                    return;
                };
                match server.handle_binding_request(data, from) {
                    StunServerResponse::Respond(resp) => out.push((resp.serialize(), from)),
                    StunServerResponse::Drop(why) => {
                        debug!("dropped binding request from {from}: {why}");
                        return;
                    }
                }
                self.peer_reflexive(from, msg.get_priority());
                let use_candidate = msg.has_use_candidate();
                let Some(i) = self.remotes.iter().position(|e| e.addr == from) else {
                    return;
                };
                let e = &mut self.remotes[i];
                if use_candidate && self.role == IceRole::Controlled {
                    e.nominated = true;
                }
                match e.state {
                    PairState::Succeeded => {
                        if e.nominated && self.selected.is_none() {
                            self.select(from, now, out);
                        }
                    }
                    PairState::InProgress => {}
                    _ => {
                        // Triggered check (RFC 8445 §7.3.1.4).
                        e.triggered = true;
                        e.state = PairState::Waiting;
                    }
                }
            }
            MessageType::BindingResponse => {
                // srflx gathering response?
                if let Some((server, _)) = self.stun_pending.remove(&msg.transaction_id) {
                    if let Some(mapped) = msg.get_xor_mapped_address() {
                        if let Some(base) = self
                            .local_candidates
                            .iter()
                            .find(|c| {
                                c.typ == CandidateType::Host && c.ip.is_ipv4() == mapped.is_ipv4()
                            })
                            .cloned()
                        {
                            let cand = IceCandidate::new_server_reflexive(
                                format!("srflx{}", self.local_candidates.len()),
                                1,
                                Protocol::Udp,
                                mapped.ip(),
                                mapped.port(),
                                base.ip,
                                base.port,
                                base.get_local_preference(),
                            );
                            debug!("server-reflexive {mapped} via {server}");
                            self.add_local_candidate(cand);
                        }
                    }
                    if self.stun_pending.is_empty() {
                        self.finish_gathering();
                    }
                    return;
                }
                let Some(i) = self
                    .remotes
                    .iter()
                    .position(|e| e.tx_id == Some(msg.transaction_id))
                else {
                    trace!("binding response from {from} with unknown transaction");
                    return;
                };
                let Some((_, remote_pwd)) = self.remote_creds.as_ref() else {
                    return;
                };
                if !matches!(
                    msg.verify_message_integrity(remote_pwd.as_bytes()),
                    Ok(true)
                ) {
                    debug!("binding response from {from} failed MESSAGE-INTEGRITY");
                    return;
                }
                let e = &mut self.remotes[i];
                if self.selected.is_some() {
                    // Keepalive / consent response.
                    e.tx_id = None;
                    return;
                }
                let was_nominating = e.nominating;
                e.state = PairState::Succeeded;
                e.tx_id = None;
                e.nominating = false;
                e.triggered = false;
                let addr = e.addr;
                let nominated = e.nominated;
                match self.role {
                    IceRole::Controlling => {
                        if was_nominating {
                            self.select(addr, now, out);
                        } else if !self.remotes.iter().any(|e| e.nominating) {
                            // Regular nomination of the first valid pair
                            // (RFC 8445 §8.1.1): re-check with USE-CANDIDATE.
                            self.send_check(i, true, now, out);
                        }
                    }
                    IceRole::Controlled => {
                        if nominated {
                            self.select(addr, now, out);
                        }
                    }
                }
            }
            MessageType::BindingErrorResponse => {
                if let Some(e) = self
                    .remotes
                    .iter_mut()
                    .find(|e| e.tx_id == Some(msg.transaction_id))
                {
                    debug!("binding error response from {from}");
                    e.state = PairState::Failed;
                    e.tx_id = None;
                    e.nominating = false;
                }
            }
            // TURN message types (Allocate/Refresh/…) never arrive on this ICE
            // socket — the TurnClient owns the TURN transaction on its own
            // socket — so anything else here is unexpected and ignored.
            _ => {}
        }
    }

    // ------------------------------------------------------------ DTLS

    fn start_dtls(&mut self, peer: SocketAddr, now: Instant, out: &mut Outgoing) -> Result<()> {
        if self.dtls.is_some() {
            return Ok(());
        }
        let role = self
            .dtls_role
            .ok_or_else(|| WebRtcError::InvalidState("DTLS role not negotiated".into()))?;
        let fp = self
            .remote_fingerprint
            .clone()
            .ok_or_else(|| WebRtcError::InvalidState("remote fingerprint unknown".into()))?;
        let ctx = DtlsContext::new(self.cert.clone(), role)
            .map_err(|e| WebRtcError::DtlsError(e.to_string()))?;
        let mut conn = DtlsConnection::new(&ctx, role, Some(fp))
            .map_err(|e| WebRtcError::DtlsError(e.to_string()))?;
        info!("DTLS handshake starting as {role:?} with {peer}");
        let (complete, flight) = conn
            .handshake(None)
            .map_err(|e| WebRtcError::DtlsError(e.to_string()))?;
        if !flight.is_empty() {
            out.push((flight, peer));
        }
        self.dtls_peer = Some(peer);
        self.dtls_started_at = Some(now);
        self.last_dtls_timer = now;
        self.dtls = Some(conn);
        if complete {
            self.finish_dtls()?;
        }
        Ok(())
    }

    fn handle_dtls(&mut self, data: &[u8], from: SocketAddr, now: Instant, out: &mut Outgoing) {
        if self.dtls.is_none() {
            // The peer (DTLS client) may start before our nomination response
            // reaches us; accept from any known remote address.
            if self.remotes.iter().any(|e| e.addr == from)
                && self.dtls_role == Some(DtlsRole::Server)
            {
                if let Err(e) = self.start_dtls(from, now, out) {
                    self.fail(format!("DTLS start: {e}"));
                    return;
                }
            } else {
                trace!("DTLS from {from} before the transport is ready; dropped");
                return;
            }
        }
        let Some(dtls) = self.dtls.as_mut() else {
            return;
        };
        if dtls.state() == DtlsState::Connected {
            // Post-handshake records (alerts, renegotiation) are not handled.
            return;
        }
        match dtls.handshake(Some(data)) {
            Ok((complete, flight)) => {
                if !flight.is_empty() {
                    out.push((flight, from));
                }
                if complete {
                    if let Err(e) = self.finish_dtls() {
                        self.fail(format!("DTLS: {e}"));
                    }
                }
            }
            Err(e) => self.fail(format!("DTLS handshake: {e}")),
        }
    }

    fn finish_dtls(&mut self) -> Result<()> {
        let dtls = self
            .dtls
            .as_ref()
            .ok_or_else(|| WebRtcError::InvalidState("no DTLS connection".into()))?;
        let (client_keys, server_keys) = dtls
            .export_srtp_keys()
            .map_err(|e| WebRtcError::DtlsError(e.to_string()))?;
        let (local, remote) = match dtls.role() {
            DtlsRole::Client => (client_keys, server_keys),
            DtlsRole::Server => (server_keys, client_keys),
        };
        info!(
            "DTLS complete ({:?}); SRTP {:?} installed",
            dtls.role(),
            local.profile
        );
        self.srtp = Some(SrtpContext::with_keys(local, remote));
        self.set_state(ConnectionState::Connected);
        self.emit(TransportEvent::Connected);
        Ok(())
    }

    // ------------------------------------------------------------ SRTP

    fn handle_srtp(&mut self, data: &[u8], from: SocketAddr) {
        if self.srtp.is_none() {
            trace!("SRTP from {from} before keys; dropped");
            return;
        }
        if data.len() < 12 {
            return;
        }
        let now = Instant::now();
        let is_rtcp = (200..=207).contains(&data[1]);
        if is_rtcp {
            let plain = match self.srtp.as_mut().map(|s| s.unprotect_rtcp(data)) {
                Some(Ok(plain)) => plain,
                Some(Err(e)) => {
                    trace!("SRTCP unprotect failed: {e}");
                    return;
                }
                None => return,
            };
            let packets = RtcpPacket::parse_compound(&plain);
            if packets.is_empty() {
                trace!("RTCP from {from} had no parseable sub-packet; dropped");
                return;
            }
            for p in &packets {
                if let RtcpPacket::SenderReport(sr) = p {
                    if let Some(src) = self.sources.get_mut(&sr.ssrc) {
                        src.on_sender_report(sr.ntp_timestamp_msw, sr.ntp_timestamp_lsw, now);
                    }
                }
            }
            self.emit(TransportEvent::Rtcp(packets));
            return;
        }
        let plain = match self.srtp.as_mut().map(|s| s.unprotect_rtp(data)) {
            Some(Ok(plain)) => plain,
            Some(Err(e)) => {
                trace!("SRTP unprotect failed: {e}");
                return;
            }
            None => return,
        };
        let pkt = match RtpPacket::parse(Bytes::from(plain)) {
            Ok(pkt) => pkt,
            Err(e) => {
                trace!("RTP parse failed: {e}");
                return;
            }
        };
        let (kind, clock) = self
            .payload_map
            .get(&pkt.header.payload_type())
            .copied()
            .unwrap_or((MediaKind::Audio, 0));
        if clock > 0 {
            self.track_source(&pkt, clock, now);
        }
        self.emit(match kind {
            MediaKind::Audio => TransportEvent::Rtp(pkt),
            MediaKind::Video => TransportEvent::VideoRtp(pkt),
        });
    }

    /// Account an inbound packet in its source's reception statistics.
    fn track_source(&mut self, pkt: &RtpPacket, clock: u32, now: Instant) {
        let ssrc = pkt.header.ssrc;
        if !self.sources.contains_key(&ssrc) {
            if self.sources.len() >= MAX_SOURCES {
                return;
            }
            self.sources.insert(ssrc, SourceStats::new(ssrc, clock));
        }
        if let Some(s) = self.sources.get_mut(&ssrc) {
            s.on_packet(pkt.header.sequence_number, pkt.header.timestamp, now);
        }
    }

    fn peer_addr(&self) -> Result<SocketAddr> {
        self.selected
            .or(self.dtls_peer)
            .ok_or_else(|| WebRtcError::InvalidState("no nominated pair".into()))
    }

    /// The report every compound RTCP packet starts with (RFC 3550 §6.1):
    /// an SR for each of our streams that has sent, the first carrying
    /// the reception report blocks, or an RR when none has; then an SDES
    /// with our CNAME for each reporting SSRC.
    fn build_report(&mut self, now: Instant) -> Vec<RtcpPacket> {
        let mut blocks: Vec<_> = self
            .sources
            .values_mut()
            .filter(|s| s.is_started())
            .take(31)
            .map(|s| s.report_block(now))
            .collect();
        let mut packets = Vec::with_capacity(3);
        let mut sdes = SourceDescription::new();
        let mut any_sender = false;
        for st in &self.senders {
            if !st.has_sent() {
                continue;
            }
            let mut sr = st.sender_report(now);
            if !any_sender {
                sr.report_blocks = std::mem::take(&mut blocks);
            }
            any_sender = true;
            packets.push(RtcpPacket::SenderReport(sr));
            sdes.add_chunk(st.ssrc(), vec![SdesItem::new(1, self.cname.clone())]);
        }
        if !any_sender {
            let mut rr = ReceiverReport::new(self.ssrc);
            rr.report_blocks = blocks;
            packets.push(RtcpPacket::ReceiverReport(rr));
            sdes.add_chunk(self.ssrc, vec![SdesItem::new(1, self.cname.clone())]);
        }
        packets.push(RtcpPacket::SourceDescription(sdes));
        packets
    }

    /// Build and protect one compound RTCP packet: the report, then
    /// `extra` (feedback the owner is sending).
    fn protect_rtcp(
        &mut self,
        extra: &[RtcpPacket],
        now: Instant,
    ) -> Result<(Vec<u8>, SocketAddr)> {
        let to = self.peer_addr()?;
        let mut plain = Vec::with_capacity(128);
        for p in self.build_report(now).iter().chain(extra) {
            plain.extend_from_slice(&p.to_bytes());
        }
        let srtp = self
            .srtp
            .as_mut()
            .ok_or_else(|| WebRtcError::InvalidState("SRTP keys not installed".into()))?;
        let bytes = srtp
            .protect_rtcp(&plain)
            .map_err(|e| WebRtcError::Internal(format!("SRTCP protect: {e}")))?;
        Ok((bytes, to))
    }

    /// Protect a pre-built video RTP packet, re-stamped with our video
    /// SSRC. The sequence number and timestamp are the producer's: a
    /// conference room's subscription numbers its own packets and answers
    /// NACKs from its own cache, so they must reach the wire unchanged.
    fn protect_video(&mut self, packet: Bytes) -> Result<(Vec<u8>, SocketAddr)> {
        if packet.len() < 12 || packet[0] >> 6 != 2 {
            return Err(WebRtcError::Internal("not an RTP packet".into()));
        }
        let to = self.peer_addr()?;
        let mut buf = BytesMut::from(packet.as_ref());
        buf[8..12].copy_from_slice(&self.video_ssrc.to_be_bytes());
        let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let mut header_len = 12 + (buf[0] & 0x0f) as usize * 4;
        if buf[0] & 0x10 != 0 && buf.len() >= header_len + 4 {
            let words = u16::from_be_bytes([buf[header_len + 2], buf[header_len + 3]]) as usize;
            header_len += 4 + words * 4;
        }
        self.senders[1].on_send(
            timestamp,
            buf.len().saturating_sub(header_len),
            Instant::now(),
        );
        let srtp = self
            .srtp
            .as_mut()
            .ok_or_else(|| WebRtcError::InvalidState("SRTP keys not installed".into()))?;
        let bytes = srtp
            .protect_rtp(&buf)
            .map_err(|e| WebRtcError::Internal(format!("SRTP protect: {e}")))?;
        Ok((bytes, to))
    }

    fn protect_rtp(
        &mut self,
        payload_type: u8,
        marker: bool,
        timestamp: u32,
        payload: Bytes,
    ) -> Result<(Vec<u8>, SocketAddr)> {
        let to = self.peer_addr()?;
        self.senders[0].on_send(timestamp, payload.len(), Instant::now());
        let srtp = self
            .srtp
            .as_mut()
            .ok_or_else(|| WebRtcError::InvalidState("SRTP keys not installed".into()))?;
        let pkt = RtpPacket::build(
            payload_type,
            self.seq,
            timestamp,
            self.ssrc,
            payload,
            marker,
        );
        self.seq = self.seq.wrapping_add(1);
        let bytes = srtp
            .protect_rtp(&pkt.to_bytes())
            .map_err(|e| WebRtcError::Internal(format!("SRTP protect: {e}")))?;
        Ok((bytes, to))
    }
}

/// Handle to a running transport. Cheap to clone; all clones share one socket.
#[derive(Clone)]
pub struct Transport {
    inner: Arc<Mutex<Inner>>,
    socket: Arc<UdpSocket>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// One handle per TURN allocation (empty when no TURN servers configured).
    turn: Arc<Vec<TurnHandle>>,
    /// Remote address → index of the TURN allocation that reaches it. A remote
    /// relay candidate is routed through our relay (relay↔relay, the path that
    /// survives symmetric NAT); a peer we first heard from over a relay is
    /// pinned to that relay so replies take the same path.
    routes: Arc<Mutex<HashMap<SocketAddr, usize>>>,
}

impl Transport {
    /// Bind the socket, gather host candidates synchronously, kick off
    /// server-reflexive gathering, and start the receive and tick tasks.
    pub async fn new(
        cfg: TransportConfig,
        cert: Arc<DtlsCertificate>,
        ssrc: u32,
        video_ssrc: u32,
        cname: String,
        state: Arc<Mutex<ConnectionState>>,
    ) -> Result<(Transport, mpsc::Receiver<TransportEvent>)> {
        let mut agent = IceAgent::new(1, cfg.local_port, vec![]);
        if !cfg.turn_servers.is_empty() {
            agent.set_turn_servers(cfg.turn_servers.clone());
        }
        agent
            .gather_candidates()
            .await
            .map_err(|e| WebRtcError::IceError(e.to_string()))?;
        let socket = agent
            .get_socket()
            .ok_or_else(|| WebRtcError::IceError("ICE agent has no socket".into()))?;
        // Take ownership of the TURN allocations gathered above; each becomes an
        // actor task that owns its socket.
        let turn_clients = agent.take_turn_clients();
        let local_addr = socket
            .local_addr()
            .map_err(|e| WebRtcError::IceError(e.to_string()))?;
        let mut host: Vec<IceCandidate> = agent
            .get_local_candidates()
            .iter()
            .filter(|c| c.ip.is_ipv4() == local_addr.is_ipv4())
            .cloned()
            .collect();
        if host.is_empty() {
            // No usable interface was found (e.g. a container with only lo):
            // advertise loopback so local tests and same-host demos work.
            let lo: IpAddr = if local_addr.is_ipv4() {
                "127.0.0.1".parse().unwrap()
            } else {
                "::1".parse().unwrap()
            };
            host.push(IceCandidate::new_host(
                "lo".into(),
                1,
                Protocol::Udp,
                lo,
                local_addr.port(),
                0,
            ));
        }

        let (events, rx) = mpsc::channel(cfg.event_capacity);
        let stun_servers = cfg.stun_servers.clone();
        let check_interval = cfg.check_interval;
        let mut inner = Inner {
            cfg,
            agent,
            local_addr,
            local_candidates: Vec::new(),
            gathering_complete: false,
            stun_pending: HashMap::new(),
            role: IceRole::Controlling,
            remote_creds: None,
            stun_server: None,
            remotes: Vec::new(),
            selected: None,
            ice_started_at: None,
            last_keepalive: None,
            cert,
            dtls_role: None,
            remote_fingerprint: None,
            dtls: None,
            dtls_peer: None,
            dtls_started_at: None,
            last_dtls_timer: Instant::now(),
            srtp: None,
            ssrc,
            video_ssrc,
            seq: (ssrc >> 16) as u16,
            cname,
            payload_map: HashMap::new(),
            sources: HashMap::new(),
            senders: [
                SenderStats::new(ssrc, 8_000),
                SenderStats::new(video_ssrc, 90_000),
            ],
            last_rtcp: None,
            state,
            events,
            closed: false,
            rtp_dropped: 0,
        };
        for c in host {
            inner.add_local_candidate(c);
        }

        // Server-reflexive gathering through *this* socket: a plain Binding
        // Request per server; the response is matched in handle_stun.
        let mut initial_out = Outgoing::new();
        for server in &stun_servers {
            match resolve_stun_server(server).await {
                Ok(addr) if addr.is_ipv4() == local_addr.is_ipv4() => {
                    let mut req = StunMessage::new_binding_request();
                    req.add_fingerprint();
                    inner
                        .stun_pending
                        .insert(req.transaction_id, (addr, Instant::now()));
                    initial_out.push((req.serialize(), addr));
                }
                Ok(addr) => debug!("skipping STUN server {addr}: address family mismatch"),
                Err(e) => warn!("STUN server {server}: {e}"),
            }
        }
        if inner.stun_pending.is_empty() {
            inner.finish_gathering();
        }

        let inner = Arc::new(Mutex::new(inner));

        // One command channel + handle per TURN allocation.
        let mut turn_handles = Vec::with_capacity(turn_clients.len());
        let mut turn_rx = Vec::with_capacity(turn_clients.len());
        for client in &turn_clients {
            let (tx, rx) = mpsc::channel::<TurnCmd>(256);
            turn_handles.push(TurnHandle {
                tx,
                relayed_addr: client.relayed_addr(),
            });
            turn_rx.push(rx);
        }

        let transport = Transport {
            inner,
            socket,
            tasks: Arc::new(Mutex::new(Vec::new())),
            turn: Arc::new(turn_handles),
            routes: Arc::new(Mutex::new(HashMap::new())),
        };
        transport.flush(initial_out).await;

        // One actor task per TURN allocation: sole reader of its socket,
        // classifying relayed media vs control responses and serving Send /
        // Permission commands.
        for (index, (client, rx)) in turn_clients.into_iter().zip(turn_rx).enumerate() {
            debug!(
                "turn actor {index} backs relay candidate {}",
                transport.turn[index].relayed_addr
            );
            let t = transport.clone();
            let handle = tokio::spawn(turn_actor(client, rx, t, index));
            transport.tasks.lock().push(handle);
        }

        // Receive task.
        let t = transport.clone();
        let recv_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                let (len, from) = match t.socket.recv_from(&mut buf).await {
                    Ok(v) => v,
                    Err(e) => {
                        if t.inner.lock().closed {
                            break;
                        }
                        warn!("socket recv: {e}");
                        continue;
                    }
                };
                let out = t
                    .inner
                    .lock()
                    .handle_packet(&buf[..len], from, Instant::now());
                t.flush(out).await;
                if t.inner.lock().closed {
                    break;
                }
            }
        });
        // Tick task.
        let t = transport.clone();
        let tick_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let out = t.inner.lock().tick(Instant::now());
                t.flush(out).await;
                if t.inner.lock().closed {
                    break;
                }
            }
        });
        transport.tasks.lock().extend([recv_task, tick_task]);
        Ok((transport, rx))
    }

    async fn flush(&self, out: Outgoing) {
        for (bytes, to) in out {
            // Copy the route out before any await — never hold the sync lock
            // across `.send`.
            let route = self.routes.lock().get(&to).copied();
            match route {
                Some(i) => {
                    if self.turn[i]
                        .tx
                        .send(TurnCmd::Send(to, bytes))
                        .await
                        .is_err()
                    {
                        trace!("turn actor {i} gone; dropped packet to {to}");
                    }
                }
                None => {
                    if let Err(e) = self.socket.send_to(&bytes, to).await {
                        trace!("send to {to} failed: {e}");
                    }
                }
            }
        }
    }

    /// Local socket address.
    pub fn local_addr(&self) -> SocketAddr {
        self.inner.lock().local_addr
    }

    /// Local ICE credentials `(ufrag, pwd)`.
    pub fn local_credentials(&self) -> (String, String) {
        let g = self.inner.lock();
        let (u, p) = g.agent.get_local_credentials();
        (u.to_string(), p.to_string())
    }

    /// Snapshot of the local candidates gathered so far.
    pub fn local_candidates(&self) -> Vec<IceCandidate> {
        self.inner.lock().local_candidates.clone()
    }

    /// Whether gathering has finished.
    pub fn gathering_complete(&self) -> bool {
        self.inner.lock().gathering_complete
    }

    /// Set the ICE role. Must be called before the remote description.
    pub fn set_role(&self, role: IceRole) {
        self.inner.lock().role = role;
    }

    /// Current ICE role.
    pub fn role(&self) -> IceRole {
        self.inner.lock().role
    }

    /// Install the remote description's transport parameters. On a
    /// renegotiation the credentials must be unchanged: an ICE restart is
    /// refused rather than half-implemented.
    pub fn set_remote(
        &self,
        ufrag: &str,
        pwd: &str,
        fingerprint: &str,
        dtls_role: DtlsRole,
        candidates: &[IceCandidate],
    ) -> Result<()> {
        let mut g = self.inner.lock();
        if let Some((u, p)) = &g.remote_creds {
            if u != ufrag || p != pwd {
                return Err(WebRtcError::IceRestartUnsupported);
            }
            if g.remote_fingerprint.as_deref() != Some(fingerprint) {
                return Err(WebRtcError::InvalidState(
                    "remote DTLS fingerprint changed; re-keying is unsupported".into(),
                ));
            }
        } else {
            g.agent
                .set_remote_credentials(ufrag.to_string(), pwd.to_string())
                .map_err(|e| WebRtcError::IceError(e.to_string()))?;
            let (local_ufrag, local_pwd) = g.agent.get_local_credentials();
            g.stun_server = Some(StunServer::new(
                local_ufrag.to_string(),
                local_pwd.as_bytes().to_vec(),
                ufrag.to_string(),
            ));
            g.remote_creds = Some((ufrag.to_string(), pwd.to_string()));
            g.remote_fingerprint = Some(fingerprint.to_string());
            g.dtls_role = Some(dtls_role);
            g.ice_started_at = Some(Instant::now());
            if g.state() == ConnectionState::Gathering || g.state() == ConnectionState::New {
                g.set_state(ConnectionState::Checking);
            }
        }
        for c in candidates {
            g.add_remote_candidate(c.clone());
        }
        drop(g);
        for c in candidates {
            self.route_relay_remote(c);
        }
        Ok(())
    }

    /// Add a trickled remote candidate.
    pub fn add_remote_candidate(&self, cand: IceCandidate) {
        self.inner.lock().add_remote_candidate(cand.clone());
        self.route_relay_remote(&cand);
    }

    /// Route a remote *relay* candidate through our own relay (relay↔relay is
    /// the path that survives symmetric NAT) and install the permission the
    /// relay needs to forward it. No-op without a TURN allocation.
    fn route_relay_remote(&self, cand: &IceCandidate) {
        if self.turn.is_empty()
            || cand.typ != CandidateType::Relay
            || cand.protocol != Protocol::Udp
            || cand.component != 1
        {
            return;
        }
        let addr = SocketAddr::new(cand.ip, cand.port);
        self.routes.lock().insert(addr, 0);
        let _ = self.turn[0].tx.try_send(TurnCmd::Permission(addr));
    }

    /// Nominated remote address, once ICE has completed.
    pub fn selected_remote(&self) -> Option<SocketAddr> {
        self.inner.lock().selected
    }

    async fn send_to_peer(&self, bytes: Vec<u8>, to: SocketAddr) -> Result<()> {
        let route = self.routes.lock().get(&to).copied();
        match route {
            Some(i) => self.turn[i]
                .tx
                .send(TurnCmd::Send(to, bytes))
                .await
                .map_err(|_| WebRtcError::Internal("turn actor gone".into()))?,
            None => {
                self.socket
                    .send_to(&bytes, to)
                    .await
                    .map_err(|e| WebRtcError::Internal(format!("send: {e}")))?;
            }
        }
        Ok(())
    }

    /// Build, protect and send one audio RTP packet.
    pub async fn send_rtp(
        &self,
        payload_type: u8,
        marker: bool,
        timestamp: u32,
        payload: Bytes,
    ) -> Result<()> {
        let (bytes, to) =
            self.inner
                .lock()
                .protect_rtp(payload_type, marker, timestamp, payload)?;
        self.send_to_peer(bytes, to).await
    }

    /// Protect and send one pre-built video RTP packet, re-stamped with
    /// our video SSRC; its sequence number, timestamp, payload type and
    /// marker are kept.
    pub async fn send_video(&self, packet: Bytes) -> Result<()> {
        let (bytes, to) = self.inner.lock().protect_video(packet)?;
        self.send_to_peer(bytes, to).await
    }

    /// Send RTCP: `packets` (feedback, BYE, …) behind the current report
    /// and SDES so the compound is well-formed, protected as SRTCP.
    pub async fn send_rtcp(&self, packets: &[RtcpPacket]) -> Result<()> {
        let (bytes, to) = self.inner.lock().protect_rtcp(packets, Instant::now())?;
        self.send_to_peer(bytes, to).await
    }

    /// Install the negotiated payload types: which stream each belongs to
    /// and its clock rate. Replaces the previous map; the audio and video
    /// senders' clocks follow the first mapping of their kind.
    pub fn set_payload_map(&self, map: &[PayloadMapping]) {
        let mut g = self.inner.lock();
        g.payload_map = map
            .iter()
            .map(|m| (m.payload_type, (m.kind, m.clock_rate)))
            .collect();
        if let Some(m) = map.iter().find(|m| m.kind == MediaKind::Audio) {
            g.senders[0].set_clock_rate(m.clock_rate);
        }
        if let Some(m) = map.iter().find(|m| m.kind == MediaKind::Video) {
            g.senders[1].set_clock_rate(m.clock_rate);
        }
    }

    /// Reception statistics for every remote SSRC heard so far.
    pub fn sources(&self) -> Vec<SourceStats> {
        self.inner.lock().sources.values().cloned().collect()
    }

    /// Our video SSRC.
    pub fn video_ssrc(&self) -> u32 {
        self.inner.lock().video_ssrc
    }

    /// Our sending SSRC.
    pub fn ssrc(&self) -> u32 {
        self.inner.lock().ssrc
    }

    /// Stop the tasks and drop the keys.
    pub fn close(&self) {
        {
            let mut g = self.inner.lock();
            if g.closed {
                return;
            }
            g.closed = true;
            g.srtp = None;
            g.dtls = None;
            g.set_state(ConnectionState::Closed);
            g.emit(TransportEvent::Closed);
        }
        for t in self.tasks.lock().drain(..) {
            t.abort();
        }
    }
}

/// One TURN allocation's actor: the sole reader of its socket. Classifies each
/// inbound datagram as relayed media (fed into the shared `Inner`) or a control
/// response, and serves `Send` / `Permission` commands from the transport.
/// Keeping it single-tasked is what lets control ops and media share one socket
/// without two readers racing for the same datagram.
async fn turn_actor(
    mut client: TurnClient,
    mut cmds: mpsc::Receiver<TurnCmd>,
    transport: Transport,
    index: usize,
) {
    let server = client.server_addr();
    // Cloned socket handle: awaited in `select!` without borrowing `client`, so
    // the branch handlers stay free to call `&mut` control ops.
    let sock = client.socket_arc();
    let mut buf = vec![0u8; 2048];
    // Refresh the allocation (lifetime 600 s) and renew permissions (300 s)
    // well inside their windows.
    let mut maint = tokio::time::interval(Duration::from_secs(200));
    maint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    maint.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                let (len, from) = match r {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match client.handle_inbound(&buf[..len], from) {
                    TurnInbound::Relayed(peer, data) => {
                        // Replies to this peer take the same relay path.
                        transport.routes.lock().insert(peer, index);
                        let out = transport
                            .inner
                            .lock()
                            .handle_packet(&data, peer, Instant::now());
                        actor_flush(&client, &transport, index, out).await;
                    }
                    TurnInbound::StaleNonce => {
                        // Nonce refreshed in place; periodic maintenance below
                        // re-sends refresh/permissions with the new nonce.
                    }
                    TurnInbound::ControlError(code) => {
                        trace!("turn {index}: control error {code}");
                    }
                    _ => {}
                }
            }
            cmd = cmds.recv() => match cmd {
                Some(TurnCmd::Send(peer, bytes)) => {
                    let _ = client.send_to(peer, &bytes).await;
                }
                Some(TurnCmd::Permission(peer)) => {
                    if let Ok(req) = client.permission_request(peer) {
                        let _ = sock.send_to(&req, server).await;
                    }
                }
                None => break, // transport dropped
            },
            _ = maint.tick() => {
                if let Ok(req) = client.refresh_request(600) {
                    let _ = sock.send_to(&req, server).await;
                }
                let peers: Vec<SocketAddr> = transport
                    .routes
                    .lock()
                    .iter()
                    .filter(|(_, i)| **i == index)
                    .map(|(a, _)| *a)
                    .collect();
                for p in peers {
                    if let Ok(req) = client.permission_request(p) {
                        let _ = sock.send_to(&req, server).await;
                    }
                }
            }
        }
        if transport.inner.lock().closed {
            break;
        }
    }
}

/// Flush `Outgoing` produced while the actor holds `client`: datagrams bound
/// for this allocation go straight through `client`; anything else is routed
/// back through the transport (its socket or another actor).
async fn actor_flush(client: &TurnClient, transport: &Transport, index: usize, out: Outgoing) {
    for (bytes, to) in out {
        let route = transport.routes.lock().get(&to).copied();
        match route {
            Some(i) if i == index => {
                let _ = client.send_to(to, &bytes).await;
            }
            Some(i) => {
                let _ = transport.turn[i].tx.try_send(TurnCmd::Send(to, bytes));
            }
            None => {
                let _ = transport.socket.send_to(&bytes, to).await;
            }
        }
    }
}

/// Resolve `stun:host[:port]` (RFC 7064) to a socket address.
async fn resolve_stun_server(uri: &str) -> Result<SocketAddr> {
    let hostport = uri
        .strip_prefix("stun:")
        .or_else(|| uri.strip_prefix("stuns:"))
        .unwrap_or(uri);
    let hostport = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:3478")
    };
    let mut addrs = tokio::net::lookup_host(hostport.as_str())
        .await
        .map_err(|e| WebRtcError::IceError(format!("resolve {hostport}: {e}")))?;
    addrs
        .next()
        .ok_or_else(|| WebRtcError::IceError(format!("{hostport} resolved to nothing")))
}
