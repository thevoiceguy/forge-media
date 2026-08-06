# Metrics Inventory

Every metric forge-media emits through the [`metrics`](https://docs.rs/metrics)
facade, in one place. This is the inventory downstream consumers (for example
SiphonAI's DEPLOY.md) point at for `forge_*` metric semantics.

Two metric systems exist in this workspace:

- **The `metrics` facade** (`counter!` / `gauge!` / `histogram!`) — everything
  in this document. These render through whatever recorder the embedding
  process installs (the standalone server's `metrics-exporter-prometheus`
  recorder, or an embedding consumer's own exporter).
- **Direct `prometheus`-crate registries** (`forge_ha_*`, `forge_event_bus_*`,
  `forge_ai_session_*`, …) — registered with inline help text and buckets in
  their own modules (for example `crates/forge-ha/src/metrics.rs`) and exposed
  only by the standalone server. Not covered here.

## Descriptions (`# HELP`)

Every facade family has a `describe_*!` registration, so any consumer's
Prometheus exporter renders a `# HELP` line for it. Descriptions live next to
the name consts in each emitting crate:

| Crate | Module | Families |
|---|---|---|
| forge-rtp | `forge_rtp::metrics` | 8 |
| forge-engine | `forge_engine::metrics` | 48 |
| forge-conference | `forge_conference::metrics` | 13 |
| forge-api | `forge_api::metrics` | 10 |

`describe_metrics()` in each module is idempotent and **must run after a
`metrics` recorder is installed** (descriptions issued to the no-op recorder
are lost). You normally never call it yourself:

- `forge_engine::SessionManager::new*` calls forge-engine's (which covers
  forge-rtp's) — embedding consumers that install their exporter before
  constructing the engine get `# HELP` for free.
- `forge_conference::ConferenceBridge::new` calls forge-conference's.
- The standalone server's `MetricsHandle::init` calls
  `forge_api::metrics::describe_metrics()`, which covers everything.

## Histogram buckets

Bucket boundaries are **exporter configuration** — a library cannot set them.
The five facade histograms have suggested-bucket consts next to their names;
the standalone server registers them with `Matcher::Full` overrides in
`MetricsHandle::init`. Embedding consumers should do the same:

```rust
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

PrometheusBuilder::new()
    .set_buckets_for_metric(
        Matcher::Full(forge_engine::metrics::M_VAD_NEURAL_INFERENCE.to_string()),
        &forge_engine::metrics::VAD_NEURAL_INFERENCE_SECONDS_BUCKETS,
    )?
    .set_buckets_for_metric(
        Matcher::Full(forge_engine::metrics::M_TRANSCODING_DURATION.to_string()),
        &forge_engine::metrics::TRANSCODING_DURATION_SECONDS_BUCKETS,
    )?
    .set_buckets_for_metric(
        Matcher::Full(forge_conference::metrics::M_MIXING_DURATION.to_string()),
        &forge_conference::metrics::MIXING_DURATION_SECONDS_BUCKETS,
    )?
    // ...
    .install_recorder()?;
```

Without buckets, `metrics-exporter-prometheus` renders a histogram as a
summary (quantiles), which cannot be aggregated across instances.

## Inventory

Label sets are per-family; a family with no labels listed has none.

### forge-rtp

| Metric | Type | Labels | Description |
|---|---|---|---|
| `forge_srtp_packets_encrypted_total` | counter | | RTP packets successfully SRTP-protected on the outbound path. |
| `forge_srtp_packets_decrypted_total` | counter | | Inbound SRTP packets successfully unprotected (auth + decrypt). |
| `forge_srtp_replay_attacks_blocked_total` | counter | | Inbound SRTP packets rejected by replay protection. |
| `forge_srtcp_packets_encrypted_total` | counter | | RTCP packets successfully SRTCP-protected on the outbound path. |
| `forge_srtcp_packets_decrypted_total` | counter | | Inbound SRTCP packets successfully unprotected (auth + decrypt). |
| `forge_srtcp_replay_attacks_blocked_total` | counter | | Inbound SRTCP packets rejected by replay protection. |
| `forge_rtp_latch_learned_total` | counter | | Remote media endpoints learned via symmetric-RTP latching. Also emitted by forge-engine. |
| `forge_rtp_latch_rejected_total` | counter | | Datagrams rejected by symmetric-RTP latching rules. Also emitted by forge-engine. |

### forge-engine

| Metric | Type | Labels | Description |
|---|---|---|---|
| `forge_active_sessions` | gauge | | Media sessions currently active on this engine. |
| `forge_rtp_packets_received_total` | counter | | RTP packets received. |
| `forge_rtp_bytes_received_total` | counter | | RTP payload bytes received. |
| `forge_rtp_packets_sent_total` | counter | | RTP packets forwarded to the peer. |
| `forge_rtp_bytes_sent_total` | counter | | RTP payload bytes forwarded to the peer. |
| `forge_rtp_unsupported_first_byte_total` | counter | | Datagrams dropped from media sockets whose first byte marks them as neither RTP/RTCP nor DTLS. |
| `forge_rtcp_packets_received_total` | counter | | RTCP packets received. |
| `forge_rtcp_bytes_received_total` | counter | | RTCP bytes received. |
| `forge_rtcp_packets_sent_total` | counter | | RTCP packets sent (locally originated). |
| `forge_rtcp_bytes_sent_total` | counter | | RTCP bytes sent (locally originated). |
| `forge_rtcp_sender_reports_sent_total` | counter | | RTCP Sender Reports originated locally. |
| `forge_rtcp_sender_packets_total` | counter | | Running sum of the cumulative sender packet counts carried in received RTCP Sender Reports. Grows with every SR received; not a wire packet count. |
| `forge_rtcp_sender_bytes_total` | counter | | Running sum of the cumulative sender octet counts carried in received RTCP Sender Reports. Grows with every SR received; not a wire byte count. |
| `forge_rtcp_highest_seq` | gauge | | Extended highest sequence number from the most recent received RTCP report block. |
| `forge_rtcp_jitter` | gauge | | Peer-reported interarrival jitter (RTP timestamp units) from the most recent received RTCP report block. |
| `forge_rtcp_packet_loss_fraction` | gauge | | Peer-reported fraction of packets lost (0.0–1.0) from the most recent received RTCP report block. |
| `forge_rtcp_packets_lost_total` | gauge | | Peer-reported cumulative packets lost from the most recent received RTCP report block (a gauge because the peer's cumulative value can decrease with late arrivals). |
| `forge_srtp_protect_errors_total` | counter | | SRTP protect (encrypt) failures on the outbound path. |
| `forge_srtp_unprotect_errors_total` | counter | | SRTP unprotect (auth/decrypt) failures on the inbound path. |
| `forge_srtcp_protect_errors_total` | counter | | SRTCP protect (encrypt) failures on the outbound path. |
| `forge_srtcp_unprotect_errors_total` | counter | | SRTCP unprotect (auth/decrypt) failures on the inbound path. |
| `forge_dtls_packets_received_total` | counter | | DTLS packets received on media sockets and routed to a handshake. |
| `forge_dtls_handshakes_completed_total` | counter | | DTLS-SRTP handshakes completed successfully. |
| `forge_dtls_handshakes_failed_total` | counter | | DTLS-SRTP handshakes failed. |
| `forge_dtls_send_errors_total` | counter | | Errors sending DTLS handshake packets. |
| `forge_dtls_packets_dropped_no_leg_total` | counter | | DTLS packets dropped because no leg of the session has DTLS configured (stale or misrouted traffic). |
| `forge_dtmf_events_total` | counter | `method`, `digit` | DTMF digit events detected, by method (`rfc2833`, `inband`) and digit. |
| `forge_dtmf_rfc2833_events_total` | counter | `digit`, `event_type` | RFC 2833 telephone-event DTMF events detected. |
| `forge_dtmf_rfc2833_packets_total` | counter | | RFC 2833 telephone-event packets received and consumed by detection. |
| `forge_dtmf_rfc2833_relayed_total` | counter | | RFC 2833 telephone-event packets relayed to the peer through the normal forwarding path instead of being consumed. |
| `forge_dtmf_rfc2833_injected_packets_total` | counter | | Locally generated RFC 2833 telephone-event packets injected into the outbound RTP stream. |
| `forge_dtmf_rfc2833_injected_bytes_total` | counter | | Bytes of locally generated RFC 2833 telephone-event packets injected into the outbound RTP stream. |
| `forge_dtmf_inband_events_total` | counter | `digit`, `event_type` | In-band (audio-analysis) DTMF events detected. |
| `forge_dtmf_inband_packets_processed_total` | counter | | Audio packets run through the in-band DTMF detector. |
| `forge_dtmf_duplicates_suppressed_total` | counter | `method`, `digit` | DTMF events suppressed because the other detection method already reported the digit. |
| `forge_generated_audio_packets_sent_total` | counter | `source` | Locally generated (not forwarded) audio packets sent, by source (`ai`, `media_bridge_audio`, `media_bridge_dtmf`). |
| `forge_generated_audio_bytes_sent_total` | counter | `source` | Locally generated (not forwarded) audio bytes sent. Wire bytes: full packet length including header and any SRTP overhead. |
| `forge_generated_media_packets_sent_total` | counter | `source` | All locally generated media packets sent — audio plus injected telephone-events. |
| `forge_generated_media_bytes_sent_total` | counter | `source` | All locally generated media bytes sent — audio plus injected telephone-events. Wire bytes: full packet length including header and any SRTP overhead. |
| `forge_ai_audio_packets_sent_total` | counter | | Locally generated audio packets sent whose source is the AI stream (subset of `forge_generated_audio_packets_sent_total`). |
| `forge_ai_audio_bytes_sent_total` | counter | | Locally generated audio wire bytes sent whose source is the AI stream (subset of `forge_generated_audio_bytes_sent_total`). |
| `forge_transcoding_packets_total` | counter | `from_codec`, `to_codec` | Packets transcoded between codecs. |
| `forge_transcoding_bytes_total` | counter | `from_codec`, `to_codec` | Output bytes produced by transcoding. |
| `forge_transcoding_errors_total` | counter | `from_codec`, `to_codec` | Transcode failures. |
| `forge_transcoding_duration_seconds` | histogram | `from_codec`, `to_codec` | Wall time of one transcode operation. Suggested buckets: `TRANSCODING_DURATION_SECONDS_BUCKETS` (0.1 ms – 20 ms; one transcode handles one packet, so healthy operation sits well under the 20 ms frame budget). |
| `forge_vad_windows_total` | counter | `backend` | VAD analysis windows processed (the energy backend counts one window per frame). |
| `forge_vad_errors_total` | counter | `backend` | VAD processing errors. |
| `forge_vad_neural_inference_seconds` | histogram | | Wall time of one neural-VAD process call, covering the model windows completed by one audio frame (usually one). Suggested buckets: `VAD_NEURAL_INFERENCE_SECONDS_BUCKETS` (0.5 ms – 100 ms). |

### forge-conference

All families except the room-lifecycle counters carry a `room_id` label.
`room_id` is operator-assigned; keep room ids bounded (do not derive them from
per-call values) or the label cardinality is unbounded.

| Metric | Type | Labels | Description |
|---|---|---|---|
| `forge_conference_rooms_created_total` | counter | | Conference rooms created. |
| `forge_conference_rooms_deleted_total` | counter | | Conference rooms deleted. |
| `forge_conference_rooms_active` | gauge | | Conference rooms currently active. |
| `forge_conference_participants_joined_total` | counter | `room_id` | Participants joined to a conference room. |
| `forge_conference_participants_left_total` | counter | `room_id` | Participants departed from a conference room. |
| `forge_conference_participants_active` | gauge | `room_id` | Participants currently in a conference room. |
| `forge_conference_mix_operations_total` | counter | `room_id` | Mixer passes executed (one output frame each). |
| `forge_conference_mixing_duration_seconds` | histogram | `room_id` | Wall time of one mixer pass. Suggested buckets: `MIXING_DURATION_SECONDS_BUCKETS` (0.5 ms – 100 ms; one pass produces one frame, so healthy operation sits well under the 20 ms frame budget). |
| `forge_conference_recordings_started_total` | counter | `room_id` | Room-level conference recordings started. |
| `forge_conference_recordings_stopped_total` | counter | `room_id` | Room-level conference recordings stopped. |
| `forge_conference_recordings_active` | gauge | `room_id` | Whether a room-level recording is running (0 or 1). |
| `forge_conference_participant_recordings_started_total` | counter | `room_id`, `participant_id` | Per-participant recordings started. |
| `forge_conference_participant_recordings_stopped_total` | counter | `room_id`, `participant_id` | Per-participant recordings stopped. |

### forge-api

Six of these families were emitted without the `forge_` prefix before the
describe-everything change (`webrtc_connections_created_total`,
`webrtc_connections_deleted_total`, and the four `sdp_*` families); dashboards
reading the old names need the one-line rename.

| Metric | Type | Labels | Description |
|---|---|---|---|
| `forge_webrtc_connections_active` | gauge | | WebRTC peer connections currently held by the API server. |
| `forge_webrtc_connections_created_total` | counter | | WebRTC peer connections created via the API. |
| `forge_webrtc_connections_deleted_total` | counter | | WebRTC peer connections deleted via the API. |
| `forge_webrtc_ice_candidates_added_total` | counter | | Remote ICE candidates added to WebRTC connections via the API. |
| `forge_webrtc_ice_candidates_gathered` | gauge | | Local ICE candidates gathered by the most recently created WebRTC connection (sampled at offer time). |
| `forge_webrtc_connection_establishment_duration_seconds` | histogram | | Time from WebRTC connection creation to the remote answer being applied. Suggested buckets: `WEBRTC_ESTABLISHMENT_SECONDS_BUCKETS` (0.1 s – 30 s). |
| `forge_sdp_negotiation_total` | counter | | SDP offer/answer negotiations attempted via the API. |
| `forge_sdp_negotiation_failures_total` | counter | `reason` | SDP negotiations failed (`missing_local_address`, `invalid_profile`, `parse_error`, `no_common_codec`, `negotiation_error`). |
| `forge_sdp_codecs_negotiated_total` | counter | `codec` | Codecs selected by successful SDP negotiations. |
| `forge_sdp_negotiation_duration_seconds` | histogram | | Wall time of one SDP parse + negotiation. Suggested buckets: `SDP_NEGOTIATION_SECONDS_BUCKETS` (0.1 ms – 50 ms; pure local computation). |

## Adding a metric

1. Emit it with a **string-literal** name (`counter!("forge_…", …)`) — the
   self-scan tests reject non-literal names so every emission stays greppable.
2. Add a `M_*` const, a `describe_*!` line, and an `ALL_*` entry in the
   emitting crate's `src/metrics.rs`. The crate's self-scan test fails the
   build until the lists match the emission sites — in both directions.
3. Histograms also get a suggested-buckets const, a `Matcher::Full` entry in
   the standalone server's `MetricsHandle::init`, and a buckets note here.
4. Document it in the table above.

forge-api's test suite additionally sweeps every crate under `crates/` and
fails if any facade emission anywhere in the workspace is missing from the
describe lists, so a brand-new emitting crate cannot ship undescribed metrics
unnoticed.
