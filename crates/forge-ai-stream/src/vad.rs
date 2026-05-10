//! Voice Activity Detection — re-exports from `forge-vad`.
//!
//! Until 2026-05 the VAD algorithm lived in this crate. It moved to
//! the standalone [`forge_vad`] crate so non-AI consumers (notably
//! `siphon-ai`, which is provider-neutral and explicitly forbidden
//! from depending on `forge-ai-stream` per CLAUDE.md §4.1) could
//! consume it.
//!
//! These re-exports preserve the existing `forge_ai_stream::vad::…`
//! import paths used by the bargein module and downstream callers.
//! New code should `use forge_vad::…` directly.

pub use forge_vad::{Result, VadConfig, VadDetector, VadError, VadState};
