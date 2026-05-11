// forge-media
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! RTP-QoS summary payload (vendor `HepProtocol::RtpQos`).
//!
//! Wire format: JSON, packed into HEP3 chunk `0x000F`. We use JSON
//! rather than a fixed-layout struct so Homer's generic-vendor
//! handler can render it without a schema upgrade and so operators
//! can `tcpdump -A`-style debug it.
//!
//! Field choices follow the subset of RTCP RR data Homer renders by
//! default — fraction lost, cumulative lost, jitter — plus the
//! jitter-buffer counters we have locally (forge-rtp's
//! `JitterStats`). Every field is optional so a partial report
//! (e.g., no jitter buffer yet) still round-trips.

use serde::{Deserialize, Serialize};

/// QoS summary serialized as JSON into the HEP payload.
///
/// All fields are optional; emit what you have.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RtpQosReport {
    /// Synchronization source — uniquely identifies the RTP stream.
    /// 32-bit; serialized as a number for compactness.
    pub ssrc: Option<u32>,
    /// Fraction of packets lost since the last RR, as RFC 3550 §6.4.1
    /// reports it (0.0–1.0). Derived from RR's `fraction_lost / 256`.
    pub fraction_lost: Option<f64>,
    /// Cumulative packets lost since session start. Signed 24-bit
    /// per RFC 3550 §6.4.1 (the spec allows negative values when
    /// duplicates outweigh genuine loss); use `i32` to round-trip
    /// cleanly through JSON.
    pub packets_lost: Option<i32>,
    /// Interarrival jitter in RTP timestamp units. RFC 3550 §6.4.1.
    pub jitter: Option<u32>,
    /// Total packets received in this stream (engine counter, not
    /// the RR's extended-highest-seq).
    pub packets_received: Option<u64>,
    /// Packets the jitter buffer dropped because they arrived too
    /// late.
    pub packets_dropped: Option<u64>,
    /// Out-of-order packets observed.
    pub packets_out_of_order: Option<u64>,
    /// Duplicate packets observed.
    pub packets_duplicate: Option<u64>,
    /// RFC 1889 MOS estimate (1.0–5.0) — opportunistic. Implementers
    /// often compute this in heplify-server from the other fields;
    /// we leave it `None` until the engine has a first-class MOS
    /// estimator.
    pub mos: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let r = RtpQosReport {
            ssrc: Some(0xDEAD_BEEF),
            fraction_lost: Some(0.0078),
            packets_lost: Some(3i32),
            jitter: Some(120),
            packets_received: Some(2000),
            packets_dropped: Some(2),
            packets_out_of_order: Some(1),
            packets_duplicate: Some(0),
            mos: None,
        };
        let bytes = serde_json::to_vec(&r).unwrap();
        let back: RtpQosReport = serde_json::from_slice(&bytes).unwrap();
        // Fields we set survive the round-trip with identical values.
        assert_eq!(back.ssrc, r.ssrc);
        assert_eq!(back.fraction_lost, r.fraction_lost);
        assert_eq!(back.packets_lost, r.packets_lost);
        assert_eq!(back.jitter, r.jitter);
        assert_eq!(back.mos, None);
    }

    #[test]
    fn default_is_all_none() {
        let r = RtpQosReport::default();
        let bytes = serde_json::to_vec(&r).unwrap();
        // Default report serializes to a compact JSON object — used
        // by callers that want to emit "we have a stream but no
        // numbers yet" so Homer can plot the heartbeat.
        let back: RtpQosReport = serde_json::from_slice(&bytes).unwrap();
        assert!(back.ssrc.is_none() && back.jitter.is_none() && back.mos.is_none());
    }
}
