# Conference AI Integration Guide

Forge Media enables AI as a first-class participant in conference rooms, allowing real-time voice AI to interact with all participants simultaneously.

## Table of Contents

- [Overview](#overview)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [API Reference](#api-reference)
- [Audio Modes](#audio-modes)
- [DTMF Integration](#dtmf-integration)
- [Configuration](#configuration)
- [Examples](#examples)
- [Troubleshooting](#troubleshooting)

---

## Overview

### Features

- **Virtual Participant**: AI joins as `__ai__` in the conference mixer
- **Bidirectional Audio**: AI hears all participants, all participants hear AI
- **DTMF Forwarding**: Participant DTMF events automatically forwarded to AI
- **Session Management**: Attach/detach AI dynamically via REST API
- **Mixed Audio Mode**: AI hears combined audio of all participants (default)
- **Recording Integration**: AI voice automatically included in room recordings
- **Multiple Conferences**: Each room can have independent AI sessions

### Supported AI Providers

- **OpenAI Realtime API** (gpt-4o-realtime-preview-2024-12-17)
- Extensible architecture for additional providers

---

## Quick Start

### 1. Create a Conference Room

```bash
curl -X POST http://localhost:8080/v1/conferences \
  -H "Content-Type: application/json" \
  -d '{
    "room_id": "conference-123"
  }'
```

### 2. Add Participants

```bash
# Add first participant
curl -X POST http://localhost:8080/v1/conferences/conference-123/participants \
  -H "Content-Type: application/json" \
  -d '{
    "participant_id": "user-001",
    "is_host": false
  }'

# Add second participant
curl -X POST http://localhost:8080/v1/conferences/conference-123/participants \
  -H "Content-Type: application/json" \
  -d '{
    "participant_id": "user-002",
    "is_host": false
  }'
```

### 3. Attach AI to Conference

```bash
curl -X POST http://localhost:8080/v1/conferences/conference-123/ai \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "sk-your-openai-api-key",
    "model": "gpt-4o-realtime-preview-2024-12-17",
    "voice": "alloy",
    "instructions": "You are a helpful conference assistant. Greet everyone and facilitate the discussion.",
    "temperature": 0.8,
    "audio_mode": "mixed"
  }'
```

The AI will immediately:
- Join the conference as a virtual participant
- Hear the combined audio from all participants
- Be able to speak back into the conference
- Receive DTMF events from any participant

### 4. Check AI Status

```bash
curl http://localhost:8080/v1/conferences/conference-123/ai
```

Response:
```json
{
  "room_id": "conference-123",
  "state": "Active",
  "model": "gpt-4o-realtime-preview-2024-12-17",
  "voice": "alloy",
  "audio_mode": "Mixed",
  "enable_transcription": false,
  "participants_heard": ["user-001", "user-002"]
}
```

### 5. Detach AI

```bash
curl -X DELETE http://localhost:8080/v1/conferences/conference-123/ai
```

---

## Architecture

### Audio Flow (Mixed Mode)

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ Participant │────▶│             │     │             │
│     001     │     │             │     │             │
└─────────────┘     │             │     │   OpenAI    │
                    │  Conference │────▶│  Realtime   │
┌─────────────┐     │    Mixer    │     │     API     │
│ Participant │────▶│             │     │             │
│     002     │     │   (48kHz)   │     │   (16kHz)   │
└─────────────┘     │             │◀────│             │
                    └──────┬──────┘     └─────────────┘
                           │
                           ▼
                    All participants
                    hear AI response
```

### Component Stack

1. **ConferenceAIManager** - Manages AI lifecycle for a conference room
   - Audio routing tasks (conference → AI, AI → conference)
   - DTMF forwarding task
   - State management (Connecting, Active, Speaking, Terminated)

2. **AISessionManager** - Core AI session management (from forge-engine)
   - OpenAI connector lifecycle
   - Audio sample rate conversion
   - DTMF event forwarding

3. **AudioMixer** - Conference audio mixing (from forge-mixer)
   - Virtual participant `__ai__` in mixer
   - Mixed audio generation (excluding AI's own voice)
   - Audio injection from AI responses

### How It Works

1. **Attachment**: When AI is attached to a conference:
   - AI session created with OpenAI Realtime API
   - Virtual participant `__ai__` added to mixer
   - Three async tasks spawned:
     - Audio routing (conference → AI)
     - Response polling (AI → conference)
     - DTMF forwarding (conference → AI)

2. **Audio Routing**:
   - Every 20ms: Get mixed audio (excluding AI's own voice)
   - Resample from 48kHz (conference) to 16kHz (AI)
   - Send to OpenAI via WebSocket

3. **AI Response**:
   - Poll OpenAI for audio responses every 100ms
   - Resample from 24kHz (AI) to 48kHz (conference)
   - Inject into mixer as participant `__ai__`
   - All participants receive AI audio in their mix

4. **DTMF Forwarding**:
   - Subscribe to conference event bus
   - Filter for DTMF digit events (End events only)
   - Forward to AI as text: "[DTMF: User pressed '5' via RFC 2833]"

---

## API Reference

### POST /v1/conferences/:room_id/ai

Attach AI to a conference room.

**Request Body**:
```json
{
  "api_key": "sk-...",           // Required: OpenAI API key
  "model": "gpt-4o-realtime-preview-2024-12-17",  // Optional
  "voice": "alloy",              // Optional: alloy, shimmer, echo, etc.
  "instructions": "You are...",  // Optional: System instructions
  "temperature": 0.8,            // Optional: 0.0-1.0
  "audio_mode": "mixed",         // Optional: "mixed" or "individual" (individual not yet implemented)
  "enable_transcription": false  // Optional: Enable participant transcription
}
```

**Response** (201 Created):
```json
{
  "room_id": "conference-123",
  "state": "Active",
  "model": "gpt-4o-realtime-preview-2024-12-17",
  "voice": "alloy",
  "audio_mode": "Mixed",
  "enable_transcription": false,
  "participants_heard": ["user-001", "user-002"]
}
```

**Error Responses**:
- `400 Bad Request` - Invalid audio_mode
- `404 Not Found` - Room not found
- `409 Conflict` - AI already attached to this room
- `422 Unprocessable Entity` - Validation error (missing/invalid fields)
- `500 Internal Server Error` - AI connection failed or Individual mode requested

### GET /v1/conferences/:room_id/ai

Get AI status for a conference room.

**Response** (200 OK):
```json
{
  "room_id": "conference-123",
  "state": "Active",
  "model": "gpt-4o-realtime-preview-2024-12-17",
  "voice": "alloy",
  "audio_mode": "Mixed",
  "enable_transcription": false,
  "participants_heard": ["user-001", "user-002"]
}
```

**Error Responses**:
- `404 Not Found` - Room not found or no AI attached

### DELETE /v1/conferences/:room_id/ai

Detach AI from a conference room.

**Response** (204 No Content)

**Error Responses**:
- `404 Not Found` - Room not found or no AI attached

---

## Audio Modes

### Mixed Mode ✅ (Default, Implemented)

AI hears all participants combined into a single audio stream.

**Advantages**:
- Lower CPU usage
- Simpler implementation
- Good for conversation and Q&A

**Use Cases**:
- Voice assistants in meetings
- Moderation/facilitation
- Q&A bots
- Meeting notes/summaries

**Example**:
```bash
curl -X POST http://localhost:8080/v1/conferences/room-123/ai \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "sk-...",
    "audio_mode": "mixed"
  }'
```

### Individual Mode ⚠️ (Not Yet Implemented)

AI would receive separate labeled audio streams per participant.

**Advantages** (when implemented):
- Better speaker identification
- Required for accurate transcription with speaker attribution
- Enables per-speaker analytics

**Use Cases** (when implemented):
- Meeting transcription with speaker labels
- Multi-speaker sentiment analysis
- Individual coaching/feedback

**Current Status**: Returns error if requested. Requires mixer enhancement to provide per-participant audio buffers.

---

## DTMF Integration

### Automatic DTMF Forwarding

When DTMF is enabled in the conference and an event bus is provided, all participant DTMF digits are automatically forwarded to the AI as text messages.

**DTMF Event Format**:
```
"[DTMF: User pressed '5' via RFC 2833]"
```

**Supported Detection Methods**:
- RFC 2833 (telephone-event RTP payload)
- Inband (audio frequency detection)
- SIP INFO

### Example Use Case: IVR Navigation

```bash
# AI with instructions to handle DTMF
curl -X POST http://localhost:8080/v1/conferences/ivr-room/ai \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "sk-...",
    "instructions": "You are an IVR system. When user presses 1, provide sales info. When user presses 2, provide support info. Acknowledge each button press."
  }'
```

When a participant presses "1":
1. DTMF detector publishes event to event bus
2. ConferenceAIManager receives event
3. AI receives: "[DTMF: User pressed '\''1'\'' via RFC 2833]"
4. AI responds: "You pressed 1 for sales. Let me connect you..."

---

## Configuration

### AI Session Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `api_key` | string | Required | OpenAI API key |
| `model` | string | `"gpt-4o-realtime-preview-2024-12-17"` | AI model |
| `voice` | string | `"alloy"` | Voice personality (alloy, shimmer, echo, etc.) |
| `instructions` | string | `"You are a helpful assistant."` | System instructions |
| `temperature` | float | `0.8` | Response randomness (0.0-1.0) |
| `audio_mode` | string | `"mixed"` | Audio routing mode |
| `enable_transcription` | bool | `false` | Enable participant transcription |

### Conference AI Constants

Defined in `crates/forge-conference-processor/src/ai_manager.rs`:

```rust
pub const AI_PARTICIPANT_ID: &str = "__ai__";  // Mixer participant ID
pub const AI_SAMPLE_RATE: u32 = 16000;         // 16kHz for speech
pub const AI_FRAME_SIZE: usize = 320;          // 20ms @ 16kHz
pub const AI_POLL_INTERVAL_MS: u64 = 100;      // Response poll interval
pub const AUDIO_TASK_SLEEP_MS: u64 = 20;       // Audio routing interval
```

---

## Examples

### Example 1: Meeting Assistant

```bash
curl -X POST http://localhost:8080/v1/conferences/team-meeting/ai \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "sk-...",
    "voice": "alloy",
    "instructions": "You are a meeting assistant. Take notes, summarize key points, and remind participants of action items. Be concise and professional."
  }'
```

### Example 2: Language Translation

```bash
curl -X POST http://localhost:8080/v1/conferences/intl-call/ai \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "sk-...",
    "voice": "shimmer",
    "instructions": "You are a real-time translator. Listen to conversations and provide translations between English and Spanish when requested. Speak clearly and wait for pauses before translating."
  }'
```

### Example 3: Conference Moderator

```bash
curl -X POST http://localhost:8080/v1/conferences/large-conf/ai \
  -H "Content-Type: application/json" \
  -d '{
    "api_key": "sk-...",
    "voice": "echo",
    "temperature": 0.7,
    "instructions": "You are a conference moderator. Manage speaker time, facilitate Q&A, and keep the discussion on track. Announce when DTMF commands are pressed (1 for questions, 2 for comments)."
  }'
```

### Example 4: Dynamic Attach/Detach

```bash
#!/bin/bash
ROOM="dynamic-room"
API_KEY="sk-..."

# Create room
curl -X POST http://localhost:8080/v1/conferences \
  -H "Content-Type: application/json" \
  -d "{\"room_id\": \"$ROOM\"}"

# Add participants
curl -X POST http://localhost:8080/v1/conferences/$ROOM/participants \
  -H "Content-Type: application/json" \
  -d '{"participant_id": "alice"}'

# Attach AI for 5 minutes
curl -X POST http://localhost:8080/v1/conferences/$ROOM/ai \
  -H "Content-Type: application/json" \
  -d "{
    \"api_key\": \"$API_KEY\",
    \"instructions\": \"Welcome everyone! I'll be your assistant for the next 5 minutes.\"
  }"

echo "AI active for 5 minutes..."
sleep 300

# Detach AI
curl -X DELETE http://localhost:8080/v1/conferences/$ROOM/ai
echo "AI detached"
```

---

## Troubleshooting

### AI Connection Fails

**Symptom**: `500 Internal Server Error` when attaching AI

**Possible Causes**:
1. Invalid OpenAI API key
2. Network connectivity issues
3. OpenAI service unavailable

**Solution**:
- Verify API key is valid
- Check network connectivity
- Check Forge logs for detailed error message

### No Audio from AI

**Symptom**: AI attached successfully but participants can't hear it

**Possible Causes**:
1. AI is not speaking (waiting for prompt)
2. Audio routing task crashed
3. Sample rate conversion issue

**Solution**:
- Check AI state via GET endpoint
- Review Forge logs for audio routing errors
- Verify participants have audio flowing (check via `/v1/conferences/:room_id/participants`)

### DTMF Not Forwarded to AI

**Symptom**: Participant presses DTMF but AI doesn't respond

**Possible Causes**:
1. DTMF not enabled in conference
2. Event bus not provided when attaching AI
3. DTMF detection not working

**Solution**:
- Ensure conference has DTMF commands enabled
- Verify event bus is passed to `attach_ai()`
- Check DTMF detection is working (test with conference DTMF commands)

### "Individual mode not implemented" Error

**Symptom**: `500 Internal Server Error` with message about Individual mode

**Cause**: Individual audio mode is not yet supported

**Solution**:
- Use `"audio_mode": "mixed"` (default)
- Individual mode requires mixer enhancements (planned for future release)

### AI Detach Hangs or Fails

**Symptom**: DELETE request times out or returns error

**Possible Causes**:
1. AI session cleanup taking too long
2. Tasks not aborting properly

**Solution**:
- Check Forge logs for task abortion errors
- The detach runs in background, so API returns immediately
- If persistent, restart Forge server

---

## Best Practices

### 1. Use Appropriate Instructions

Tailor system instructions to the conference use case:

```json
{
  "instructions": "You are a [role]. Your goals are: [1] ..., [2] ..., [3] ... Be [concise/detailed/professional/casual]."
}
```

### 2. Handle Concurrent Conferences

Each conference can have its own independent AI session:

```bash
# Conference 1: Sales meeting
curl -X POST .../sales-meeting/ai -d '{"api_key": "...", "instructions": "Sales assistant"}'

# Conference 2: Support call
curl -X POST .../support-call/ai -d '{"api_key": "...", "instructions": "Support agent"}'
```

### 3. Monitor AI State

Poll AI status periodically to ensure it's still active:

```bash
watch -n 5 'curl -s http://localhost:8080/v1/conferences/room-123/ai | jq .state'
```

### 4. Graceful Cleanup

Always detach AI when conference ends:

```bash
# Delete conference (automatically cleans up AI)
curl -X DELETE http://localhost:8080/v1/conferences/room-123
```

### 5. Use Recording for Compliance

If recording the conference, AI audio is automatically included:

```bash
# Start recording
curl -X POST http://localhost:8080/v1/conferences/room-123/recording \
  -d '{"output_path": "meeting.wav"}'

# Recording will include all participants + AI
```

---

## See Also

- [AI Integration Guide](./AI_INTEGRATION.md) - For 1:1 session AI integration
- [Conference Features](../README.md#conference-features) - Conference system overview
- [DTMF Integration](./DTMF_INTEGRATION.md) - DTMF detection and handling
- [API Reference](./API.md) - Complete API documentation

---

**Version**: 0.3.0
**Last Updated**: 2025-12-16
