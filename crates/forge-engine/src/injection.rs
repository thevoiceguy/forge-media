//! Audio injection and playback management
//!
//! This module provides audio injection capabilities for media sessions,
//! allowing audio sources (files, TTS, tones) to be played into active calls.

use anyhow::{Context, Result};
use dashmap::DashMap;
use forge_core::CallId;
use forge_injection::AudioSource;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info};

/// Unique identifier for a playback session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaybackId(u64);

impl PlaybackId {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for PlaybackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "playback-{}", self.0)
    }
}

/// Audio injection target
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTarget {
    /// Inject audio to participant A only
    ParticipantA,
    /// Inject audio to participant B only
    ParticipantB,
    /// Inject audio to both participants
    Both,
}

/// Mix mode for audio injection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixMode {
    /// Mix injected audio with existing audio
    Mix,
    /// Replace existing audio with injected audio
    Replace,
    /// Lower existing audio volume during injection (ducking)
    Duck,
}

/// Playback completion status
#[derive(Debug, Clone)]
pub enum PlaybackStatus {
    /// Playback completed successfully
    Completed,
    /// Playback stopped by request
    Stopped,
    /// Playback failed with error
    Failed(String),
}

/// Handle for controlling an active playback
#[derive(Debug)]
pub struct PlaybackHandle {
    id: PlaybackId,
    call_id: CallId,
    stop_tx: mpsc::UnboundedSender<()>,
    completion_rx: Arc<RwLock<Option<oneshot::Receiver<PlaybackStatus>>>>,
}

impl PlaybackHandle {
    /// Get the playback ID
    pub fn id(&self) -> PlaybackId {
        self.id
    }

    /// Get the call ID this playback is associated with
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }

    /// Stop the playback
    pub async fn stop(&self) -> Result<()> {
        self.stop_tx
            .send(())
            .context("Failed to send stop signal")?;
        Ok(())
    }

    /// Wait for playback to complete and get the status
    pub async fn wait_completion(&self) -> Result<PlaybackStatus> {
        let mut rx_guard = self.completion_rx.write().await;
        if let Some(rx) = rx_guard.take() {
            rx.await.context("Playback completion channel closed")
        } else {
            Err(anyhow::anyhow!(
                "Completion already awaited or playback handle dropped"
            ))
        }
    }
}

/// Internal playback state
struct PlaybackInternal {
    id: PlaybackId,
    call_id: CallId,
    source: Box<dyn AudioSource>,
    target: AudioTarget,
    mix_mode: MixMode,
    active: AtomicBool,
    stop_rx: mpsc::UnboundedReceiver<()>,
    completion_tx: Option<oneshot::Sender<PlaybackStatus>>,
    /// RTP sequence number (increments per packet)
    rtp_seq: u16,
    /// RTP timestamp (increments by samples per packet)
    rtp_timestamp: u32,
    /// RTP SSRC (randomly generated per playback)
    rtp_ssrc: u32,
}

/// Manages active audio playbacks for sessions
pub struct PlaybackManager {
    /// Active playbacks by call ID
    playbacks: DashMap<CallId, Vec<PlaybackId>>,
    /// Playback state by ID
    playback_state: DashMap<PlaybackId, Arc<RwLock<PlaybackInternal>>>,
    /// Reference to session manager for accessing RTP sockets
    session_manager: Option<Arc<crate::manager::SessionManager>>,
}

impl PlaybackManager {
    /// Create a new playback manager
    pub fn new() -> Self {
        Self {
            playbacks: DashMap::new(),
            playback_state: DashMap::new(),
            session_manager: None,
        }
    }

    /// Create a new playback manager with session manager reference
    pub fn new_with_session_manager(session_manager: Arc<crate::manager::SessionManager>) -> Self {
        Self {
            playbacks: DashMap::new(),
            playback_state: DashMap::new(),
            session_manager: Some(session_manager),
        }
    }

    /// Start a new playback
    pub async fn start_playback(
        &self,
        call_id: CallId,
        source: Box<dyn AudioSource>,
        target: AudioTarget,
        mix_mode: MixMode,
    ) -> Result<PlaybackHandle> {
        let id = PlaybackId::new();
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = oneshot::channel();

        info!(
            call_id = %call_id,
            playback_id = %id,
            target = ?target,
            mix_mode = ?mix_mode,
            "Starting audio playback"
        );

        // Generate random RTP parameters for this playback
        let rtp_ssrc = rand::random::<u32>();
        let rtp_seq = rand::random::<u16>();
        let rtp_timestamp = rand::random::<u32>();

        let internal = PlaybackInternal {
            id,
            call_id: call_id.clone(),
            source,
            target,
            mix_mode,
            active: AtomicBool::new(true),
            stop_rx,
            completion_tx: Some(completion_tx),
            rtp_seq,
            rtp_timestamp,
            rtp_ssrc,
        };

        let internal = Arc::new(RwLock::new(internal));
        self.playback_state.insert(id, Arc::clone(&internal));

        // Add to call's playback list
        self.playbacks
            .entry(call_id.clone())
            .or_insert_with(Vec::new)
            .push(id);

        let handle = PlaybackHandle {
            id,
            call_id,
            stop_tx,
            completion_rx: Arc::new(RwLock::new(Some(completion_rx))),
        };

        // Spawn playback task
        let manager = self.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.run_playback(internal).await {
                error!(playback_id = %id, error = %e, "Playback task failed");
            }
        });

        Ok(handle)
    }

    /// Stop a specific playback
    pub async fn stop_playback(&self, id: PlaybackId) -> Result<()> {
        if let Some(state) = self.playback_state.get(&id) {
            let internal = state.value();
            let mut guard = internal.write().await;
            guard.active.store(false, Ordering::Relaxed);
            debug!(playback_id = %id, "Stopped playback");
        }
        Ok(())
    }

    /// Stop all playbacks for a call
    pub async fn stop_all_playbacks(&self, call_id: &CallId) -> Result<()> {
        if let Some(playback_ids) = self.playbacks.get(call_id) {
            for id in playback_ids.value() {
                self.stop_playback(*id).await?;
            }
        }
        Ok(())
    }

    /// Get active playback count for a call
    pub fn active_playback_count(&self, call_id: &CallId) -> usize {
        self.playbacks
            .get(call_id)
            .map(|ids| ids.len())
            .unwrap_or(0)
    }

    /// Run the playback loop
    async fn run_playback(&self, internal: Arc<RwLock<PlaybackInternal>>) -> Result<()> {
        let (id, call_id) = {
            let guard = internal.read().await;
            (guard.id, guard.call_id.clone())
        };

        info!(playback_id = %id, call_id = %call_id, "Playback task started");

        let status = loop {
            // Check if stopped
            {
                let mut guard = internal.write().await;
                if let Ok(()) = guard.stop_rx.try_recv() {
                    info!(playback_id = %id, "Playback stopped by request");
                    break PlaybackStatus::Stopped;
                }
            }

            // Read next frame from source
            let frame_result = {
                let mut guard = internal.write().await;
                guard.source.read_frame(160) // 20ms at 8kHz (standard for G.711)
            };

            match frame_result {
                Ok(frame) => {
                    debug!(
                        playback_id = %id,
                        samples = frame.len(),
                        "Read audio frame"
                    );

                    // Send RTP packet(s) for this audio frame
                    if let Err(e) = self.send_audio_frame(&internal, &frame).await {
                        error!(playback_id = %id, error = %e, "Failed to send RTP packet");
                        // Continue playback despite RTP errors
                    }

                    // Simulate playback timing (20ms per frame)
                    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
                }
                Err(e) => {
                    // Check if source is finished
                    let is_finished = {
                        let guard = internal.read().await;
                        guard.source.is_finished()
                    };

                    if is_finished {
                        info!(playback_id = %id, "Playback completed");
                        break PlaybackStatus::Completed;
                    } else {
                        error!(playback_id = %id, error = %e, "Failed to read audio frame");
                        break PlaybackStatus::Failed(e.to_string());
                    }
                }
            }
        };

        // Extract completion_tx from internal state before cleanup
        let completion_tx = {
            let mut guard = internal.write().await;
            guard.completion_tx.take()
        };

        // Remove from active playbacks
        self.playback_state.remove(&id);
        if let Some(mut ids) = self.playbacks.get_mut(&call_id) {
            ids.retain(|&pid| pid != id);
        }

        // Send completion status to waiting handle
        if let Some(tx) = completion_tx {
            if let Err(_) = tx.send(status.clone()) {
                debug!(playback_id = %id, "Failed to send completion status - receiver dropped");
            }
        }

        info!(playback_id = %id, status = ?status, "Playback task completed");

        Ok(())
    }

    /// Send an audio frame as RTP packet(s)
    async fn send_audio_frame(
        &self,
        internal: &Arc<RwLock<PlaybackInternal>>,
        frame: &[i16],
    ) -> Result<()> {
        use bytes::Bytes;

        debug!("send_audio_frame called, frame samples: {}", frame.len());

        // Get playback state
        let (call_id, target, rtp_seq, rtp_timestamp, rtp_ssrc) = {
            let guard = internal.read().await;
            (
                guard.call_id.clone(),
                guard.target,
                guard.rtp_seq,
                guard.rtp_timestamp,
                guard.rtp_ssrc,
            )
        };

        debug!(
            call_id = %call_id,
            target = ?target,
            rtp_seq = rtp_seq,
            rtp_timestamp = rtp_timestamp,
            rtp_ssrc = rtp_ssrc,
            "Got playback state"
        );

        // Get session manager
        let session_manager = self
            .session_manager
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Session manager not set"))?;

        debug!("Session manager is set");

        // Get session
        let session = session_manager
            .get_session(&call_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found for call {}", call_id.0))?;

        debug!(call_id = %call_id, "Got session");

        // Convert i16 PCM samples to u8 G.711 µ-law (payload type 0)
        // For simplicity, assume the audio source provides 8kHz mono PCM
        // In production, this should handle sample rate conversion and proper encoding
        let payload = pcm_to_ulaw(frame);

        // Create RTP packet
        // Payload type 0 = PCMU (G.711 µ-law)
        let packet = forge_rtp::rtp::RtpPacket::build(
            0,            // payload_type: PCMU
            rtp_seq,      // sequence number
            rtp_timestamp, // timestamp
            rtp_ssrc,     // SSRC
            Bytes::from(payload),
            false,        // marker: false for continuous audio
        );

        // Serialize to bytes
        let packet_bytes = packet.to_bytes();

        // Get RTP sockets from session
        let sockets = session.sockets();

        // Determine which participant(s) to send to based on target
        let send_to_a = matches!(target, AudioTarget::ParticipantA | AudioTarget::Both);
        let send_to_b = matches!(target, AudioTarget::ParticipantB | AudioTarget::Both);

        // Get participant endpoints
        let participant_a = session.participant_a();
        let participant_b = session.participant_b();

        let addr_a = if send_to_a {
            participant_a.read().await.remote_addr
        } else {
            None
        };

        let addr_b = if send_to_b {
            participant_b.read().await.remote_addr
        } else {
            None
        };

        debug!(
            addr_a = ?addr_a,
            addr_b = ?addr_b,
            packet_size = packet_bytes.len(),
            "Prepared to send RTP packets"
        );

        // Send RTP packets to target participant(s)
        if let Some(addr) = addr_a {
            debug!("Sending RTP packet to participant A at {}", addr);
            if let Err(e) = sockets.send_rtp_to(&packet_bytes, addr).await {
                error!("Failed to send RTP to participant A at {}: {}", addr, e);
            } else {
                debug!("Successfully sent RTP to participant A at {}", addr);
            }
        } else {
            debug!("No address for participant A, skipping send");
        }

        if let Some(addr) = addr_b {
            debug!("Sending RTP packet to participant B at {}", addr);
            if let Err(e) = sockets.send_rtp_to(&packet_bytes, addr).await {
                error!("Failed to send RTP to participant B at {}: {}", addr, e);
            } else {
                debug!("Successfully sent RTP to participant B at {}", addr);
            }
        } else {
            debug!("No address for participant B, skipping send");
        }

        // Update RTP state (increment seq and timestamp)
        {
            let mut guard = internal.write().await;
            guard.rtp_seq = guard.rtp_seq.wrapping_add(1);
            // Timestamp increments by number of samples at the codec sample rate
            // For G.711 at 8kHz: 160 samples for 20ms
            guard.rtp_timestamp = guard.rtp_timestamp.wrapping_add(frame.len() as u32);
        }

        debug!("RTP packet sent successfully");
        Ok(())
    }
}

/// Convert PCM i16 samples to G.711 µ-law encoding
///
/// This implements the ITU-T G.711 µ-law compression algorithm.
/// Reference: ITU-T Recommendation G.711 (1988)
fn pcm_to_ulaw(samples: &[i16]) -> Vec<u8> {
    const BIAS: i16 = 0x84; // 132 in decimal
    const CLIP: i16 = 32635;

    samples.iter().map(|&sample| {
        // Get sign and magnitude
        let sign = if sample < 0 { 0x80 } else { 0x00 };
        let mut magnitude = sample.abs().min(CLIP) as i16;

        // Add bias
        magnitude = magnitude + BIAS;

        // Find segment (exponent) by finding the position of the highest set bit
        let exponent = if magnitude < 256 {
            0
        } else if magnitude < 512 {
            1
        } else if magnitude < 1024 {
            2
        } else if magnitude < 2048 {
            3
        } else if magnitude < 4096 {
            4
        } else if magnitude < 8192 {
            5
        } else if magnitude < 16384 {
            6
        } else {
            7
        };

        // Extract mantissa (4 bits after the segment bit)
        let mantissa = if exponent == 0 {
            (magnitude >> 4) & 0x0F
        } else {
            (magnitude >> (exponent + 3)) & 0x0F
        };

        // Combine sign, exponent, and mantissa, then invert all bits
        let encoded = sign | (exponent << 4) | (mantissa as u8);
        encoded ^ 0xFF
    }).collect()
}

impl Clone for PlaybackManager {
    fn clone(&self) -> Self {
        Self {
            playbacks: self.playbacks.clone(),
            playback_state: self.playback_state.clone(),
            session_manager: self.session_manager.clone(),
        }
    }
}

impl Default for PlaybackManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_injection::ToneGenerator;

    #[tokio::test]
    async fn test_playback_lifecycle() {
        let manager = PlaybackManager::new();
        let call_id = CallId::new();

        // Create a simple tone source
        let source = Box::new(ToneGenerator::silence(8000));

        let handle = manager
            .start_playback(call_id, source, AudioTarget::Both, MixMode::Replace)
            .await
            .unwrap();

        assert_eq!(manager.active_playback_count(&call_id), 1);

        // Stop playback
        handle.stop().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(manager.active_playback_count(&call_id), 0);
    }

    #[tokio::test]
    async fn test_multiple_playbacks() {
        let manager = PlaybackManager::new();
        let call_id = CallId::new();

        // Start two playbacks
        let source1 = Box::new(ToneGenerator::silence(8000));
        let source2 = Box::new(ToneGenerator::silence(8000));

        let _handle1 = manager
            .start_playback(call_id, source1, AudioTarget::ParticipantA, MixMode::Mix)
            .await
            .unwrap();

        let _handle2 = manager
            .start_playback(call_id, source2, AudioTarget::ParticipantB, MixMode::Mix)
            .await
            .unwrap();

        assert_eq!(manager.active_playback_count(&call_id), 2);

        // Stop all
        manager.stop_all_playbacks(&call_id).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(manager.active_playback_count(&call_id), 0);
    }
}
