# Releasing forge-media

This document records the release conventions for the forge-media workspace,
adopted from [siphon-rs's model](https://github.com/thevoiceguy/siphon-rs/blob/main/RELEASING.md).
The first release cut under these rules was `v2026.08.24`.

## The two-layer versioning model

forge-media deliberately separates two things that single-crate projects
collapse into one:

1. **Release tags are CalVer** (`v2026.08.24`). A tag names a *snapshot of the
   whole repository*. An aggregate of 25 crates cannot make a single honest
   compatibility promise (one release may break forge-webrtc while forge-rtp
   gets a patch fix), so the tag deliberately doesn't try — it's a pinnable,
   changelog-aligned name for a commit.
2. **Crate versions are SemVer** (`forge-webrtc = 0.4.0`). Compatibility lives
   at the crate level, and each crate's `Cargo.toml` version is where that
   signal is kept honest.

Keep the layers distinct. Do **not** copy the CalVer date into crate versions —
that would erase the per-crate compatibility signal — and do not invent a
workspace-wide SemVer number, since it would be meaningless for the same
reason. (The root `forge-media` package's `version.workspace` is the standalone
server's version, not a workspace aggregate.)

## Tag format

- One annotated tag per release: `vYYYY.MM.DD` (e.g. `v2026.08.24`), matching
  the changelog section heading for that release.
- **Same-day counter**: if a second release must ship on the same day (e.g. a
  hotfix right after a deployment), suffix a counter starting at `.1`:
  `v2026.08.24.1`, `v2026.08.24.2`, … The un-suffixed tag is implicitly the
  day's first release.
- Tags are **immutable**. Never move or delete a pushed tag; if a tagged
  release is bad, cut a new one.
- The tag message lists every crate's version in the release and the embedded
  siphon-rs tag (see `git tag -n99 v2026.08.24` for the template).

## The siphon-rs submodule rule

forge-media embeds siphon-rs as the `external/siphon-rs` submodule, and the
two repos are commonly consumed together (siphon-ai pins both). Therefore:

- **A release's submodule must point at a *tagged* siphon-rs commit**, never
  an untagged rev. If the submodule is mid-stream at release time, first bump
  it to the nearest siphon-rs tag (or cut a siphon-rs release).
- The changelog section and the tag message both **record the embedded
  siphon-rs tag** — "forge-media v2026.08.24 embeds siphon-rs v2026.08.24" is
  a checkable statement, not two SHAs correlated by hand.
- Downstream consumers pinning both repos should pin the pair a forge-media
  release names, so the siphon-rs revision on their SIP path and the one
  inside forge's media path cannot silently diverge (the class of bug behind
  siphon-ai #405/#406).

## Crate version bump conventions

All crates are pre-1.0, so per the Cargo interpretation of SemVer the middle
digit is the compatibility boundary:

| Change since the crate's last bump | Bump |
|---|---|
| Breaking API change (signature, return type, removed/renamed item, `Result`-ification) | minor (`0.x.0`) |
| New public API, additive (new methods, defaulted trait methods, new config fields) | minor (`0.x.0`) |
| Fixes / behavior corrections only | patch (`0.x.y`) |
| Only fmt drift, clippy chores, or ride-along test edits | no bump |

Notes:

- A return-type change counts as breaking even when existing callers compile —
  version it as breaking and note the source-compatibility in the changelog.
- Behavior changes that are observable on the wire or in metrics (a fixed KDF
  label, a renamed metric) are at least a patch, and the changelog entry must
  say what an operator will see change.
- Internal dependency migrations with no public API change do not by
  themselves require a bump; a major bump of a dependency whose types appear
  in public API does.
- Inter-crate dependencies are `path`-only (no version requirements), so
  bumping a crate never requires editing its dependents' manifests.

To audit what changed since a crate's last bump:

```bash
# Find the commit that last changed the version line
git log -L '/^version/,+1:crates/<crate>/Cargo.toml' --format='%h %ad %s' --date=short

# List commits touching the crate since then
git log --oneline <bump-commit>..HEAD -- crates/<crate>/src crates/<crate>/Cargo.toml
```

**Baseline caveat:** the repo pre-dates tags, and `v2026.08.24` was cut as a
baseline — its crate versions are as stamped by the PRs that landed the work,
with breaking-change bumps applied by audit (forge-webrtc, forge-ice,
forge-rtp). The strict audit discipline applies from the release after it.

## Changelog

`CHANGELOG.md` follows Keep a Changelog. Ongoing work accumulates under
`## [Unreleased]`; a release converts that section into
`## [YYYY-MM-DD] — workspace release`, which must include:

1. The full list of crate versions in the release (including "unchanged"
   crates) and the embedded siphon-rs tag.
2. A **Breaking changes** paragraph naming each breaking API change and its
   PR/issue number.
3. The accumulated entries.

A fresh empty `## [Unreleased]` goes above it. (Sections below the first
dated workspace release predate these rules and keep their historical
per-crate format.)

## Release checklist

1. **Audit**: for each crate, determine what landed since its last bump and
   classify per the table above.
2. **Bump** the `version =` lines in the affected `Cargo.toml`s.
3. **Submodule**: confirm `external/siphon-rs` points at a tagged siphon-rs
   commit; bump it first if not.
4. **Changelog**: backfill any commits missing from `[Unreleased]`, then
   convert it into the dated release section.
5. **Verify**: workspace build and tests pass; clippy is clean on the pinned
   CI toolchain.
6. **PR** the bump + changelog to `main` and merge it.
7. **Tag** the merge commit:

   ```bash
   git tag -a vYYYY.MM.DD -m "workspace release YYYY-MM-DD

   embeds siphon-rs <tag>

   <crate version list>" <merge-sha>
   git push origin vYYYY.MM.DD
   ```

   Anything merged to `main` after the changelog PR but before tagging either
   gets a changelog entry first, or an explicit callout in the release notes.
8. **GitHub Release**: `gh release create vYYYY.MM.DD` with the crate version
   table, the breaking-changes summary, the embedded siphon-rs tag, and the
   downstream pinning snippet. There is no release automation on tags — the
   old `release.yml` (which cross-compiled standalone-server binaries and
   never ran) was removed when these rules were adopted; if server binaries
   are ever wanted as release artifacts, that's a new decision, not a revert.
9. **Downstream**: update consumers (siphon-ai) when they're ready to absorb
   the release — see below.

## Downstream consumption

Consumers depend on forge-media via git dependencies and should pin tags, not
revs or branches:

```toml
forge-engine = { git = "https://github.com/thevoiceguy/forge-media", tag = "v2026.08.24", default-features = false, features = ["g722", "dtls", "opus", "neural-vad"] }
```

- **Pin every `forge-*` crate in a consumer to the same tag.** Different refs
  of the same repo are different sources to Cargo, which duplicates shared
  crates (two `forge-core`s) and produces impossible type-mismatch errors.
  Defining the pin once in the consumer's `[workspace.dependencies]` keeps it
  single-sourced.
- Upgrading = bump the tag, `cargo update`, then work through that release's
  breaking-changes paragraph in the changelog.
- When a consumer also pins siphon-rs directly (siphon-ai does), prefer the
  siphon-rs tag the forge-media release embeds.
- For a consumer that needs a fix without absorbing newer breakage: branch
  from its pinned tag, cherry-pick, and tag the result with the same-day
  counter rule.

## If crates are ever published to crates.io

Per-crate SemVer becomes the real release mechanism: version requirements
replace git pins, per-crate tags (`forge-webrtc-v0.4.0`) become meaningful,
and tooling like `release-plz`/`cargo-release` can automate bumps. The dated
workspace tags remain valid alongside — nothing above needs to be undone.
