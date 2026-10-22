//! A third party on a two-party session: a supervisor who monitors,
//! whispers or barges.
//!
//! The supervisor has a port of their own (a phone is sent there and sends
//! there), so the session's two legs are untouched: A and B keep their
//! addresses, codecs, SRTP and relay. What the supervisor hears and says is
//! mixed by [`forge_mixer::SupervisionMix`], the core every media path
//! shares:
//!
//! - both legs' audio, already decoded for every packet the forwarding loop
//!   relays, is written into the mix ([`MediaSession::feed_party`]);
//! - the supervisor's packets are read on their own port and written in;
//! - every 20 ms the mix is ticked: the supervisor is sent A and B summed,
//!   and a leg that hears the supervisor (the coached leg of a whisper,
//!   both legs of a barge) has its relay suppressed and is sent the other
//!   leg with the supervisor summed in, through the leg's own scheduled
//!   playout — in its codec, on its generated stream, SRTP as the leg has.
//!
//! The mix runs at 8 kHz, so a leg that hears the supervisor hears
//! narrowband audio while they do. The supervisor's own leg is plain RTP in
//! G.711 (PCMU or PCMA): a desk phone or a softphone.

use crate::session::{MediaSession, ParticipantLabel, ScheduledPlayoutSource};
use forge_core::{AudioCodec, ForgeError, Result};
use forge_mixer::{CallParty, SupervisionMix, SupervisionMode, Voice};
use forge_rtp::{PortPair, RtpSocketPair};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;

/// The mix's rate.
pub const PARTY_SAMPLE_RATE: u32 = 8000;
/// Samples per 20 ms frame at the mix's rate.
const FRAME_SAMPLES: usize = 160;
const FRAME: Duration = Duration::from_millis(20);

/// The supervisor's side of a supervised session.
pub(crate) struct Party {
    ports: PortPair,
    sockets: Arc<RtpSocketPair>,
    /// Where the supervisor is: from their SDP, then latched to where they
    /// send from.
    remote: Mutex<Option<SocketAddr>>,
    /// The supervisor's codec and payload type, once connected.
    codec: Mutex<Option<(AudioCodec, u8)>>,
    mix: Mutex<SupervisionMix>,
    /// The legs whose relay this party suppresses now.
    suppressing: Mutex<[bool; 2]>,
    /// The RTP stream toward the supervisor.
    stream: Mutex<Stream>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

struct Stream {
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    started: bool,
}

impl Party {
    pub(crate) fn ports(&self) -> PortPair {
        self.ports
    }

    fn leg_index(leg: ParticipantLabel) -> usize {
        match leg {
            ParticipantLabel::A => 0,
            ParticipantLabel::B => 1,
        }
    }
}

fn call_party(leg: ParticipantLabel) -> CallParty {
    match leg {
        ParticipantLabel::A => CallParty::A,
        ParticipantLabel::B => CallParty::B,
    }
}

impl MediaSession {
    /// Add a supervisor in `mode`: a port of their own is bound, and its
    /// pair returned for the SDP offered to (or answered to) their phone.
    /// Nothing is sent until [`MediaSession::connect_party`] names their
    /// address and codec. A session has one supervisor at a time.
    pub async fn add_party(self: &Arc<Self>, mode: SupervisionMode) -> Result<PortPair> {
        if self.party().is_some() {
            return Err(ForgeError::ResourceLimit(
                "the session already has a supervisor".into(),
            ));
        }
        let (ports, sockets, mut guard) = crate::session::allocate_and_bind(
            self.port_pool(),
            self.config().socket_config.clone(),
            self.config().min_free_port_pairs,
        )
        .await?;
        guard.disarm();
        let party = Arc::new(Party {
            ports,
            sockets: Arc::new(sockets),
            remote: Mutex::new(None),
            codec: Mutex::new(None),
            mix: Mutex::new(SupervisionMix::new(mode, FRAME_SAMPLES)),
            suppressing: Mutex::new([false; 2]),
            stream: Mutex::new(Stream {
                ssrc: rand::random(),
                sequence: rand::random(),
                timestamp: rand::random(),
                started: false,
            }),
            tasks: Mutex::new(Vec::new()),
        });
        *self.party_slot().write().expect("party slot") = Some(Arc::clone(&party));
        self.apply_party_mode(&party, mode);

        let receiver = tokio::spawn(receive(Arc::downgrade(&party)));
        let ticker = tokio::spawn(tick(Arc::downgrade(self), Arc::downgrade(&party)));
        party
            .tasks
            .lock()
            .expect("tasks")
            .extend([receiver, ticker]);
        tracing::info!(
            call_id = %self.call_id().0,
            port = ports.rtp_port,
            mode = ?mode,
            "Supervisor added"
        );
        Ok(ports)
    }

    /// Where the supervisor is and what they send: G.711 only.
    pub fn connect_party(
        &self,
        remote: SocketAddr,
        codec: AudioCodec,
        payload_type: u8,
    ) -> Result<()> {
        let party = self.party().ok_or_else(|| {
            ForgeError::ParticipantNotFound("the session has no supervisor".into())
        })?;
        if !matches!(codec, AudioCodec::PCMU | AudioCodec::PCMA) {
            return Err(ForgeError::Codec(format!(
                "a supervisor sends PCMU or PCMA, not {codec:?}"
            )));
        }
        *party.remote.lock().expect("remote") = Some(remote);
        *party.codec.lock().expect("codec") = Some((codec, payload_type));
        Ok(())
    }

    /// Change how the supervisor takes part.
    pub fn set_party_mode(&self, mode: SupervisionMode) -> Result<()> {
        let party = self.party().ok_or_else(|| {
            ForgeError::ParticipantNotFound("the session has no supervisor".into())
        })?;
        party.mix.lock().expect("mix").set_mode(mode);
        self.apply_party_mode(&party, mode);
        Ok(())
    }

    /// The supervisor's mode, if there is one.
    pub fn party_mode(&self) -> Option<SupervisionMode> {
        self.party().map(|p| p.mix.lock().expect("mix").mode())
    }

    /// The supervisor's port pair, if there is one.
    pub fn party_ports(&self) -> Option<PortPair> {
        self.party().map(|p| p.ports())
    }

    /// Remove the supervisor: the legs are relayed as before and the port
    /// goes back to the pool.
    pub async fn remove_party(&self) -> Result<()> {
        let Some(party) = self.party_slot().write().expect("party slot").take() else {
            return Ok(());
        };
        for task in party.tasks.lock().expect("tasks").drain(..) {
            task.abort();
        }
        let suppressing = *party.suppressing.lock().expect("suppressing");
        for (i, leg) in [ParticipantLabel::A, ParticipantLabel::B]
            .into_iter()
            .enumerate()
        {
            if suppressing[i] {
                self.release_relay_to(leg);
            }
        }
        self.port_pool().deallocate(party.ports).await;
        tracing::info!(call_id = %self.call_id().0, "Supervisor removed");
        Ok(())
    }

    /// A leg's decoded audio, at `sample_rate`, for the supervisor's mix.
    /// Nothing when the session has no supervisor.
    pub(crate) fn feed_party(&self, leg: ParticipantLabel, samples: &[i16], sample_rate: u32) {
        let Some(party) = self.party() else {
            return;
        };
        let samples = crate::forwarding::ForwardingEngine::resample_audio(
            samples,
            sample_rate,
            PARTY_SAMPLE_RATE,
        );
        let voice = match leg {
            ParticipantLabel::A => Voice::A,
            ParticipantLabel::B => Voice::B,
        };
        party.mix.lock().expect("mix").write(voice, &samples);
    }

    /// Suppress the relay to each leg that hears the supervisor, release it
    /// for each that no longer does.
    fn apply_party_mode(&self, party: &Party, mode: SupervisionMode) {
        let mut suppressing = party.suppressing.lock().expect("suppressing");
        for leg in [ParticipantLabel::A, ParticipantLabel::B] {
            let i = Party::leg_index(leg);
            let wanted = mode.heard_by(call_party(leg));
            if wanted && !suppressing[i] {
                self.suppress_relay_to(leg);
            } else if !wanted && suppressing[i] {
                self.release_relay_to(leg);
            }
            suppressing[i] = wanted;
        }
    }
}

/// Read the supervisor's packets into the mix.
async fn receive(party: Weak<Party>) {
    loop {
        let Some(sockets) = party.upgrade().map(|p| Arc::clone(&p.sockets)) else {
            return;
        };
        let (bytes, source) = match sockets.recv_rtp_raw().await {
            Ok(received) => received,
            Err(e) => {
                tracing::debug!(error = %e, "Supervisor socket closed");
                return;
            }
        };
        let Some(party) = party.upgrade() else {
            return;
        };
        let Ok(packet) = forge_rtp::RtpPacket::parse(bytes) else {
            continue;
        };
        {
            // Symmetric RTP: the supervisor is where they send from, so a
            // phone behind NAT still hears.
            let mut remote = party.remote.lock().expect("remote");
            if remote.is_none_or(|r| r.ip() == source.ip()) {
                *remote = Some(source);
            }
        }
        let Some((codec, payload_type)) = *party.codec.lock().expect("codec") else {
            continue;
        };
        if packet.header.payload_type() != payload_type {
            continue;
        }
        let samples: Vec<i16> = match codec {
            AudioCodec::PCMA => packet
                .payload
                .iter()
                .map(|&b| forge_codecs::g711::decode_alaw(b))
                .collect(),
            _ => packet
                .payload
                .iter()
                .map(|&b| forge_codecs::g711::decode_ulaw(b))
                .collect(),
        };
        party
            .mix
            .lock()
            .expect("mix")
            .write(Voice::Supervisor, &samples);
    }
}

/// Every 20 ms: the supervisor is sent both legs, and each leg that hears
/// the supervisor is sent its mix.
async fn tick(session: Weak<MediaSession>, party: Weak<Party>) {
    let mut interval = tokio::time::interval(FRAME);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let (Some(session), Some(party)) = (session.upgrade(), party.upgrade()) else {
            return;
        };
        let frame = party.mix.lock().expect("mix").tick();
        for (leg, mixed) in [
            (ParticipantLabel::A, frame.a),
            (ParticipantLabel::B, frame.b),
        ] {
            if let Some(mixed) = mixed {
                let target = match leg {
                    ParticipantLabel::A => crate::media_bridge::MediaTarget::A,
                    ParticipantLabel::B => crate::media_bridge::MediaTarget::B,
                };
                if let Err(e) = session
                    .schedule_audio_playout(
                        target,
                        PARTY_SAMPLE_RATE,
                        &mixed,
                        None,
                        crate::media_bridge::PlayoutMode::Append,
                        ScheduledPlayoutSource::Supervision,
                    )
                    .await
                {
                    tracing::debug!(error = %e, leg = leg.as_str(), "A supervised leg's mix was not scheduled");
                }
            }
        }
        send_to_supervisor(&party, &frame.supervisor).await;
    }
}

async fn send_to_supervisor(party: &Party, samples: &[i16]) {
    let Some(remote) = *party.remote.lock().expect("remote") else {
        return;
    };
    let Some((codec, payload_type)) = *party.codec.lock().expect("codec") else {
        return;
    };
    let payload: Vec<u8> = match codec {
        AudioCodec::PCMA => samples
            .iter()
            .map(|&s| forge_codecs::g711::encode_alaw(s))
            .collect(),
        _ => samples
            .iter()
            .map(|&s| forge_codecs::g711::encode_ulaw(s))
            .collect(),
    };
    let packet = {
        let mut stream = party.stream.lock().expect("stream");
        let marker = !stream.started;
        stream.started = true;
        let packet = forge_rtp::RtpPacket::build(
            payload_type,
            stream.sequence,
            stream.timestamp,
            stream.ssrc,
            bytes::Bytes::from(payload),
            marker,
        );
        stream.sequence = stream.sequence.wrapping_add(1);
        stream.timestamp = stream.timestamp.wrapping_add(samples.len() as u32);
        packet
    };
    if let Err(e) = party.sockets.send_rtp_to(&packet.to_bytes(), remote).await {
        tracing::debug!(error = %e, "A packet to the supervisor was not sent");
    }
}
