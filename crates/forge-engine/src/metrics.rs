//! Metric descriptions for the families this crate emits.
//!
//! Every `counter!`/`gauge!`/`histogram!` name emitted by forge-engine
//! has a `M_*` const here, a `describe_*!` registration in
//! [`describe_metrics`], and an entry in the `ALL_*` lists. A self-scan
//! test walks this crate's sources and fails if an emission site and
//! these lists ever disagree.
//!
//! `forge_rtp_latch_learned_total` / `forge_rtp_latch_rejected_total`
//! are emitted both here and in forge-rtp; forge-rtp owns their
//! descriptions, so they are deliberately absent from the lists below
//! (the self-scan accepts names owned by forge-rtp).
//!
//! [`describe_metrics`] must run *after* a `metrics` recorder is
//! installed — descriptions issued to the no-op recorder are lost.
//! Every `SessionManager` constructor calls it, so embedding consumers
//! that install their exporter before constructing the engine (the
//! normal order) get `# HELP` lines for free.

use metrics::{describe_counter, describe_gauge, describe_histogram};

pub const M_ACTIVE_SESSIONS: &str = "forge_active_sessions";
pub const M_AI_AUDIO_BYTES_SENT: &str = "forge_ai_audio_bytes_sent_total";
pub const M_AI_AUDIO_PACKETS_SENT: &str = "forge_ai_audio_packets_sent_total";
pub const M_DTLS_HANDSHAKES_COMPLETED: &str = "forge_dtls_handshakes_completed_total";
pub const M_DTLS_HANDSHAKES_FAILED: &str = "forge_dtls_handshakes_failed_total";
pub const M_DTLS_DROPPED_NO_LEG: &str = "forge_dtls_packets_dropped_no_leg_total";
pub const M_DTLS_PACKETS_RECEIVED: &str = "forge_dtls_packets_received_total";
pub const M_DTLS_SEND_ERRORS: &str = "forge_dtls_send_errors_total";
pub const M_DTMF_DUPLICATES_SUPPRESSED: &str = "forge_dtmf_duplicates_suppressed_total";
pub const M_DTMF_EVENTS: &str = "forge_dtmf_events_total";
pub const M_DTMF_INBAND_EVENTS: &str = "forge_dtmf_inband_events_total";
pub const M_DTMF_INBAND_PACKETS: &str = "forge_dtmf_inband_packets_processed_total";
pub const M_DTMF_RFC2833_EVENTS: &str = "forge_dtmf_rfc2833_events_total";
pub const M_DTMF_RFC2833_INJECTED_BYTES: &str = "forge_dtmf_rfc2833_injected_bytes_total";
pub const M_DTMF_RFC2833_INJECTED_PACKETS: &str = "forge_dtmf_rfc2833_injected_packets_total";
pub const M_DTMF_RFC2833_PACKETS: &str = "forge_dtmf_rfc2833_packets_total";
pub const M_DTMF_RFC2833_RELAYED: &str = "forge_dtmf_rfc2833_relayed_total";
pub const M_GENERATED_AUDIO_BYTES_SENT: &str = "forge_generated_audio_bytes_sent_total";
pub const M_GENERATED_AUDIO_PACKETS_SENT: &str = "forge_generated_audio_packets_sent_total";
pub const M_GENERATED_MEDIA_BYTES_SENT: &str = "forge_generated_media_bytes_sent_total";
pub const M_GENERATED_MEDIA_PACKETS_SENT: &str = "forge_generated_media_packets_sent_total";
pub const M_RTCP_BYTES_RECEIVED: &str = "forge_rtcp_bytes_received_total";
pub const M_RTCP_BYTES_SENT: &str = "forge_rtcp_bytes_sent_total";
pub const M_RTCP_HIGHEST_SEQ: &str = "forge_rtcp_highest_seq";
pub const M_RTCP_JITTER: &str = "forge_rtcp_jitter";
pub const M_RTCP_LOSS_FRACTION: &str = "forge_rtcp_packet_loss_fraction";
pub const M_RTCP_PACKETS_LOST: &str = "forge_rtcp_packets_lost_total";
pub const M_RTCP_PACKETS_RECEIVED: &str = "forge_rtcp_packets_received_total";
pub const M_RTCP_PACKETS_SENT: &str = "forge_rtcp_packets_sent_total";
pub const M_RTCP_SENDER_BYTES: &str = "forge_rtcp_sender_bytes_total";
pub const M_RTCP_SENDER_PACKETS: &str = "forge_rtcp_sender_packets_total";
pub const M_RTCP_SR_SENT: &str = "forge_rtcp_sender_reports_sent_total";
pub const M_RTP_BYTES_RECEIVED: &str = "forge_rtp_bytes_received_total";
pub const M_RTP_BYTES_SENT: &str = "forge_rtp_bytes_sent_total";
pub const M_RTP_PACKETS_RECEIVED: &str = "forge_rtp_packets_received_total";
pub const M_RTP_PACKETS_SENT: &str = "forge_rtp_packets_sent_total";
pub const M_RTP_UNSUPPORTED_FIRST_BYTE: &str = "forge_rtp_unsupported_first_byte_total";
pub const M_SRTCP_PROTECT_ERRORS: &str = "forge_srtcp_protect_errors_total";
pub const M_SRTCP_UNPROTECT_ERRORS: &str = "forge_srtcp_unprotect_errors_total";
pub const M_SRTP_PROTECT_ERRORS: &str = "forge_srtp_protect_errors_total";
pub const M_SRTP_UNPROTECT_ERRORS: &str = "forge_srtp_unprotect_errors_total";
pub const M_TRANSCODING_BYTES: &str = "forge_transcoding_bytes_total";
pub const M_TRANSCODING_DURATION: &str = "forge_transcoding_duration_seconds";
pub const M_TRANSCODING_ERRORS: &str = "forge_transcoding_errors_total";
pub const M_TRANSCODING_PACKETS: &str = "forge_transcoding_packets_total";
pub const M_VAD_ERRORS: &str = "forge_vad_errors_total";
pub const M_VAD_NEURAL_INFERENCE: &str = "forge_vad_neural_inference_seconds";
pub const M_VAD_WINDOWS: &str = "forge_vad_windows_total";

/// Every counter family forge-engine emits (latch counters excluded —
/// forge-rtp owns those).
pub const ALL_COUNTERS: &[&str] = &[
    M_AI_AUDIO_BYTES_SENT,
    M_AI_AUDIO_PACKETS_SENT,
    M_DTLS_HANDSHAKES_COMPLETED,
    M_DTLS_HANDSHAKES_FAILED,
    M_DTLS_DROPPED_NO_LEG,
    M_DTLS_PACKETS_RECEIVED,
    M_DTLS_SEND_ERRORS,
    M_DTMF_DUPLICATES_SUPPRESSED,
    M_DTMF_EVENTS,
    M_DTMF_INBAND_EVENTS,
    M_DTMF_INBAND_PACKETS,
    M_DTMF_RFC2833_EVENTS,
    M_DTMF_RFC2833_INJECTED_BYTES,
    M_DTMF_RFC2833_INJECTED_PACKETS,
    M_DTMF_RFC2833_PACKETS,
    M_DTMF_RFC2833_RELAYED,
    M_GENERATED_AUDIO_BYTES_SENT,
    M_GENERATED_AUDIO_PACKETS_SENT,
    M_GENERATED_MEDIA_BYTES_SENT,
    M_GENERATED_MEDIA_PACKETS_SENT,
    M_RTCP_BYTES_RECEIVED,
    M_RTCP_BYTES_SENT,
    M_RTCP_PACKETS_RECEIVED,
    M_RTCP_PACKETS_SENT,
    M_RTCP_SENDER_BYTES,
    M_RTCP_SENDER_PACKETS,
    M_RTCP_SR_SENT,
    M_RTP_BYTES_RECEIVED,
    M_RTP_BYTES_SENT,
    M_RTP_PACKETS_RECEIVED,
    M_RTP_PACKETS_SENT,
    M_RTP_UNSUPPORTED_FIRST_BYTE,
    M_SRTCP_PROTECT_ERRORS,
    M_SRTCP_UNPROTECT_ERRORS,
    M_SRTP_PROTECT_ERRORS,
    M_SRTP_UNPROTECT_ERRORS,
    M_TRANSCODING_BYTES,
    M_TRANSCODING_ERRORS,
    M_TRANSCODING_PACKETS,
    M_VAD_ERRORS,
    M_VAD_WINDOWS,
];

/// Every gauge family forge-engine emits.
pub const ALL_GAUGES: &[&str] = &[
    M_ACTIVE_SESSIONS,
    M_RTCP_HIGHEST_SEQ,
    M_RTCP_JITTER,
    M_RTCP_LOSS_FRACTION,
    M_RTCP_PACKETS_LOST,
];

/// Every histogram family forge-engine emits.
pub const ALL_HISTOGRAMS: &[&str] = &[M_TRANSCODING_DURATION, M_VAD_NEURAL_INFERENCE];

/// Suggested buckets for `forge_transcoding_duration_seconds`.
///
/// One transcode handles one RTP packet's payload (a 20 ms frame in the
/// common telephony case), so healthy operation sits well under the
/// 0.02 s frame budget; the top buckets exist to make a stall visible.
pub const TRANSCODING_DURATION_SECONDS_BUCKETS: [f64; 8] =
    [0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.02];

/// Suggested buckets for `forge_vad_neural_inference_seconds`.
///
/// One record covers the model windows completed by a single frame
/// (usually one). CPU inference lands in the low milliseconds; anything
/// approaching the 0.02 s frame budget threatens the audio path.
pub const VAD_NEURAL_INFERENCE_SECONDS_BUCKETS: [f64; 8] =
    [0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1];

/// Register a description for every metric family this crate emits,
/// plus forge-rtp's (this crate re-emits two of its latch counters and
/// is the natural init choke point for embedded use).
///
/// Idempotent and cheap. Descriptions only reach the recorder installed
/// at call time, so call it (again) once your recorder is installed.
/// Every `SessionManager` constructor calls this.
///
/// Histogram buckets cannot be set from here — they are exporter
/// configuration. Consumers using `metrics-exporter-prometheus` should
/// register [`ALL_HISTOGRAMS`] (and the other crates' lists) with
/// `set_buckets_for_metric`; see the bucket consts above and
/// `docs/METRICS.md`.
pub fn describe_metrics() {
    forge_rtp::metrics::describe_metrics();

    describe_gauge!(
        M_ACTIVE_SESSIONS,
        "Media sessions currently active on this engine."
    );
    describe_counter!(
        M_AI_AUDIO_PACKETS_SENT,
        "Locally generated audio packets sent whose source is the AI stream \
         (subset of forge_generated_audio_packets_sent_total)."
    );
    describe_counter!(
        M_AI_AUDIO_BYTES_SENT,
        "Locally generated audio wire bytes sent whose source is the AI \
         stream (subset of forge_generated_audio_bytes_sent_total)."
    );
    describe_counter!(
        M_DTLS_HANDSHAKES_COMPLETED,
        "DTLS-SRTP handshakes completed successfully."
    );
    describe_counter!(M_DTLS_HANDSHAKES_FAILED, "DTLS-SRTP handshakes failed.");
    describe_counter!(
        M_DTLS_DROPPED_NO_LEG,
        "DTLS packets dropped because no leg of the session has DTLS configured \
         (stale or misrouted traffic)."
    );
    describe_counter!(
        M_DTLS_PACKETS_RECEIVED,
        "DTLS packets received on media sockets and routed to a handshake."
    );
    describe_counter!(M_DTLS_SEND_ERRORS, "Errors sending DTLS handshake packets.");
    describe_counter!(
        M_DTMF_DUPLICATES_SUPPRESSED,
        "DTMF events suppressed because the other detection method already \
         reported the digit, by method and digit."
    );
    describe_counter!(
        M_DTMF_EVENTS,
        "DTMF digit events detected, by method (rfc2833, inband) and digit."
    );
    describe_counter!(
        M_DTMF_INBAND_EVENTS,
        "In-band (audio-analysis) DTMF events detected, by digit and event_type."
    );
    describe_counter!(
        M_DTMF_INBAND_PACKETS,
        "Audio packets run through the in-band DTMF detector."
    );
    describe_counter!(
        M_DTMF_RFC2833_EVENTS,
        "RFC 2833 telephone-event DTMF events detected, by digit and event_type."
    );
    describe_counter!(
        M_DTMF_RFC2833_INJECTED_PACKETS,
        "Locally generated RFC 2833 telephone-event packets injected into the \
         outbound RTP stream."
    );
    describe_counter!(
        M_DTMF_RFC2833_INJECTED_BYTES,
        "Bytes of locally generated RFC 2833 telephone-event packets injected \
         into the outbound RTP stream."
    );
    describe_counter!(
        M_DTMF_RFC2833_PACKETS,
        "RFC 2833 telephone-event packets received and consumed by detection."
    );
    describe_counter!(
        M_DTMF_RFC2833_RELAYED,
        "RFC 2833 telephone-event packets relayed to the peer through the \
         normal forwarding path instead of being consumed."
    );
    describe_counter!(
        M_GENERATED_AUDIO_PACKETS_SENT,
        "Locally generated (not forwarded) audio packets sent, by source \
         (ai, media_bridge_audio, media_bridge_dtmf)."
    );
    describe_counter!(
        M_GENERATED_AUDIO_BYTES_SENT,
        "Locally generated (not forwarded) audio bytes sent, by source. \
         Wire bytes: full packet length including header and any SRTP \
         overhead."
    );
    describe_counter!(
        M_GENERATED_MEDIA_PACKETS_SENT,
        "All locally generated media packets sent — audio plus injected \
         telephone-events — by source."
    );
    describe_counter!(
        M_GENERATED_MEDIA_BYTES_SENT,
        "All locally generated media bytes sent — audio plus injected \
         telephone-events — by source. Wire bytes: full packet length \
         including header and any SRTP overhead."
    );
    describe_counter!(M_RTCP_PACKETS_RECEIVED, "RTCP packets received.");
    describe_counter!(M_RTCP_BYTES_RECEIVED, "RTCP bytes received.");
    describe_counter!(
        M_RTCP_PACKETS_SENT,
        "RTCP packets sent (locally originated)."
    );
    describe_counter!(M_RTCP_BYTES_SENT, "RTCP bytes sent (locally originated).");
    describe_counter!(
        M_RTCP_SENDER_PACKETS,
        "RTP packets peers report having sent, accumulated per-SSRC as \
         deltas of the cumulative sender packet count across received RTCP \
         Sender Reports (RFC 3550 section 6.4.1), restart-aware."
    );
    describe_counter!(
        M_RTCP_SENDER_BYTES,
        "RTP payload octets peers report having sent, accumulated per-SSRC \
         as deltas of the cumulative sender octet count across received \
         RTCP Sender Reports (RFC 3550 section 6.4.1), restart- and \
         wrap-aware."
    );
    describe_counter!(M_RTCP_SR_SENT, "RTCP Sender Reports originated locally.");
    describe_gauge!(
        M_RTCP_HIGHEST_SEQ,
        "Extended highest sequence number from the most recent received RTCP \
         report block."
    );
    describe_gauge!(
        M_RTCP_JITTER,
        "Peer-reported interarrival jitter (RTP timestamp units) from the \
         most recent received RTCP report block."
    );
    describe_gauge!(
        M_RTCP_LOSS_FRACTION,
        "Peer-reported fraction of packets lost (0.0-1.0) from the most \
         recent received RTCP report block."
    );
    describe_gauge!(
        M_RTCP_PACKETS_LOST,
        "Peer-reported cumulative packets lost from the most recent received \
         RTCP report block (a gauge because the peer's cumulative value can \
         decrease with late arrivals)."
    );
    describe_counter!(M_RTP_PACKETS_RECEIVED, "RTP packets received.");
    describe_counter!(M_RTP_BYTES_RECEIVED, "RTP payload bytes received.");
    describe_counter!(M_RTP_PACKETS_SENT, "RTP packets forwarded to the peer.");
    describe_counter!(M_RTP_BYTES_SENT, "RTP payload bytes forwarded to the peer.");
    describe_counter!(
        M_RTP_UNSUPPORTED_FIRST_BYTE,
        "Datagrams dropped from media sockets whose first byte marks them as \
         neither RTP/RTCP nor DTLS."
    );
    describe_counter!(
        M_SRTP_PROTECT_ERRORS,
        "SRTP protect (encrypt) failures on the outbound path."
    );
    describe_counter!(
        M_SRTP_UNPROTECT_ERRORS,
        "SRTP unprotect (auth/decrypt) failures on the inbound path."
    );
    describe_counter!(
        M_SRTCP_PROTECT_ERRORS,
        "SRTCP protect (encrypt) failures on the outbound path."
    );
    describe_counter!(
        M_SRTCP_UNPROTECT_ERRORS,
        "SRTCP unprotect (auth/decrypt) failures on the inbound path."
    );
    describe_counter!(
        M_TRANSCODING_PACKETS,
        "Packets transcoded between codecs, by from_codec and to_codec."
    );
    describe_counter!(
        M_TRANSCODING_BYTES,
        "Output bytes produced by transcoding, by from_codec and to_codec."
    );
    describe_counter!(
        M_TRANSCODING_ERRORS,
        "Transcode failures, by from_codec and to_codec."
    );
    describe_histogram!(
        M_TRANSCODING_DURATION,
        "Wall time of one transcode operation, by from_codec and to_codec."
    );
    describe_counter!(M_VAD_ERRORS, "VAD processing errors, by backend.");
    describe_counter!(
        M_VAD_WINDOWS,
        "VAD analysis windows processed, by backend (the energy backend \
         counts one window per frame)."
    );
    describe_histogram!(
        M_VAD_NEURAL_INFERENCE,
        "Wall time of one neural-VAD process call, covering the model \
         windows completed by one audio frame (usually one)."
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

        let own: BTreeSet<&str> = ALL_COUNTERS
            .iter()
            .chain(ALL_GAUGES.iter())
            .chain(ALL_HISTOGRAMS.iter())
            .copied()
            .collect();
        // Names this crate re-emits but forge-rtp owns and describes.
        let rtp_owned: BTreeSet<&str> = forge_rtp::metrics::ALL_COUNTERS
            .iter()
            .chain(forge_rtp::metrics::ALL_GAUGES.iter())
            .chain(forge_rtp::metrics::ALL_HISTOGRAMS.iter())
            .copied()
            .collect();

        for (kind, name) in &emitted {
            assert!(
                name.starts_with("forge_"),
                "metric `{name}` breaks the forge_ naming convention"
            );
            if rtp_owned.contains(name.as_str()) {
                continue;
            }
            assert!(
                own.contains(name.as_str()),
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
        for name in own {
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
