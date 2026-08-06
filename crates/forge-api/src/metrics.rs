//! Metric descriptions for the families this crate emits, plus the
//! wiring that gives the standalone server descriptions for the whole
//! workspace.
//!
//! Every `counter!`/`gauge!`/`histogram!` name emitted by forge-api has
//! a `M_*` const here, a `describe_*!` registration in
//! [`describe_metrics`], and an entry in the `ALL_*` lists. A self-scan
//! test walks this crate's sources and fails if an emission site and
//! these lists ever disagree; a second, workspace-wide sweep walks every
//! crate under `crates/` and fails if any facade emission in the whole
//! workspace is missing from the four describe lists (forge-rtp,
//! forge-engine, forge-conference, forge-api) — so a new crate that
//! starts emitting undescribed metrics fails CI here.
//!
//! [`describe_metrics`] must run *after* a `metrics` recorder is
//! installed — descriptions issued to the no-op recorder are lost.
//! `MetricsHandle::init` calls it right after installing the recorder.

use metrics::{describe_counter, describe_gauge, describe_histogram};
use std::sync::Once;

pub const M_WEBRTC_CONNECTIONS_ACTIVE: &str = "forge_webrtc_connections_active";
pub const M_WEBRTC_CONNECTIONS_CREATED: &str = "forge_webrtc_connections_created_total";
pub const M_WEBRTC_CONNECTIONS_DELETED: &str = "forge_webrtc_connections_deleted_total";
pub const M_WEBRTC_ICE_CANDIDATES_ADDED: &str = "forge_webrtc_ice_candidates_added_total";
pub const M_WEBRTC_ICE_CANDIDATES_GATHERED: &str = "forge_webrtc_ice_candidates_gathered";
pub const M_WEBRTC_ESTABLISHMENT: &str = "forge_webrtc_connection_establishment_duration_seconds";
pub const M_SDP_NEGOTIATIONS: &str = "forge_sdp_negotiation_total";
pub const M_SDP_NEGOTIATION_FAILURES: &str = "forge_sdp_negotiation_failures_total";
pub const M_SDP_CODECS_NEGOTIATED: &str = "forge_sdp_codecs_negotiated_total";
pub const M_SDP_NEGOTIATION_DURATION: &str = "forge_sdp_negotiation_duration_seconds";

/// Every counter family forge-api emits.
pub const ALL_COUNTERS: &[&str] = &[
    M_WEBRTC_CONNECTIONS_CREATED,
    M_WEBRTC_CONNECTIONS_DELETED,
    M_WEBRTC_ICE_CANDIDATES_ADDED,
    M_SDP_NEGOTIATIONS,
    M_SDP_NEGOTIATION_FAILURES,
    M_SDP_CODECS_NEGOTIATED,
];

/// Every gauge family forge-api emits.
pub const ALL_GAUGES: &[&str] = &[M_WEBRTC_CONNECTIONS_ACTIVE, M_WEBRTC_ICE_CANDIDATES_GATHERED];

/// Every histogram family forge-api emits.
pub const ALL_HISTOGRAMS: &[&str] = &[M_WEBRTC_ESTABLISHMENT, M_SDP_NEGOTIATION_DURATION];

/// Suggested buckets for
/// `forge_webrtc_connection_establishment_duration_seconds` — ICE plus
/// DTLS across real networks: sub-second locally, seconds through TURN
/// or lossy paths.
pub const WEBRTC_ESTABLISHMENT_SECONDS_BUCKETS: [f64; 8] =
    [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0];

/// Suggested buckets for `forge_sdp_negotiation_duration_seconds` —
/// parsing plus codec negotiation is pure local computation, normally
/// well under a millisecond.
pub const SDP_NEGOTIATION_SECONDS_BUCKETS: [f64; 8] =
    [0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.05];

/// Register a description for every facade metric family in the
/// workspace: this crate's, forge-engine's (which covers forge-rtp),
/// and forge-conference's.
///
/// Idempotent; call after a `metrics` recorder is installed.
/// `MetricsHandle::init` does this for the standalone server.
pub fn describe_metrics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        forge_engine::metrics::describe_metrics();
        forge_conference::metrics::describe_metrics();

        describe_gauge!(
            M_WEBRTC_CONNECTIONS_ACTIVE,
            "WebRTC peer connections currently held by the API server."
        );
        describe_counter!(
            M_WEBRTC_CONNECTIONS_CREATED,
            "WebRTC peer connections created via the API."
        );
        describe_counter!(
            M_WEBRTC_CONNECTIONS_DELETED,
            "WebRTC peer connections deleted via the API."
        );
        describe_counter!(
            M_WEBRTC_ICE_CANDIDATES_ADDED,
            "Remote ICE candidates added to WebRTC connections via the API."
        );
        describe_gauge!(
            M_WEBRTC_ICE_CANDIDATES_GATHERED,
            "Local ICE candidates gathered by the most recently created WebRTC \
             connection (sampled at offer time)."
        );
        describe_histogram!(
            M_WEBRTC_ESTABLISHMENT,
            "Time from WebRTC connection creation to the remote answer being \
             applied."
        );
        describe_counter!(
            M_SDP_NEGOTIATIONS,
            "SDP offer/answer negotiations attempted via the API."
        );
        describe_counter!(
            M_SDP_NEGOTIATION_FAILURES,
            "SDP negotiations failed, by reason (missing_local_address, \
             invalid_profile, parse_error, no_common_codec, negotiation_error)."
        );
        describe_counter!(
            M_SDP_CODECS_NEGOTIATED,
            "Codecs selected by successful SDP negotiations, by codec."
        );
        describe_histogram!(
            M_SDP_NEGOTIATION_DURATION,
            "Wall time of one SDP parse + negotiation."
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn union<'a>(lists: &[&'a [&'a str]]) -> BTreeSet<&'a str> {
        lists.iter().flat_map(|l| l.iter()).copied().collect()
    }

    fn workspace_counters() -> BTreeSet<&'static str> {
        union(&[
            ALL_COUNTERS,
            forge_engine::metrics::ALL_COUNTERS,
            forge_conference::metrics::ALL_COUNTERS,
            forge_rtp::metrics::ALL_COUNTERS,
        ])
    }

    fn workspace_gauges() -> BTreeSet<&'static str> {
        union(&[
            ALL_GAUGES,
            forge_engine::metrics::ALL_GAUGES,
            forge_conference::metrics::ALL_GAUGES,
            forge_rtp::metrics::ALL_GAUGES,
        ])
    }

    fn workspace_histograms() -> BTreeSet<&'static str> {
        union(&[
            ALL_HISTOGRAMS,
            forge_engine::metrics::ALL_HISTOGRAMS,
            forge_conference::metrics::ALL_HISTOGRAMS,
            forge_rtp::metrics::ALL_HISTOGRAMS,
        ])
    }

    #[test]
    fn own_lists_match_own_emission_sites() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let emitted = forge_core::metrics_scan::facade_emissions_in_dir(&src_dir);

        let listed = union(&[ALL_COUNTERS, ALL_GAUGES, ALL_HISTOGRAMS]);
        for (kind, name) in &emitted {
            assert!(
                listed.contains(name.as_str()),
                "{kind}!(\"{name}\") is emitted by forge-api but missing from its \
                 ALL_* lists — add it to metrics.rs"
            );
        }
        let emitted_names: BTreeSet<&str> =
            emitted.iter().map(|(_, name)| name.as_str()).collect();
        for name in listed {
            assert!(
                emitted_names.contains(name),
                "`{name}` is listed/described but no longer emitted anywhere in \
                 forge-api — remove it from metrics.rs"
            );
        }
    }

    #[test]
    fn every_facade_emission_in_the_workspace_is_described() {
        // crates/forge-api -> crates/
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir")
            .to_path_buf();

        let counters = workspace_counters();
        let gauges = workspace_gauges();
        let histograms = workspace_histograms();

        let mut seen = 0usize;
        for entry in std::fs::read_dir(&crates_dir).expect("read crates dir") {
            let crate_dir = entry.expect("dir entry").path();
            let src = crate_dir.join("src");
            if !src.is_dir() {
                continue;
            }
            for (kind, name) in forge_core::metrics_scan::facade_emissions_in_dir(&src) {
                seen += 1;
                assert!(
                    name.starts_with("forge_"),
                    "metric `{name}` (emitted under {}) breaks the forge_ naming \
                     convention",
                    crate_dir.display()
                );
                let described = match kind.as_str() {
                    "counter" => counters.contains(name.as_str()),
                    "gauge" => gauges.contains(name.as_str()),
                    _ => histograms.contains(name.as_str()),
                };
                assert!(
                    described,
                    "{kind}!(\"{name}\") (emitted under {}) is not in any crate's \
                     ALL_* describe lists — every facade metric must be described; \
                     see forge-rtp/engine/conference/api src/metrics.rs",
                    crate_dir.display()
                );
            }
        }
        // Guard against the sweep silently scanning nothing.
        assert!(
            seen >= 70,
            "workspace sweep found only {seen} emission sites — scan is broken"
        );
    }
}
