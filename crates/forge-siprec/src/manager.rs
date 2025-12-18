//! SIPREC compliance recording scaffolding.
//!
//! Provides a lightweight wrapper around the recorder to capture mixed media for
//! compliance/export workflows. This is intentionally minimal and can be extended
//! with SIP signaling in a future iteration.

use dashmap::DashMap;
use forge_core::{AudioFormat, CallId, EventBus, ForgeEvent, ParticipantId};
use forge_recorder::AudioRecorder;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::info;

#[derive(Debug, Error)]
pub enum SiprecError {
    #[error("Recorder error: {0}")]
    Recorder(#[from] forge_recorder::RecorderError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SiprecError>;

/// Configuration for a SIPREC capture.
#[derive(Debug, Clone)]
pub struct SiprecConfig {
    /// Base directory to store recordings
    pub output_dir: PathBuf,
    /// Audio format to use for the capture
    pub format: AudioFormat,
}

impl Default for SiprecConfig {
    fn default() -> Self {
        Self {
            output_dir: "/var/lib/forge/siprec".into(),
            format: AudioFormat::pcm_mono(),
        }
    }
}

/// A SIPREC session capturing a call leg or mixed stream.
pub struct SiprecSession {
    call_id: CallId,
    participant_a: ParticipantId,
    participant_b: ParticipantId,
    recorder: AudioRecorder,
}

impl SiprecSession {
    /// Create a new SIPREC session recorder.
    pub async fn new<P: AsRef<Path>>(
        call_id: CallId,
        participant_a: ParticipantId,
        participant_b: ParticipantId,
        output_path: P,
        format: AudioFormat,
    ) -> Result<Self> {
        let recorder = AudioRecorder::new(output_path, format).await?;
        Ok(Self {
            call_id,
            participant_a,
            participant_b,
            recorder,
        })
    }

    /// Start capturing.
    pub fn start(&self) -> Result<()> {
        info!(
            "Starting SIPREC capture for call {} ({} <-> {})",
            self.call_id.0, self.participant_a.0, self.participant_b.0
        );
        Ok(self.recorder.start()?)
    }

    /// Record a mixed chunk of PCM samples.
    pub fn write_mixed_samples(&self, samples: &[i16]) -> Result<()> {
        Ok(self.recorder.write_samples(samples)?)
    }

    /// Stop the capture and finalize the file.
    pub fn stop(&self) -> Result<()> {
        info!("Stopping SIPREC capture for call {}", self.call_id.0);
        Ok(self.recorder.stop()?)
    }
}

/// Configuration for the SIPREC manager
#[derive(Debug, Clone)]
pub struct SiprecManagerConfig {
    pub output_dir: PathBuf,
    pub format: AudioFormat,
    pub auth_token: Option<String>,
}

impl Default for SiprecManagerConfig {
    fn default() -> Self {
        Self {
            output_dir: "/var/lib/forge/siprec".into(),
            format: AudioFormat::pcm_mono(),
            auth_token: None,
        }
    }
}

/// Manages SIPREC sessions driven by engine events
pub struct SiprecManager {
    #[allow(dead_code)]
    sessions: Arc<DashMap<CallId, SiprecSession>>,
    shutdown: Arc<Notify>,
    handle: JoinHandle<()>,
}

impl SiprecManager {
    pub fn start(config: SiprecManagerConfig, event_bus: Arc<EventBus>) -> Self {
        let sessions = Arc::new(DashMap::new());
        let shutdown = Arc::new(Notify::new());

        let handle = {
            let sessions = Arc::clone(&sessions);
            let shutdown_signal = Arc::clone(&shutdown);
            let SiprecManagerConfig {
                output_dir, format, ..
            } = config.clone();

            tokio::spawn(async move {
                let mut rx = event_bus.subscribe();
                loop {
                    tokio::select! {
                        _ = shutdown_signal.notified() => {
                            break;
                        }
                        evt = rx.recv() => {
                            match evt {
                                Ok(ForgeEvent::SessionActive { call_id, .. }) => {
                                    let _ = start_session(&sessions, &call_id, &output_dir, format.clone()).await;
                                }
                                Ok(ForgeEvent::SessionTerminated { call_id, .. }) => {
                                    let _ = stop_session(&sessions, &call_id);
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                }

                // Best-effort cleanup
                for entry in sessions.iter() {
                    let _ = entry.value().stop();
                }
            })
        };

        Self {
            sessions,
            shutdown,
            handle,
        }
    }
}

impl Drop for SiprecManager {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        self.handle.abort();
    }
}

async fn start_session(
    sessions: &DashMap<CallId, SiprecSession>,
    call_id: &CallId,
    output_dir: &Path,
    format: AudioFormat,
) -> Result<()> {
    let path = output_dir.join(format!("{}.wav", call_id.0));
    let recorder = AudioRecorder::new(&path, format).await?;
    recorder.start()?;

    let session = SiprecSession {
        call_id: call_id.clone(),
        participant_a: ParticipantId::generate(),
        participant_b: ParticipantId::generate(),
        recorder,
    };

    info!(
        "SIPREC capture started for call {} -> {:?}",
        call_id.0, path
    );
    sessions.insert(call_id.clone(), session);
    Ok(())
}

fn stop_session(sessions: &DashMap<CallId, SiprecSession>, call_id: &CallId) -> Result<()> {
    if let Some((_, session)) = sessions.remove(call_id) {
        session.stop()?;
        info!("SIPREC capture stopped for call {}", call_id.0);
    }
    Ok(())
}
