//! Audio injection and playback management
//!
//! This module provides audio injection capabilities for media sessions,
//! allowing audio sources (files, TTS, tones) to be played into active calls.
//!
//! A playback reads its source a 20 ms frame at a time and hands each frame
//! to the session's scheduled playout, which encodes it in the target leg's
//! negotiated codec (resampling from the source's rate), protects it with
//! the leg's SRTP and sends it on the leg's own RTP stream. A looped
//! playback starts its source again at the end (hold music); a `Replace`
//! playback holds back the other leg's relayed audio toward its target
//! while it runs, so it is heard alone.

use anyhow::{Context, Result};
use dashmap::DashMap;
use forge_core::CallId;
use forge_injection::AudioSource;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};

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

/// How a playback runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackOptions {
    pub target: AudioTarget,
    pub mix_mode: MixMode,
    /// Start the source again at its end, until stopped.
    pub looped: bool,
}

impl PlaybackOptions {
    pub fn new(target: AudioTarget, mix_mode: MixMode) -> Self {
        Self {
            target,
            mix_mode,
            looped: false,
        }
    }

    pub fn looped(mut self) -> Self {
        self.looped = true;
        self
    }
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
    options: PlaybackOptions,
    active: AtomicBool,
    stop_rx: mpsc::UnboundedReceiver<()>,
    completion_tx: Option<oneshot::Sender<PlaybackStatus>>,
}

/// Manages active audio playbacks for sessions
pub struct PlaybackManager {
    /// Active playbacks by call ID
    playbacks: Arc<DashMap<CallId, Vec<PlaybackId>>>,
    /// Playback state by ID
    playback_state: Arc<DashMap<PlaybackId, Arc<RwLock<PlaybackInternal>>>>,
    /// Reference to session manager for accessing RTP sockets
    session_manager: Option<Arc<crate::manager::SessionManager>>,
}

impl PlaybackManager {
    /// Create a new playback manager
    pub fn new() -> Self {
        Self {
            playbacks: Arc::new(DashMap::new()),
            playback_state: Arc::new(DashMap::new()),
            session_manager: None,
        }
    }

    /// Create a new playback manager with session manager reference
    pub fn new_with_session_manager(session_manager: Arc<crate::manager::SessionManager>) -> Self {
        Self {
            playbacks: Arc::new(DashMap::new()),
            playback_state: Arc::new(DashMap::new()),
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
        self.start_playback_with(call_id, source, PlaybackOptions::new(target, mix_mode))
            .await
    }

    /// Start a new playback with its options (a looped one plays until
    /// stopped).
    pub async fn start_playback_with(
        &self,
        call_id: CallId,
        source: Box<dyn AudioSource>,
        options: PlaybackOptions,
    ) -> Result<PlaybackHandle> {
        let id = PlaybackId::new();
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = oneshot::channel();

        info!(
            call_id = %call_id,
            playback_id = %id,
            target = ?options.target,
            mix_mode = ?options.mix_mode,
            looped = options.looped,
            "Starting audio playback"
        );

        let internal = PlaybackInternal {
            id,
            call_id: call_id.clone(),
            source,
            options,
            active: AtomicBool::new(true),
            stop_rx,
            completion_tx: Some(completion_tx),
        };

        let internal = Arc::new(RwLock::new(internal));
        self.playback_state.insert(id, Arc::clone(&internal));

        // Add to call's playback list
        self.playbacks.entry(call_id.clone()).or_default().push(id);

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
            let guard = internal.write().await;
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

    /// The session a playback plays into, if it is still there.
    fn session(&self, call_id: &CallId) -> Option<Arc<crate::session::MediaSession>> {
        self.session_manager.as_ref()?.get_session(call_id)
    }

    /// Run the playback loop: a frame every 20 ms into the session's
    /// scheduled playout.
    async fn run_playback(&self, internal: Arc<RwLock<PlaybackInternal>>) -> Result<()> {
        use crate::media_bridge::{MediaTarget, PlayoutMode};
        use crate::session::{ParticipantLabel, ScheduledPlayoutSource};

        let (id, call_id, options, sample_rate, channels) = {
            let guard = internal.read().await;
            (
                guard.id,
                guard.call_id.clone(),
                guard.options,
                guard.source.sample_rate().max(1),
                guard.source.channels().max(1),
            )
        };
        let media_target = match options.target {
            AudioTarget::ParticipantA => MediaTarget::A,
            AudioTarget::ParticipantB => MediaTarget::B,
            AudioTarget::Both => MediaTarget::Both,
        };
        let legs: Vec<ParticipantLabel> = [ParticipantLabel::A, ParticipantLabel::B]
            .into_iter()
            .filter(|leg| media_target.includes(*leg))
            .collect();
        let tag = id.to_string();

        info!(playback_id = %id, call_id = %call_id, sample_rate, channels, "Playback task started");

        // Heard alone: the other leg's relayed audio is held back.
        let suppressed = if options.mix_mode == MixMode::Replace {
            self.session(&call_id).map(|session| {
                for leg in &legs {
                    session.suppress_relay_to(*leg);
                }
                session
            })
        } else {
            None
        };

        let frame_len = (sample_rate as usize / 50).max(1) * channels as usize;
        let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(20));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let status = loop {
            ticker.tick().await;
            {
                let mut guard = internal.write().await;
                if let Ok(()) = guard.stop_rx.try_recv() {
                    info!(playback_id = %id, "Playback stopped by request");
                    break PlaybackStatus::Stopped;
                }
                if !guard.active.load(Ordering::Relaxed) {
                    info!(playback_id = %id, "Playback stopped via manager");
                    break PlaybackStatus::Stopped;
                }
            }

            let frame = {
                let mut guard = internal.write().await;
                guard.source.read_frame(frame_len)
            };
            match frame {
                Ok(frame) if !frame.is_empty() => {
                    let mono = downmix(&frame, channels);
                    match self.session(&call_id) {
                        Some(session) => {
                            if let Err(e) = session
                                .schedule_audio_playout(
                                    media_target,
                                    sample_rate,
                                    &mono,
                                    Some(tag.clone()),
                                    PlayoutMode::Append,
                                    ScheduledPlayoutSource::Injection,
                                )
                                .await
                            {
                                warn!(playback_id = %id, error = %e, "Failed to schedule a playback frame");
                            }
                        }
                        None if self.session_manager.is_some() => {
                            info!(playback_id = %id, "The session is gone; playback ends");
                            break PlaybackStatus::Stopped;
                        }
                        None => {}
                    }
                }
                result => {
                    let finished = {
                        let guard = internal.read().await;
                        guard.source.is_finished() || result.is_ok()
                    };
                    if finished && options.looped {
                        let reset = {
                            let mut guard = internal.write().await;
                            guard.source.reset()
                        };
                        match reset {
                            Ok(()) => {
                                debug!(playback_id = %id, "Playback looped");
                                continue;
                            }
                            Err(e) => {
                                error!(playback_id = %id, error = %e, "A looped source cannot start again");
                                break PlaybackStatus::Failed(e.to_string());
                            }
                        }
                    }
                    if finished {
                        // What is queued is still being sent: let it go
                        // before the playback reports it is done.
                        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
                        info!(playback_id = %id, "Playback completed");
                        break PlaybackStatus::Completed;
                    }
                    let e = match result {
                        Err(e) => e.to_string(),
                        Ok(_) => "empty frame".to_string(),
                    };
                    error!(playback_id = %id, error = %e, "Failed to read audio frame");
                    break PlaybackStatus::Failed(e);
                }
            }
        };

        if matches!(status, PlaybackStatus::Stopped) {
            if let Some(session) = self.session(&call_id) {
                session
                    .clear_scheduled_playout(Some(media_target), Some(&tag))
                    .await;
            }
        }
        if let Some(session) = suppressed {
            for leg in &legs {
                session.release_relay_to(*leg);
            }
        }

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
            if tx.send(status.clone()).is_err() {
                debug!(playback_id = %id, "Failed to send completion status - receiver dropped");
            }
        }

        info!(playback_id = %id, status = ?status, "Playback task completed");

        Ok(())
    }
}

/// Interleaved samples as mono: the channels of each frame averaged.
fn downmix(samples: &[i16], channels: u8) -> Vec<i16> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channels as usize)
        .map(|frame| (frame.iter().map(|&s| s as i32).sum::<i32>() / frame.len() as i32) as i16)
        .collect()
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
        let call_id = CallId::generate();

        // Create a simple tone source
        let source = Box::new(ToneGenerator::silence(8000));

        let handle = manager
            .start_playback(call_id.clone(), source, AudioTarget::Both, MixMode::Replace)
            .await
            .unwrap();

        assert_eq!(manager.active_playback_count(&call_id), 1);

        // Stop playback
        handle.stop().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(manager.active_playback_count(&call_id), 0);
    }

    /// A source of `frames` frames that can start again.
    struct Finite {
        frames: usize,
        read: usize,
        resets: Arc<AtomicU64>,
    }

    impl AudioSource for Finite {
        fn read_frame(&mut self, n: usize) -> forge_injection::Result<forge_core::AudioFrame> {
            if self.read >= self.frames {
                return Err(forge_injection::InjectionError::Internal("end".into()));
            }
            self.read += 1;
            Ok(vec![100; n])
        }
        fn sample_rate(&self) -> u32 {
            8000
        }
        fn channels(&self) -> u8 {
            1
        }
        fn is_finished(&self) -> bool {
            self.read >= self.frames
        }
        fn reset(&mut self) -> forge_injection::Result<()> {
            self.read = 0;
            self.resets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_looped_playback_starts_its_source_again_until_stopped() {
        let manager = PlaybackManager::new();
        let call_id = CallId::generate();
        let resets = Arc::new(AtomicU64::new(0));
        let source = Box::new(Finite {
            frames: 2,
            read: 0,
            resets: Arc::clone(&resets),
        });
        let handle = manager
            .start_playback_with(
                call_id.clone(),
                source,
                PlaybackOptions::new(AudioTarget::ParticipantA, MixMode::Replace).looped(),
            )
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        assert!(
            resets.load(Ordering::SeqCst) >= 2,
            "the source never looped"
        );
        assert_eq!(manager.active_playback_count(&call_id), 1);
        handle.stop().await.unwrap();
        assert!(matches!(
            handle.wait_completion().await.unwrap(),
            PlaybackStatus::Stopped
        ));
    }

    #[tokio::test]
    async fn a_playback_that_is_not_looped_completes() {
        let manager = PlaybackManager::new();
        let resets = Arc::new(AtomicU64::new(0));
        let source = Box::new(Finite {
            frames: 2,
            read: 0,
            resets: Arc::clone(&resets),
        });
        let handle = manager
            .start_playback(CallId::generate(), source, AudioTarget::Both, MixMode::Mix)
            .await
            .unwrap();
        assert!(matches!(
            handle.wait_completion().await.unwrap(),
            PlaybackStatus::Completed
        ));
        assert_eq!(resets.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stereo_is_averaged_to_mono() {
        assert_eq!(downmix(&[100, 300, -200, 200], 2), vec![200, 0]);
        assert_eq!(downmix(&[1, 2, 3], 1), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_multiple_playbacks() {
        let manager = PlaybackManager::new();
        let call_id = CallId::generate();

        // Start two playbacks
        let source1 = Box::new(ToneGenerator::silence(8000));
        let source2 = Box::new(ToneGenerator::silence(8000));

        let _handle1 = manager
            .start_playback(
                call_id.clone(),
                source1,
                AudioTarget::ParticipantA,
                MixMode::Mix,
            )
            .await
            .unwrap();

        let _handle2 = manager
            .start_playback(
                call_id.clone(),
                source2,
                AudioTarget::ParticipantB,
                MixMode::Mix,
            )
            .await
            .unwrap();

        assert_eq!(manager.active_playback_count(&call_id), 2);

        // Stop all
        manager.stop_all_playbacks(&call_id).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(manager.active_playback_count(&call_id), 0);
    }
}
