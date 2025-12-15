# Forge Media Examples

This directory contains example scripts and applications demonstrating various Forge Media features.

## AI Integration Examples

### Prerequisites

All AI examples require an OpenAI API key:

```bash
export OPENAI_API_KEY="your-api-key-here"
```

Ensure the Forge Media server is running:

```bash
cargo run --release
```

### Available Examples

#### 1. Basic AI Integration

**File**: `ai_integration_example.sh`

Demonstrates basic AI integration with OpenAI's Realtime API.

```bash
./ai_integration_example.sh
```

**What it does**:
- Creates a media session
- Attaches OpenAI AI with customer service instructions
- Shows how audio flows bidirectionally (RTP ↔ AI)
- Displays session status and statistics

**Use cases**:
- Voice assistants
- Automated customer service
- Call screening

---

#### 2. AI-Powered IVR

**File**: `ai_ivr_example.sh`

Shows how to build an Interactive Voice Response (IVR) system with DTMF integration.

```bash
./ai_ivr_example.sh
```

**What it does**:
- Creates an IVR session with menu options
- AI greets caller and presents menu ("Press 1 for Sales...")
- DTMF tones automatically forwarded to AI
- AI responds based on user input

**Key feature**: No custom DTMF handling code needed - the AI understands DTMF events naturally!

**Use cases**:
- Phone menus
- Self-service systems
- Appointment scheduling
- Order status checks

---

#### 3. Function Calling

**File**: `ai_function_calling_example.sh`

Demonstrates how AI can trigger actions in your application via function calling.

```bash
./ai_function_calling_example.sh
```

**What it does**:
- Defines custom functions (get balance, transfer call, schedule callback)
- AI can call these functions during conversation
- Shows how to respond with function results
- AI uses results in natural conversation

**Functions defined**:
- `get_account_balance(account_number)` - Query customer account
- `transfer_call(department, reason)` - Transfer to another department
- `schedule_callback(phone_number, time, topic)` - Schedule a callback

**Use cases**:
- Dynamic information lookup
- Call transfers and routing
- Database queries
- External API integration
- Workflow automation

---

## Running the Examples

### Basic Usage

```bash
# Use default settings
./ai_integration_example.sh

# Customize server URL
FORGE_URL=http://192.168.1.100:8080 ./ai_integration_example.sh

# Use different OpenAI model
OPENAI_MODEL=gpt-4o-realtime-preview-2024-12-17 ./ai_integration_example.sh

# Use different voice
OPENAI_VOICE=shimmer ./ai_ivr_example.sh
```

### Available Voices

- `alloy` - Neutral, balanced (default)
- `shimmer` - Warm, empathetic
- `echo` - Clear, professional
- `ash` - Calm, informative
- `ballad` - Smooth, conversational
- `coral` - Friendly, energetic

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `FORGE_URL` | `http://localhost:8080` | Forge Media server URL |
| `OPENAI_API_KEY` | (required) | OpenAI API key |
| `OPENAI_MODEL` | `gpt-4o-realtime-preview-2024-12-17` | Model to use |
| `OPENAI_VOICE` | `alloy` | Voice personality |
| `CALL_ID` | Auto-generated | Custom call identifier |

---

## Understanding the Examples

### Audio Flow

```
Phone/WebRTC Client → RTP → Forge Media → WebSocket → OpenAI API
                                                          ↓
                                                    AI Processing
                                                          ↓
Phone/WebRTC Client ← RTP ← Forge Media ← WebSocket ← AI Response
```

### DTMF Flow (IVR Example)

```
User Presses Key → DTMF Detector → EventBus → AI Session → OpenAI
                                                              ↓
                                            AI receives: "[DTMF: User pressed '5']"
```

### Function Calling Flow

```
User: "What's my balance?"
   ↓
AI decides to call get_account_balance("12345")
   ↓
EventBus publishes AIEvent::FunctionCall
   ↓
Your Application handles event
   ↓
Query database → Returns $245.67
   ↓
POST /v1/sessions/:id/ai/function-response
   ↓
AI: "Your current balance is $245.67"
```

---

## Customizing the Examples

### Change AI Instructions

Edit the `instructions` field in the attach AI request:

```json
{
  "instructions": "You are a technical support agent specializing in VoIP issues. Be concise and ask diagnostic questions."
}
```

### Adjust Turn Detection (Interruptions)

```json
{
  "turn_detection": {
    "type": "server_vad",
    "threshold": 0.7,           // Higher = less sensitive
    "prefix_padding_ms": 500,   // More context before speech
    "silence_duration_ms": 300  // Faster interruption detection
  }
}
```

### Add Custom Functions

```json
{
  "tools": [
    {
      "type": "function",
      "name": "check_service_status",
      "description": "Check if a service is operational",
      "parameters": {
        "type": "object",
        "properties": {
          "service_name": {
            "type": "string",
            "enum": ["voip", "sms", "video"]
          }
        },
        "required": ["service_name"]
      }
    }
  ]
}
```

---

## Monitoring and Debugging

### Check AI Session Status

```bash
curl http://localhost:8080/v1/sessions/call-001/ai | jq '.'
```

**Response**:
```json
{
  "call_id": "call-001",
  "state": "connected",
  "stats": {
    "audio_samples_sent": 48000,
    "audio_chunks_received": 24,
    "events_sent": 15,
    "events_received": 12,
    "started_at": "2025-12-15T10:30:00Z"
  }
}
```

### Enable Debug Logging

```bash
RUST_LOG=forge=debug cargo run --release
```

### Watch Events in Real-Time

```bash
# In one terminal
RUST_LOG=forge_engine::ai_integration=debug cargo run --release

# In another terminal
./ai_integration_example.sh
```

Look for log entries:
- `Sending audio to AI` - RTP → AI working
- `Received AI audio response` - AI → RTP working
- `Received DTMF event` - DTMF detection working
- `AI event received` - OpenAI events

---

## Troubleshooting

### "Connection refused" Error

**Problem**: Forge server not running

**Solution**:
```bash
cargo run --release
```

### "Invalid API key" Error

**Problem**: OPENAI_API_KEY not set or invalid

**Solution**:
```bash
export OPENAI_API_KEY="sk-your-actual-key-here"
```

### No Audio from AI

**Check stats**:
```bash
curl localhost:8080/v1/sessions/call-001/ai | jq '.stats'
```

If `audio_chunks_received` is 0:
- Check OpenAI API key
- Verify model supports audio output
- Check `modalities` includes `"audio"`

### DTMF Not Detected

- Ensure RFC 2833 is enabled in your SDP
- Check for `telephone-event` payload type
- Verify DTMF events in logs: `RUST_LOG=forge_dtmf=debug`

---

## Next Steps

1. **Read the full guide**: [AI Integration Guide](../docs/AI_INTEGRATION.md)
2. **Explore API endpoints**: [API Reference](../docs/API.md)
3. **Build your own**: Integrate AI into your Rust application
4. **Customize**: Adapt examples for your use case

---

## Additional Resources

- [OpenAI Realtime API Docs](https://platform.openai.com/docs/guides/realtime)
- [Forge Architecture](../FORGE%20ARCHITECTURE.md)
- [DTMF Integration](../docs/DTMF_INTEGRATION.md)
- [GitHub Issues](https://github.com/ferrous-comms/forge-media/issues)

---

## Contributing

Found a bug or have an example to share? Please open an issue or pull request!

