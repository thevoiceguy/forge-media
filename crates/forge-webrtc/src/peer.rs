//! WebRTC `PeerConnection`: an endpoint-shaped peer connection that can
//! offer *and* answer, trickles its candidates, renegotiates on the same
//! transport, and moves audio frames — and, when configured, a video
//! stream — over DTLS-SRTP without an engine session.
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
//!                                           video_sender().send_packet(rtp)
//! events: VideoRtp(pkt) / Rtcp(feedback)    send_rtcp(&[pli])
//! create_offer() again (re-offer, same ICE credentials) / rollback_local_offer()
//! ```
//!
//! Video is one section (`a=mid:1`, BUNDLE'd with the audio) carrying one
//! negotiated codec — and, with [`VideoConfig::content`], a second
//! (`a=mid:2`, `a=content:slides`) for a shared screen. The peer connection
//! does not encode or decode: it hands inbound video packets up as
//! [`TransportEvent::VideoRtp`] or [`TransportEvent::ContentRtp`], sorted by
//! the `sdes:mid` header extension it offers on every section (or the
//! signalled SSRCs), and sends packets a producer (a conference room
//! subscription, a forwarder) has already built, re-stamped with its own
//! SSRC for the section ([`PeerConnection::video_sender`],
//! [`PeerConnection::content_sender`]). Feedback the remote negotiated
//! (`nack`, `nack pli`, `ccm fir`, `goog-remb`) arrives parsed in
//! [`TransportEvent::Rtcp`] and goes out through
//! [`PeerConnection::send_rtcp`]; SR/RR reports run on their own.
//!
//! ICE restart is unsupported by design (a remote description that changes
//! the ICE credentials is refused with [`WebRtcError::IceRestartUnsupported`]).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use forge_core::{AudioCodec, VideoCodec};
use forge_ice::IceCandidate;
use forge_rtp::dtls::{DtlsCertificate, DtlsRole};
use forge_rtp::RtcpPacket;
use forge_sdp::DtlsSetup;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::sdp::{
    self, rtp_clock, video_from_answer, Direction, LocalParams, LocalVideo, NegotiatedVideo,
    RemoteDescription,
};
use crate::transport::{
    DemuxConfig, IceRole, MediaKind, PayloadMapping, Transport, TransportConfig, TransportEvent,
    VideoStream,
};

/// The id this endpoint offers the `sdes:mid` header extension under. An
/// answer mirrors whatever the offer chose.
const MID_EXTENSION_ID: u8 = 1;
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

/// The video section a peer connection offers or is willing to answer.
#[derive(Debug, Clone)]
pub struct VideoConfig {
    /// Codecs in preference order, each with the payload type used when
    /// offering it. Answers pick the first entry the remote offered and
    /// mirror the remote's payload type.
    pub codecs: Vec<(VideoCodec, u8)>,
    /// Direction we want for the video section.
    pub direction: Direction,
    /// The H.264 `profile-level-id` offered (six hex digits). Constrained
    /// Baseline level 3.1 by default, which every browser and phone
    /// decodes; answers echo the remote's parameters instead.
    pub h264_profile_level_id: String,
    /// Bitrate cap advertised as `b=AS` on the section, in kb/s.
    /// Browsers treat it as the most they will send.
    pub max_kbps: Option<u32>,
    /// Offer (or accept) a second video section for a shared screen
    /// (`a=mid:2`, `a=content:slides`), with the same codecs and
    /// direction. Its packets arrive as [`TransportEvent::ContentRtp`]
    /// and leave through [`PeerConnection::content_sender`].
    pub content: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codecs: [
                VideoCodec::H264,
                VideoCodec::VP8,
                VideoCodec::VP9,
                VideoCodec::AV1,
            ]
            .iter()
            .map(|c| (*c, c.default_payload_type()))
            .collect(),
            direction: Direction::SendRecv,
            h264_profile_level_id: "42e01f".to_string(),
            max_kbps: None,
            content: false,
        }
    }
}

/// Peer connection configuration.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    /// STUN servers (`stun:host:port`).
    pub stun_servers: Vec<String>,
    /// Direction we want for the audio section.
    pub direction: Direction,
    /// Codecs we offer (or are willing to answer with), in preference
    /// order, with the payload type used when offering each. Answers pick
    /// the first entry the remote offered and mirror the remote's payload
    /// type. Supported: [`AudioCodec::Opus`], [`AudioCodec::G722`],
    /// [`AudioCodec::PCMU`], [`AudioCodec::PCMA`] — G.711 is
    /// mandatory-to-implement in WebRTC (RFC 7874 §3) and G.722 ships in
    /// every browser, so preferring one of them over Opus lets a bridge
    /// match a SIP leg or a mixer and skip transcoding.
    pub codecs: Vec<(AudioCodec, u8)>,
    /// Offer telephone-event (RFC 4733) as well.
    pub dtmf: bool,
    /// The video section, if any. `None` (the default) offers audio only
    /// and rejects offered video.
    pub video: Option<VideoConfig>,
    /// Transport tunables.
    pub transport: TransportConfig,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            stun_servers: vec![],
            direction: Direction::SendRecv,
            codecs: vec![
                (AudioCodec::Opus, 111),
                (AudioCodec::PCMU, 0),
                (AudioCodec::PCMA, 8),
            ],
            dtmf: true,
            video: None,
            transport: TransportConfig::default(),
        }
    }
}

/// A WebRTC peer connection (one audio section and an optional video
/// section, BUNDLE, rtcp-mux, DTLS-SRTP, trickle ICE).
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
    video_ssrc: u32,
    content_ssrc: u32,
    cname: String,
    msid: String,
    /// The codec pinned by offer/answer (with the payload type the remote
    /// expects), once negotiation completed.
    negotiated: Option<(AudioCodec, u8)>,
    /// The video section pinned by offer/answer, once negotiation
    /// completed with both sides accepting one.
    negotiated_video: Option<NegotiatedVideo>,
    /// The shared screen's section, pinned the same way.
    negotiated_content: Option<NegotiatedVideo>,
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
        // The video SSRCs are even, so they can never equal the odd audio
        // one, and differ from each other by construction.
        let video_ssrc = ((rnd >> 96) as u32 & !1).max(2);
        let content_ssrc = ((rnd >> 64) as u32 & !3).wrapping_add(2).max(4);
        let content_ssrc = if content_ssrc == video_ssrc {
            content_ssrc.wrapping_add(4)
        } else {
            content_ssrc
        };
        let session_id = ((rnd >> 64) as u64) & 0x7fff_ffff_ffff_ffff;
        Ok(Self {
            cname: format!("forge-{}", &connection_id[7..15]),
            msid: format!("forge-{}", &connection_id[7..15]),
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
            video_ssrc,
            content_ssrc,
            negotiated: None,
            negotiated_video: None,
            negotiated_content: None,
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
        let (t, rx) = Transport::new(
            tcfg,
            self.cert.clone(),
            self.ssrc,
            self.video_ssrc,
            self.content_ssrc,
            self.cname.clone(),
            self.state.clone(),
        )
        .await?;
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
        mid_ext: Option<u8>,
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
            msid: &self.msid,
            direction: self.cfg.direction,
            codecs: &self.cfg.codecs,
            dtmf_pt: if self.cfg.dtmf { Some(101) } else { None },
            mid: "0",
            video: self.cfg.video.as_ref().map(|v| LocalVideo {
                ssrc: self.video_ssrc,
                codecs: &v.codecs,
                direction: v.direction,
                mid: "1",
                h264_profile_level_id: &v.h264_profile_level_id,
                max_kbps: v.max_kbps,
                content: false,
            }),
            content: self
                .cfg
                .video
                .as_ref()
                .filter(|v| v.content)
                .map(|v| LocalVideo {
                    ssrc: self.content_ssrc,
                    codecs: &v.codecs,
                    direction: v.direction,
                    mid: "2",
                    h264_profile_level_id: &v.h264_profile_level_id,
                    max_kbps: v.max_kbps,
                    content: true,
                }),
            mid_ext,
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
        let sdp = sdp::build_offer(&self.local_params(
            &t,
            &creds,
            &candidates,
            DtlsSetup::Actpass,
            Some(MID_EXTENSION_ID),
        ));
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
        // The extension id is the offer's to choose; an answer mirrors it.
        let (answer, negotiated) = sdp::build_answer(
            &self.local_params(&t, &creds, &candidates, setup, remote.mid_ext),
            &remote,
        )?;
        let selected = negotiated.audio;
        self.negotiated = Some(selected);
        self.negotiated_video = negotiated.video;
        self.negotiated_content = negotiated.content;
        self.install_demux(&t, remote.mid_ext);
        self.local_sdp = Some(answer.clone());
        self.signaling = SignalingState::Stable;
        debug!(
            "{}: created answer v{} ({:?} pt {}, video {:?})",
            self.connection_id,
            self.session_version,
            selected.0,
            selected.1,
            self.negotiated_video
                .as_ref()
                .map(|v| (v.codec, v.payload_type))
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
        let Some(audio) = remote.audio.as_ref() else {
            return Err(WebRtcError::SdpError(forge_sdp::SdpError::Internal(
                "answer rejected the audio section".into(),
            )));
        };
        // The answer pins the codec: the first of our preferences it
        // accepted (a conformant answer only lists codecs we offered).
        let selected = sdp::select_codec(&self.cfg.codecs, audio)
            .ok_or(WebRtcError::SdpError(forge_sdp::SdpError::NoCommonCodec))?;
        // Video is pinned only when we offered it and the answer kept one
        // of our codecs; a rejected section (or one we never offered)
        // leaves it unset.
        let video = match (&self.cfg.video, remote.video.as_ref()) {
            (Some(cfg), Some(rv)) => video_from_answer(&cfg.codecs, cfg.direction, rv),
            _ => None,
        };
        let content = match (&self.cfg.video, remote.content.as_ref()) {
            (Some(cfg), Some(rv)) if cfg.content => {
                video_from_answer(&cfg.codecs, cfg.direction, rv)
            }
            _ => None,
        };
        // The extension is on only if the answer kept it.
        let mid_ext = remote.mid_ext.filter(|id| *id == MID_EXTENSION_ID);
        t.set_remote(
            &remote.ufrag,
            &remote.pwd,
            &remote.fingerprint,
            dtls_role,
            &remote.candidates,
        )?;
        self.remote_sdp = Some(sdp.to_string());
        self.remote = Some(remote);
        self.negotiated = Some(selected);
        self.negotiated_video = video;
        self.negotiated_content = content;
        self.install_demux(&t, mid_ext);
        self.local_sdp = self.pending_local_sdp.take();
        self.signaling = SignalingState::Stable;
        Ok(())
    }

    /// Tell the transport which payload types belong to which stream, so
    /// inbound packets are sorted and their jitter measured at the right
    /// clock — and how the two video sections, which share payload types,
    /// are told apart: by the `sdes:mid` extension when `mid_ext` was
    /// negotiated, else by the SSRCs the remote signalled.
    fn install_demux(&self, t: &Transport, mid_ext: Option<u8>) {
        let mut map = Vec::with_capacity(5);
        if let Some((codec, pt)) = self.negotiated {
            map.push(PayloadMapping {
                payload_type: pt,
                kind: MediaKind::Audio,
                clock_rate: rtp_clock(codec),
            });
        }
        if let Some(audio) = self.remote.as_ref().and_then(|r| r.audio.as_ref()) {
            for &(pt, clock) in &audio.dtmf_pts {
                map.push(PayloadMapping {
                    payload_type: pt,
                    kind: MediaKind::Audio,
                    clock_rate: clock,
                });
            }
        }
        for v in self.negotiated_video.iter().chain(&self.negotiated_content) {
            if !map.iter().any(|m| m.payload_type == v.payload_type) {
                map.push(PayloadMapping {
                    payload_type: v.payload_type,
                    kind: MediaKind::Video,
                    clock_rate: VideoCodec::CLOCK_RATE,
                });
            }
        }
        t.set_demux(&DemuxConfig {
            payload_map: map,
            mid_ext,
            video_mid: self.negotiated_video.as_ref().and_then(|v| v.mid.clone()),
            content_mid: self.negotiated_content.as_ref().and_then(|v| v.mid.clone()),
            video_ssrc: self.negotiated_video.as_ref().and_then(|v| v.remote_ssrc),
            content_ssrc: self.negotiated_content.as_ref().and_then(|v| v.remote_ssrc),
        });
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

    /// Direction for the audio section in the next offer or answer.
    pub fn set_direction(&mut self, direction: Direction) {
        self.cfg.direction = direction;
    }

    /// The video section for the next offer or answer: `Some` adds or keeps
    /// one, `None` drops it (a re-offer then rejects it with port 0). The
    /// current negotiation is untouched until the next description.
    pub fn set_video(&mut self, video: Option<VideoConfig>) {
        self.cfg.video = video;
    }

    /// The configured video section, if any.
    pub fn video_config(&self) -> Option<&VideoConfig> {
        self.cfg.video.as_ref()
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
        let (codec, payload_type) = self.negotiated_codec();
        Ok(AudioSender {
            transport: t,
            codec,
            payload_type,
            timestamp: self.audio_ts.clone(),
            started: self.audio_started.clone(),
        })
    }

    /// A cloneable handle for sending video packets from another task.
    /// Fails until a video section is negotiated.
    pub fn video_sender(&self) -> Result<VideoSender> {
        let t = self
            .transport
            .clone()
            .ok_or_else(|| WebRtcError::InvalidState("no transport yet".into()))?;
        let v = self
            .negotiated_video
            .as_ref()
            .ok_or_else(|| WebRtcError::InvalidState("no video section negotiated".into()))?;
        Ok(VideoSender {
            transport: t,
            stream: VideoStream::Camera,
            ssrc: self.video_ssrc,
            codec: v.codec,
            payload_type: v.payload_type,
        })
    }

    /// A cloneable handle for sending a shared screen's packets on the
    /// content section. Fails until one is negotiated.
    pub fn content_sender(&self) -> Result<VideoSender> {
        let t = self
            .transport
            .clone()
            .ok_or_else(|| WebRtcError::InvalidState("no transport yet".into()))?;
        let v = self
            .negotiated_content
            .as_ref()
            .ok_or_else(|| WebRtcError::InvalidState("no content section negotiated".into()))?;
        Ok(VideoSender {
            transport: t,
            stream: VideoStream::Content,
            ssrc: self.content_ssrc,
            codec: v.codec,
            payload_type: v.payload_type,
        })
    }

    /// Send RTCP feedback (PLI, FIR, NACK, REMB, …) or a BYE. The transport
    /// prefixes its current report and SDES so the packet on the wire is a
    /// proper compound; periodic reports need no call here.
    pub async fn send_rtcp(&self, packets: &[RtcpPacket]) -> Result<()> {
        let t = self
            .transport
            .as_ref()
            .ok_or_else(|| WebRtcError::InvalidState("no transport yet".into()))?;
        t.send_rtcp(packets).await
    }

    /// The video section pinned by offer/answer, once both sides accepted
    /// one.
    pub fn negotiated_video(&self) -> Option<&NegotiatedVideo> {
        self.negotiated_video.as_ref()
    }

    /// The shared screen's section pinned by offer/answer, once both
    /// sides accepted one.
    pub fn negotiated_content(&self) -> Option<&NegotiatedVideo> {
        self.negotiated_content.as_ref()
    }

    /// Our video sending SSRC (the camera's section).
    pub fn video_ssrc(&self) -> u32 {
        self.video_ssrc
    }

    /// Our sending SSRC for the shared screen's section.
    pub fn content_ssrc(&self) -> u32 {
        self.content_ssrc
    }

    /// Reception statistics for every remote SSRC heard so far.
    pub fn sources(&self) -> Vec<forge_rtp::SourceStats> {
        self.transport
            .as_ref()
            .map(|t| t.sources())
            .unwrap_or_default()
    }

    /// The codec pinned by offer/answer, with the payload type the remote
    /// expects for it. Before negotiation completes this falls back to the
    /// first configured preference.
    pub fn negotiated_codec(&self) -> (AudioCodec, u8) {
        self.negotiated.unwrap_or_else(|| {
            self.cfg
                .codecs
                .first()
                .copied()
                .unwrap_or((AudioCodec::Opus, 111))
        })
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

    /// Our audio sending SSRC.
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
    codec: AudioCodec,
    payload_type: u8,
    timestamp: Arc<AtomicU32>,
    started: Arc<AtomicBool>,
}

impl AudioSender {
    /// Send one encoded frame covering `samples` samples at the codec clock
    /// (a 20 ms frame is 960 for Opus at 48 kHz, 160 for PCMU/PCMA at
    /// 8 kHz — see [`AudioSender::samples_per_20ms`]). The RTP marker bit
    /// is set on the first packet of the stream.
    pub async fn send_audio(&self, frame: Bytes, samples: u32) -> Result<()> {
        let ts = self.timestamp.fetch_add(samples, Ordering::SeqCst);
        let marker = !self.started.swap(true, Ordering::SeqCst);
        self.transport
            .send_rtp(self.payload_type, marker, ts, frame)
            .await
    }

    /// The negotiated codec this sender's frames must be encoded with.
    pub fn codec(&self) -> AudioCodec {
        self.codec
    }

    /// The `samples` value for a 20 ms frame of the negotiated codec, at
    /// its RTP clock (160 for G.722 too, which is clocked at 8 kHz on the
    /// wire).
    pub fn samples_per_20ms(&self) -> u32 {
        rtp_clock(self.codec) / 50
    }

    /// The RTP clock rate of the negotiated codec.
    pub fn clock_rate(&self) -> u32 {
        rtp_clock(self.codec)
    }

    /// Send RTCP from this task; see [`PeerConnection::send_rtcp`].
    pub async fn send_rtcp(&self, packets: &[RtcpPacket]) -> Result<()> {
        self.transport.send_rtcp(packets).await
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

/// Sends pre-built video RTP packets as SRTP on one video section. Clone
/// freely.
#[derive(Clone)]
pub struct VideoSender {
    transport: Transport,
    stream: VideoStream,
    ssrc: u32,
    codec: VideoCodec,
    payload_type: u8,
}

impl VideoSender {
    /// Send one RTP packet the producer built: its sequence number,
    /// timestamp, payload type and marker go out unchanged, the SSRC is
    /// replaced by [`VideoSender::ssrc`]. A conference room subscription's
    /// packets — and the retransmissions it answers NACKs with — go through
    /// here as they are.
    pub async fn send_packet(&self, packet: Bytes) -> Result<()> {
        self.transport.send_video_on(packet, self.stream).await
    }

    /// Which section this sender's packets go out on.
    pub fn stream(&self) -> VideoStream {
        self.stream
    }

    /// Send RTCP from this task; see [`PeerConnection::send_rtcp`].
    pub async fn send_rtcp(&self, packets: &[RtcpPacket]) -> Result<()> {
        self.transport.send_rtcp(packets).await
    }

    /// The SSRC packets leave with.
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// The negotiated video codec.
    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// Payload type the remote expects for it.
    pub fn payload_type(&self) -> u8 {
        self.payload_type
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
