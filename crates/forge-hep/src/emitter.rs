// forge-media
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Process-wide `ForgeHepEmitter` install + access.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use hep_rs::{HepPacket, HepProtocol, HepSink, IpProto};
use once_cell::sync::OnceCell;
use tracing::warn;

use crate::qos::RtpQosReport;
use crate::Direction;

/// Shared handle to a [`ForgeHepEmitter`]. forge-engine looks up the
/// global once at session start and caches it for the lifetime of
/// the session.
pub type ForgeHepHandle = Arc<ForgeHepEmitter>;

/// Builds [`HepPacket`]s for RTCP and RTP-QoS and forwards them to
/// a [`HepSink`] (typically `hep_rs::UdpHepSink`).
pub struct ForgeHepEmitter {
    sink: Arc<dyn HepSink>,
    capture_id: u32,
    capture_password: Option<String>,
}

impl ForgeHepEmitter {
    /// Construct an emitter from a sink + Homer agent ID.
    pub fn new(sink: Arc<dyn HepSink>, capture_id: u32) -> Self {
        Self {
            sink,
            capture_id,
            capture_password: None,
        }
    }

    /// Set the HEPlify-Server shared password (chunk `0x000E`).
    /// Required by deployments where the collector enforces it.
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.capture_password = Some(password.into());
        self
    }

    /// Emit one raw RTCP packet. `direction` is informational; the
    /// HEP packet's src/dst carry the actual flow direction.
    /// Non-blocking.
    pub fn emit_rtcp(
        &self,
        _direction: Direction,
        transport: IpProto,
        src: SocketAddr,
        dst: SocketAddr,
        rtcp_bytes: &[u8],
        correlation_id: Option<&str>,
    ) {
        // RTCP packets are tiny (a few hundred bytes); a payload past
        // 60 KiB indicates either a bug or a hostile peer. Truncate
        // with a warning so Homer still gets the head for triage.
        const MAX_RTCP_PAYLOAD: usize = 60 * 1024;
        let payload = if rtcp_bytes.len() > MAX_RTCP_PAYLOAD {
            warn!(
                len = rtcp_bytes.len(),
                "RTCP payload exceeds HEP packet capacity; truncating to {MAX_RTCP_PAYLOAD} bytes"
            );
            rtcp_bytes[..MAX_RTCP_PAYLOAD].to_vec()
        } else {
            rtcp_bytes.to_vec()
        };

        self.sink.send(HepPacket {
            capture_id: self.capture_id,
            capture_password: self.capture_password.clone(),
            protocol: HepProtocol::Rtcp,
            transport,
            src,
            dst,
            timestamp: SystemTime::now(),
            correlation_id: correlation_id.map(|s| s.to_string()),
            payload,
        });
    }

    /// Emit an RTP-QoS summary. The payload is the JSON
    /// serialization of [`RtpQosReport`] — small, human-readable,
    /// and round-trips through HEPlify-Server's generic-vendor
    /// path.
    ///
    /// Use case: once per RTCP RR, build a report from the latest
    /// stats and call this. Homer renders QoS reports alongside SIP
    /// flow for the same correlation ID.
    pub fn emit_rtp_qos(
        &self,
        transport: IpProto,
        src: SocketAddr,
        dst: SocketAddr,
        report: &RtpQosReport,
        correlation_id: Option<&str>,
    ) {
        let payload = match serde_json::to_vec(report) {
            Ok(bytes) => bytes,
            Err(e) => {
                // Serialization can't actually fail for our schema
                // (no maps with non-string keys, no NaN floats), but
                // log defensively rather than panicking on the
                // observability path.
                warn!(error = %e, "failed to serialize RtpQosReport; dropping packet");
                return;
            }
        };

        self.sink.send(HepPacket {
            capture_id: self.capture_id,
            capture_password: self.capture_password.clone(),
            protocol: HepProtocol::RtpQos,
            transport,
            src,
            dst,
            timestamp: SystemTime::now(),
            correlation_id: correlation_id.map(|s| s.to_string()),
            payload,
        });
    }
}

static FORGE_HEP: OnceCell<ForgeHepHandle> = OnceCell::new();

/// Install the global emitter. Returns `true` on first install,
/// `false` when already configured — mirrors `sip-observe` /
/// `sip-hep` semantics.
#[must_use]
pub fn set_emitter(handle: ForgeHepHandle) -> bool {
    if FORGE_HEP.set(handle).is_ok() {
        true
    } else {
        warn!("forge-hep emitter already configured");
        false
    }
}

/// Look up the global emitter. `None` when no HEP shipping is
/// configured for this process — the recommended `if let Some(...)`
/// guard at call sites makes the check zero-cost.
pub fn forge_hep() -> Option<&'static ForgeHepHandle> {
    FORGE_HEP.get()
}
