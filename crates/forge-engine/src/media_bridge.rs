//! Generic bidirectional media bridge for session-scoped WebSocket streaming.
//!
//! This provides a call-scoped PCM bridge between the RTP forwarding loop and
//! higher-level application code. The forwarding engine pushes decoded inbound
//! audio frames into the bridge, while external controllers push outbound PCM
//! frames back into the call for packetization and RTP playout.

use crate::session::ParticipantLabel;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use forge_core::{CallId, ForgeError, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

const DEFAULT_INBOUND_CAPACITY: usize = 64;
const DEFAULT_OUTBOUND_CAPACITY: usize = 64;

/// Outbound target for injected media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaTarget {
    /// Send to leg A only.
    A,
    /// Send to leg B only.
    B,
    /// Send to both call legs.
    Both,
}

impl MediaTarget {
    /// Whether this target includes the specified participant leg.
    pub fn includes(self, leg: ParticipantLabel) -> bool {
        matches!(self, Self::Both)
            || matches!(
                (self, leg),
                (Self::A, ParticipantLabel::A) | (Self::B, ParticipantLabel::B)
            )
    }
}

/// Decoded inbound audio frame emitted by the forwarding engine.
#[derive(Debug, Clone)]
pub struct InboundMediaFrame {
    pub leg: ParticipantLabel,
    pub codec: forge_core::AudioCodec,
    pub payload_type: u8,
    pub sample_rate: u32,
    pub timestamp: u32,
    pub sequence_number: u16,
    pub samples: Vec<i16>,
}

/// PCM frame to inject into one or both legs of a call.
#[derive(Debug, Clone)]
pub struct OutboundMediaFrame {
    pub target: MediaTarget,
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

struct MediaBridgeSession {
    inbound_tx: mpsc::Sender<InboundMediaFrame>,
    outbound_rx: Mutex<mpsc::Receiver<OutboundMediaFrame>>,
}

/// Handle owned by the application-facing side of a media bridge.
pub struct MediaBridgeHandle {
    call_id: CallId,
    manager: Arc<MediaBridgeManager>,
    inbound_rx: mpsc::Receiver<InboundMediaFrame>,
    outbound_tx: mpsc::Sender<OutboundMediaFrame>,
    detached: bool,
}

impl MediaBridgeHandle {
    /// Call associated with this bridge handle.
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }

    /// Cloneable sender for queuing outbound PCM audio.
    pub fn outbound_sender(&self) -> mpsc::Sender<OutboundMediaFrame> {
        self.outbound_tx.clone()
    }

    /// Receive the next decoded inbound frame from the call.
    pub async fn recv_frame(&mut self) -> Option<InboundMediaFrame> {
        self.inbound_rx.recv().await
    }

    /// Queue PCM audio for outbound playout into the call.
    pub async fn send_audio(&self, frame: OutboundMediaFrame) -> Result<()> {
        self.outbound_tx
            .send(frame)
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to queue outbound media: {}", e)))
    }

    /// Detach the bridge from the call.
    pub fn detach(&mut self) {
        if self.detached {
            return;
        }

        self.manager.detach_call(&self.call_id);
        self.detached = true;
    }
}

impl Drop for MediaBridgeHandle {
    fn drop(&mut self) {
        self.detach();
    }
}

/// Manager for session-scoped media bridge handles.
pub struct MediaBridgeManager {
    sessions: DashMap<CallId, MediaBridgeSession>,
    inbound_capacity: usize,
    outbound_capacity: usize,
}

impl Default for MediaBridgeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaBridgeManager {
    /// Create a new media bridge manager with default queue sizes.
    pub fn new() -> Self {
        Self::with_capacities(DEFAULT_INBOUND_CAPACITY, DEFAULT_OUTBOUND_CAPACITY)
    }

    /// Create a new media bridge manager with explicit queue sizes.
    pub fn with_capacities(inbound_capacity: usize, outbound_capacity: usize) -> Self {
        Self {
            sessions: DashMap::new(),
            inbound_capacity: inbound_capacity.max(1),
            outbound_capacity: outbound_capacity.max(1),
        }
    }

    /// Attach a new bridge handle to a call.
    ///
    /// Uses [`dashmap::Entry`] so the existence check and the insert are a
    /// single critical section per shard. A previous `contains_key` +
    /// `insert` pair could race on concurrent upgrades for the same call,
    /// silently replacing the first attachment and leaking its mpsc
    /// channels (the orphaned WebSocket would then hang forever).
    pub fn attach_call(self: &Arc<Self>, call_id: CallId) -> Result<MediaBridgeHandle> {
        match self.sessions.entry(call_id.clone()) {
            Entry::Occupied(_) => Err(ForgeError::ResourceLimit(format!(
                "Media bridge already attached for session {}",
                call_id.0
            ))),
            Entry::Vacant(slot) => {
                let (inbound_tx, inbound_rx) = mpsc::channel(self.inbound_capacity);
                let (outbound_tx, outbound_rx) = mpsc::channel(self.outbound_capacity);
                slot.insert(MediaBridgeSession {
                    inbound_tx,
                    outbound_rx: Mutex::new(outbound_rx),
                });
                Ok(MediaBridgeHandle {
                    call_id,
                    manager: Arc::clone(self),
                    inbound_rx,
                    outbound_tx,
                    detached: false,
                })
            }
        }
    }

    /// Detach any active media bridge for a call.
    pub fn detach_call(&self, call_id: &CallId) -> bool {
        self.sessions.remove(call_id).is_some()
    }

    /// Whether a bridge is currently attached for the call.
    pub fn has_bridge(&self, call_id: &CallId) -> bool {
        self.sessions.contains_key(call_id)
    }

    /// Push a decoded inbound frame toward the application.
    pub fn try_send_inbound_frame(&self, call_id: &CallId, frame: InboundMediaFrame) -> Result<()> {
        let entry = self
            .sessions
            .get(call_id)
            .ok_or_else(|| ForgeError::SessionNotFound(call_id.0.clone()))?;

        entry.inbound_tx.try_send(frame).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => ForgeError::ResourceLimit(format!(
                "Inbound media queue full for session {}",
                call_id.0
            )),
            mpsc::error::TrySendError::Closed(_) => ForgeError::Internal(format!(
                "Inbound media bridge closed for session {}",
                call_id.0
            )),
        })
    }

    /// Pull the next outbound frame queued by the application, if any.
    pub async fn try_recv_outbound_frame(&self, call_id: &CallId) -> Option<OutboundMediaFrame> {
        let entry = self.sessions.get(call_id)?;
        let mut outbound_rx = entry.outbound_rx.lock().await;
        outbound_rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_media_bridge_attach_and_flow() {
        let manager = Arc::new(MediaBridgeManager::new());
        let call_id = CallId::new("call-123");

        let mut handle = manager.attach_call(call_id.clone()).unwrap();
        assert!(manager.has_bridge(&call_id));

        manager
            .try_send_inbound_frame(
                &call_id,
                InboundMediaFrame {
                    leg: ParticipantLabel::A,
                    codec: forge_core::AudioCodec::PCMU,
                    payload_type: 0,
                    sample_rate: 8000,
                    timestamp: 1234,
                    sequence_number: 77,
                    samples: vec![1, 2, 3, 4],
                },
            )
            .unwrap();

        let inbound = handle.recv_frame().await.unwrap();
        assert_eq!(inbound.leg, ParticipantLabel::A);
        assert_eq!(inbound.sequence_number, 77);
        assert_eq!(inbound.samples, vec![1, 2, 3, 4]);

        handle
            .send_audio(OutboundMediaFrame {
                target: MediaTarget::B,
                sample_rate: 16000,
                samples: vec![10, 20, 30],
            })
            .await
            .unwrap();

        let outbound = manager.try_recv_outbound_frame(&call_id).await.unwrap();
        assert_eq!(outbound.target, MediaTarget::B);
        assert_eq!(outbound.sample_rate, 16000);
        assert_eq!(outbound.samples, vec![10, 20, 30]);

        drop(handle);
        assert!(!manager.has_bridge(&call_id));
    }

    // Regression: two concurrent attach_call invocations for the same
    // call_id must result in exactly one successful handle. The previous
    // contains_key+insert pair could race on the dashmap shard and let
    // both pass the check, with the second insert silently replacing the
    // first and leaking its mpsc channels.
    #[tokio::test]
    async fn test_attach_call_is_atomic_under_contention() {
        let manager = Arc::new(MediaBridgeManager::new());
        let call_id = CallId::new("call-race");

        // Spawn many attach attempts, all racing for the same call_id.
        let mut joins = Vec::new();
        for _ in 0..32 {
            let m = Arc::clone(&manager);
            let id = call_id.clone();
            joins.push(tokio::spawn(async move { m.attach_call(id) }));
        }

        let mut successes = Vec::new();
        let mut failures = 0;
        for j in joins {
            match j.await.unwrap() {
                Ok(h) => successes.push(h),
                Err(_) => failures += 1,
            }
        }
        assert_eq!(successes.len(), 1, "exactly one attach must win the race");
        assert_eq!(failures, 31, "all losers must report ResourceLimit");
        assert!(manager.has_bridge(&call_id));
    }
}
