# Audio Injection Feature

## Overview

This PR adds audio injection capabilities to forge-media, enabling playback of audio files, TTS, and tones into active RTP sessions. This is a foundational feature for implementing IVR systems, announcements, and programmable voice applications.

## What's Added

### 1. PlaybackManager (`forge-engine/src/injection.rs`)

New module for managing audio playback sessions:

- **PlaybackManager**: Coordinates multiple concurrent playbacks across different calls
- **PlaybackHandle**: Control handle for stopping playback and awaiting completion
- **PlaybackId**: Unique identifier for tracking playbacks
- **AudioTarget**: Enum for targeting audio to ParticipantA, ParticipantB, or Both
- **MixMode**: Enum for Mix, Replace, or Duck mixing strategies
- **PlaybackStatus**: Completion status (Completed, Stopped, Failed)

### 2. API Changes

#### PlaybackManager

```rust
use forge_engine::{PlaybackManager, AudioTarget, MixMode};
use forge_injection::FileSource;

let manager = PlaybackManager::new();

// Start playing a file
let source = Box::new(FileSource::new("greeting.wav")?);
let handle = manager.start_playback(
    call_id,
    source,
    AudioTarget::Both,
    MixMode::Replace,
).await?;

// Wait for completion
let status = handle.wait_completion().await?;

// Or stop early
handle.stop().await?;
```

#### Integration with Media Sessions (Future)

```rust
// This will be added in a follow-up PR
session.inject_audio(source, AudioTarget::ParticipantA, MixMode::Mix).await?;
```

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                   Application Layer                       │
│  (IVR systems, programmable voice, announcements)        │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│                  PlaybackManager                          │
│  • Manages playback lifecycle                            │
│  • Coordinates multiple concurrent playbacks             │
│  • Provides PlaybackHandle for control                   │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│              forge-injection (AudioSource)                │
│  • FileSource (WAV, MP3, FLAC, etc.)                    │
│  • TtsSource (Google, AWS, Azure - providers needed)    │
│  • ToneGenerator (DTMF, silence, comfort noise)         │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│              ForwardingEngine (Future)                    │
│  • Inject PCM frames into RTP streams                    │
│  • Handle codec transcoding if needed                    │
│  • Mix or replace existing audio                         │
└──────────────────────────────────────────────────────────┘
```

## Current Limitations

This PR provides the **framework** for audio injection. The following integration work is deferred to follow-up PRs:

### 1. RTP Integration (Next PR)

The playback loop currently simulates frame timing but doesn't inject into actual RTP streams. Next PR will:

- Integrate PlaybackManager with ForwardingEngine
- Add actual RTP packet injection
- Handle codec transcoding (source PCM → session codec)
- Implement Mix/Replace/Duck audio mixing

### 2. MediaSession API (Next PR)

Add convenience methods to MediaSession:

```rust
impl MediaSession {
    pub async fn inject_audio(
        &self,
        source: Box<dyn AudioSource>,
        target: AudioTarget,
        mix_mode: MixMode,
    ) -> Result<PlaybackHandle>;

    pub async fn stop_all_playbacks(&self) -> Result<()>;
}
```

### 3. TTS Provider Integration (Future)

The TtsSource framework exists but needs actual provider SDKs:

- Google Cloud Text-to-Speech API
- AWS Polly SDK
- Azure Speech Services SDK

### 4. Advanced Features (Future)

- Playback progress callbacks
- Volume control during playback
- Fade in/fade out effects
- Loop/repeat support
- Playback queueing

## Testing

Basic tests are included:

```bash
cargo test -p forge-engine injection
```

Tests cover:
- Playback lifecycle (start/stop)
- Multiple concurrent playbacks
- Cleanup after completion

## Migration Guide

This is a new feature with no breaking changes. Existing code continues to work unchanged.

To use audio injection:

```rust
// 1. Add dependency
forge-engine = { version = "0.3", features = ["injection"] }
forge-injection = { version = "0.1" }

// 2. Create PlaybackManager
let manager = PlaybackManager::new();

// 3. Start playback
let source = Box::new(FileSource::new("audio.wav")?);
let handle = manager.start_playback(
    call_id,
    source,
    AudioTarget::Both,
    MixMode::Replace,
).await?;

// 4. Handle completion
tokio::spawn(async move {
    match handle.wait_completion().await {
        Ok(PlaybackStatus::Completed) => info!("Playback finished"),
        Ok(PlaybackStatus::Stopped) => info!("Playback stopped"),
        Ok(PlaybackStatus::Failed(e)) => error!("Playback failed: {}", e),
        Err(e) => error!("Completion error: {}", e),
    }
});
```

## Use Cases Enabled

With this PR + RTP integration:

1. **IVR Systems**: Play prompts and gather DTMF input
2. **Announcements**: Play pre-recorded messages to callers
3. **Hold Music**: Inject music during call hold
4. **Voice Menus**: Multi-level menu navigation
5. **TTS Responses**: Dynamic speech generation (when providers added)
6. **DTMF Tones**: Generate touchtone signals
7. **Comfort Noise**: Provide audio feedback during silence

## Example: Simple IVR

```rust
// Play greeting
let greeting = Box::new(FileSource::new("welcome.wav")?);
let handle = manager.start_playback(
    call_id,
    greeting,
    AudioTarget::Both,
    MixMode::Replace,
).await?;

handle.wait_completion().await?;

// Play menu
let menu = Box::new(FileSource::new("press_1_for_sales.wav")?);
let handle = manager.start_playback(
    call_id,
    menu,
    AudioTarget::Both,
    MixMode::Replace,
).await?;

// Await DTMF input...
```

## Performance Considerations

- Each playback spawns a tokio task
- Memory usage: ~1-2 KB per active playback + audio buffer
- CPU: Minimal, mostly I/O bound (reading frames from source)
- Network: No additional RTP bandwidth (replaces/mixes existing streams)

## Future Enhancements

Planned for follow-up PRs:

1. **RTP Integration**: Complete the injection pipeline
2. **Session API**: Convenience methods on MediaSession
3. **TTS Providers**: Google/AWS/Azure integrations
4. **Playback Events**: Progress callbacks and notifications
5. **Advanced Mixing**: Volume control, fade effects
6. **Recording Integration**: Record playback for CDR/compliance

## Dependencies Added

- `forge-injection` - Already existed, now used by forge-engine
- `anyhow` - For error handling in injection module

## Breaking Changes

None. This is purely additive.

## Checklist

- [x] Core PlaybackManager implementation
- [x] PlaybackHandle with stop/wait_completion
- [x] Basic unit tests
- [x] Documentation
- [ ] RTP integration (deferred to next PR)
- [ ] MediaSession convenience API (deferred to next PR)
- [ ] TTS provider SDKs (deferred to future PR)

## Related Issues

This enables audio playback features needed for:
- IVR system implementation
- Programmable voice applications
- Call center features (hold music, announcements)

## Questions for Reviewers

1. Is the PlaybackManager API intuitive?
2. Should we add sync/blocking variants for non-async contexts?
3. Any concerns about the playback task lifecycle?
4. Suggestions for the RTP integration approach?
