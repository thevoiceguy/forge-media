//! Metric descriptions for the families this crate emits.
//!
//! Every `counter!`/`gauge!`/`histogram!` name emitted by forge-rtp has a
//! `M_*` const here, a `describe_*!` registration in [`describe_metrics`],
//! and an entry in the `ALL_*` lists. A self-scan test walks this crate's
//! sources and fails if an emission site and these lists ever disagree.
//!
//! [`describe_metrics`] must run *after* a `metrics` recorder is
//! installed — descriptions issued to the no-op recorder are lost.
//! forge-engine's `SessionManager` constructors call it, so embedding
//! consumers that install their exporter before constructing the engine
//! (the normal order) get `# HELP` lines for free.

use metrics::describe_counter;

pub const M_SRTP_PACKETS_ENCRYPTED: &str = "forge_srtp_packets_encrypted_total";
pub const M_SRTP_PACKETS_DECRYPTED: &str = "forge_srtp_packets_decrypted_total";
pub const M_SRTP_REPLAY_BLOCKED: &str = "forge_srtp_replay_attacks_blocked_total";
pub const M_SRTCP_PACKETS_ENCRYPTED: &str = "forge_srtcp_packets_encrypted_total";
pub const M_SRTCP_PACKETS_DECRYPTED: &str = "forge_srtcp_packets_decrypted_total";
pub const M_SRTCP_REPLAY_BLOCKED: &str = "forge_srtcp_replay_attacks_blocked_total";
pub const M_RTP_LATCH_LEARNED: &str = "forge_rtp_latch_learned_total";
pub const M_RTP_LATCH_REJECTED: &str = "forge_rtp_latch_rejected_total";

/// Every counter family forge-rtp emits.
pub const ALL_COUNTERS: &[&str] = &[
    M_SRTP_PACKETS_ENCRYPTED,
    M_SRTP_PACKETS_DECRYPTED,
    M_SRTP_REPLAY_BLOCKED,
    M_SRTCP_PACKETS_ENCRYPTED,
    M_SRTCP_PACKETS_DECRYPTED,
    M_SRTCP_REPLAY_BLOCKED,
    M_RTP_LATCH_LEARNED,
    M_RTP_LATCH_REJECTED,
];

/// Every gauge family forge-rtp emits.
pub const ALL_GAUGES: &[&str] = &[];

/// Every histogram family forge-rtp emits.
pub const ALL_HISTOGRAMS: &[&str] = &[];

/// Register a description for every metric family this crate emits.
///
/// Idempotent and cheap. Descriptions only reach the recorder installed
/// at call time, so call it (again) once your recorder is installed.
pub fn describe_metrics() {
    describe_counter!(
        M_SRTP_PACKETS_ENCRYPTED,
        "RTP packets successfully SRTP-protected on the outbound path."
    );
    describe_counter!(
        M_SRTP_PACKETS_DECRYPTED,
        "Inbound SRTP packets successfully unprotected (auth + decrypt)."
    );
    describe_counter!(
        M_SRTP_REPLAY_BLOCKED,
        "Inbound SRTP packets rejected by replay protection."
    );
    describe_counter!(
        M_SRTCP_PACKETS_ENCRYPTED,
        "RTCP packets successfully SRTCP-protected on the outbound path."
    );
    describe_counter!(
        M_SRTCP_PACKETS_DECRYPTED,
        "Inbound SRTCP packets successfully unprotected (auth + decrypt)."
    );
    describe_counter!(
        M_SRTCP_REPLAY_BLOCKED,
        "Inbound SRTCP packets rejected by replay protection."
    );
    describe_counter!(
        M_RTP_LATCH_LEARNED,
        "Remote media endpoints learned via symmetric-RTP latching."
    );
    describe_counter!(
        M_RTP_LATCH_REJECTED,
        "Datagrams rejected by symmetric-RTP latching rules."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn described_lists_match_emission_sites() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let emitted = forge_core::metrics_scan::facade_emissions_in_dir(&src_dir);

        let listed: BTreeSet<&str> = ALL_COUNTERS
            .iter()
            .chain(ALL_GAUGES.iter())
            .chain(ALL_HISTOGRAMS.iter())
            .copied()
            .collect();

        for (kind, name) in &emitted {
            assert!(
                name.starts_with("forge_"),
                "metric `{name}` breaks the forge_ naming convention"
            );
            assert!(
                listed.contains(name.as_str()),
                "{kind}!(\"{name}\") is emitted but missing from the ALL_* lists \
                 (and therefore undescribed) — add it to metrics.rs"
            );
            let expected_list: &[&str] = match kind.as_str() {
                "counter" => ALL_COUNTERS,
                "gauge" => ALL_GAUGES,
                _ => ALL_HISTOGRAMS,
            };
            assert!(
                expected_list.contains(&name.as_str()),
                "`{name}` is emitted as a {kind} but listed under a different type"
            );
        }

        let emitted_names: BTreeSet<&str> = emitted.iter().map(|(_, name)| name.as_str()).collect();
        for name in listed {
            assert!(
                emitted_names.contains(name),
                "`{name}` is listed/described but no longer emitted anywhere in \
                 this crate — remove it from metrics.rs"
            );
        }
    }

    #[test]
    fn every_emission_uses_a_string_literal() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(
            forge_core::metrics_scan::non_literal_emissions_in_dir(&src_dir),
            0,
            "emission macros must take a string-literal name so the self-scan \
             (and plain grep) can see them"
        );
    }
}
