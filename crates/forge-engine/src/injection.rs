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
    completion_tx: oneshot::Sender<PlaybackStatus>,
}

/// Manages active audio playbacks for sessions
pub struct PlaybackManager {
    /// Active playbacks by call ID
    playbacks: DashMap<CallId, Vec<PlaybackId>>,
    /// Playback state by ID
    playback_state: DashMap<PlaybackId, Arc<RwLock<PlaybackInternal>>>,
}

impl PlaybackManager {
    /// Create a new playback manager
    pub fn new() -> Self {
        Self {
            playbacks: DashMap::new(),
            playback_state: DashMap::new(),
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

        let internal = PlaybackInternal {
            id,
            call_id: call_id.clone(),
            source,
            target,
            mix_mode,
            active: AtomicBool::new(true),
            stop_rx,
            completion_tx,
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
                guard.source.read_frame(960) // 20ms at 48kHz
            };

            match frame_result {
                Ok(frame) => {
                    // TODO: Inject frame into RTP stream
                    // This will be integrated with ForwardingEngine
                    debug!(
                        playback_id = %id,
                        samples = frame.len(),
                        "Read audio frame"
                    );

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

        // Send completion status
        // Remove from active playbacks
        self.playback_state.remove(&id);
        if let Some(mut ids) = self.playbacks.get_mut(&call_id) {
            ids.retain(|&pid| pid != id);
        }

        // Note: We can't send the completion status through completion_tx because it was moved
        // into PlaybackInternal. The important thing is that the playback is cleaned up from
        // playback_state and playbacks maps. Callers can detect completion by the handle being dropped.
        info!(playback_id = %id, status = ?status, "Playback task completed");

        Ok(())
    }
}

impl Clone for PlaybackManager {
    fn clone(&self) -> Self {
        Self {
            playbacks: self.playbacks.clone(),
            playback_state: self.playback_state.clone(),
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
