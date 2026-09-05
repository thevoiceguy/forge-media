# Changelog

All notable changes to the Forge Media Engine project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

**Video substrate (FCP video conferencing, phase 1).** Everything a video mixer needs below the
codecs, so forge can carry, inspect and re-emit video RTP without decoding it: RTCP feedback,
the RTP payload formats, frame assembly with loss detection, a retransmission cache, a stream
rewriter for switching sources on a keyframe, and SDP video negotiation. Design and rationale in
FCP's `docs/VIDEO_CONFERENCING.md`.

**Crate versions:** **forge-rtp 0.4.0**, forge-core 0.2.1, forge-sdp 0.2.1.

**Breaking changes:** **forge-rtp 0.4.0** — `RtcpPacketType` gains `RTPFB` (205) and `PSFB`
(206) and `RtcpPacket` gains `TransportFeedback` / `PayloadFeedback` variants; exhaustive matches
need an arm. `RtcpPacket::ReceiverReport(..).to_bytes()` now writes the correct length field
(it was one 32-bit word short, which any receiver walking a compound packet tripped over).

### Added

- **forge-core**: `VideoCodec` (H.264, H.265, VP8, VP9, AV1) with SDP names, the 90 kHz clock and
  non-colliding default payload types.
- **forge-rtp / rtcp**: Generic NACK (RFC 4585), PLI, FIR (RFC 5104) and REMB parse and build,
  `RtcpPacket::parse_compound` (skips sub-packets forge does not model instead of failing),
  `is_keyframe_request`, `feedback_media_ssrc`.
- **forge-rtp / video**: `inspect` — frame start and keyframe detection per codec (RFC 6184
  single NAL / STAP-A / FU-A, RFC 7798, RFC 7741, RFC 9628, AV1 aggregation header);
  `payload::{packetize, depacketize}` for all five codecs (H.264/H.265 as Annex B, VP8/VP9 raw
  frames, AV1 temporal units with size fields restored); `FrameAssembler` (reorder window,
  gap → `Lost`, skip to the next frame start, `needs_keyframe`, `missing` for NACKs, frame size
  limit); `RtxCache` for answering NACKs; `KeyframeRequestGate`; `StreamRewriter` (one outgoing
  SSRC / sequence / timestamp line across source switches, switching only at a keyframe).
- **forge-sdp / video**: `CodecInfo::to_video_codec`, `a=rtcp-fb` parsing and emission
  (`VideoAttributesExt`), `H264Fmtp` (`profile-level-id`, `packetization-mode`, forwardability),
  `choose_video_codec`, `answer_video` (RFC 3264 answer for one codec: offered PT, fmtp echoed,
  supported feedback kept, `mid` / `rtcp-mux` mirrored, direction per §6.1), `reject_section`
  (port 0, formats and `mid` mirrored, `a=inactive`), `SdpProfile::audio_video` with
  `with_local_addr_video`.

### Known gaps

- `sip-sdp` has no `RTP/AVPF` protocol variant; an offer using it does not parse. Needed before
  FCP phase 3 (some SIP video endpoints offer it).
- `StreamRewriter` assumes a 30 fps minimum step when a switch happens with no recent packet.

## [2026-09-03] — workspace release

**A frame clock for the conference mixer.** A conference room has more than one reader of each
participant's audio — every other participant's N-1 mix, the room recorder, an AI tap — and the
mixer handed out audio by *draining* it on read. Two readers were fine only by luck of timing;
with three or more participants each 20 ms frame of a caller's speech reached whichever listener's
tick came first and nobody else, so three-party conferences on a per-participant mixing loop were
audibly broken. Cascaded conferencing (FCP distributed conferencing, phase 2) adds a further
reader per inter-node trunk and needs a mix that excludes a *set* of participants, which the old
API could not express at all. This release separates taking audio from reading it.

**Embeds siphon-rs `v2026.09.03`** (`external/siphon-rs`; `forge-sdp` pins the same tag) — that
release adds mutual TLS to the SIP transport. forge consumes only `sip-sdp`, unchanged, so the
media path does not move; the pin keeps a consumer that patches `sip-sdp` to its own siphon-rs
checkout on one copy.

**Crate versions:** **forge-mixer 0.3.0**, **forge-conference 0.4.0**. (bcg729-sys 0.1.0,
forge-ai-stream 0.2.0, forge-api 0.4.0, forge-codecs 0.2.0, forge-core 0.2.0, forge-dtmf 0.2.1,
forge-engine 0.5.1, forge-ha 0.2.0, forge-hep 0.0.1, forge-ice 0.3.0, forge-injection 0.1.1,
forge-kernel 0.2.0, forge-kernel-ebpf 0.2.0, forge-recorder 0.2.0, forge-resampler 0.1.1,
forge-rtp 0.3.1, forge-sdp 0.2.0, forge-siprec 0.2.1, forge-storage 0.2.0, forge-transcoder 0.2.0,
forge-transcription 0.2.0, forge-vad 0.2.0, forge-webrtc 0.5.0, forge-media 0.2.0 unchanged.)

**Breaking changes:** **forge-mixer 0.3.0** — `MixerOptions` gains a public `frame_clock: bool`
field. Source-breaking only for callers constructing `MixerOptions` with an exhaustive struct
literal; `..MixerOptions::default()` is unaffected, and the `false` default preserves the
drain-on-read behaviour exactly. Nothing about `mix()`, `mix_excluding()`,
`get_participant_audio()` or `get_all_participant_audio()` changes unless the clock is on.

### Added

- **`forge-mixer`: frame-clock mode** (`MixerOptions::frame_clock`). With it on, nothing is
  consumed by reading: `AudioMixer::advance_frame()` moves one frame from every active
  participant's buffer into a per-participant snapshot, and every mix until the next advance reads
  those snapshots, so any number of consumers see the same frame and none can starve another. The
  legacy `mix*` / `get_*_audio` calls read the snapshot in this mode, so existing consumers
  (recorder, AI manager) work unchanged once something drives the clock. New:
  `mix_frame_filtered(|id| …)` mixes the participants a predicate accepts — the send-mix for a
  trunk to a peer node excludes every other trunk, so a full mesh never carries audio back to its
  source — plus `mix_frame()`, `mix_frame_excluding()`, `frame_seq()`, `frame_clock_enabled()`.
  A participant that has run more than five frames ahead of the clock is trimmed back to two, so a
  sender with a slightly fast clock cannot walk the room up to a second of delay before the buffer
  cap starts dropping audio.

- **`forge-conference`: the room drives the clock.** `ConferenceRoom::start_frame_clock()` spawns
  a task that advances the mixer once per frame period (`frame_duration()`), feeds the room
  recorder from the new frame, and publishes the frame sequence on a `tokio::sync::watch` that
  `frame_clock()` subscribes to — a participant's outbound loop awaits the tick instead of running
  its own interval. Idempotent; the task holds a weak reference and `delete_room` / drop stop it.
  `advance_frame()` for drivers with their own clock, `mix_frame_filtered()` passed through.
  `add_virtual_participant(id)` adds a participant that stands for something other than a caller
  (an inter-node trunk) and so bypasses lock, wait-for-moderator and capacity, with no join sound
  and no host bookkeeping — the caller is responsible for having authenticated it.
  `AUDIO_FEEDBACK_PARTICIPANT_ID` is now public so a send-mix can leave the feedback channel out.

## [2026-08-26] — workspace release

**A pinnable WebRTC media port.** A WebRTC connection uses exactly one socket (BUNDLE plus
`a=rtcp-mux`) and forge always bound it ephemerally — right for a browser, wrong for a server
whose media port range is a firewall rule, a capacity budget, and a pool with a reserved band
all at once. This release lets a server place that socket deliberately, and lets media that
isn't a `MediaSession` draw from the same pool on the same terms.

Cut for siphon-ai's WebRTC media leg (`DEV_PLAN_WebRTC.md` §4.4), which is the first consumer
of both APIs.

**Embeds siphon-rs `v2026.08.24`** (`external/siphon-rs`), unchanged from the previous two
releases: nothing on the SIP-adjacent path moved, and forge consumes only `sip-sdp` from the
submodule.

**Crate versions:** **forge-webrtc 0.5.0**, **forge-engine 0.5.1**. (bcg729-sys 0.1.0,
forge-ai-stream 0.2.0, forge-api 0.4.0, forge-codecs 0.2.0, forge-conference 0.3.1,
forge-core 0.2.0, forge-dtmf 0.2.1, forge-ha 0.2.0, forge-hep 0.0.1, forge-ice 0.3.0,
forge-injection 0.1.1, forge-kernel 0.2.0, forge-kernel-ebpf 0.2.0, forge-mixer 0.2.0,
forge-recorder 0.2.0, forge-resampler 0.1.1, forge-rtp 0.3.1, forge-sdp 0.2.0,
forge-siprec 0.2.1, forge-storage 0.2.0, forge-transcoder 0.2.0, forge-transcription 0.2.0,
forge-vad 0.2.0, forge-media 0.2.0 unchanged.)

**Breaking changes:** **forge-webrtc 0.5.0** — `TransportConfig` gains a public `local_port`
field ([#134](https://github.com/thevoiceguy/forge-media/pull/134)). Source-breaking only for
callers constructing `TransportConfig` with an exhaustive struct literal; anyone using
`..TransportConfig::default()` is unaffected, and the field's `0` default preserves the
previous ephemeral-bind behaviour exactly.

### Added

- **`forge-webrtc`: `TransportConfig::local_port` — pin the ICE socket's UDP port.** A WebRTC
  connection needs exactly one socket (BUNDLE plus `a=rtcp-mux`), and it was always bound
  ephemerally. That is right for a browser and wrong for a *server*, which typically has a media
  port range its operator opened in a firewall and sized as a capacity budget: an ephemeral socket
  sits outside the firewall rule and outside the accounting. Setting `local_port` places the socket
  deliberately; `0` remains the default and still means "let the OS choose", so existing callers are
  unaffected. An occupied port fails the connection rather than falling back to an ephemeral one —
  silently landing outside the range would defeat the reason for asking. Binds the *host* socket
  only; a TURN allocation gathers through its own OS-assigned socket.

- **`forge-engine`: `SessionManager::reserve_port_pair` / `release_port_pair`.** The port pool is a
  budget, not just a source of socket numbers — it is what an operator sized for concurrent calls,
  opened in a firewall, and reserved a band of via `MediaSessionConfig::min_free_port_pairs`. Media
  that terminates somewhere other than a `MediaSession` still occupies that budget, and drawing from
  outside the pool leaves the capacity gauge, the reserved band, and the firewall range all
  describing a call load that no longer matches reality. These let such a leg draw a pair on the same
  terms a session does, `min_free` included. Releasing a pair that is not allocated is a no-op, so a
  teardown path that runs twice is safe.

## [2026-08-25] — workspace release

**A crash fix.** One reordered RTP packet could abort the process via a stack overflow in
`JitterBuffer::pop`. **No released consumer was affected** — the buffer is exported but was used
nowhere in forge-media or siphon-ai — but anything about to consume it (siphon-ai's WebRTC media leg
is the first) wants this release rather than `v2026.08.24`.

**Embeds siphon-rs `v2026.08.24`** (`external/siphon-rs`), unchanged from the previous release: forge
consumes only `sip-sdp` from the submodule, and that crate is identical in siphon-rs `v2026.08.25`, so
there was nothing to gain from moving it. A downstream pinning siphon-rs directly may sit on the newer
tag without divergence on the media path.

**Crate versions:** **forge-rtp 0.3.1**. (forge-api 0.4.0, forge-ai-stream 0.2.0, bcg729-sys 0.1.0,
forge-codecs 0.2.0, forge-conference 0.3.1, forge-core 0.2.0, forge-dtmf 0.2.1, forge-engine 0.5.0,
forge-ha 0.2.0, forge-hep 0.0.1, forge-ice 0.3.0, forge-injection 0.1.1, forge-kernel 0.2.0,
forge-kernel-ebpf 0.2.0, forge-mixer 0.2.0, forge-recorder 0.2.0, forge-resampler 0.1.1,
forge-sdp 0.2.0, forge-siprec 0.2.1, forge-storage 0.2.0, forge-transcoder 0.2.0,
forge-transcription 0.2.0, forge-vad 0.2.0, forge-webrtc 0.4.0, forge-media 0.2.0 unchanged.)

**Breaking changes:** none.

### Fixed

- **`forge-rtp`: one reordered RTP packet crashed the process (stack overflow).** `JitterBuffer::pop`'s
  missing-packet branch recursed into itself after advancing the expected sequence number by one. A late
  arrival — a packet whose sequence is *behind* what the buffer is waiting for — stays in the map where
  `packets.get(&next_seq)` cannot see it, but it *is* what `packets.iter().next()` returns, and
  `sequence_distance` is an unsigned `wrapping_sub`, so the distance from `next_seq` back to it reads as a
  ~65000-packet **forward** gap. That satisfied the `gap > 10` skip rule, which advanced by one and
  recursed, ~65k frames deep, until the stack was gone and the process took `SIGABRT`. Packet reordering
  is ordinary on any internet path, so this needed no attacker — just a normal network.

  `pop` is now a bounded loop rather than recursion (so a pathological gap costs iterations, not the
  stack), and the root cause is fixed separately: packets older than `next_seq` are dropped up front,
  since their playout moment has passed and they can never be returned. They are counted in
  `packets_dropped`. Three regression tests cover the single late packet, several late packets with a
  live stream continuing behind them, and a wide forward gap being skipped promptly.

  **No released consumer was affected**: `JitterBuffer` is exported but nothing in forge-media or
  siphon-ai used it — the bug was found by siphon-ai's WebRTC leg (`DEV_PLAN_WebRTC.md` Phase 2), which
  is its first consumer and reuses it rather than writing a fourth reorder queue.

## [2026-08-24] — workspace release

**The first tagged release** (`v2026.08.24`), cut under the conventions in
`RELEASING.md` (adopted from siphon-rs): CalVer tags name repository
snapshots, SemVer lives per-crate. This section accumulates everything since
the last dated sections below (2025-12) — the SRTP/STUN correctness arc that
made forge interoperate with real carriers and browsers, the metrics
overhaul, neural VAD, the endpoint-shaped WebRTC `PeerConnection` with TURN
and G.711, and the RTP port-pool reservation band.

**Embeds siphon-rs `v2026.08.24`** (`external/siphon-rs` submodule).

**Crate versions:** forge-api 0.4.0 · forge-ai-stream 0.2.0 ·
bcg729-sys 0.1.0 · forge-codecs 0.2.0 · forge-conference 0.3.1 ·
forge-core 0.2.0 · forge-dtmf 0.2.1 · forge-engine 0.5.0 · forge-ha 0.2.0 ·
forge-hep 0.0.1 · **forge-ice 0.3.0** · forge-injection 0.1.1 ·
forge-kernel 0.2.0 · forge-kernel-ebpf 0.2.0 · forge-mixer 0.2.0 ·
forge-recorder 0.2.0 · forge-resampler 0.1.1 · **forge-rtp 0.3.0** ·
forge-sdp 0.2.0 · forge-siprec 0.2.1 · forge-storage 0.2.0 ·
forge-transcoder 0.2.0 · forge-transcription 0.2.0 · forge-vad 0.2.0 ·
**forge-webrtc 0.4.0** · forge-media (standalone server) 0.2.0.
Bold = bumped at this release cut by audit; the rest are as stamped by the
PRs that landed their work (baseline release — the strict per-crate audit
discipline applies from the next release).

**Breaking changes:**

- **forge-webrtc 0.4.0** ([#130](https://github.com/thevoiceguy/forge-media/pull/130)):
  `PeerConfig::opus_pt` → `PeerConfig::codecs` (preference-ordered
  `Vec<(AudioCodec, u8)>`); `negotiated_opus_pt()` →
  `negotiated_codec()`. Also from earlier in this release's arc
  ([#116](https://github.com/thevoiceguy/forge-media/pull/116)): the
  `PeerConnection` API was rebuilt endpoint-shaped (JSEP-style
  offer/answer both roles, events, `AudioSender`).
- **forge-rtp 0.3.0**: `SenderReport::parse` / `ReceiverReport::parse` now
  take an explicit `report_count: u8` (the compound-RTCP fix below); no
  callers outside `RtcpPacket::parse` were known.
- **forge-ice 0.3.0**: additive TURN/STUN API, but `StunMessage` is now
  constructed only through its constructors (raw bytes are private).
- **forge-engine 0.5.0 / forge-vad 0.2.0** (stamped when the work landed):
  engine-level `VadConfig.detector` → `VadConfig.engine`
  (`forge_vad::VadEngineConfig`), and `MediaSession::vad_detector()` returns
  `Arc<Mutex<AnyVadDetector>>` (see the neural-VAD entry below).

### Added

- **`forge-webrtc` negotiates G.711 (PCMU/PCMA) alongside Opus.** G.711 is mandatory-to-implement in WebRTC (RFC 7874 §3), so every browser accepts it — a bridge terminating a G.711 SIP leg can now prefer it on the browser leg and skip transcoding entirely (filed from siphon-ai's `DEV_PLAN_WebRTC.md` §1, which ships Opus-transcode-first and named this the upstream optimization). `PeerConfig::codecs` replaces `PeerConfig::opus_pt`: a preference-ordered `Vec<(AudioCodec, u8)>` (default `[(Opus, 111), (PCMU, 0), (PCMA, 8)]` — Opus stays first, so existing behaviour against a browser is unchanged). Offers list every configured codec; an answer accepts exactly **one** — the first local preference the remote offered, at the remote's payload type — so the negotiated codec is pinned deterministically, and `PeerConnection::negotiated_codec()` / `AudioSender::codec()` (replacing `negotiated_opus_pt()`) report it from both directions of the handshake. Static payload types 0/8 are recognised with no `a=rtpmap` line (RFC 3551 §6), the shape SIP-gateway offers often have.

  Two telephone-event fixes ride along, both consequences of codecs no longer all being 48 kHz: answers now mirror the remote's telephone-event payload type **clock-matched to the selected codec** (RFC 4733 §2.1 — Chrome's `126 telephone-event/8000` for a G.711 answer, `110/48000` for Opus; previously the 48 kHz one was always picked and re-declared at 8 kHz), and offers clock telephone-event at the preferred codec's rate instead of hardcoding 8 kHz. An offer that omits Opus but carries G.711 — previously a `NoCommonCodec` rejection — now answers G.711.

### Changed

- **`forge-vad` depends on `forge-core`, and its two `ForgeEvent::Speech*` references are live intra-doc links again.** [#122](https://github.com/thevoiceguy/forge-media/pull/122) made them plain code spans because the dependency was absent; this adds it and restores the links. The dependency is types-only — the crate still never constructs or publishes an event, `VadDetector::process` returns a `(VadState, f32)` and `forge-engine`'s forwarding loop is what maps transitions onto `ForgeEvent::SpeechStarted` / `SpeechStopped`.

  It costs nothing in practice: every consumer of `forge-vad` (`forge-engine`, `forge-ai-stream`, and downstream `siphon-ai`) already depends on `forge-core`, so **`Cargo.lock` moves by one line and no new crate enters the graph**. The property the [2026-05 extraction](https://github.com/thevoiceguy/forge-media/commit/d36df52) was protecting is intact either way — that was about not dragging `forge-ai-stream`'s OpenAI / Anthropic / Deepgram / ElevenLabs WebSocket clients into provider-neutral consumers, which `forge-core` does not do. The crate docs' "zero external runtime dependencies" line, which described the detector's hot-path behaviour rather than the manifest, is reworded to say what it actually meant: synchronous, one fixed-size allocation, nothing reached on the detection path.

### Fixed

- **CI's "Check for broken links" step never checked anything, and eleven unresolved intra-doc links had accumulated behind it.** The step ran `cargo doc … | grep -i "warning.*broken" && exit 1 || exit 0`, but rustdoc's wording is `unresolved link to \`Foo\`` — "broken" appears only in the lint's *name*, which is not printed on the warning line. The grep therefore never matched, `|| exit 0` swallowed the result, and the job passed unconditionally. It now denies the lint by name (`RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links"`), which is what the step was always trying to express and reports against the offending line instead of re-deriving it from stderr. The step is also scoped to workspace-owned packages the way the fmt/clippy/test jobs already are, since `--workspace` documents the `external/siphon-rs` submodule and its doc warnings are governed separately.

  The eleven links this uncovered are fixed: associated-item and method links that needed a `Self::` / type qualifier (`RecordingSession::MAX_METADATA_BYTES`, `SessionRecordingClient`'s `forward_rtp` / `mute_participant` / `unmute_participant`, `FileSource`'s `open_in_sandbox` ×2, `MediaSession::enable_dtls` ×2), and `crate::ForwardingEngine` from a module-level doc in another file. *(Backfilled entries below this line were added at the release cut.)*

### Added

- **`forge-ice`: TURN client (RFC 8656)** — long-term-credential allocation, permissions, and relay candidates for un-punchable NATs ([#119](https://github.com/thevoiceguy/forge-media/pull/119)); **`forge-webrtc` carries media over TURN relay candidates end to end** ([#121](https://github.com/thevoiceguy/forge-media/pull/121)), with `TurnServer` config on the transport and a live-server integration test (`tests/turn_relay.rs`, gated on `FORGE_TURN_URI/USER/PASS`).

### Dependencies

- siphon-rs submodule: `v2026.08.19` → `v2026.08.24` ([#115](https://github.com/thevoiceguy/forge-media/pull/115), [#129](https://github.com/thevoiceguy/forge-media/pull/129)). Routine bumps: `redis` 0.24 → 1.2 ([#107](https://github.com/thevoiceguy/forge-media/pull/107)), `tokio-tungstenite` 0.21 → 0.29, `validator` 0.20 → 0.21 ([#110](https://github.com/thevoiceguy/forge-media/pull/110)), `h2` 0.4.16 + dropped forge-ai-stream's unused `reqwest` 0.11 for RUSTSEC-2026-0258 ([#114](https://github.com/thevoiceguy/forge-media/pull/114)), and a patch-updates group ([#113](https://github.com/thevoiceguy/forge-media/pull/113)). The two `forge_core::ForgeEvent::Speech*` links in `forge-vad` become plain code spans instead. Adding the dependency would work — `forge-core` has no `forge-*` dependencies, so there is no cycle and no AI provider comes with it — but the crate was carved out of `forge-ai-stream` precisely so provider-neutral consumers could use VAD without inheriting a dependency tree, with "zero non-`thiserror` dependencies" called out as a property of the extraction. It also never publishes events: `VadDetector::process` returns `(VadState, f32)` and `forge-engine`'s forwarding loop is what maps transitions onto `ForgeEvent::SpeechStarted` / `SpeechStopped`, so `forge-core`'s types never appear in its API and the sentence is describing a consumer's behaviour rather than its own. (`forge-dtmf` does depend on `forge-core`, because it constructs the event itself via `to_forge_event` — the split tracks who builds the event.)

### Added

- **`PortPool::allocate_reserving(min_free)` and `MediaSessionConfig::min_free_port_pairs`: a reserved band in the RTP port pool** (downstream: [thevoiceguy/siphon-ai#556](https://github.com/thevoiceguy/siphon-ai/issues/556)). A media server that both answers and originates calls draws both directions from one pool, first-come-first-served. A surge in one direction can therefore starve the other *completely*, and the direction that wins shows no symptom at all while it happens — from its point of view nothing failed. Measured downstream on a pool shrunk to 60 calls with 50 inbound + 20 outbound offered: inbound established 50/50 and stayed healthy for the whole window while 10 of 20 originations failed. The starved side is typically the one with a deadline attached (a scheduled callback, a notification), which is the opposite of how an operator would prioritise it.

  `allocate_reserving(min_free)` allocates only if at least `min_free` pairs would remain available afterwards; `allocate()` is now exactly `allocate_reserving(0)` and is unchanged for every existing caller, including taking the pool's last pair. Sessions reach it through the new `MediaSessionConfig::min_free_port_pairs` (default `0`), which sits beside `socket_config` because the floor is decided per call and not per manager — the same `SessionManager` gates one direction and not the other.

  This belongs in the pool rather than in each caller because a caller's own `available_count()`-then-`allocate()` is two critical sections: `K` concurrent callers can each read the same free count and dip up to `K-1` pairs below the floor before any of them lands. Evaluating the floor under the lock that removes the port makes it exact for the same single `Mutex` acquisition — pinned by a test that races 64 gated callers at a 50-pair pool and demands *exactly* the floor left standing.

  The floor composes with the [#111](https://github.com/thevoiceguy/forge-media/issues/111) bind-retry: pairs rejected for `AddrInUse` are held rather than released, so mid-retry the pool reads up to four pairs lower than it will settle at, and `allocate_and_bind` discounts them before comparing. Without that, one squatted port would turn an allocation that leaves the floor perfectly intact into a spurious refusal. An exhausted pool and a pool at its floor are the same `ForgeError::ResourceLimit` variant — both mean "no port for *you*, right now" — but carry different messages, because one says buy more ports and the other says the reservation is working.

### Dependencies

- `sha1` 0.10 → 0.11, `hmac` 0.12 → 0.13, `sha2` 0.10 → 0.11 — the coordinated digest-0.11 ecosystem move ([#90](https://github.com/thevoiceguy/forge-media/pull/90) could not land alone: sha1 0.11 and hmac 0.12 sit on incompatible `digest` majors, so `Hmac<Sha1>` in forge-rtp SRTP auth and forge-ice STUN MESSAGE-INTEGRITY failed to compile). `new_from_slice` moved from the `Mac` trait to `KeyInit`; call sites updated, no behavioural change (all SRTP/STUN test vectors pass unchanged). forge-ice's privately-pinned `hmac`/`sha1` now inherit from the workspace so the two halves of `Hmac<Sha1>` can't drift across majors again.

### Fixed

- **`forge_rtcp_sender_{packets,bytes}_total` now count what peers actually sent, not a quadratically growing sum of running totals** ([#103](https://github.com/thevoiceguy/forge-media/issues/103)). The RTCP receive path incremented these counters by each Sender Report's `sender_packet_count` / `sender_octet_count` — cumulative totals since the sender began transmitting (RFC 3550 §6.4.1) — so every SR re-added everything the previous SRs had already added (a 50 pps stream reporting every 5 s reads 19 500 after 60 s against a wire truth of 3 000, and diverges from there). A new per-session `SrCounterTracker` remembers the last cumulative pair per sender SSRC and increments by the delta, making the counters wire-truth and `rate()` over them physical. A packet count below the previous report reads as a sender restart and re-baselines (the new totals count in full); outside a restart the octet delta uses wrapping subtraction, so the u32 octet counter's legitimate mid-stream wrap (§6.4.1, ~4.3 GB) reads as the small forward step it is rather than a restart. Per-SSRC baselines also make the cross-sender aggregation meaningful — two concurrent senders no longer read each other's counts as restarts, the same independent-sequence-space lesson as the [#94](https://github.com/thevoiceguy/forge-media/pull/94) receive-stats fix. The describe strings and `docs/METRICS.md` rows from [#102](https://github.com/thevoiceguy/forge-media/pull/102), which documented the old behavior honestly ("running sum of cumulative counts; not a wire count"), now describe the fixed semantics.

- **Every `metrics`-facade family now has a description, so `# HELP` lines reach every consumer's exporter** ([#101](https://github.com/thevoiceguy/forge-media/issues/101)). All 79 `counter!`/`gauge!`/`histogram!` families (forge-rtp 8, forge-engine 48, forge-conference 13, forge-api 10) were emitted with no `describe_*!` registration anywhere in the tree, so they rendered as a bare `# TYPE` with no help text through the standalone server and every embedding consumer alike — found live on a SiphonAI 0.48.5 box whose own metrics all carry HELP. Each emitting crate now has a `src/metrics.rs` with name consts, `describe_*!` registrations, and `ALL_COUNTERS`/`ALL_GAUGES`/`ALL_HISTOGRAMS` lists; `SessionManager::new*`, `ConferenceBridge::new`, and the standalone server's `MetricsHandle::init` call the (idempotent, post-recorder) `describe_metrics()` so both deployment shapes get descriptions without new API calls. Coverage is self-detecting in both directions: per-crate self-scan tests walk the crate's sources and fail if an emission site and the lists disagree (including a name emitted under the wrong type, or a non-string-literal name that grep and the scanner couldn't see), and a workspace-wide sweep in forge-api fails if any facade emission in any crate under `crates/` is missing from the describe lists — so a brand-new emitting crate cannot ship undescribed metrics.

  The five facade histograms (`forge_vad_neural_inference_seconds`, `forge_transcoding_duration_seconds`, `forge_conference_mixing_duration_seconds`, `forge_webrtc_connection_establishment_duration_seconds`, `forge_sdp_negotiation_duration_seconds`) also gained suggested-bucket consts next to their names, and the standalone server registers them as `Matcher::Full` overrides — `forge_vad_neural_inference_seconds` previously matched no bucket rule at all (the generic rule matches the suffix `duration_seconds`) and rendered as a summary, which cannot be aggregated across instances; the other three traded the generic 1 ms–10 s latency buckets for scales that resolve their actual per-frame budgets. Embedding consumers get the same buckets by registering the exported consts; `docs/METRICS.md` (new) shows how, and is the full inventory downstream docs can point at.

  Six metrics are renamed to gain the `forge_` prefix they were missing: `webrtc_connections_created_total`, `webrtc_connections_deleted_total`, `sdp_negotiation_total`, `sdp_negotiation_failures_total`, `sdp_codecs_negotiated_total`, and `sdp_negotiation_duration_seconds` (all forge-api). The missing prefix is also why they evaded the issue's original inventory — the audit grepped for `"forge_` names — and the workspace sweep now enforces the prefix, so a stray name fails the build instead of hiding. Any dashboard reading the old names needs the one-line rename.

- **`forge-engine`: receive-side stream stats are now per-SSRC, so a mid-call stream change stops fabricating packet loss** (downstream: [thevoiceguy/siphon-ai#330](https://github.com/thevoiceguy/siphon-ai/issues/330)). `RxStreamStats` carried a single sequence baseline with no SSRC field at all, comparing sequence numbers across sources that RFC 3550 §8 gives independent, randomly-initialised sequence spaces. `record` now takes the packet's SSRC and re-baselines when it changes, folding the finished stream's real loss into a carry so per-call totals (`packets_received`, reorder, duplicate, and cumulative loss) stay honest across the switch. The first-packet branch also had to change from `packets_received = 1` to `+= 1`, since it now runs for the first packet of *every* stream rather than only the call's first.

  The field symptom this fixes: an outbound call over a Twilio trunk reported `rx_packets_lost` exactly equal to the ring duration in packets (`ring_seconds × 50` — 621 on a 12.42 s ring), frozen at that value for the rest of the call, which pinned the transport MOS estimate to **1.0, the floor of the scale**, for every call with a normal ring time. The effect was inverted from reality — the longer a call rang, the worse its reported quality — and it propagated into the CDR `quality` block and Homer's HEP QoS, contaminating historical quality data and any MOS-based alerting at the source.

  A production capture of this trunk shows it changing SSRC mid-call — 4 SSRCs on one 5-tuple — which is what makes the sequence spaces incomparable. The phantom loss is a **sequence discontinuity at the stream change**, not uncounted packets: early-media RTP is received *and counted* (verified against a live daemon — a call with ringback and no post-answer media reports the ringback packets in `rx_packets_received`, with zero latch rejections and zero SRTP unprotect failures). An answered stream whose start sequence does not continue the ringback stream's — a media server deriving it from a clock running since call setup — lands `ring_seconds × 50` ahead, and the SSRC-blind baseline absorbs that jump into the expected-packet span. The regression test models it at the capture's real sizes (749 ringback packets, 1812 answered) and reproduces `749` lost with the re-baseline disabled, `0` with it.

  The exact discontinuity on the failing call is a hypothesis, labelled as such: its packets were never captured, and the capture in hand shows *continuous* sequence across its SSRC changes, so that particular call was healthy. What is established is that comparing sequence numbers across independent sequence spaces is wrong however the discontinuity arises, and re-baselining handles all of them. Six new tests cover the forward-jump (phantom loss) and backward-landing (phantom reorder/duplicate) stream changes, loss carried across a switch, single-SSRC behaviour being byte-identical, and the field-sized replay above.

- **`forge-engine`: `ParticipantStats::bytes_sent` now counts RTP payload octets on every send path**. The counter had two different units depending on which path produced the packet: the bridging path recorded the *received* payload length (so a transcoded leg was billed the source codec's frame size, not the one actually sent — G.722 at 160 B/frame reported as G.729's 10 B, or vice versa), while the generated-audio path (`send_generated_rtp_packet`, used for AI audio, playout, and injected RFC 2833 DTMF) recorded the full SRTP-protected wire length, header and auth tag included. Both now record the payload octets actually transmitted, matching `bytes_received`, matching the `sender_octet_count` the SR path already reports (RFC 3550 §6.4.1), and giving the new `MediaStatsSnapshot::tx_octets_sent` a single well-defined meaning. Surfaced while wiring [#92](https://github.com/thevoiceguy/forge-media/issues/92) — publishing the counter on the event bus would have shipped the ambiguity to embedders. The `forge_rtp_bytes_sent_total` metric picks up the same correction on the transcoding path; `forge_generated_*_bytes_sent_total` still track wire bytes, which is what those byte-rate metrics want. Any consumer of `MediaSessionStats`/`ParticipantStats::bytes_sent` that was reading generated-audio byte counts will see them drop by the per-packet header + SRTP overhead (12 B + tag), which is the correct figure.

- **`forge-rtp`: AES-CM SRTP/SRTCP IV byte offsets now match RFC 3711 §4.1.1 / §4.1.2**. The four `protect_aes_cm` / `unprotect_aes_cm` / `protect_rtcp_aes_cm` / `unprotect_rtcp_aes_cm` IV-construction sites XOR'd the packet index into the wrong bytes of the 128-bit IV — 48-bit RTP packet index landed at `iv[6..12]` instead of `iv[8..14]`, and 32-bit SRTCP index at `iv[8..12]` instead of `iv[10..14]`. The bug stems directly from how `(i * 2^16)` reads in RFC 3711 §4.1.1: shifting a 48-bit value left by 16 bits in a 128-bit field places it at bits 16..63, i.e. bytes 8..13 (MSB at byte 8) — not bytes 6..11. Every existing protect/unprotect round-trip test passed because both sides used the same wrong offsets and the AES-CTR keystream cancelled out, so the bug stayed invisible until a spec-correct peer (Twilio Secure Trunking) was on the wire: our outbound was unrecoverable garbage to them (caller heard white noise instead of the bot's greeting) and their inbound was unrecoverable garbage to us (STT received bytes that decoded as PCMU but no recognisable speech, so the bot never produced a turn). Two new tests (`test_srtp_aes_cm_iv_matches_rfc3711_spec`, `test_srtcp_aes_cm_iv_matches_rfc3711_spec`) compute the IV independently via `u128` arithmetic that mirrors the spec's algebraic formula `(k_s * 2^16) XOR (SSRC * 2^64) XOR (i * 2^16)`, then verify the first AES-CTR keystream block from `protect_aes_cm` / `protect_rtcp_aes_cm` matches byte-for-byte — breaking the symmetry trap that the existing round-trip tests fall into. DTLS-SRTP also goes through this code path; existing DTLS callers were silently affected the same way against any spec-correct peer (the 0.3.0 DTLS-SRTP test coverage was self-paired and didn't surface it).

- **`forge-rtp`: SRTCP key derivation now uses the spec-correct KDF labels (RFC 3711 §4.3.3)**. `derive_session_keys` always derived with the *SRTP* labels (`0x00` / `0x01` / `0x02`) regardless of which protocol was calling it. SRTCP requires labels `0x03` / `0x04` / `0x05` per §4.3.3 "List of Reserved Labels" — so every SRTCP packet from a spec-correct peer (Twilio, FreeSWITCH, every WebRTC stack) was discarded with "SRTCP authentication failed" because the peer's auth tag was computed against label `0x04` and ours against label `0x01`. Surfaced immediately in production once SDES SRTP shipped on the siphon-ai side and real carrier RTCP started arriving (DTLS-SRTP 0.3.0 test coverage was hand-driven and audio-focused; SRTCP wasn't exercised end-to-end). The function is now split into `derive_srtp_session_keys` and `derive_srtcp_session_keys`, both delegating to a private `derive_session_keys_with_labels` that takes the three labels as parameters. The four production call sites (`protect_rtp`, `unprotect_rtp`, `protect_rtcp`, `unprotect_rtcp`) point at the right variant. A new regression test pins that the two key sets are distinct — if a future refactor collapses them back, the test fails loudly with the wrong key bytes. SRTP path unchanged.

### Added

- **`forge-webrtc` 0.3.0: an endpoint-shaped `PeerConnection` — answerer role, trickle ICE, single-leg SRTP media, renegotiation** (DSIP WebRTC Media Binding `transport:webrtc` 1.0, round one). The previous `PeerConnection` could only offer, gathered every candidate before producing SDP, ran connectivity checks as one blocking call, drove DTLS in a task that then stopped reading the socket, and exported SRTP keys only to log them — no RTP ever flowed. It is now a running endpoint: `create_offer` / `set_remote_offer` / `create_answer` / `set_remote_answer` (JSEP-shaped, both roles), `add_ice_candidate` for trickled candidates with local candidates and connection progress delivered as `PeerEvent`s, `sender().send_audio(frame, samples)` for Opus frames over DTLS-SRTP with keys installed straight from the DTLS export (no engine session), re-offers on the same transport (`create_offer` again after `Connected`, `rollback_local_offer` when the peer rejects) and `Direction` control for `recvonly` screening answers that later escalate. The DTLS role follows `a=setup` (offer `actpass`, answer `active`), and the peer's certificate is verified against the fingerprint in its signalled SDP before any key is exported. Answers mirror the remote offer — payload types, `a=mid`, protocol — and reject non-audio sections with port 0, so a Chrome offer carrying video or data sections gets a valid RFC 3264 answer. ICE restart is refused by design (`WebRtcError::IceRestartUnsupported` when a remote description changes the ICE credentials) rather than half-implemented. A new loopback integration test drives two peer connections through offer/answer, trickle in both directions, DTLS in both roles, SRTP both ways, a screening `recvonly` answer escalated by re-offer, a rejected re-offer rolled back, and a refused ICE restart. `set_remote_answer` no longer blocks until ICE completes; `wait_connected(timeout)` or the events give the same signal.

  Internals: one task owns the UDP socket and demultiplexes STUN / DTLS / SRTP by first byte (RFC 7983); connectivity checks, nomination (regular nomination with USE-CANDIDATE), triggered checks, peer-reflexive learning and consent keepalives all run on that socket; server-reflexive gathering sends its Binding Requests from the same socket so the mapped address is the one media will use. `forge-ice` gains the `USE-CANDIDATE` STUN attribute, `StunMessage::{add_use_candidate,has_use_candidate,get_priority,get_username}`, and `IceAgent::{form_candidate_pairs_incremental,candidate_pairs_mut,is_controlling,tie_breaker,get_remote_credentials}`; `forge-rtp` gains `DtlsConnection::handle_timeout`, which drives OpenSSL's DTLS retransmission timer (`DTLS_CTRL_HANDLE_TIMEOUT`) for the memory-BIO handshake so a lost flight is resent instead of stalling. `forge-api`'s WebRTC routes compile unchanged.

- **TX packet/octet counters and RR cumulative-loss on the event bus** ([#92](https://github.com/thevoiceguy/forge-media/issues/92)). Two per-call quality numbers forge already computed internally never reached `ForgeEvent` consumers, so an embedder could report what a call *received* but never what it *sent*. `ForgeEvent::MediaStatsSnapshot` gains `tx_packets_sent: u64` and `tx_octets_sent: u64` — cumulative-since-call-start like the existing `rx_*` fields, sourced from `ParticipantStats` and covering both bridged packets and forge-generated ones (AI audio, playout, injected DTMF); the event is already per-leg, so attribution is unambiguous. `ForgeEvent::RtcpReportReceived` gains `cumulative_lost: i32` and `extended_highest_seq: u32`, passed straight through from the reception-report block: `fraction_lost` (and thus the existing `packet_loss_ratio`) is an *interval* measure, so averaging it across a call yields a mean of interval fractions that won't reconcile against a carrier's cumulative figure — `cumulative_lost` is the whole-stream total and `extended_highest_seq` supplies the expected-packet denominator (RFC 3550 §A.3). `cumulative_lost` stays signed because duplicates can legitimately push a remote's packets-received past packets-expected. Together these let an embedder finally state the thing operators ask for after a bad call: "we transmitted 1,914 packets; the far end reported 12 lost." `MediaStatsSnapshot` also now publishes for a leg that has carried RTP in *either* direction — previously a leg with no receive-side packets was skipped entirely, which silenced the snapshot on exactly the send-only leg where the new TX counters matter most. Additive struct-variant fields; consumers matching with `..` (siphon-ai's tap does) are unaffected. Unblocks [thevoiceguy/siphon-ai#320](https://github.com/thevoiceguy/siphon-ai/issues/320).

- **Neural (Silero) VAD backend** (`forge-vad` 0.2.0, `forge-engine` 0.5.0). `forge-vad` gains a second detection backend behind the new `VadEngineConfig` / `AnyVadDetector` enum dispatch: `NeuralVadDetector` runs Silero VAD v6.2.1 via the pure-Rust `tract-onnx` runtime (static-musl-friendly — no C++ ONNX runtime), embedded as two per-sample-rate ONNX graphs specialized from the upstream "ifless" export (same weights, control flow removed; provenance + SHA-256s in `crates/forge-vad/models/README.md`). Same `process`/`state`/`reset` surface and the same `SpeechStarted`/`SpeechStopped` event contract; hysteresis mirrors the energy detector's `min_speech_duration_ms`/`min_silence_duration_ms` semantics with dual entry/exit probability thresholds (0.5/0.35 defaults). All off by default behind Cargo features (`forge-vad/neural`, `forge-engine/neural-vad`, root `neural-vad`) — default builds carry no ML runtime and behave identically. **Breaking (contained)**: engine-level `VadConfig.detector: forge_vad::VadConfig` is now `VadConfig.engine: forge_vad::VadEngineConfig` (default unchanged: energy+ZCR; siphon-ai constructs no VadConfig today), and `MediaSession::vad_detector()` now returns `Arc<Mutex<AnyVadDetector>>`. The forwarding loop passes the decoded stream's true PCM rate to VAD: a neural detector configured at the wrong rate is rebuilt once at the stream rate (`warn!`), and an unsupported rate (no 48 kHz model, no resampling in v1) disables VAD for that session loudly instead of scoring wrong-rate audio. New metrics: `forge_vad_windows_total{backend}`, `forge_vad_neural_inference_seconds`, `forge_vad_errors_total{backend}`. New `benches/vad_bench.rs` (~60–81 µs per 32 ms window vs the ≤1.5 ms budget; energy path ~200 ns). Feature-enabled builds need rustc ≥ 1.91 (tract 0.23). Docs: `docs/NEURAL_VAD.md`. Driving consumer: siphon-ai barge-in false-positive reduction (its ROADMAP P2 upstream-gated item); siphon-ai integration is Phase 2 in `NEURAL_VAD_PLAN.md` §5.4.

- **`forge-engine::srtp_install::install_srtp_keys`** — exchange-agnostic SRTP key installer. Takes a pre-derived `SrtpKeyMaterial` pair (local + remote) and installs them into an existing `Arc<Mutex<SrtpContext>>`. Same wire behaviour as the existing `dtls_srtp::install_keys` helper, but lives outside the `dtls` feature gate so the SDES path (which doesn't need OpenSSL) can call it without pulling DTLS in. The DTLS install path now delegates to this helper, so existing callers see no behaviour change. Required for [thevoiceguy/siphon-ai's 0.3.1 SDES outbound wiring](https://github.com/thevoiceguy/siphon-ai) — `forge_sdp::sdes::answer_sdes()` already returns key material in the shape this helper expects, so the siphon-ai side is now pure plumbing.

### Changed

- **`forge-rtp`: DTLS certificates are now ECDSA P-256 instead of RSA-2048.** Browsers and webrtc-rs present P-256 certificates, `DtlsContext` already offers `ECDHE-ECDSA-*` first ([#117](https://github.com/thevoiceguy/forge-media/pull/117)), and P-256 key generation is on the order of a millisecond where RSA-2048 is tens to hundreds — which an endpoint pays per peer connection. The SDP `a=fingerprint` stays SHA-256 over the DER certificate; nothing on the wire changes shape. Verified against webrtc-rs in both DTLS roles (DSIP cross-backend test, 4/4).

### Fixed

- **`forge-ice`: STUN MESSAGE-INTEGRITY and FINGERPRINT were computed over the wrong bytes, so no other STUN implementation could verify forge's checks — or be verified by forge.** RFC 8489 §14.5 defines the HMAC input as the message *up to but not including* the MESSAGE-INTEGRITY attribute, with the header's length field adjusted to count the attribute; forge HMAC'd the zeroed attribute as well. §14.7 defines FINGERPRINT's CRC-32 the same way (length adjusted to include FINGERPRINT); forge left the length short. Both ends of a forge↔forge call shared the mistake, so it was invisible until forge faced webrtc-rs, where every connectivity check in both directions was dropped with "integrity check failed". The codec now builds both inputs per the RFC; a parsed message verifies over the bytes it was received in (attribute padding is arbitrary on the wire — RFC 5769's sample request pads USERNAME with spaces — so re-serialising cannot be trusted); and the RFC 5769 §2.1 test vector is checked end to end (MESSAGE-INTEGRITY with its password, FINGERPRINT, and a locally rebuilt copy). `StunMessage` gains `new_with_transaction` and `verify_fingerprint`; its raw bytes are a private field, so external code constructs messages through the constructors.

- **`forge-rtp`: the DTLS cipher list offered only `ECDHE-RSA-*` suites, which fails against every peer that presents an ECDSA certificate as DTLS server** — browsers and webrtc-rs do. The peer selected an RSA suite it could not honour and forge's OpenSSL client aborted in `tls_post_process_server_certificate` with "wrong certificate type". The list now carries the `ECDHE-ECDSA-*` GCM suites ahead of the RSA ones. (forge's own certificate is still RSA-2048; that is accepted by browsers as a server certificate and is unchanged here.)

- **`forge-ice`: `StunServer` checked the ICE USERNAME in the wrong order, so a forge endpoint dropped every connectivity check from another forge endpoint.** RFC 8445 §7.2.2 has the sender build `USERNAME` as `<peer's ufrag>:<own ufrag>`, which is what `checks.rs` does — so the receiver sees `<its own ufrag>:<peer's ufrag>`. `StunServer::handle_binding_request` required the opposite (`<remote>:<local>`), and its tests asserted the inverted form, so forge's STUN client and STUN server never agreed with each other; nothing noticed because checks had only ever been run against third-party responders. The server now validates `<local_ufrag>:<remote_ufrag>`, the tests assert the RFC order, and a new test pins that the inverted form is rejected.

- **`forge-ice`: connectivity checks no longer leave the media socket.** `perform_connectivity_check` bound a second `SO_REUSEPORT` socket on the candidate's address for each check, so the kernel could steer the peer's DTLS flights and SRTP to a socket that was about to be closed. The `forge-webrtc` transport sends its checks from the one media socket; the legacy helper is unchanged for callers that still use it.

- **`forge-rtp`: SR/RR parsers now honour the `RC` count from the RTCP header**. Previously `SenderReport::parse` / `ReceiverReport::parse` greedily consumed 24-byte chunks until the input buffer ran out, treating bytes that actually belonged to the next sub-packet of a *compound* RTCP packet (RFC 3550 §6.1 — SR + SDES is the standard, not the exception) as phantom reception report blocks. The wrong bytes landed in `jitter`, `cumulative_lost`, `extended_highest_seq`, `last_sr`, and `dlsr`, silently corrupting every downstream QoS metric and event for every real peer (Twilio, FreeSWITCH, Asterisk, every WebRTC browser). Observed impact in siphon-ai: `siphon_ai_rtp_jitter_ms` averaged ~113,000,000 ms per RR — the formula `(block.jitter as f32) / 8000.0 * 1000.0` decoded ASCII CNAME bytes from the trailing SDES (e.g. `b"sipp"` → 0x73697070) and produced ~242M ms per "block." `SenderReport::parse` and `ReceiverReport::parse` now take an explicit `report_count: u8` argument (sourced from the RTCP common header's `RC` field by `RtcpPacket::parse`) and stop after exactly that many blocks, returning an error if the buffer is too short for the declared count instead of silently truncating. Five regression tests cover the compound SR+SDES (RC=0 and RC=N) and RR+SDES paths plus the malformed-too-short case. (No downstream callers outside `RtcpPacket::parse` were affected.)

## [0.2.0] - 2025-12-16 - Codec Enhancement

### Added - Comprehensive Codec Support

#### forge-codecs v0.2.0
- **G.729 Codec Implementation** - Production-ready via bcg729 FFI
  - Created `bcg729-sys` crate with raw FFI bindings to libbcg729
  - Safe Rust wrappers (`G729Encoder`, `G729Decoder`) with proper Drop implementations
  - Support for G.729 Annex A (8 kbit/s standard)
  - Support for G.729 Annex B (VAD/DTX for bandwidth savings)
  - **Length-prefixed framing** for variable-length VAD frames
    - Format: `[len:u8][data:len bytes]...`
    - Handles 0-byte (untransmitted), 2-byte (SID), and 10-byte (speech) frames unambiguously
  - **Packet Loss Concealment (PLC)** API
    - `encode_frame_unframed()` - Raw frame encoding for RTP
    - `decode_frame_with_plc(&data, is_erasure)` - Decoding with erasure flag
    - Exposes bcg729's native PLC for graceful degradation
  - **Proper error handling**
    - `G729Codec::new()` returns `Result` (was panicking)
    - Initialization errors propagate immediately
    - Added `CodecError::InitializationFailed` variant
  - **Corrected variant metadata**
    - Removed misleading `G729Variant::G729`
    - Only `G729A` and `G729B` (matches bcg729 implementation)
    - Fixed bit rates (both 8 kbps, not 11.8 kbps)
    - `max_frame_size()` always 10 bytes (was variable)
  - **11 comprehensive tests** all passing
    - Basic encode/decode, silence, multi-frame
    - Annex B VAD, reset, PLC
    - Sine wave, invalid frame size handling

- **G.722 Critical Fixes**
  - Fixed magnitude calculation (`saturating_abs` instead of XOR)
  - Added **auxiliary bit support** for 56k/48k modes
    - `encode_with_aux()` / `decode_with_aux()` APIs
    - Embeds/extracts data in LSBs of encoded frames
  - Removed duplicate G.722 stub from media-processor
  - All 10 tests passing with new aux-bit test

- **AudioCodecType Enhancement**
  - `native_format()` now returns `AudioCodecType::G729` (was `PCM`)
  - Fixes transcoder selection and SDP mapping

### Breaking Changes
- `G729Codec::new()` now returns `Result<G729Codec>` (was `G729Codec`)
- `G729Variant::G729` removed (use `G729A`)
- G.729 framed format adds 1-byte length prefix per frame
- No `Default` trait for `G729Codec` (construction can fail)
- `G729Variant::frame_size()` renamed to `max_frame_size()`

### Documentation
- Comprehensive codec comparison matrix in README
- Detailed codec feature descriptions (G.711, G.722, G.729, Opus)
- Feature flag usage examples
- G.729 requirements and installation instructions
- Links to integration guide (docs/CODEC_G729_GUIDE.md)

### Technical Details
- **Dependencies**: `bcg729-sys` with pkg-config build script
- **Feature flags**: `g729` (optional), `all-codecs` (includes all)
- **Requirements**: `libbcg729-dev` for G.729 support
- **Build**: `cargo build --features g729`

## [0.4.0] - 2025-12-16

### Added - Conference AI Integration

#### forge-conference-processor v0.4.0
- **AI as Virtual Conference Participant** - AI joins as first-class participant
  - `ConferenceAIManager` lifecycle management
  - Virtual participant ID `__ai__` in AudioMixer
  - Bidirectional audio routing (conference ↔ AI)
  - Three async tasks: audio routing, response polling, DTMF forwarding
  - Automatic sample rate conversion (48kHz conference ↔ 16kHz AI)
  - State management (Connecting, Active, Speaking, Terminated)

- **DTMF Forwarding** - Automatic DTMF event routing to AI
  - Event bus subscription for participant DTMF events
  - Filters for "End" events to avoid duplicates
  - Forwards as text: "[DTMF: User pressed '5' via RFC 2833]"
  - Enables IVR scenarios in conferences
  - Support for RFC 2833, Inband, SIP INFO detection

- **Audio Modes**
  - **Mixed Mode** (✅ Implemented) - AI hears combined audio from all participants
    - Single audio stream
    - Lower CPU usage (~1-2% per session)
    - Good for conversation, Q&A, facilitation
  - **Individual Mode** (⚠️ Not Yet Implemented) - Per-participant labeled streams
    - Requires mixer enhancement for per-participant buffer access
    - Better speaker identification
    - Required for accurate transcription with speaker attribution
    - Higher CPU (~2-4% per session)

- **Conference Room Methods**
  - `attach_ai()` - Attach AI manager with event bus
  - `detach_ai()` - Remove AI and cleanup tasks
  - `has_ai()` - Check if AI is attached
  - `ai_state()` - Get current AI state

#### forge-engine v0.4.0
- **DTMF Forwarding Support** in AISessionManager
  - `send_dtmf_event()` method for manual forwarding
  - Supports all detection methods (RFC 2833, Inband, SIP INFO)
  - Integration with OpenAI Realtime API
  - Formats as text message to AI

#### forge-api v0.4.0
- **Conference AI Endpoints**
  - `POST /v1/conferences/:room_id/ai` - Attach AI to conference
    - Request: api_key, model, voice, instructions, temperature, audio_mode
    - Returns: room_id, state, model, voice, audio_mode, participants_heard
    - Status: 201 Created, 404 Not Found, 409 Conflict
  - `GET /v1/conferences/:room_id/ai` - Get AI status
    - Returns current state and configuration
    - Status: 200 OK, 404 Not Found
  - `DELETE /v1/conferences/:room_id/ai` - Detach AI
    - Graceful cleanup of tasks and resources
    - Status: 204 No Content, 404 Not Found

- **AppState Enhancement**
  - Added `core_event_bus` field for media events (DTMF)
  - Separate from WebSocket event bus
  - Passed to conference AI manager on attachment

### Documentation
- **Conference AI Integration Guide** (docs/CONFERENCE_AI_INTEGRATION.md)
  - 572-line comprehensive guide
  - Quick start with curl examples
  - Architecture diagrams (audio flow, component stack)
  - Complete API reference for all 3 endpoints
  - Audio modes comparison (Mixed vs Individual)
  - DTMF integration with IVR example
  - Configuration options and constants
  - 4 real-world examples:
    - Meeting assistant
    - Language translation
    - Conference moderator
    - Dynamic attach/detach
  - Comprehensive troubleshooting guide
  - Best practices section

### Tests
- **Integration Tests** - 9 new tests in conference_ai_tests.rs
  - test_attach_ai_to_conference
  - test_attach_ai_already_attached_error
  - test_attach_ai_invalid_audio_mode
  - test_attach_ai_individual_mode_not_implemented
  - test_get_ai_status_not_attached
  - test_detach_ai_not_attached
  - test_attach_ai_missing_api_key
  - test_attach_ai_invalid_temperature
  - test_attach_ai_to_nonexistent_room

- **Test Coverage**
  - All 31 conference tests passing (22 existing + 9 new)
  - 4 unit tests in ai_manager.rs
  - Validation, error handling, status codes
  - Edge cases and state management

### Architecture
- **Event Bus Separation**
  - `crate::EventBus` - WebSocket conference state events
  - `forge_core::EventBus` - Media events (DTMF, audio)
  - Clear separation of concerns

- **Task Management**
  - Audio routing task: 20ms polling interval
  - AI response polling: 100ms interval
  - DTMF forwarding: Event-driven
  - Graceful task cleanup on detach

### Recording Integration
- AI audio automatically included in conference recordings
- AI is regular participant in mixer
- Room mix includes AI voice
- Can add AI metadata to recording info

### Use Cases
- Voice assistants in meetings
- Meeting moderation and facilitation
- Real-time translation
- IVR systems in conferences
- Meeting notes and summaries
- Q&A bots

### Changed
- forge-conference-processor: 0.3.0 → 0.4.0
- forge-engine: 0.2.0 → 0.4.0 (DTMF forwarding added)
- forge-api: 0.2.0 → 0.4.0

## [0.3.0] - 2025-12-16

### Added - Conference Features

#### forge-conference-processor v0.3.0
- **Audio Feedback System** - Play sound files at conference events
  - `AudioFeedbackPlayer` for loading and decoding WAV files
  - Support for 8, 16, 24, and 32-bit PCM WAV files
  - Automatic stereo-to-mono conversion
  - Sample rate resampling using linear interpolation
  - `ConferenceSounds` struct for pre-loaded conference sounds
  - Integration via virtual participant in mixer
  - Sounds: join, exit, alone, recording start/stop, PIN prompts, etc.

- **Capacity Management** - Control conference size and access
  - `max_channels` - Limit number of participants per room
  - Automatic capacity enforcement with `ConferenceFull` error
  - Per-room configuration overrides

- **Wait-for-Moderator** - Hold participants until host joins
  - `wait_for_moderator` flag in room configuration
  - Automatic waiting room management
  - Host tracking with `hosts` set
  - `WaitingForModerator` error for held participants
  - Automatic release when first host joins
  - Automatic hold when last host leaves

- **Meeting Requirements** - Enforce minimum participation
  - `min_users` - Minimum participants before meeting starts
  - `min_recording_participants` - Auto-start recording threshold
  - Automatic recording start when threshold reached

- **Conference Lock** - Control room access
  - `default_locked` - Lock conference by default
  - `is_locked` state management
  - `ConferenceLocked` error for denied entry

- **Room Configuration System** - Per-room customization
  - `RoomConfig` with optional overrides for all settings
  - `EffectiveRoomConfig` merging room + global defaults
  - Per-room PINs, capacity, DTMF, meeting requirements
  - Audio feedback sound paths (12 configurable sounds)

- **Helper Methods**
  - `is_host()`, `host_count()`, `waiting_count()`
  - `is_at_capacity()`, `meets_min_users_requirement()`
  - `get_effective_config()`, `waiting_participants()`
  - `promote_to_host()`

#### forge-api v0.3.0
- **Conference Configuration Endpoints**
  - `POST /v1/conferences/:room_id/configure` - Configure room settings
  - `GET /v1/conferences/:room_id/config` - Get room configuration

- **Participant Management Endpoints**
  - `GET /v1/conferences/:room_id/participants` - List with host status
  - `GET /v1/conferences/:room_id/waiting` - List waiting participants
  - `POST /v1/conferences/:room_id/participants/:id/promote` - Promote to host

- **Enhanced Participant Request**
  - `is_host` field in `AddParticipantRequest`
  - Direct host join support

### Configuration
- **conference.toml** - Comprehensive conference configuration file
  - Security settings (PINs, lockout, default locked state)
  - DTMF command bindings (participant and host commands)
  - Audio settings (sample rate, buffer size, VAD)
  - Recording settings (format, auto-record)
  - Capacity settings (max channels, wait for moderator)
  - Meeting requirements (min users, min recording participants)
  - Audio feedback (12 configurable sound file paths)
  - Extensive inline documentation (195 lines)

### Dependencies
- Added `hound = "3.5"` for WAV file decoding

### Tests
- 8 new tests for audio feedback system
- WAV loading, resampling, stereo conversion tests
- All 39 conference processor tests passing
- All API tests passing

## [Unreleased - AI Integration]

### Added - AI Integration

#### forge-ai-stream
- **OpenAI Realtime API Connector** - Full WebSocket-based integration
  - Bidirectional audio streaming (PCM16, G.711 µ-law/A-law)
  - Session configuration (model, voice, instructions, temperature)
  - Voice Activity Detection (VAD) / turn detection
  - Function calling support with JSON schemas
  - Event streaming (transcription, function calls, interruptions)
  - Connection statistics and monitoring
  - 12 comprehensive tests

#### forge-engine - AI Integration Module
- **AISessionManager** - Lifecycle management for AI sessions
  - Session creation, attachment, detachment
  - Audio routing to/from AI
  - Event bus integration for DTMF forwarding
  - Session statistics and status monitoring
  - 18 comprehensive tests

- **Audio Routing** - Bidirectional RTP ↔ AI audio flow
  - RTP → AI: Audio tap from forwarding loop (non-blocking)
  - AI → RTP: Response injection with codec conversion
  - Automatic sample rate conversion (8kHz/16kHz/24kHz)
  - Linear interpolation resampler
  - G.711 µ-law/A-law and Opus encoding support
  - Special AI SSRC (0xA1A1A1A1) for tracking
  - 10 audio routing tests

- **DTMF Integration** - Automatic DTMF forwarding to AI
  - EventBus subscription for DTMF events
  - RFC 2833, Inband, SIP INFO detection methods
  - Sent as text to AI: "[DTMF: User pressed '5' via rfc2833]"
  - Enables IVR scenarios without custom programming

#### forge-api - AI REST Endpoints
- **POST /v1/sessions/:id/ai** - Attach AI to session
- **GET /v1/sessions/:id/ai** - Get AI status and statistics
- **DELETE /v1/sessions/:id/ai** - Detach AI from session
- **POST /v1/sessions/:id/ai/function-response** - Send function results
- Complete request/response validation
- Error handling and status codes

#### forge-siprec - AI Recording Metadata
- **add_ai_metadata()** - Add AI provider/model/voice to recordings
- **add_ai_participant()** - Create virtual AI participant in SIPREC
- Extension data for compliance recording
- 6 new tests for AI metadata

#### forge-codecs
- Made G.711 encode functions public for AI audio encoding

### Documentation
- **AI Integration Guide** (docs/AI_INTEGRATION.md)
  - Complete API reference
  - Configuration guide
  - Audio routing architecture
  - DTMF integration examples
  - SIPREC recording with AI metadata
  - Troubleshooting guide
  - Performance considerations
  - Security best practices

- **Example Scripts** (examples/)
  - ai_integration_example.sh - Basic AI voice agent
  - ai_ivr_example.sh - IVR with DTMF
  - ai_function_calling_example.sh - Function calling demo
  - README.md - Complete examples guide

- **Updated README.md**
  - AI Integration section with quick start
  - API reference for AI endpoints
  - Link to comprehensive guide

### Test Coverage
- forge-ai-stream: 12 tests (OpenAI connector)
- forge-engine: 28 tests (18 AI + 10 audio routing)
- forge-siprec: 6 new AI metadata tests
- All 46 new tests passing

### Changed
- forge-ai-stream version: 0.1.0 → 0.2.0
- forge-engine version: 0.1.0 → 0.2.0
- forge-api version: 0.1.0 → 0.2.0
- forge-codecs version: 0.1.0 → 0.1.1

## [0.2.0] - 2025-12-15

### Added - forge-dtmf

#### Core Features
- **Digit Buffer with Timeouts** - `DtmfBuffer` for IVR digit collection
  - Configurable inter-digit timeout (default: 3 seconds)
  - Total collection timeout (default: 30 seconds)
  - Maximum digits limit
  - Terminator digit support (e.g., # to end input)
  - 8 comprehensive tests

- **DTMF Relay** - `DtmfRelay` for method conversion
  - Inband audio → RFC 2833 conversion
  - RFC 2833 → Tone generation instructions
  - Multi-digit state management
  - 5 comprehensive tests

- **Unified DTMF Processor** - `DtmfProcessor` high-level API
  - Single interface combining all detection methods
  - Automatic deduplication with priority handling
  - Optional digit buffering
  - Flexible configuration
  - 6 comprehensive tests

- **Integration Tests** - 5 end-to-end tests
  - Complete DTMF flow validation
  - Multi-method deduplication
  - Relay conversion testing
  - All 16 DTMF digits validation

#### Test Coverage
- Total: 47 tests passing (up from 22)
- All DTMF digits (0-9, *, #, A-D) validated
- RFC 2833 parsing and generation
- Goertzel inband detection
- Event deduplication with priority
- Digit buffering and timeouts
- Method conversion and relay

### Documentation
- Comprehensive inline documentation for all new modules
- Usage examples in module docs
- Integration test examples

### Changed
- forge-dtmf version bumped to 0.2.0
- Improved module organization with new exports

## [0.1.0] - 2025-12-15

### Added - forge-siprec

#### Phase 2: SIPREC Implementation (RFC 7865/7866)

- **SIP Message Builder** - Complete SIP signaling for SIPREC
  - INVITE, BYE request generation
  - SDP with multipart MIME (SDP + metadata XML)
  - Dialog state machine (Initial → Confirmed → Terminated)
  - Call-ID, tags, CSeq management

- **Metadata Generation** - RFC 7865 XML metadata
  - RecordingSession with participants and streams
  - Participant roles (caller, callee)
  - Media stream descriptions
  - RTP session information
  - Extension data support
  - XML serialization/deserialization

- **RTP Media Forking** - MediaForker for stream duplication
  - ForkedStream management
  - Packet forwarding to multiple destinations
  - Statistics tracking (packets, bytes, errors)
  - Stream lifecycle management

- **SRTP Key Management** - SrtpKeyManager for secure recording
  - SDP crypto attribute parsing (a=crypto)
  - Key material extraction (master key + salt)
  - Multiple crypto suite support (AES-CM-128, AEAD-AES-128/256-GCM)
  - SSRC-based key lookup
  - Base64 encoding/decoding

- **SRC Implementation** - SessionRecordingClient
  - Full recording session lifecycle
  - Metadata generation with participants
  - SIP dialog management
  - SDP generation with media streams
  - SRTP key extraction and forwarding
  - Failover to backup SRS support

- **SRS Implementation** - SessionRecordingServer
  - Recording session acceptance
  - Length-prefixed RTP packet storage
  - Metadata XML persistence
  - Session statistics (packet/byte counts)
  - Concurrent file I/O with tokio
  - Session limit enforcement

- **End-to-End Tests** - 3 comprehensive integration tests
  - Full SRC→SRS recording flow
  - SRTP key extraction validation
  - Primary/backup SRS failover

#### Test Coverage
- 40 tests passing across all modules
- RFC 7865/7866 compliance validated
- All SIPREC components tested

### Added - forge-dtmf (Initial)

- **RFC 2833** - Telephone-event RTP payload
  - Event parsing and generation
  - Rfc2833Detector, Rfc2833Generator
  - All 16 DTMF digits support

- **Inband Detection** - Goertzel algorithm
  - GoertzelDetector for frequency analysis
  - Configurable thresholds
  - 100ms minimum detection duration

- **Event Deduplication** - DtmfDeduplicator
  - Priority-based filtering
  - 100ms deduplication window

#### Test Coverage
- 22 tests passing for core functionality

[Unreleased]: https://github.com/forge-media/forge-media/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/forge-media/forge-media/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/forge-media/forge-media/releases/tag/v0.1.0
