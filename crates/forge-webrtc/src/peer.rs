//! WebRTC `PeerConnection`: an endpoint-shaped peer connection that can
//! offer *and* answer, trickles its candidates, renegotiates on the same
//! transport, and moves Opus frames over DTLS-SRTP without an engine session.
//!
//! Shape (mirrors the W3C/JSEP model closely enough that signalling glue
//! written for a browser maps one-to-one):
//!
//! ```text
//! offerer                                   answerer
//! create_offer()  ── SDP ──────────────▶    set_remote_offer(sdp)
//!                                           create_answer()  ── SDP ──▶ set_remote_answer(sdp)
//! events: LocalCandidate ── trickle ──▶     add_ice_candidate()      (both directions)
//! events: IceConnected → Connected          sender().send_audio(frame, 960)
//! create_offer() again (re-offer, same ICE credentials) / rollback_local_offer()
//! ```
//!
//! ICE restart is unsupported by design (a remote description that changes
//! the ICE credentials is refused with [`WebRtcError::IceRestartUnsupported`]).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use forge_ice::IceCandidate;
use forge_rtp::dtls::{DtlsCertificate, DtlsRole};
use forge_sdp::DtlsSetup;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::sdp::{self, Direction, LocalParams, RemoteDescription};
use crate::transport::{IceRole, Transport, TransportConfig, TransportEvent};
use crate::{Result, WebRtcError};

/// WebRTC connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Created; no description yet.
    New,
    /// A local description exists; candidates are being gathered/trickled.
    Gathering,
    /// Remote description applied; ICE checks and DTLS in progress.
    Checking,
    /// DTLS complete, SRTP keys installed.
    Connected,
    /// Failed (no recovery; ICE restart is unsupported).
    Failed,
    /// Closed locally.
    Closed,
}

/// JSEP-style signalling state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalingState {
    /// No offer outstanding.
    Stable,
    /// We sent an offer and await the answer.
    HaveLocalOffer,
    /// We received an offer and owe an answer.
    HaveRemoteOffer,
}

/// Events from a peer connection (re-exported transport events).
pub type PeerEvent = TransportEvent;

/// Peer connection configuration.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    /// STUN servers (`stun:host:port`).
    pub stun_servers: Vec<String>,
    /// Direction we want for the audio section.
    pub direction: Direction,
    /// Opus payload type in our offers (answers mirror the remote's).
    pub opus_pt: u8,
    /// Offer telephone-event (RFC 4733) as well.
    pub dtmf: bool,
    /// Transport tunables.
    pub transport: TransportConfig,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            stun_servers: vec![],
            direction: Direction::SendRecv,
            opus_pt: 111,
            dtmf: true,
            transport: TransportConfig::default(),
        }
    }
}

/// A WebRTC peer connection (one audio section, BUNDLE, rtcp-mux, DTLS-SRTP,
/// trickle ICE).
pub struct PeerConnection {
    connection_id: String,
    cfg: PeerConfig,
    cert: Arc<DtlsCertificate>,
    state: Arc<Mutex<ConnectionState>>,
    transport: Option<Transport>,
    events: Option<mpsc::Receiver<PeerEvent>>,
    signaling: SignalingState,
    local_sdp: Option<String>,
    pending_local_sdp: Option<String>,
    remote_sdp: Option<String>,
    remote: Option<RemoteDescription>,
    session_id: u64,
    session_version: u64,
    ssrc: u32,
    cname: String,
    audio_ts: Arc<AtomicU32>,
    audio_started: Arc<AtomicBool>,
}

impl PeerConnection {
    /// Create a peer connection with default configuration and the given
    /// STUN servers.
    pub async fn new(stun_servers: Vec<String>) -> Result<Self> {
        Self::with_config(PeerConfig {
            stun_servers,
            ..PeerConfig::default()
        })
        .await
    }

    /// Create a peer connection.
    pub async fn with_config(cfg: PeerConfig) -> Result<Self> {
        let connection_id = format!("webrtc-{}", uuid::Uuid::new_v4());
        info!("Creating PeerConnection {connection_id}");
        let cert = Arc::new(
            DtlsCertificate::generate().map_err(|e| WebRtcError::DtlsError(e.to_string()))?,
        );
        let rnd = uuid::Uuid::new_v4().as_u128();
        let ssrc = (rnd as u32) | 1;
        let session_id = ((rnd >> 64) as u64) & 0x7fff_ffff_ffff_ffff;
        Ok(Self {
            cname: format!("forge-{}", &connection_id[7..15]),
            connection_id,
            cfg,
            cert,
            state: Arc::new(Mutex::new(ConnectionState::New)),
            transport: None,
            events: None,
            signaling: SignalingState::Stable,
            local_sdp: None,
            pending_local_sdp: None,
            remote_sdp: None,
            remote: None,
            session_id,
            session_version: 0,
            ssrc,
            audio_ts: Arc::new(AtomicU32::new((rnd >> 32) as u32)),
            audio_started: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn ensure_transport(&mut self) -> Result<Transport> {
        if let Some(t) = &self.transport {
            return Ok(t.clone());
        }
        let mut tcfg = self.cfg.transport.clone();
        tcfg.stun_servers = self.cfg.stun_servers.clone();
        let (t, rx) =
            Transport::new(tcfg, self.cert.clone(), self.ssrc, self.state.clone()).await?;
        self.transport = Some(t.clone());
        self.events = Some(rx);
        Ok(t)
    }

    fn local_params<'a>(
        &'a self,
        t: &'a Transport,
        creds: &'a (String, String),
        candidates: &'a [IceCandidate],
        setup: DtlsSetup,
    ) -> LocalParams<'a> {
        LocalParams {
            ufrag: &creds.0,
            pwd: &creds.1,
            fingerprint: &self.cert.fingerprint,
            setup,
            candidates,
            end_of_candidates: t.gathering_complete(),
            ssrc: self.ssrc,
            cname: &self.cname,
            direction: self.cfg.direction,
            opus_pt: self.cfg.opus_pt,
            dtmf_pt: if self.cfg.dtmf { Some(101) } else { None },
            mid: "0",
            session_id: self.session_id,
            session_version: self.session_version,
        }
    }

    // ------------------------------------------------------------ offer/answer

    /// Create an SDP offer. The first call starts the transport (ICE
    /// controlling role) and returns as soon as host candidates are known;
    /// further candidates arrive as [`TransportEvent::LocalCandidate`]. Later
    /// calls are re-offers on the same transport (same ICE credentials, same
    /// certificate); the direction comes from [`PeerConnection::set_direction`].
    pub async fn create_offer(&mut self) -> Result<String> {
        if self.signaling != SignalingState::Stable {
            return Err(WebRtcError::InvalidState(format!(
                "cannot create offer in signaling state {:?}",
                self.signaling
            )));
        }
        let t = self.ensure_transport().await?;
        if self.remote.is_none() {
            t.set_role(IceRole::Controlling);
        }
        let creds = t.local_credentials();
        let candidates = t.local_candidates();
        self.session_version += 1;
        let sdp = sdp::build_offer(&self.local_params(&t, &creds, &candidates, DtlsSetup::Actpass));
        self.pending_local_sdp = Some(sdp.clone());
        self.signaling = SignalingState::HaveLocalOffer;
        if *self.state.lock() == ConnectionState::New {
            *self.state.lock() = ConnectionState::Gathering;
        }
        debug!(
            "{}: created offer v{}",
            self.connection_id, self.session_version
        );
        Ok(sdp)
    }

    /// Apply a remote offer (initial, or a re-offer on the same transport).
    pub async fn set_remote_offer(&mut self, sdp: &str) -> Result<()> {
        if self.signaling != SignalingState::Stable {
            return Err(WebRtcError::InvalidState(format!(
                "cannot set remote offer in signaling state {:?}",
                self.signaling
            )));
        }
        let remote = sdp::parse_remote(sdp)?;
        if remote.audio.is_none() {
            return Err(WebRtcError::SdpError(forge_sdp::SdpError::MissingField(
                "audio section".into(),
            )));
        }
        let t = self.ensure_transport().await?;
        if self.remote.is_none() {
            t.set_role(IceRole::Controlled);
        }
        // Our DTLS role follows the offer's a=setup (RFC 8842 §5.3):
        // actpass/passive → we are active (client); active → we are passive.
        let dtls_role = match remote.setup {
            DtlsSetup::Actpass | DtlsSetup::Passive => DtlsRole::Client,
            DtlsSetup::Active => DtlsRole::Server,
            DtlsSetup::Holdconn => {
                return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
                    "a=setup:holdconn is not supported".into(),
                )))
            }
        };
        t.set_remote(
            &remote.ufrag,
            &remote.pwd,
            &remote.fingerprint,
            dtls_role,
            &remote.candidates,
        )?;
        self.remote_sdp = Some(sdp.to_string());
        self.remote = Some(remote);
        self.signaling = SignalingState::HaveRemoteOffer;
        if *self.state.lock() == ConnectionState::New {
            *self.state.lock() = ConnectionState::Gathering;
        }
        Ok(())
    }

    /// Create the SDP answer to the remote offer.
    pub async fn create_answer(&mut self) -> Result<String> {
        if self.signaling != SignalingState::HaveRemoteOffer {
            return Err(WebRtcError::InvalidState(format!(
                "cannot create answer in signaling state {:?}",
                self.signaling
            )));
        }
        let t = self.ensure_transport().await?;
        let remote = self
            .remote
            .clone()
            .ok_or_else(|| WebRtcError::InvalidState("no remote offer".into()))?;
        let setup = match remote.setup {
            DtlsSetup::Active => DtlsSetup::Passive,
            _ => DtlsSetup::Active,
        };
        let creds = t.local_credentials();
        let candidates = t.local_candidates();
        self.session_version += 1;
        let answer =
            sdp::build_answer(&self.local_params(&t, &creds, &candidates, setup), &remote)?;
        self.local_sdp = Some(answer.clone());
        self.signaling = SignalingState::Stable;
        debug!(
            "{}: created answer v{}",
            self.connection_id, self.session_version
        );
        Ok(answer)
    }

    /// Apply the remote answer to our offer. Returns as soon as the answer
    /// is applied; connectivity proceeds in the background — use
    /// [`PeerConnection::wait_connected`] or the events.
    pub async fn set_remote_answer(&mut self, sdp: &str) -> Result<()> {
        if self.signaling != SignalingState::HaveLocalOffer {
            return Err(WebRtcError::InvalidState(format!(
                "cannot set remote answer in signaling state {:?}",
                self.signaling
            )));
        }
        let remote = sdp::parse_remote(sdp)?;
        let t = self.ensure_transport().await?;
        let dtls_role = match remote.setup {
            DtlsSetup::Active => DtlsRole::Server,
            DtlsSetup::Passive => DtlsRole::Client,
            DtlsSetup::Actpass | DtlsSetup::Holdconn => {
                return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
                    "answer must carry a=setup:active or passive".into(),
                )))
            }
        };
        if remote.audio.is_none() {
            return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
                "answer rejected the audio section".into(),
            )));
        }
        t.set_remote(
            &remote.ufrag,
            &remote.pwd,
            &remote.fingerprint,
            dtls_role,
            &remote.candidates,
        )?;
        self.remote_sdp = Some(sdp.to_string());
        self.remote = Some(remote);
        self.local_sdp = self.pending_local_sdp.take();
        self.signaling = SignalingState::Stable;
        Ok(())
    }

    /// Discard an outstanding local offer (the peer rejected the
    /// renegotiation, or glare was lost). The transport is untouched because
    /// a re-offer never changes it.
    pub fn rollback_local_offer(&mut self) -> Result<()> {
        if self.signaling != SignalingState::HaveLocalOffer {
            return Err(WebRtcError::InvalidState(
                "no local offer to roll back".into(),
            ));
        }
        self.pending_local_sdp = None;
        self.signaling = SignalingState::Stable;
        Ok(())
    }

    /// Direction for the next offer or answer.
    pub fn set_direction(&mut self, direction: Direction) {
        self.cfg.direction = direction;
    }

    // ------------------------------------------------------------ candidates

    /// Add a remote ICE candidate received over signalling.
    pub async fn add_ice_candidate(&mut self, candidate: IceCandidate) -> Result<()> {
        let t = self.ensure_transport().await?;
        t.add_remote_candidate(candidate);
        Ok(())
    }

    /// Add a remote candidate from its `candidate:` attribute string.
    pub async fn add_ice_candidate_str(&mut self, candidate: &str) -> Result<()> {
        let c = IceCandidate::from_sdp_attribute(candidate)
            .map_err(|e| WebRtcError::IceError(format!("bad candidate: {e}")))?;
        self.add_ice_candidate(c).await
    }

    /// Take the event receiver (once). Events are buffered from creation of
    /// the transport, so nothing is lost by taking it after the offer.
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<PeerEvent>> {
        self.events.take()
    }

    /// Wait until the connection is established (or fails/times out).
    pub async fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.get_state() {
                ConnectionState::Connected => return Ok(()),
                ConnectionState::Failed => {
                    return Err(WebRtcError::ConnectionFailed("transport failed".into()))
                }
                ConnectionState::Closed => return Err(WebRtcError::InvalidState("closed".into())),
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(WebRtcError::ConnectionFailed(format!(
                    "not connected after {timeout:?} (state {:?})",
                    self.get_state()
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // ------------------------------------------------------------ media

    /// A cloneable handle for sending audio from another task.
    pub fn sender(&self) -> Result<AudioSender> {
        let t = self
            .transport
            .clone()
            .ok_or_else(|| WebRtcError::InvalidState("no transport yet".into()))?;
        Ok(AudioSender {
            transport: t,
            payload_type: self.negotiated_opus_pt(),
            timestamp: self.audio_ts.clone(),
            started: self.audio_started.clone(),
        })
    }

    /// Payload type the peer expects for Opus: the one in the remote
    /// description (an answer mirrors our offer, so both agree).
    pub fn negotiated_opus_pt(&self) -> u8 {
        self.remote
            .as_ref()
            .and_then(|r| r.audio.as_ref())
            .and_then(|a| a.opus_pt)
            .unwrap_or(self.cfg.opus_pt)
    }

    // ------------------------------------------------------------ accessors

    /// Connection state.
    pub fn get_state(&self) -> ConnectionState {
        *self.state.lock()
    }

    /// Signalling state.
    pub fn signaling_state(&self) -> SignalingState {
        self.signaling
    }

    /// Connection id.
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// Our DTLS certificate fingerprint (SHA-256).
    pub fn dtls_fingerprint(&self) -> &str {
        &self.cert.fingerprint
    }

    /// Current local description (the last applied offer or answer, or the
    /// outstanding offer).
    pub fn local_sdp(&self) -> Option<&str> {
        self.pending_local_sdp
            .as_deref()
            .or(self.local_sdp.as_deref())
    }

    /// Current remote description.
    pub fn remote_sdp(&self) -> Option<&str> {
        self.remote_sdp.as_deref()
    }

    /// Number of local candidates gathered so far.
    pub async fn local_candidate_count(&self) -> usize {
        self.local_candidates().len()
    }

    /// Local candidates gathered so far.
    pub fn local_candidates(&self) -> Vec<IceCandidate> {
        self.transport
            .as_ref()
            .map(|t| t.local_candidates())
            .unwrap_or_default()
    }

    /// Our sending SSRC.
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Close the connection.
    pub fn close(&mut self) {
        if let Some(t) = &self.transport {
            t.close();
        } else {
            *self.state.lock() = ConnectionState::Closed;
        }
        self.signaling = SignalingState::Stable;
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        if let Some(t) = &self.transport {
            t.close();
        }
    }
}

/// Sends encoded audio frames as SRTP. Clone freely; clones share the RTP
/// timestamp so frames from any clone stay on one timeline.
#[derive(Clone)]
pub struct AudioSender {
    transport: Transport,
    payload_type: u8,
    timestamp: Arc<AtomicU32>,
    started: Arc<AtomicBool>,
}

impl AudioSender {
    /// Send one encoded frame covering `samples` samples at the codec clock
    /// (Opus: 48 kHz, so a 20 ms frame is 960). The RTP marker bit is set on
    /// the first packet of the stream.
    pub async fn send_audio(&self, frame: Bytes, samples: u32) -> Result<()> {
        let ts = self.timestamp.fetch_add(samples, Ordering::SeqCst);
        let marker = !self.started.swap(true, Ordering::SeqCst);
        self.transport
            .send_rtp(self.payload_type, marker, ts, frame)
            .await
    }

    /// Send a raw RTP payload with an explicit payload type and timestamp
    /// (telephone-event, comfort noise, …).
    pub async fn send_rtp(
        &self,
        payload_type: u8,
        marker: bool,
        timestamp: u32,
        payload: Bytes,
    ) -> Result<()> {
        self.transport
            .send_rtp(payload_type, marker, timestamp, payload)
            .await
    }

    /// Payload type in use for audio.
    pub fn payload_type(&self) -> u8 {
        self.payload_type
    }

    /// RTP timestamp the next frame will carry.
    pub fn timestamp(&self) -> u32 {
        self.timestamp.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_offer_sets_state_and_contains_ice_dtls() {
        let mut peer = PeerConnection::new(vec![]).await.unwrap();
        assert_eq!(peer.get_state(), ConnectionState::New);
        let offer = peer.create_offer().await.unwrap();
        assert_eq!(peer.get_state(), ConnectionState::Gathering);
        assert_eq!(peer.signaling_state(), SignalingState::HaveLocalOffer);
        for needle in [
            "v=0",
            "a=ice-ufrag:",
            "a=ice-pwd:",
            "a=fingerprint:sha-256 ",
            "a=setup:actpass",
            "a=mid:0",
            "a=rtcp-mux",
            "a=group:BUNDLE 0",
        ] {
            assert!(offer.contains(needle), "missing {needle} in {offer}");
        }
        assert!(peer.local_candidate_count().await > 0);
        assert!(peer.local_sdp().is_some());
    }

    #[tokio::test]
    async fn second_offer_before_answer_is_refused_and_rollback_clears_it() {
        let mut peer = PeerConnection::new(vec![]).await.unwrap();
        peer.create_offer().await.unwrap();
        assert!(matches!(
            peer.create_offer().await,
            Err(WebRtcError::InvalidState(_))
        ));
        peer.rollback_local_offer().unwrap();
        assert_eq!(peer.signaling_state(), SignalingState::Stable);
        peer.create_offer().await.unwrap();
    }

    #[tokio::test]
    async fn answer_requires_remote_offer() {
        let mut peer = PeerConnection::new(vec![]).await.unwrap();
        assert!(matches!(
            peer.create_answer().await,
            Err(WebRtcError::InvalidState(_))
        ));
    }
}
