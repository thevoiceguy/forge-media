# Fuzzing

`cargo-fuzz` targets for the parsers that read bytes this workspace did not
write: the five video depacketizers, the frame assembler, the RTCP parser and
the WebM reader. Each of those turns attacker-controlled input into structure,
and each is reached before any authentication — an RTP or RTCP packet is
whatever arrives on the port, and a recording on disk may be whatever a crashed
node left behind. A panic in any of them is a way to stop a media node.

| Target | What it reads |
|---|---|
| `depacketize_h264` | RFC 6184 payloads — single NAL, STAP-A, FU-A |
| `depacketize_h265` | RFC 7798 payloads — single NAL, AP, FU |
| `depacketize_vp8` | RFC 7741 payloads |
| `depacketize_vp9` | RFC 9628 payloads |
| `depacketize_av1` | AV1 RTP payload format — OBUs and their size fields |
| `frame_assembler` | A run of RTP packets: reordering, loss, frame boundaries |
| `rtcp_parse` | Compound RTCP — counted, length-prefixed sub-packets |
| `webm_read_summary` | Matroska/EBML, including truncated recordings |

## Running

Needs a nightly toolchain (libFuzzer and AddressSanitizer) and `cargo-fuzz`:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

Seed the corpora from the round-trip paths, then run a target:

```bash
FORGE_FUZZ_SEED_DIR="$PWD/fuzz/corpus" \
  cargo test -p forge-rtp -p forge-webm -p forge-bfcp --test fuzz_seeds

cargo fuzz run depacketize_h264                       # until you stop it
cargo fuzz run rtcp_parse -- -max_total_time=60       # for a minute
```

A crash is written to `fuzz/artifacts/<target>/`, and replaying it is

```bash
cargo fuzz run <target> fuzz/artifacts/<target>/<file>
```

## The corpora

`crates/forge-rtp/tests/fuzz_seeds.rs`, `crates/forge-webm/tests/fuzz_seeds.rs` and
`crates/forge-bfcp/tests/fuzz_seeds.rs`
build the seeds by *packetizing and encoding real frames* and writing what
comes out — the same round-trip the unit tests assert on. They are ordinary
stable-toolchain tests and write nothing unless `FORGE_FUZZ_SEED_DIR` is set,
so `cargo test` only checks that the corpora can still be built.

Deriving the corpus rather than committing one means it cannot rot: a change to
a payload format updates the seeds with it, and a seed that stops round-tripping
fails the test instead of quietly becoming a blob the fuzzer wastes its budget
on.

Targets that take a *sequence* of packets read their input as `u16`
big-endian length-prefixed records. `forge_media_fuzz::frames` reads that;
`framed` in the seed test writes it, and asserts the round-trip, because the
two halves live in crates that no single toolchain compiles.

## In CI

`.github/workflows/fuzz.yml`, weekly on Monday at 08:00 UTC, ten minutes a
target, plus `workflow_dispatch` for a longer run or a single target. The
corpus is cached between runs, so each week starts from what the last one found
rather than from the seeds.

The design (§15.5 of FCP's `docs/VIDEO_CONFERENCING.md`) first called for a
60-second run per target on every pull request. Weekly is the better trade at
any budget: a fuzzer's value is almost all in how long it runs, and a minute
per target on each PR mostly re-covers what the round-trip tests already
assert, at the cost of an AddressSanitizer build every time.
