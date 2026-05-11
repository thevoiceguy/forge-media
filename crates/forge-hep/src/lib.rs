// forge-media
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HEP3 (Homer Encapsulation Protocol v3) emission for forge-media.
//!
//! This crate is the integration glue between forge-media's RTP/RTCP
//! pipeline and a [`hep_rs::HepSink`] running off-process toward
//! [Homer](https://sipcapture.io/) / HEPIC / HEPlify-Server. When an
//! emitter is installed, forge ships:
//!
//! * **RTCP** packets (chunk type `0x05`) — Sender Reports, Receiver
//!   Reports, SDES, BYE — observed on the RTCP socket in both
//!   directions. The captured payload is the raw RTCP bytes so
//!   Homer's existing parser can render them in its UI.
//! * **RTP-QoS summaries** (vendor chunk, `HepProtocol::RtpQos`) —
//!   periodic call-quality reports derived from the most recent RR
//!   plus jitter buffer counters. Payload is a small JSON blob so
//!   it round-trips through HEPlify-Server's generic-vendor handler
//!   intact.
//!
//! # Design
//!
//! Mirrors `siphon-rs::sip_hep`'s shape: a single
//! [`ForgeHepEmitter`] holds deployment-wide config (capture ID,
//! optional shared password) and forwards [`hep_rs::HepPacket`]s
//! through the supplied sink. Per-call code passes the correlation
//! ID (typically the SIP Call-ID) at the emit call site.
//!
//! Without an emitter installed, [`forge_hep()`] returns `None` —
//! the recommended `if let Some(...)` guard at hook sites is a
//! single atomic load + null check.
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::net::SocketAddr;
//! use hep_rs::{UdpHepSink, UdpHepSinkConfig};
//! use forge_hep::{set_emitter, ForgeHepEmitter};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let collector: SocketAddr = "127.0.0.1:9060".parse()?;
//! let (sink, _worker) = UdpHepSink::start(UdpHepSinkConfig::new(collector)).await?;
//! let emitter = ForgeHepEmitter::new(Arc::new(sink), 2001);
//! forge_hep::set_emitter(Arc::new(emitter));
//! # Ok(()) }
//! ```

mod emitter;
mod qos;

pub use emitter::{forge_hep, set_emitter, ForgeHepEmitter, ForgeHepHandle};
pub use qos::RtpQosReport;

// Re-export the hep-rs types callers (forge-engine, forge-rtp) need
// to construct packets, so they don't have to add a second dep on
// hep-rs just to name `IpProto::Udp`.
pub use hep_rs::{HepProtocol, HepSink, IpProto};

/// Direction a media packet was observed in. Surfaces the intent
/// at the hook site even though the underlying HEP packet encodes
/// it via src/dst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Inbound — observed on the recv side of the RTCP socket.
    Inbound,
    /// Outbound — observed on the send side.
    Outbound,
}
