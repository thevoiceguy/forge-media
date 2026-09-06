//! Metric descriptions for the families this crate emits.
//!
//! Every `counter!`/`gauge!`/`histogram!` name emitted by
//! forge-conference has a `M_*` const here, a `describe_*!` registration
//! in [`describe_metrics`], and an entry in the `ALL_*` lists. A
//! self-scan test walks this crate's sources and fails if an emission
//! site and these lists ever disagree.
//!
//! [`describe_metrics`] must run *after* a `metrics` recorder is
//! installed — descriptions issued to the no-op recorder are lost.
//! `ConferenceBridge::new` calls it, so consumers that install their
//! exporter before constructing the bridge (the normal order) get
//! `# HELP` lines for free.

use metrics::{describe_counter, describe_gauge, describe_histogram};

pub const M_ROOMS_CREATED: &str = "forge_conference_rooms_created_total";
pub const M_ROOMS_DELETED: &str = "forge_conference_rooms_deleted_total";
pub const M_ROOMS_ACTIVE: &str = "forge_conference_rooms_active";
pub const M_PARTICIPANTS_JOINED: &str = "forge_conference_participants_joined_total";
pub const M_PARTICIPANTS_LEFT: &str = "forge_conference_participants_left_total";
pub const M_PARTICIPANTS_ACTIVE: &str = "forge_conference_participants_active";
pub const M_MIX_OPERATIONS: &str = "forge_conference_mix_operations_total";
pub const M_MIXING_DURATION: &str = "forge_conference_mixing_duration_seconds";
pub const M_RECORDINGS_STARTED: &str = "forge_conference_recordings_started_total";
pub const M_RECORDINGS_STOPPED: &str = "forge_conference_recordings_stopped_total";
pub const M_RECORDINGS_ACTIVE: &str = "forge_conference_recordings_active";
pub const M_PARTICIPANT_RECORDINGS_STARTED: &str =
    "forge_conference_participant_recordings_started_total";
pub const M_PARTICIPANT_RECORDINGS_STOPPED: &str =
    "forge_conference_participant_recordings_stopped_total";

// Video (the `video` module).
pub const M_VIDEO_ROOMS: &str = "forge_conference_video_rooms";
pub const M_VIDEO_SOURCES: &str = "forge_conference_video_sources";
pub const M_VIDEO_ENCODERS: &str = "forge_conference_video_encoders";
pub const M_VIDEO_FPS: &str = "forge_conference_video_fps";
pub const M_VIDEO_TICKS: &str = "forge_conference_video_ticks_total";
pub const M_VIDEO_FRAMES_DECODED: &str = "forge_conference_video_frames_decoded_total";
pub const M_VIDEO_FRAMES_LOST: &str = "forge_conference_video_frames_lost_total";
pub const M_VIDEO_FRAMES_DROPPED: &str = "forge_conference_video_frames_dropped_total";
pub const M_VIDEO_DECODE_ERRORS: &str = "forge_conference_video_decode_errors_total";
pub const M_VIDEO_ENCODE_ERRORS: &str = "forge_conference_video_encode_errors_total";
pub const M_VIDEO_KEYFRAMES_SENT: &str = "forge_conference_video_keyframes_sent_total";
pub const M_VIDEO_PACKETS_SENT: &str = "forge_conference_video_packets_sent_total";
pub const M_VIDEO_PLIS_SENT: &str = "forge_conference_video_plis_sent_total";
pub const M_VIDEO_PLIS_RECEIVED: &str = "forge_conference_video_plis_received_total";
pub const M_VIDEO_NACKS_SENT: &str = "forge_conference_video_nacks_sent_total";
pub const M_VIDEO_NACKS_RECEIVED: &str = "forge_conference_video_nacks_received_total";
pub const M_VIDEO_COMPOSE_DURATION: &str = "forge_conference_video_compose_duration_seconds";

/// Every counter family forge-conference emits.
pub const ALL_COUNTERS: &[&str] = &[
    M_ROOMS_CREATED,
    M_ROOMS_DELETED,
    M_PARTICIPANTS_JOINED,
    M_PARTICIPANTS_LEFT,
    M_MIX_OPERATIONS,
    M_RECORDINGS_STARTED,
    M_RECORDINGS_STOPPED,
    M_PARTICIPANT_RECORDINGS_STARTED,
    M_PARTICIPANT_RECORDINGS_STOPPED,
    M_VIDEO_TICKS,
    M_VIDEO_FRAMES_DECODED,
    M_VIDEO_FRAMES_LOST,
    M_VIDEO_FRAMES_DROPPED,
    M_VIDEO_DECODE_ERRORS,
    M_VIDEO_ENCODE_ERRORS,
    M_VIDEO_KEYFRAMES_SENT,
    M_VIDEO_PACKETS_SENT,
    M_VIDEO_PLIS_SENT,
    M_VIDEO_PLIS_RECEIVED,
    M_VIDEO_NACKS_SENT,
    M_VIDEO_NACKS_RECEIVED,
];

/// Every gauge family forge-conference emits.
pub const ALL_GAUGES: &[&str] = &[
    M_ROOMS_ACTIVE,
    M_PARTICIPANTS_ACTIVE,
    M_RECORDINGS_ACTIVE,
    M_VIDEO_ROOMS,
    M_VIDEO_SOURCES,
    M_VIDEO_ENCODERS,
    M_VIDEO_FPS,
];

/// Every histogram family forge-conference emits.
pub const ALL_HISTOGRAMS: &[&str] = &[M_MIXING_DURATION, M_VIDEO_COMPOSE_DURATION];

/// Suggested buckets for `forge_conference_video_compose_duration_seconds`.
///
/// One compose pass renders every layout output and encodes every
/// flavor; at 15 fps the budget is 66 ms and at 30 fps 33 ms, and three
/// overruns in a row halve the room's frame rate.
pub const VIDEO_COMPOSE_DURATION_SECONDS_BUCKETS: [f64; 9] =
    [0.001, 0.002, 0.005, 0.01, 0.02, 0.033, 0.066, 0.1, 0.2];

/// Suggested buckets for `forge_conference_mixing_duration_seconds`.
///
/// One mix pass produces one output frame, so healthy operation sits
/// well under the 0.02 s frame budget; the top buckets exist to make an
/// overloaded mixer visible.
pub const MIXING_DURATION_SECONDS_BUCKETS: [f64; 8] =
    [0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1];

/// Register a description for every metric family this crate emits.
///
/// Idempotent and cheap. Descriptions only reach the recorder installed
/// at call time, so call it (again) once your recorder is installed.
/// `ConferenceBridge::new` calls this.
pub fn describe_metrics() {
    describe_counter!(M_ROOMS_CREATED, "Conference rooms created.");
    describe_counter!(M_ROOMS_DELETED, "Conference rooms deleted.");
    describe_gauge!(M_ROOMS_ACTIVE, "Conference rooms currently active.");
    describe_counter!(
        M_PARTICIPANTS_JOINED,
        "Participants joined to a conference room, by room_id."
    );
    describe_counter!(
        M_PARTICIPANTS_LEFT,
        "Participants departed from a conference room, by room_id."
    );
    describe_gauge!(
        M_PARTICIPANTS_ACTIVE,
        "Participants currently in a conference room, by room_id."
    );
    describe_counter!(
        M_MIX_OPERATIONS,
        "Mixer passes executed (one output frame each), by room_id."
    );
    describe_histogram!(
        M_MIXING_DURATION,
        "Wall time of one mixer pass, by room_id."
    );
    describe_counter!(
        M_RECORDINGS_STARTED,
        "Room-level conference recordings started, by room_id."
    );
    describe_counter!(
        M_RECORDINGS_STOPPED,
        "Room-level conference recordings stopped, by room_id."
    );
    describe_gauge!(
        M_RECORDINGS_ACTIVE,
        "Whether a room-level recording is running (0 or 1), by room_id."
    );
    describe_counter!(
        M_PARTICIPANT_RECORDINGS_STARTED,
        "Per-participant recordings started, by room_id and participant_id."
    );
    describe_counter!(
        M_PARTICIPANT_RECORDINGS_STOPPED,
        "Per-participant recordings stopped, by room_id and participant_id."
    );
    describe_gauge!(M_VIDEO_ROOMS, "Rooms with video running.");
    describe_gauge!(M_VIDEO_SOURCES, "Participants sending video, node-wide.");
    describe_gauge!(M_VIDEO_ENCODERS, "Video encoders running, node-wide.");
    describe_gauge!(
        M_VIDEO_FPS,
        "A room's current video frame rate, by room_id."
    );
    describe_counter!(
        M_VIDEO_TICKS,
        "Video clock ticks (compose passes), by room_id."
    );
    describe_counter!(
        M_VIDEO_FRAMES_DECODED,
        "Video frames decoded from participants, by room_id."
    );
    describe_counter!(
        M_VIDEO_FRAMES_LOST,
        "Video frames the assembler gave up on (loss or invalid), by room_id."
    );
    describe_counter!(
        M_VIDEO_FRAMES_DROPPED,
        "Video frames not decoded (invalid picture, queue, rate or size limit), by room_id."
    );
    describe_counter!(M_VIDEO_DECODE_ERRORS, "Video decoder errors, by room_id.");
    describe_counter!(M_VIDEO_ENCODE_ERRORS, "Video encoder errors, by room_id.");
    describe_counter!(
        M_VIDEO_KEYFRAMES_SENT,
        "Keyframes produced by composite encoders, by room_id."
    );
    describe_counter!(
        M_VIDEO_PACKETS_SENT,
        "Composite video frames handed to subscribers (one per subscriber per frame), by room_id."
    );
    describe_counter!(M_VIDEO_PLIS_SENT, "PLIs sent to video sources, by room_id.");
    describe_counter!(
        M_VIDEO_PLIS_RECEIVED,
        "PLIs and FIRs received from video subscribers, by room_id."
    );
    describe_counter!(
        M_VIDEO_NACKS_SENT,
        "NACKs sent to video sources, by room_id."
    );
    describe_counter!(
        M_VIDEO_NACKS_RECEIVED,
        "NACKs received from video subscribers, by room_id."
    );
    describe_histogram!(
        M_VIDEO_COMPOSE_DURATION,
        "Wall time of one video compose pass (render and encode), by room_id."
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
}
