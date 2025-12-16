# Multiple AI Provider Support

**Status**: Implemented (v0.6.0) - Stub implementations for new providers

## Overview

Forge Media now supports multiple AI providers for real-time voice conversations, allowing you to choose the best AI service for your use case:

- ✅ **OpenAI** - Realtime API (fully implemented)
- 🚧 **Anthropic** - Claude Voice API (stub implementation)
- 🚧 **ElevenLabs** - Conversational AI (stub implementation)
- 🚧 **Deepgram** - Voice Agent API (stub implementation)
- 📋 **Google Dialogflow** - (planned)
- 📋 **Amazon Lex** - (planned)
- 📋 **Azure Cognitive Services** - (planned)
- ✅ **Custom** - Roll your own connector

## Provider Comparison

| Provider | Best For | Key Features | Pricing |
|----------|----------|--------------|---------|
| **OpenAI** | General conversation, function calling | GPT-4 intelligence, realtime API, tool use | ~$0.06-0.24/min |
| **Anthropic** | Long context, nuanced conversation | Claude 3 models, 200K context, thoughtful responses | ~$0.15/min (estimated) |
| **ElevenLabs** | Natural voice, character voices | Ultra-realistic voices, voice cloning, emotion | ~$0.18-0.30/min |
| **Deepgram** | Speech accuracy, low latency | Nova-2 STT, Aura TTS, voice agents | ~$0.0125/min STT + TTS |

## Provider Status

### ✅ OpenAI (Production Ready)

**Fully implemented** with complete WebSocket protocol support.

**Features**:
- Real-time bidirectional audio streaming
- Function/tool calling
- Voice Activity Detection (VAD)
- Barge-in support
- Multiple voice options
- Session persistence

**Models**:
- `gpt-4o-realtime-preview` (recommended)
- `gpt-4o-realtime-preview-2024-10-01`

**Documentation**: Fully documented with production examples

### 🚧 Anthropic (Stub Implementation)

**Status**: Architecture in place, protocol implementation needed

**What's Implemented**:
- Connector trait implementation
- Configuration structure
- Model definitions (Claude 3 Opus, Sonnet, Haiku)
- Test scaffolding

**What's Needed**:
- WebSocket protocol implementation
- Authentication flow
- Message parsing (Anthropic's event format)
- Audio encoding/decoding
- Tool/function calling support

**Estimated Effort**: 2-3 days of development + API testing

### 🚧 ElevenLabs (Stub Implementation)

**Status**: Architecture in place, protocol implementation needed

**What's Implemented**:
- Connector trait implementation
- Configuration structure
- Model definitions (Turbo v2, Multilingual v2, etc.)
- Voice library constants (Rachel, Drew, Antoni, etc.)
- Test scaffolding

**What's Needed**:
- WebSocket protocol implementation
- Agent configuration
- Audio streaming (base64 encoding)
- Transcript event handling
- Interruption support

**Estimated Effort**: 2-3 days of development + API testing

### 🚧 Deepgram (Stub Implementation)

**Status**: Architecture in place, protocol implementation needed

**What's Implemented**:
- Connector trait implementation
- Configuration structure
- Model definitions (Nova-2, Enhanced, Whisper)
- Aura voice library constants
- Test scaffolding

**What's Needed**:
- WebSocket protocol implementation (bi-directional STT+TTS)
- Query parameter configuration
- Binary audio streaming
- Transcript result parsing
- Agent state management

**Estimated Effort**: 2-3 days of development + API testing

## Using Multiple Providers

### Configuration

Specify the provider in your AI session configuration:

```rust
use forge_engine::{AISessionConfig, AIConnectorType};

// OpenAI (production)
let openai_config = AISessionConfig {
    connector_type: AIConnectorType::OpenAI,
    api_key: "sk-...".to_string(),
    model: "gpt-4o-realtime-preview".to_string(),
    voice: Some("alloy".to_string()),
    temperature: Some(0.8),
    ..Default::default()
};

// Anthropic (when implemented)
let anthropic_config = AISessionConfig {
    connector_type: AIConnectorType::Anthropic,
    api_key: "sk-ant-...".to_string(),
    model: "claude-3-opus-20240229".to_string(),
    voice: None, // Anthropic may not support voice selection
    temperature: Some(0.7),
    ..Default::default()
};

// ElevenLabs (when implemented)
let elevenlabs_config = AISessionConfig {
    connector_type: AIConnectorType::ElevenLabs,
    api_key: "your-elevenlabs-key".to_string(),
    model: "eleven_turbo_v2".to_string(),
    voice: Some("21m00Tcm4TlvDq8ikWAM".to_string()), // Rachel
    ..Default::default()
};

// Deepgram (when implemented)
let deepgram_config = AISessionConfig {
    connector_type: AIConnectorType::Deepgram,
    api_key: "your-deepgram-key".to_string(),
    model: "nova-2".to_string(),
    voice: Some("aura-asteria-en".to_string()),
    ..Default::default()
};
```

### API Endpoint Usage

Attach AI to a call with provider selection:

```bash
# OpenAI (production)
curl -X POST http://localhost:8080/v1/calls/abc123/ai \
  -H "Content-Type: application/json" \
  -d '{
    "connector_type": "OpenAI",
    "api_key": "sk-...",
    "model": "gpt-4o-realtime-preview",
    "voice": "alloy",
    "temperature": 0.8,
    "instructions": "You are a helpful assistant."
  }'

# Anthropic (when implemented)
curl -X POST http://localhost:8080/v1/calls/abc123/ai \
  -H "Content-Type: application/json" \
  -d '{
    "connector_type": "Anthropic",
    "api_key": "sk-ant-...",
    "model": "claude-3-opus-20240229",
    "temperature": 0.7,
    "instructions": "You are Claude, a thoughtful AI assistant."
  }'

# ElevenLabs (when implemented)
curl -X POST http://localhost:8080/v1/calls/abc123/ai \
  -H "Content-Type: application/json" \
  -d '{
    "connector_type": "ElevenLabs",
    "api_key": "your-elevenlabs-key",
    "model": "eleven_turbo_v2",
    "voice": "21m00Tcm4TlvDq8ikWAM",
    "instructions": "You are a friendly voice assistant."
  }'

# Deepgram (when implemented)
curl -X POST http://localhost:8080/v1/calls/abc123/ai \
  -H "Content-Type: application/json" \
  -d '{
    "connector_type": "Deepgram",
    "api_key": "your-deepgram-key",
    "model": "nova-2",
    "voice": "aura-asteria-en",
    "instructions": "You are a professional voice agent."
  }'
```

## Voice Selection Guide

### OpenAI Voices (Available Now)

```rust
// Available voices
"alloy"   // Neutral, balanced
"echo"    // Male, clear
"fable"   // British, expressive
"onyx"    // Deep, authoritative
"nova"    // Female, energetic
"shimmer" // Warm, friendly
```

### ElevenLabs Voices (When Implemented)

```rust
use forge_ai_stream::ElevenLabsVoices;

// Popular pre-made voices
ElevenLabsVoices::RACHEL   // American female, calm
ElevenLabsVoices::DREW     // American male, well-rounded
ElevenLabsVoices::ANTONI   // American male, well-rounded
ElevenLabsVoices::BELLA    // American female, soft
ElevenLabsVoices::DOMI     // American female, strong
ElevenLabsVoices::DAVE     // British-Essex male
ElevenLabsVoices::FIN      // Irish male, sailor
ElevenLabsVoices::CLYDE    // American male, war veteran
ElevenLabsVoices::PAUL     // American male, ground reporter
ElevenLabsVoices::THOMAS   // American male, calm

// Or use custom voice IDs from your ElevenLabs account
"your-custom-voice-id"
```

### Deepgram Aura Voices (When Implemented)

```rust
use forge_ai_stream::DeepgramVoices;

// Female voices
DeepgramVoices::AURA_ASTERIA_EN  // Conversational
DeepgramVoices::AURA_LUNA_EN     // Expressive
DeepgramVoices::AURA_STELLA_EN   // Professional
DeepgramVoices::AURA_ATHENA_EN   // Authoritative
DeepgramVoices::AURA_HERA_EN     // Warm

// Male voices
DeepgramVoices::AURA_ORION_EN    // Confident
DeepgramVoices::AURA_ARCAS_EN    // Calm
DeepgramVoices::AURA_PERSEUS_EN  // Dynamic
DeepgramVoices::AURA_ANGUS_EN    // Friendly
DeepgramVoices::AURA_ORPHEUS_EN  // Warm
```

## Implementing a Connector

If you want to complete one of the stub implementations or add a new provider, follow this pattern:

### 1. Define Your Connector

```rust
use forge_ai_stream::{AIConnector, AIConnectorConfig, AIConnectorType, AIEvent};
use async_trait::async_trait;

pub struct MyCustomConnector {
    config: AIConnectorConfig,
    session: Option<AISession>,
    stats: AIStreamStats,
    ws: Option<WebSocketStream<...>>,
}
```

### 2. Implement the AIConnector Trait

```rust
#[async_trait]
impl AIConnector for MyCustomConnector {
    async fn connect(&mut self) -> Result<String> {
        // 1. Connect to WebSocket
        // 2. Authenticate
        // 3. Initialize session
        // 4. Return session ID
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Close WebSocket and cleanup
    }

    async fn send_audio(&mut self, audio_data: &[i16], sample_rate: u32) -> Result<()> {
        // 1. Encode audio (PCM16 → base64 or binary)
        // 2. Create protocol message
        // 3. Send via WebSocket
    }

    async fn next_event(&mut self) -> Result<Option<AIEvent>> {
        // 1. Read WebSocket message
        // 2. Parse protocol-specific format
        // 3. Convert to AIEvent
        // 4. Return event
    }

    // ... implement other required methods
}
```

### 3. Register in lib.rs

```rust
// In forge-ai-stream/src/lib.rs
pub mod mycustom;
pub use mycustom::{MyCustomConnector, MyCustomConfig};
```

### 4. Add to AIConnectorType

```rust
// In connector.rs
pub enum AIConnectorType {
    // ...
    MyCustom,
}
```

## Testing Providers

### Unit Tests

Each connector includes unit tests:

```bash
# Test all connectors
cargo test --package forge-ai-stream

# Test specific connector
cargo test --package forge-ai-stream anthropic
cargo test --package forge-ai-stream elevenlabs
cargo test --package forge-ai-stream deepgram
```

### Integration Testing

Test with a real API key:

```bash
# Set API key
export ANTHROPIC_API_KEY="sk-ant-..."

# Run integration test
cargo test --package forge-ai-stream --features anthropic-integration -- --ignored
```

## Provider-Specific Features

### OpenAI Features

- ✅ Real-time audio streaming
- ✅ Function/tool calling
- ✅ Voice Activity Detection
- ✅ Barge-in support
- ✅ Multiple voices
- ✅ Session config updates

### Anthropic Features (When Implemented)

- 🚧 200K context window
- 🚧 Extended conversations
- 🚧 Vision capabilities
- 🚧 Tool use
- 🚧 Thinking mode

### ElevenLabs Features (When Implemented)

- 🚧 Ultra-realistic voices
- 🚧 Voice cloning
- 🚧 Emotion control
- 🚧 Voice library (1000+ voices)
- 🚧 Multi-language support

### Deepgram Features (When Implemented)

- 🚧 Industry-leading STT accuracy
- 🚧 Nova-2 model
- 🚧 Aura TTS voices
- 🚧 Low latency (<300ms)
- 🚧 Custom vocabulary

## Migration Between Providers

### OpenAI → Anthropic

```rust
// Change connector type and model
config.connector_type = AIConnectorType::Anthropic;
config.model = "claude-3-opus-20240229".to_string();
// Keep same instructions, adjust temperature if needed
```

### OpenAI → ElevenLabs

```rust
// Change connector type, select voice
config.connector_type = AIConnectorType::ElevenLabs;
config.model = "eleven_turbo_v2".to_string();
config.voice = Some(ElevenLabsVoices::RACHEL.to_string());
```

### OpenAI → Deepgram

```rust
// Change connector type
config.connector_type = AIConnectorType::Deepgram;
config.model = "nova-2".to_string();
config.voice = Some(DeepgramVoices::AURA_ASTERIA_EN.to_string());
```

## Cost Optimization

### Strategy 1: Provider Fallback

```rust
// Try ElevenLabs first (best voice quality)
// Fall back to OpenAI if unavailable
// Fall back to Deepgram for budget calls

let providers = vec![
    (AIConnectorType::ElevenLabs, elevenlabs_config),
    (AIConnectorType::OpenAI, openai_config),
    (AIConnectorType::Deepgram, deepgram_config),
];

for (provider_type, config) in providers {
    match manager.attach_ai(call_id.clone(), config, None).await {
        Ok(_) => {
            info!("Connected with {:?}", provider_type);
            break;
        }
        Err(e) => {
            warn!("Failed to connect with {:?}: {}", provider_type, e);
            continue;
        }
    }
}
```

### Strategy 2: Feature-Based Selection

```rust
// Use best provider for each use case
let provider = match use_case {
    UseCase::CustomerSupport => AIConnectorType::OpenAI,     // Function calling
    UseCase::VoiceActing => AIConnectorType::ElevenLabs,     // Best voices
    UseCase::Transcription => AIConnectorType::Deepgram,     // Best accuracy
    UseCase::LongContext => AIConnectorType::Anthropic,      // 200K context
};
```

### Strategy 3: Time-Based Routing

```rust
// Use cheaper providers during off-peak hours
let provider = if is_peak_hours() {
    AIConnectorType::Deepgram  // Lower cost
} else {
    AIConnectorType::ElevenLabs  // Premium experience
};
```

## Troubleshooting

### Provider Not Available Error

```
Error: "Anthropic connector is a stub implementation - connection not implemented"
```

**Solution**: The provider is not yet fully implemented. Either:
1. Complete the stub implementation (see "Implementing a Connector")
2. Use OpenAI (fully supported)
3. Wait for official implementation

### Authentication Errors

```
Error: "Authentication failed: Invalid API key"
```

**Solution**:
- Verify API key format for each provider:
  - OpenAI: `sk-...`
  - Anthropic: `sk-ant-...`
  - ElevenLabs: Standard key
  - Deepgram: Standard key

### Connection Errors

```
Error: "WebSocket connection failed"
```

**Solution**:
- Check endpoint URL (use default or verify custom endpoint)
- Verify network connectivity
- Check firewall rules for WebSocket connections

## API Reference

### AIConnectorType Enum

```rust
pub enum AIConnectorType {
    OpenAI,       // ✅ Fully implemented
    Anthropic,    // 🚧 Stub implementation
    ElevenLabs,   // 🚧 Stub implementation
    Deepgram,     // 🚧 Stub implementation
    Dialogflow,   // 📋 Planned
    Lex,          // 📋 Planned
    Azure,        // 📋 Planned
    Custom,       // ✅ For custom implementations
}
```

### AIConnectorConfig Struct

```rust
pub struct AIConnectorConfig {
    pub connector_type: AIConnectorType,
    pub api_key: String,
    pub endpoint: Option<String>,
    pub model: String,
    pub voice: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub instructions: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub enable_vad: bool,
    pub enable_barge_in: bool,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}
```

## Roadmap

### v0.6.0 (Current)
- ✅ Multiple provider architecture
- ✅ AIConnectorType enum with new providers
- ✅ Stub implementations for Anthropic, ElevenLabs, Deepgram
- ✅ Comprehensive documentation

### v0.7.0 (Planned)
- 🚧 Complete Anthropic Claude Voice implementation
- 🚧 Complete ElevenLabs Conversational AI implementation
- 🚧 Complete Deepgram Voice Agent implementation
- 🚧 Provider comparison benchmarks

### v0.8.0 (Future)
- 📋 Google Dialogflow integration
- 📋 Amazon Lex integration
- 📋 Azure Cognitive Services integration
- 📋 Provider health checks and auto-failover

## Contributing

To contribute a provider implementation:

1. Fork the repository
2. Complete the stub implementation in `forge-ai-stream/src/<provider>.rs`
3. Add tests with your API key (don't commit the key!)
4. Update documentation with real-world examples
5. Submit a pull request

**Implementation Checklist**:
- [ ] WebSocket connection and authentication
- [ ] Audio streaming (encode/decode)
- [ ] Event parsing (protocol → AIEvent)
- [ ] Error handling
- [ ] Unit tests
- [ ] Integration tests (with real API)
- [ ] Documentation with examples
- [ ] Voice/model selection guide

## See Also

- [AI Integration Guide](./AI_INTEGRATION.md)
- [Conference AI Integration](./CONFERENCE_AI_INTEGRATION.md)
- [AI Session Persistence](./AI_SESSION_PERSISTENCE.md)

---

**Questions?** Check the [API documentation](../crates/forge-ai-stream/) or [open an issue](https://github.com/your-repo/forge-media/issues).
