# Forge Media Engine - New Features Design

## 1. SIPREC Support (RFC 7865/7866)

### Overview

SIPREC (Session Recording Protocol) enables compliant call recording by forking media streams to a Session Recording Server (SRS). Forge supports both SRC (Session Recording Client) and SRS roles.

### Crate Structure

```
forge-siprec/
├── src/
│   ├── lib.rs
│   ├── metadata.rs      # SIPREC metadata XML parsing/generation
│   ├── src_client.rs    # Session Recording Client implementation
│   ├── srs_server.rs    # Session Recording Server implementation
│   ├── session.rs       # Recording session management
│   └── config.rs        # SIPREC configuration
```

### SIPREC Metadata (RFC 7865)

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// SIPREC recording session metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingMetadata {
    pub recording_session_id: Uuid,
    pub session: SessionInfo,
    pub participants: Vec<Participant>,
    pub streams: Vec<MediaStream>,
    pub groups: Vec<ParticipantGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub start_time: DateTime<Utc>,
    pub communication_session_id: Option<String>,
    pub reason: Option<RecordingReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecordingReason {
    Compliance,
    QualityAssurance,
    Training,
    Legal,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub participant_id: Uuid,
    pub aor: String,                    // Address of Record (SIP URI)
    pub name: Option<String>,
    pub start_time: DateTime<Utc>,
    pub role: ParticipantRole,
    pub media_streams: Vec<Uuid>,       // Stream IDs for this participant
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParticipantRole {
    Caller,
    Callee,
    Agent,
    Customer,
    Supervisor,
    Observer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaStream {
    pub stream_id: Uuid,
    pub label: String,
    pub mode: StreamMode,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamMode {
    SendOnly,
    ReceiveOnly,
    SendReceive,
    Inactive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantGroup {
    pub group_id: Uuid,
    pub participant_ids: Vec<Uuid>,
    pub association_type: AssociationType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AssociationType {
    Calling,        // Participants on calling side
    Called,         // Participants on called side
    Consultation,   // Consultation call group
    Transfer,       // Transfer group
}
```

### SIPREC XML Parser/Generator

```rust
use quick_xml::{Reader, Writer, events::Event};

pub struct SiprecMetadataParser;

impl SiprecMetadataParser {
    /// Parse SIPREC metadata XML from SIP body
    pub fn parse(xml: &str) -> Result<RecordingMetadata, SiprecError> {
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        
        let mut metadata = RecordingMetadata::default();
        let mut buf = Vec::new();
        
        loop {
            match reader.read_event_into(&mut buf)? {
                Event::Start(e) => {
                    match e.name().as_ref() {
                        b"recording" => {
                            metadata.recording_session_id = Self::parse_uuid_attr(&e, "session")?;
                        }
                        b"session" => {
                            metadata.session = Self::parse_session(&mut reader)?;
                        }
                        b"participant" => {
                            metadata.participants.push(Self::parse_participant(&mut reader, &e)?);
                        }
                        b"stream" => {
                            metadata.streams.push(Self::parse_stream(&mut reader, &e)?);
                        }
                        b"group" => {
                            metadata.groups.push(Self::parse_group(&mut reader, &e)?);
                        }
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        
        Ok(metadata)
    }
    
    /// Generate SIPREC metadata XML for SIP body
    pub fn generate(metadata: &RecordingMetadata) -> Result<String, SiprecError> {
        let mut writer = Writer::new(Vec::new());
        
        // XML declaration
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
        
        // Root element with namespaces
        let mut recording = BytesStart::new("recording");
        recording.push_attribute(("xmlns", "urn:ietf:params:xml:ns:recording:1"));
        recording.push_attribute(("session", metadata.recording_session_id.to_string().as_str()));
        writer.write_event(Event::Start(recording))?;
        
        // Session info
        Self::write_session(&mut writer, &metadata.session)?;
        
        // Participants
        for participant in &metadata.participants {
            Self::write_participant(&mut writer, participant)?;
        }
        
        // Streams
        for stream in &metadata.streams {
            Self::write_stream(&mut writer, stream)?;
        }
        
        // Groups
        for group in &metadata.groups {
            Self::write_group(&mut writer, group)?;
        }
        
        writer.write_event(Event::End(BytesEnd::new("recording")))?;
        
        Ok(String::from_utf8(writer.into_inner())?)
    }
}
```

### Session Recording Client (SRC)

```rust
use tokio::sync::mpsc;

/// SRC - Sends recording sessions to SRS
pub struct SessionRecordingClient {
    config: SrcConfig,
    srs_endpoints: Vec<SrsEndpoint>,
    active_sessions: DashMap<Uuid, RecordingSession>,
    sip_client: Arc<SipClient>,  // From Siphon
}

#[derive(Debug, Clone)]
pub struct SrcConfig {
    /// SRS servers to send recordings to
    pub srs_servers: Vec<SrsEndpoint>,
    /// Recording mode
    pub mode: RecordingMode,
    /// Whether to fork all calls or selective
    pub selective: bool,
    /// Recording policy rules
    pub policy: RecordingPolicy,
    /// Failover to next SRS on failure
    pub failover_enabled: bool,
    /// Retry attempts per SRS
    pub max_retries: u32,
}

#[derive(Debug, Clone)]
pub struct SrsEndpoint {
    pub uri: String,
    pub transport: Transport,
    pub priority: u32,
    pub weight: u32,
}

#[derive(Debug, Clone)]
pub enum RecordingMode {
    /// Fork media at call setup
    EarlyMedia,
    /// Fork media when call is answered
    AnsweredOnly,
    /// Fork media on-demand via API
    OnDemand,
}

#[derive(Debug, Clone)]
pub struct RecordingPolicy {
    /// Record based on caller/callee patterns
    pub patterns: Vec<RecordingPattern>,
    /// Exclude patterns
    pub exclusions: Vec<String>,
    /// Record only specific directions
    pub direction: RecordingDirection,
}

#[derive(Debug, Clone)]
pub enum RecordingDirection {
    Both,
    Inbound,
    Outbound,
}

impl SessionRecordingClient {
    pub fn new(config: SrcConfig, sip_client: Arc<SipClient>) -> Self {
        Self {
            config,
            srs_endpoints: config.srs_servers.clone(),
            active_sessions: DashMap::new(),
            sip_client,
        }
    }
    
    /// Start recording a call
    pub async fn start_recording(
        &self,
        call_id: &CallId,
        participants: Vec<Participant>,
        streams: Vec<MediaStreamInfo>,
    ) -> Result<Uuid, SiprecError> {
        let recording_id = Uuid::new_v4();
        
        // Build metadata
        let metadata = RecordingMetadata {
            recording_session_id: recording_id,
            session: SessionInfo {
                session_id: call_id.to_string(),
                start_time: Utc::now(),
                communication_session_id: Some(call_id.to_string()),
                reason: Some(RecordingReason::Compliance),
            },
            participants,
            streams: streams.iter().map(|s| MediaStream {
                stream_id: s.stream_id,
                label: s.label.clone(),
                mode: StreamMode::SendReceive,
                start_time: Utc::now(),
                end_time: None,
            }).collect(),
            groups: vec![],
        };
        
        // Generate SDP for recording streams
        let sdp = self.generate_recording_sdp(&streams)?;
        
        // Send INVITE to SRS
        let srs = self.select_srs().await?;
        let invite = self.build_siprec_invite(&metadata, &sdp)?;
        
        let response = self.sip_client.send_request(invite).await?;
        
        if response.status.is_success() {
            // Parse SRS SDP answer
            let srs_sdp = response.body_as_sdp()?;
            
            // Create recording session
            let session = RecordingSession {
                id: recording_id,
                call_id: call_id.clone(),
                srs_endpoint: srs.clone(),
                metadata,
                srs_sdp,
                state: RecordingState::Active,
                start_time: Utc::now(),
            };
            
            self.active_sessions.insert(recording_id, session);
            
            // Start forking media to SRS
            self.start_media_fork(recording_id, &streams, &srs_sdp).await?;
            
            Ok(recording_id)
        } else {
            // Try failover to next SRS
            if self.config.failover_enabled {
                self.failover_recording(call_id, participants, streams).await
            } else {
                Err(SiprecError::SrsRejected(response.status))
            }
        }
    }
    
    /// Stop recording
    pub async fn stop_recording(&self, recording_id: Uuid) -> Result<(), SiprecError> {
        if let Some((_, mut session)) = self.active_sessions.remove(&recording_id) {
            // Stop media fork
            self.stop_media_fork(recording_id).await?;
            
            // Send BYE to SRS
            let bye = self.build_bye(&session)?;
            self.sip_client.send_request(bye).await?;
            
            session.state = RecordingState::Completed;
        }
        Ok(())
    }
    
    /// Update recording metadata (participant join/leave)
    pub async fn update_recording(
        &self,
        recording_id: Uuid,
        update: MetadataUpdate,
    ) -> Result<(), SiprecError> {
        if let Some(mut session) = self.active_sessions.get_mut(&recording_id) {
            // Apply update to metadata
            match update {
                MetadataUpdate::ParticipantJoined(p) => {
                    session.metadata.participants.push(p);
                }
                MetadataUpdate::ParticipantLeft(id) => {
                    session.metadata.participants.retain(|p| p.participant_id != id);
                }
                MetadataUpdate::StreamAdded(s) => {
                    session.metadata.streams.push(s);
                }
                MetadataUpdate::StreamRemoved(id) => {
                    session.metadata.streams.retain(|s| s.stream_id != id);
                }
            }
            
            // Send re-INVITE with updated metadata
            let reinvite = self.build_siprec_reinvite(&session)?;
            self.sip_client.send_request(reinvite).await?;
        }
        Ok(())
    }
    
    fn generate_recording_sdp(&self, streams: &[MediaStreamInfo]) -> Result<String, SiprecError> {
        let mut sdp = SdpBuilder::new()
            .origin("-", Uuid::new_v4().as_u128() as u64, 1, "IN", "IP4", "0.0.0.0")
            .session_name("SIPREC Recording")
            .connection("IN", "IP4", "0.0.0.0");
        
        for stream in streams {
            // Each stream gets its own m= line with label attribute
            sdp = sdp.media_audio(stream.port, &stream.codecs)
                .attribute("label", &stream.label)
                .attribute("sendonly", "");
        }
        
        Ok(sdp.build())
    }
    
    fn build_siprec_invite(
        &self,
        metadata: &RecordingMetadata,
        sdp: &str,
    ) -> Result<SipRequest, SiprecError> {
        let metadata_xml = SiprecMetadataParser::generate(metadata)?;
        
        // Build multipart MIME body
        let boundary = format!("siprec-{}", Uuid::new_v4());
        let body = format!(
            "--{boundary}\r\n\
            Content-Type: application/sdp\r\n\r\n\
            {sdp}\r\n\
            --{boundary}\r\n\
            Content-Type: application/rs-metadata+xml\r\n\
            Content-Disposition: recording-session\r\n\r\n\
            {metadata_xml}\r\n\
            --{boundary}--\r\n",
            boundary = boundary,
            sdp = sdp,
            metadata_xml = metadata_xml
        );
        
        let invite = SipRequest::builder()
            .method(Method::INVITE)
            .uri(&self.srs_endpoints[0].uri)
            .header("Require", "siprec")
            .header("Content-Type", &format!("multipart/mixed;boundary={}", boundary))
            .body(body)
            .build();
        
        Ok(invite)
    }
    
    async fn start_media_fork(
        &self,
        recording_id: Uuid,
        streams: &[MediaStreamInfo],
        srs_sdp: &SessionDescription,
    ) -> Result<(), SiprecError> {
        // For each stream, create a fork that copies packets to SRS
        for (i, stream) in streams.iter().enumerate() {
            let srs_media = srs_sdp.media.get(i)
                .ok_or(SiprecError::SdpMismatch)?;
            
            // Get SRS RTP address from SDP
            let srs_addr = SocketAddr::new(
                srs_sdp.connection.as_ref()
                    .map(|c| c.address.parse().unwrap())
                    .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                srs_media.port,
            );
            
            // Register fork with media engine
            self.register_media_fork(
                stream.stream_id,
                srs_addr,
                recording_id,
            ).await?;
        }
        Ok(())
    }
}
```

### Session Recording Server (SRS)

```rust
/// SRS - Receives and stores recording sessions
pub struct SessionRecordingServer {
    config: SrsConfig,
    sessions: DashMap<Uuid, SrsSession>,
    storage: Arc<dyn RecordingStorage>,
    sip_server: Arc<SipServer>,  // From Siphon
}

#[derive(Debug, Clone)]
pub struct SrsConfig {
    /// Listen address for SIPREC INVITEs
    pub listen_addr: SocketAddr,
    /// RTP port range for receiving media
    pub rtp_port_range: (u16, u16),
    /// Storage backend
    pub storage_backend: StorageBackend,
    /// Recording format
    pub format: RecordingFormat,
    /// Metadata storage
    pub metadata_storage: MetadataStorage,
    /// Maximum concurrent recordings
    pub max_sessions: usize,
}

pub struct SrsSession {
    pub id: Uuid,
    pub metadata: RecordingMetadata,
    pub state: RecordingState,
    pub streams: Vec<RecordingStream>,
    pub start_time: DateTime<Utc>,
    pub dialog: SipDialog,
}

pub struct RecordingStream {
    pub stream_id: Uuid,
    pub label: String,
    pub local_port: u16,
    pub codec: Codec,
    pub recorder: ActiveRecording,
}

impl SessionRecordingServer {
    pub fn new(
        config: SrsConfig,
        storage: Arc<dyn RecordingStorage>,
        sip_server: Arc<SipServer>,
    ) -> Self {
        Self {
            config,
            sessions: DashMap::new(),
            storage,
            sip_server,
        }
    }
    
    /// Handle incoming SIPREC INVITE
    pub async fn handle_invite(&self, request: &SipRequest) -> Result<SipResponse, SiprecError> {
        // Check Require header for siprec
        if !request.header("Require").map(|h| h.contains("siprec")).unwrap_or(false) {
            return Ok(SipResponse::bad_request("Missing Require: siprec"));
        }
        
        // Parse multipart body
        let (sdp, metadata) = self.parse_siprec_body(request)?;
        
        // Check capacity
        if self.sessions.len() >= self.config.max_sessions {
            return Ok(SipResponse::service_unavailable("Recording capacity exceeded"));
        }
        
        // Allocate RTP ports for each stream
        let mut streams = Vec::new();
        let mut answer_sdp = SdpBuilder::new()
            .origin("-", Uuid::new_v4().as_u128() as u64, 1, "IN", "IP4", 
                    &self.config.listen_addr.ip().to_string())
            .session_name("SIPREC SRS")
            .connection("IN", "IP4", &self.config.listen_addr.ip().to_string());
        
        for (i, media) in sdp.media.iter().enumerate() {
            let port = self.allocate_port()?;
            let stream_id = metadata.streams.get(i)
                .map(|s| s.stream_id)
                .unwrap_or_else(Uuid::new_v4);
            
            // Select codec
            let codec = self.select_codec(media)?;
            
            // Start recording for this stream
            let recorder = self.start_stream_recording(
                &metadata,
                stream_id,
                port,
                &codec,
            ).await?;
            
            streams.push(RecordingStream {
                stream_id,
                label: metadata.streams.get(i)
                    .map(|s| s.label.clone())
                    .unwrap_or_else(|| format!("stream-{}", i)),
                local_port: port,
                codec: codec.clone(),
                recorder,
            });
            
            // Add to answer SDP
            answer_sdp = answer_sdp.media_audio(port, &[codec])
                .attribute("recvonly", "");
        }
        
        // Create session
        let session = SrsSession {
            id: metadata.recording_session_id,
            metadata: metadata.clone(),
            state: RecordingState::Active,
            streams,
            start_time: Utc::now(),
            dialog: SipDialog::from_request(request),
        };
        
        self.sessions.insert(session.id, session);
        
        // Store metadata
        self.storage.store_metadata(&metadata).await?;
        
        // Build 200 OK response
        let response = SipResponse::ok()
            .header("Content-Type", "application/sdp")
            .body(answer_sdp.build());
        
        Ok(response)
    }
    
    /// Handle BYE - recording ended
    pub async fn handle_bye(&self, request: &SipRequest) -> Result<SipResponse, SiprecError> {
        let dialog = SipDialog::from_request(request);
        
        // Find session by dialog
        let session_id = self.find_session_by_dialog(&dialog)?;
        
        if let Some((_, mut session)) = self.sessions.remove(&session_id) {
            // Stop all stream recordings
            for stream in &mut session.streams {
                stream.recorder.stop().await?;
            }
            
            session.state = RecordingState::Completed;
            
            // Finalize metadata
            let mut metadata = session.metadata.clone();
            for stream in &mut metadata.streams {
                stream.end_time = Some(Utc::now());
            }
            self.storage.update_metadata(&metadata).await?;
            
            // Emit completion event
            self.emit_event(SrsEvent::RecordingCompleted {
                recording_id: session_id,
                duration: Utc::now() - session.start_time,
                metadata,
            }).await;
        }
        
        Ok(SipResponse::ok())
    }
    
    /// Handle re-INVITE - metadata update
    pub async fn handle_reinvite(&self, request: &SipRequest) -> Result<SipResponse, SiprecError> {
        let (sdp, metadata) = self.parse_siprec_body(request)?;
        
        if let Some(mut session) = self.sessions.get_mut(&metadata.recording_session_id) {
            // Update metadata
            session.metadata = metadata.clone();
            self.storage.update_metadata(&metadata).await?;
            
            // Handle stream changes
            // ... add/remove streams as needed
        }
        
        Ok(SipResponse::ok())
    }
    
    async fn start_stream_recording(
        &self,
        metadata: &RecordingMetadata,
        stream_id: Uuid,
        port: u16,
        codec: &Codec,
    ) -> Result<ActiveRecording, SiprecError> {
        let recording_path = self.generate_recording_path(metadata, stream_id);
        
        let recording = RecordingBuilder::new()
            .format(self.config.format.clone())
            .codec(codec.clone())
            .path(&recording_path)
            .metadata(RecordingMetadataFile {
                recording_session_id: metadata.recording_session_id,
                stream_id,
                participants: metadata.participants.clone(),
            })
            .build()?;
        
        // Start receiving RTP on allocated port
        let socket = UdpSocket::bind(SocketAddr::new(
            self.config.listen_addr.ip(),
            port,
        )).await?;
        
        let active = recording.start_with_socket(socket).await?;
        
        Ok(active)
    }
}
```

---

## 2. AI Service Streaming

### Overview

Stream real-time audio to AI services for voice assistants, real-time transcription, sentiment analysis, and interactive IVR. Supports bidirectional streaming for conversational AI.

### Crate Structure

```
forge-ai-stream/
├── src/
│   ├── lib.rs
│   ├── config.rs
│   ├── session.rs           # AI streaming session management
│   ├── audio_bridge.rs      # RTP to AI audio format bridge
│   ├── providers/
│   │   ├── mod.rs
│   │   ├── openai.rs        # OpenAI Realtime API
│   │   ├── google.rs        # Google Cloud Speech/Dialogflow
│   │   ├── dialogflow.rs    # Dialogflow CX
│   │   ├── aws.rs           # Amazon Lex/Transcribe
│   │   ├── azure.rs         # Azure Speech/Bot Framework
│   │   └── custom.rs        # Custom WebSocket services
│   └── events.rs
```

### Core Types

```rust
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// AI service provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AiProvider {
    OpenAI {
        api_key: String,
        model: String,  // "gpt-4o-realtime-preview"
        voice: String,  // "alloy", "echo", "shimmer"
        instructions: Option<String>,
        tools: Vec<AiTool>,
    },
    GoogleSpeech {
        credentials_path: String,
        language_code: String,
        model: String,  // "latest_long", "latest_short", "telephony"
        enable_automatic_punctuation: bool,
    },
    DialogflowCX {
        project_id: String,
        location: String,
        agent_id: String,
        credentials_path: String,
        language_code: String,
        session_parameters: HashMap<String, Value>,
    },
    AmazonLex {
        bot_id: String,
        bot_alias_id: String,
        locale_id: String,
        region: String,
        session_attributes: HashMap<String, String>,
    },
    AzureSpeech {
        subscription_key: String,
        region: String,
        language: String,
        voice: String,
        bot_framework: Option<AzureBotConfig>,
    },
    Custom {
        websocket_url: String,
        auth_header: Option<String>,
        protocol: CustomProtocol,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema
}

/// AI streaming session configuration
#[derive(Debug, Clone)]
pub struct AiStreamConfig {
    pub provider: AiProvider,
    /// Input audio format (from RTP)
    pub input_format: AudioFormat,
    /// Output audio format (to RTP)
    pub output_format: AudioFormat,
    /// Enable VAD for barge-in
    pub enable_vad: bool,
    /// Barge-in sensitivity
    pub barge_in_threshold: f32,
    /// Maximum session duration
    pub max_duration: Duration,
    /// Idle timeout
    pub idle_timeout: Duration,
    /// Webhook for events
    pub webhook_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub encoding: AudioEncoding,
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Clone)]
pub enum AudioEncoding {
    LinearPcm16,
    Mulaw,
    Alaw,
    Opus,
}

/// Events from AI service
#[derive(Debug, Clone, Serialize)]
pub enum AiEvent {
    /// Transcription result
    Transcript {
        text: String,
        is_final: bool,
        confidence: f32,
    },
    /// AI response started
    ResponseStarted {
        response_id: String,
    },
    /// AI text response
    ResponseText {
        response_id: String,
        text: String,
        is_final: bool,
    },
    /// AI audio response (for TTS playback)
    ResponseAudio {
        response_id: String,
        audio: Vec<i16>,
        sample_rate: u32,
    },
    /// Intent detected (Dialogflow/Lex)
    IntentDetected {
        intent_name: String,
        confidence: f32,
        parameters: HashMap<String, Value>,
    },
    /// Tool/function call requested
    ToolCall {
        tool_name: String,
        call_id: String,
        arguments: serde_json::Value,
    },
    /// Session ended
    SessionEnded {
        reason: EndReason,
        summary: Option<String>,
    },
    /// Error occurred
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub enum EndReason {
    Completed,
    UserHangup,
    Timeout,
    Error,
    MaxDuration,
}
```

### AI Streaming Session

```rust
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::WebSocketStream;

/// Manages a single AI streaming session
pub struct AiStreamSession {
    id: Uuid,
    config: AiStreamConfig,
    provider: Box<dyn AiProviderConnection>,
    state: watch::Sender<SessionState>,
    
    // Audio channels
    audio_input: mpsc::Sender<AudioFrame>,
    audio_output: mpsc::Receiver<AudioFrame>,
    
    // Event channel
    events: mpsc::Sender<AiEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    Connecting,
    Active,
    Processing,  // AI is generating response
    Paused,
    Ended,
}

/// Audio frame for streaming
pub struct AudioFrame {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub timestamp: u64,
}

#[async_trait]
pub trait AiProviderConnection: Send + Sync {
    /// Connect to the AI service
    async fn connect(&mut self) -> Result<(), AiStreamError>;
    
    /// Send audio to AI service
    async fn send_audio(&mut self, audio: &AudioFrame) -> Result<(), AiStreamError>;
    
    /// Receive events from AI service
    async fn receive(&mut self) -> Result<Option<AiEvent>, AiStreamError>;
    
    /// Send tool result back to AI
    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: serde_json::Value,
    ) -> Result<(), AiStreamError>;
    
    /// Interrupt current response (barge-in)
    async fn interrupt(&mut self) -> Result<(), AiStreamError>;
    
    /// Close connection
    async fn close(&mut self) -> Result<(), AiStreamError>;
}

impl AiStreamSession {
    pub async fn new(
        config: AiStreamConfig,
        events_tx: mpsc::Sender<AiEvent>,
    ) -> Result<Self, AiStreamError> {
        let (audio_in_tx, audio_in_rx) = mpsc::channel(100);
        let (audio_out_tx, audio_out_rx) = mpsc::channel(100);
        let (state_tx, _) = watch::channel(SessionState::Connecting);
        
        // Create provider connection
        let provider: Box<dyn AiProviderConnection> = match &config.provider {
            AiProvider::OpenAI { .. } => {
                Box::new(OpenAiRealtimeConnection::new(&config).await?)
            }
            AiProvider::GoogleSpeech { .. } => {
                Box::new(GoogleSpeechConnection::new(&config).await?)
            }
            AiProvider::DialogflowCX { .. } => {
                Box::new(DialogflowConnection::new(&config).await?)
            }
            AiProvider::AmazonLex { .. } => {
                Box::new(AmazonLexConnection::new(&config).await?)
            }
            AiProvider::AzureSpeech { .. } => {
                Box::new(AzureSpeechConnection::new(&config).await?)
            }
            AiProvider::Custom { .. } => {
                Box::new(CustomWebSocketConnection::new(&config).await?)
            }
        };
        
        let session = Self {
            id: Uuid::new_v4(),
            config,
            provider,
            state: state_tx,
            audio_input: audio_in_tx,
            audio_output: audio_out_rx,
            events: events_tx,
        };
        
        Ok(session)
    }
    
    /// Start the streaming session
    pub async fn start(mut self) -> Result<AiStreamHandle, AiStreamError> {
        // Connect to provider
        self.provider.connect().await?;
        self.state.send(SessionState::Active)?;
        
        let id = self.id;
        let (stop_tx, stop_rx) = oneshot::channel();
        
        // Spawn processing task
        let handle = tokio::spawn(async move {
            self.run(stop_rx).await
        });
        
        Ok(AiStreamHandle {
            id,
            audio_input: self.audio_input.clone(),
            stop: Some(stop_tx),
            task: handle,
        })
    }
    
    async fn run(mut self, mut stop: oneshot::Receiver<()>) {
        let mut audio_rx = self.audio_input;  // Take ownership
        let vad = if self.config.enable_vad {
            Some(VoiceActivityDetector::new(self.config.barge_in_threshold))
        } else {
            None
        };
        
        loop {
            tokio::select! {
                // Receive audio from RTP
                Some(frame) = audio_rx.recv() => {
                    // Check for barge-in
                    if let Some(ref vad) = vad {
                        if vad.is_speech(&frame.samples) && 
                           *self.state.borrow() == SessionState::Processing {
                            // User is speaking during AI response - interrupt
                            if let Err(e) = self.provider.interrupt().await {
                                tracing::warn!("Failed to interrupt: {}", e);
                            }
                            self.events.send(AiEvent::ResponseInterrupted).await.ok();
                        }
                    }
                    
                    // Send audio to AI
                    if let Err(e) = self.provider.send_audio(&frame).await {
                        tracing::error!("Failed to send audio: {}", e);
                        break;
                    }
                }
                
                // Receive events from AI
                result = self.provider.receive() => {
                    match result {
                        Ok(Some(event)) => {
                            // Handle special events
                            match &event {
                                AiEvent::ResponseStarted { .. } => {
                                    self.state.send(SessionState::Processing).ok();
                                }
                                AiEvent::ResponseAudio { audio, sample_rate, .. } => {
                                    // Send audio to output for RTP
                                    let frame = AudioFrame {
                                        samples: audio.clone(),
                                        sample_rate: *sample_rate,
                                        timestamp: 0,  // Will be set by RTP sender
                                    };
                                    self.audio_output.send(frame).await.ok();
                                }
                                AiEvent::SessionEnded { .. } => {
                                    self.state.send(SessionState::Ended).ok();
                                }
                                _ => {}
                            }
                            
                            // Forward event
                            self.events.send(event).await.ok();
                        }
                        Ok(None) => {
                            // Connection closed
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Provider error: {}", e);
                            self.events.send(AiEvent::Error {
                                code: "PROVIDER_ERROR".to_string(),
                                message: e.to_string(),
                            }).await.ok();
                            break;
                        }
                    }
                }
                
                // Stop signal
                _ = &mut stop => {
                    break;
                }
            }
        }
        
        // Cleanup
        self.provider.close().await.ok();
        self.state.send(SessionState::Ended).ok();
    }
}

/// Handle for controlling an active AI stream session
pub struct AiStreamHandle {
    pub id: Uuid,
    audio_input: mpsc::Sender<AudioFrame>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl AiStreamHandle {
    /// Send audio frame to AI
    pub async fn send_audio(&self, frame: AudioFrame) -> Result<(), AiStreamError> {
        self.audio_input.send(frame).await
            .map_err(|_| AiStreamError::SessionClosed)
    }
    
    /// Stop the session
    pub async fn stop(mut self) -> Result<(), AiStreamError> {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        self.task.await?;
        Ok(())
    }
}
```

### OpenAI Realtime API Provider

```rust
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};

pub struct OpenAiRealtimeConnection {
    config: OpenAiConfig,
    ws: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    response_in_progress: bool,
}

#[derive(Debug, Clone)]
struct OpenAiConfig {
    api_key: String,
    model: String,
    voice: String,
    instructions: Option<String>,
    tools: Vec<AiTool>,
}

impl OpenAiRealtimeConnection {
    pub async fn new(config: &AiStreamConfig) -> Result<Self, AiStreamError> {
        let openai_config = match &config.provider {
            AiProvider::OpenAI { api_key, model, voice, instructions, tools } => {
                OpenAiConfig {
                    api_key: api_key.clone(),
                    model: model.clone(),
                    voice: voice.clone(),
                    instructions: instructions.clone(),
                    tools: tools.clone(),
                }
            }
            _ => return Err(AiStreamError::InvalidProvider),
        };
        
        Ok(Self {
            config: openai_config,
            ws: None,
            response_in_progress: false,
        })
    }
}

#[async_trait]
impl AiProviderConnection for OpenAiRealtimeConnection {
    async fn connect(&mut self) -> Result<(), AiStreamError> {
        let url = format!(
            "wss://api.openai.com/v1/realtime?model={}",
            self.config.model
        );
        
        let request = Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("OpenAI-Beta", "realtime=v1")
            .body(())?;
        
        let (ws, _) = connect_async(request).await?;
        self.ws = Some(ws);
        
        // Send session configuration
        let session_config = serde_json::json!({
            "type": "session.update",
            "session": {
                "modalities": ["text", "audio"],
                "instructions": self.config.instructions,
                "voice": self.config.voice,
                "input_audio_format": "pcm16",
                "output_audio_format": "pcm16",
                "input_audio_transcription": {
                    "model": "whisper-1"
                },
                "turn_detection": {
                    "type": "server_vad",
                    "threshold": 0.5,
                    "prefix_padding_ms": 300,
                    "silence_duration_ms": 500
                },
                "tools": self.config.tools.iter().map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    })
                }).collect::<Vec<_>>()
            }
        });
        
        if let Some(ws) = &mut self.ws {
            ws.send(Message::Text(session_config.to_string())).await?;
        }
        
        Ok(())
    }
    
    async fn send_audio(&mut self, audio: &AudioFrame) -> Result<(), AiStreamError> {
        if let Some(ws) = &mut self.ws {
            // Convert to base64-encoded PCM16
            let audio_bytes: Vec<u8> = audio.samples.iter()
                .flat_map(|s| s.to_le_bytes())
                .collect();
            let audio_b64 = base64::encode(&audio_bytes);
            
            let msg = serde_json::json!({
                "type": "input_audio_buffer.append",
                "audio": audio_b64
            });
            
            ws.send(Message::Text(msg.to_string())).await?;
        }
        Ok(())
    }
    
    async fn receive(&mut self) -> Result<Option<AiEvent>, AiStreamError> {
        if let Some(ws) = &mut self.ws {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let event: serde_json::Value = serde_json::from_str(&text)?;
                    
                    match event["type"].as_str() {
                        Some("conversation.item.input_audio_transcription.completed") => {
                            return Ok(Some(AiEvent::Transcript {
                                text: event["transcript"].as_str().unwrap_or("").to_string(),
                                is_final: true,
                                confidence: 1.0,
                            }));
                        }
                        Some("response.created") => {
                            self.response_in_progress = true;
                            return Ok(Some(AiEvent::ResponseStarted {
                                response_id: event["response"]["id"].as_str()
                                    .unwrap_or("").to_string(),
                            }));
                        }
                        Some("response.audio.delta") => {
                            // Decode base64 audio
                            let audio_b64 = event["delta"].as_str().unwrap_or("");
                            let audio_bytes = base64::decode(audio_b64)?;
                            let samples: Vec<i16> = audio_bytes.chunks_exact(2)
                                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                                .collect();
                            
                            return Ok(Some(AiEvent::ResponseAudio {
                                response_id: event["response_id"].as_str()
                                    .unwrap_or("").to_string(),
                                audio: samples,
                                sample_rate: 24000,  // OpenAI uses 24kHz
                            }));
                        }
                        Some("response.audio_transcript.delta") => {
                            return Ok(Some(AiEvent::ResponseText {
                                response_id: event["response_id"].as_str()
                                    .unwrap_or("").to_string(),
                                text: event["delta"].as_str().unwrap_or("").to_string(),
                                is_final: false,
                            }));
                        }
                        Some("response.done") => {
                            self.response_in_progress = false;
                        }
                        Some("response.function_call_arguments.done") => {
                            return Ok(Some(AiEvent::ToolCall {
                                tool_name: event["name"].as_str().unwrap_or("").to_string(),
                                call_id: event["call_id"].as_str().unwrap_or("").to_string(),
                                arguments: serde_json::from_str(
                                    event["arguments"].as_str().unwrap_or("{}")
                                )?,
                            }));
                        }
                        Some("error") => {
                            return Ok(Some(AiEvent::Error {
                                code: event["error"]["code"].as_str()
                                    .unwrap_or("UNKNOWN").to_string(),
                                message: event["error"]["message"].as_str()
                                    .unwrap_or("Unknown error").to_string(),
                            }));
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) => {
                    return Ok(Some(AiEvent::SessionEnded {
                        reason: EndReason::Completed,
                        summary: None,
                    }));
                }
                Some(Err(e)) => {
                    return Err(AiStreamError::WebSocket(e));
                }
                None => {
                    return Ok(None);
                }
                _ => {}
            }
        }
        Ok(None)
    }
    
    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: serde_json::Value,
    ) -> Result<(), AiStreamError> {
        if let Some(ws) = &mut self.ws {
            let msg = serde_json::json!({
                "type": "conversation.item.create",
                "item": {
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": result.to_string()
                }
            });
            ws.send(Message::Text(msg.to_string())).await?;
            
            // Trigger response generation
            let response_msg = serde_json::json!({
                "type": "response.create"
            });
            ws.send(Message::Text(response_msg.to_string())).await?;
        }
        Ok(())
    }
    
    async fn interrupt(&mut self) -> Result<(), AiStreamError> {
        if let Some(ws) = &mut self.ws {
            let msg = serde_json::json!({
                "type": "response.cancel"
            });
            ws.send(Message::Text(msg.to_string())).await?;
        }
        Ok(())
    }
    
    async fn close(&mut self) -> Result<(), AiStreamError> {
        if let Some(ws) = &mut self.ws {
            ws.close(None).await?;
        }
        Ok(())
    }
}
```

### Google Dialogflow CX Provider

```rust
use google_cloud_dialogflow_cx::client::SessionsClient;

pub struct DialogflowConnection {
    config: DialogflowConfig,
    client: Option<SessionsClient>,
    session_path: String,
    audio_config: InputAudioConfig,
}

#[derive(Debug, Clone)]
struct DialogflowConfig {
    project_id: String,
    location: String,
    agent_id: String,
    credentials_path: String,
    language_code: String,
    session_parameters: HashMap<String, Value>,
}

impl DialogflowConnection {
    pub async fn new(config: &AiStreamConfig) -> Result<Self, AiStreamError> {
        let df_config = match &config.provider {
            AiProvider::DialogflowCX { 
                project_id, location, agent_id, 
                credentials_path, language_code, session_parameters 
            } => {
                DialogflowConfig {
                    project_id: project_id.clone(),
                    location: location.clone(),
                    agent_id: agent_id.clone(),
                    credentials_path: credentials_path.clone(),
                    language_code: language_code.clone(),
                    session_parameters: session_parameters.clone(),
                }
            }
            _ => return Err(AiStreamError::InvalidProvider),
        };
        
        let session_id = Uuid::new_v4().to_string();
        let session_path = format!(
            "projects/{}/locations/{}/agents/{}/sessions/{}",
            df_config.project_id,
            df_config.location,
            df_config.agent_id,
            session_id
        );
        
        Ok(Self {
            config: df_config,
            client: None,
            session_path,
            audio_config: InputAudioConfig {
                audio_encoding: AudioEncoding::Linear16,
                sample_rate_hertz: 16000,
                language_code: df_config.language_code.clone(),
                enable_word_info: true,
                ..Default::default()
            },
        })
    }
}

#[async_trait]
impl AiProviderConnection for DialogflowConnection {
    async fn connect(&mut self) -> Result<(), AiStreamError> {
        // Set credentials
        std::env::set_var("GOOGLE_APPLICATION_CREDENTIALS", &self.config.credentials_path);
        
        let client = SessionsClient::new().await?;
        self.client = Some(client);
        
        Ok(())
    }
    
    async fn send_audio(&mut self, audio: &AudioFrame) -> Result<(), AiStreamError> {
        if let Some(client) = &self.client {
            // Convert samples to bytes
            let audio_bytes: Vec<u8> = audio.samples.iter()
                .flat_map(|s| s.to_le_bytes())
                .collect();
            
            let request = StreamingDetectIntentRequest {
                session: self.session_path.clone(),
                query_input: Some(QueryInput {
                    input: Some(query_input::Input::Audio(AudioInput {
                        config: Some(self.audio_config.clone()),
                        audio: audio_bytes,
                    })),
                    language_code: self.config.language_code.clone(),
                }),
                ..Default::default()
            };
            
            // Send via streaming
            client.streaming_detect_intent(request).await?;
        }
        Ok(())
    }
    
    async fn receive(&mut self) -> Result<Option<AiEvent>, AiStreamError> {
        // Handle streaming response
        // This would be implemented with proper gRPC streaming
        Ok(None)
    }
    
    async fn send_tool_result(
        &mut self,
        _call_id: &str,
        _result: serde_json::Value,
    ) -> Result<(), AiStreamError> {
        // Dialogflow uses webhook fulfillment instead of tool results
        Ok(())
    }
    
    async fn interrupt(&mut self) -> Result<(), AiStreamError> {
        // Dialogflow doesn't support mid-stream interruption
        Ok(())
    }
    
    async fn close(&mut self) -> Result<(), AiStreamError> {
        self.client = None;
        Ok(())
    }
}
```

### Audio Bridge (RTP ↔ AI)

```rust
/// Bridges RTP audio to AI service format
pub struct AiAudioBridge {
    call_id: CallId,
    leg: LegId,
    session: AiStreamHandle,
    resampler: Option<Resampler>,
    decoder: Box<dyn Decoder>,
    encoder: Box<dyn Encoder>,
}

impl AiAudioBridge {
    pub fn new(
        call_id: CallId,
        leg: LegId,
        session: AiStreamHandle,
        rtp_codec: Codec,
        ai_format: AudioFormat,
    ) -> Result<Self, AiStreamError> {
        let decoder = create_decoder(&rtp_codec)?;
        let encoder = create_encoder(&rtp_codec)?;
        
        // Resampler if sample rates differ
        let resampler = if decoder.sample_rate() != ai_format.sample_rate {
            Some(Resampler::new(
                decoder.sample_rate(),
                ai_format.sample_rate,
            )?)
        } else {
            None
        };
        
        Ok(Self {
            call_id,
            leg,
            session,
            resampler,
            decoder,
            encoder,
        })
    }
    
    /// Process incoming RTP packet, send to AI
    pub async fn process_rtp_to_ai(&mut self, packet: &RtpPacket) -> Result<(), AiStreamError> {
        // Decode RTP payload
        let mut samples = self.decoder.decode(&packet.payload)?;
        
        // Resample if needed
        if let Some(ref mut resampler) = self.resampler {
            samples = resampler.process(&samples)?;
        }
        
        // Send to AI
        let frame = AudioFrame {
            samples,
            sample_rate: self.session.config.input_format.sample_rate,
            timestamp: packet.header.timestamp as u64,
        };
        
        self.session.send_audio(frame).await?;
        
        Ok(())
    }
    
    /// Process AI audio output, generate RTP
    pub fn process_ai_to_rtp(
        &mut self,
        ai_audio: &[i16],
        ai_sample_rate: u32,
    ) -> Result<Vec<RtpPacket>, AiStreamError> {
        let mut samples = ai_audio.to_vec();
        
        // Resample to RTP codec rate if needed
        if ai_sample_rate != self.encoder.sample_rate() {
            let mut resampler = Resampler::new(ai_sample_rate, self.encoder.sample_rate())?;
            samples = resampler.process(&samples)?;
        }
        
        // Encode and packetize
        let mut packets = Vec::new();
        let frame_size = self.encoder.frame_size();
        
        for chunk in samples.chunks(frame_size) {
            let encoded = self.encoder.encode(chunk)?;
            // Create RTP packet (header will be filled by RTP sender)
            packets.push(encoded);
        }
        
        Ok(packets)
    }
}
```

---

## 3. Media Failover & High Availability

### Overview

Seamless media failover ensures RTP streams continue when a media server fails. This requires session state replication, coordinated failover, and minimal packet loss.

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Media Cluster                                │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────┐     │
│  │  Forge #1    │────▶│   Redis/     │◀────│  Forge #2    │     │
│  │  (Primary)   │     │   etcd       │     │  (Standby)   │     │
│  └──────┬───────┘     │  (State)     │     └──────┬───────┘     │
│         │             └──────────────┘            │              │
│         │                                         │              │
│         ▼                                         ▼              │
│  ┌──────────────┐                         ┌──────────────┐      │
│  │ RTP Streams  │                         │ RTP Streams  │      │
│  │ 30000-40000  │                         │ 30000-40000  │      │
│  └──────────────┘                         └──────────────┘      │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │              Virtual IP / Anycast / Load Balancer         │   │
│  │                      203.0.113.50                         │   │
│  └──────────────────────────────────────────────────────────┘   │
│                              │                                   │
└──────────────────────────────┼───────────────────────────────────┘
                               │
                               ▼
                        ┌──────────────┐
                        │   Clients    │
                        └──────────────┘
```

### Crate Structure

```
forge-ha/
├── src/
│   ├── lib.rs
│   ├── config.rs
│   ├── cluster.rs           # Cluster membership
│   ├── state_sync.rs        # State replication
│   ├── failover.rs          # Failover coordination
│   ├── vip.rs               # Virtual IP management
│   ├── health.rs            # Health monitoring
│   └── storage/
│       ├── mod.rs
│       ├── redis.rs         # Redis backend
│       └── etcd.rs          # etcd backend
```

### Core Types

```rust
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// HA cluster configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HaConfig {
    /// This node's ID
    pub node_id: String,
    /// This node's address
    pub node_addr: SocketAddr,
    /// Cluster mode
    pub mode: ClusterMode,
    /// State storage backend
    pub storage: StorageConfig,
    /// Peer nodes
    pub peers: Vec<PeerConfig>,
    /// Virtual IP configuration
    pub vip: Option<VipConfig>,
    /// Health check settings
    pub health: HealthConfig,
    /// Failover settings
    pub failover: FailoverConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClusterMode {
    /// One active, one standby
    ActiveStandby,
    /// All nodes active, sessions distributed
    ActiveActive,
    /// N active, 1+ standby
    NPlus1 { active_count: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StorageConfig {
    Redis {
        url: String,
        cluster_mode: bool,
        key_prefix: String,
    },
    Etcd {
        endpoints: Vec<String>,
        prefix: String,
    },
    /// In-memory for testing (no persistence)
    Memory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub node_id: String,
    pub addr: SocketAddr,
    /// RPC port for cluster communication
    pub rpc_port: u16,
    /// Priority (lower = higher priority for primary)
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VipConfig {
    /// Virtual IP address
    pub address: IpAddr,
    /// Network interface for VIP
    pub interface: String,
    /// Gratuitous ARP on failover
    pub garp_enabled: bool,
    /// Use BGP anycast instead of VIP
    pub anycast: Option<AnycastConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnycastConfig {
    /// BGP router ID
    pub router_id: IpAddr,
    /// Local AS number
    pub local_as: u32,
    /// BGP neighbors
    pub neighbors: Vec<BgpNeighbor>,
    /// Routes to announce
    pub announce_routes: Vec<IpNetwork>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
    /// Peer timeout before failover
    pub peer_timeout: Duration,
    /// Number of missed heartbeats before failover
    pub missed_heartbeats: u32,
    /// Health check endpoints
    pub check_endpoints: Vec<HealthEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverConfig {
    /// Automatic failover enabled
    pub auto_failover: bool,
    /// Minimum time between failovers
    pub failover_cooldown: Duration,
    /// Maximum sessions to migrate per second
    pub migration_rate: u32,
    /// Notify SIP servers on failover
    pub notify_sip_servers: bool,
    /// Graceful takeover (wait for current packets)
    pub graceful: bool,
}

/// Node role in cluster
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    Primary,
    Standby,
    /// Receiving sessions during failover
    TakingOver,
    /// Transitioning sessions during planned maintenance
    Draining,
}

/// Session state for replication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicatedSessionState {
    pub call_id: CallId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub owning_node: String,
    
    /// Legs in this session
    pub legs: Vec<ReplicatedLegState>,
    /// Connections between legs
    pub connections: Vec<(LegId, LegId)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicatedLegState {
    pub leg_id: LegId,
    pub local_addr: SocketAddr,
    pub remote_addr: Option<SocketAddr>,
    
    /// SRTP keys (encrypted in storage)
    pub srtp_send_key: EncryptedKey,
    pub srtp_recv_key: EncryptedKey,
    pub srtp_profile: SrtpProfile,
    
    /// RTP state
    pub rtp_seq: u16,
    pub rtp_ts: u32,
    pub rtp_ssrc: u32,
    
    /// RTCP state
    pub rtcp_sr_ts: Option<u32>,
    pub rtcp_rr_seq: u16,
    
    /// Jitter buffer state
    pub jitter_buffer_delay: u32,
    
    /// Codec
    pub codec: Codec,
    pub payload_type: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKey {
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; 12],
}
```

### Cluster Manager

```rust
use tokio::sync::{mpsc, watch, RwLock};

/// Manages cluster membership and coordination
pub struct ClusterManager {
    config: HaConfig,
    node_id: String,
    role: watch::Sender<NodeRole>,
    peers: RwLock<HashMap<String, PeerState>>,
    storage: Arc<dyn StateStorage>,
    vip_manager: Option<VipManager>,
    health_monitor: HealthMonitor,
}

pub struct PeerState {
    pub config: PeerConfig,
    pub last_heartbeat: Instant,
    pub role: NodeRole,
    pub session_count: usize,
    pub healthy: bool,
}

#[async_trait]
pub trait StateStorage: Send + Sync {
    /// Store session state
    async fn store_session(&self, state: &ReplicatedSessionState) -> Result<(), HaError>;
    
    /// Get session state
    async fn get_session(&self, call_id: &CallId) -> Result<Option<ReplicatedSessionState>, HaError>;
    
    /// Delete session state
    async fn delete_session(&self, call_id: &CallId) -> Result<(), HaError>;
    
    /// List all sessions for a node
    async fn list_sessions(&self, node_id: &str) -> Result<Vec<ReplicatedSessionState>, HaError>;
    
    /// Watch for session changes
    async fn watch_sessions(&self) -> Result<mpsc::Receiver<SessionChange>, HaError>;
    
    /// Acquire leadership lock
    async fn acquire_lock(&self, lock_name: &str, ttl: Duration) -> Result<bool, HaError>;
    
    /// Release leadership lock
    async fn release_lock(&self, lock_name: &str) -> Result<(), HaError>;
    
    /// Extend lock TTL
    async fn extend_lock(&self, lock_name: &str, ttl: Duration) -> Result<bool, HaError>;
}

impl ClusterManager {
    pub async fn new(config: HaConfig) -> Result<Self, HaError> {
        let storage: Arc<dyn StateStorage> = match &config.storage {
            StorageConfig::Redis { url, cluster_mode, key_prefix } => {
                Arc::new(RedisStorage::new(url, *cluster_mode, key_prefix).await?)
            }
            StorageConfig::Etcd { endpoints, prefix } => {
                Arc::new(EtcdStorage::new(endpoints, prefix).await?)
            }
            StorageConfig::Memory => {
                Arc::new(MemoryStorage::new())
            }
        };
        
        let vip_manager = config.vip.as_ref().map(|vip| {
            VipManager::new(vip.clone())
        });
        
        let health_monitor = HealthMonitor::new(&config.health);
        
        let (role_tx, _) = watch::channel(NodeRole::Standby);
        
        Ok(Self {
            node_id: config.node_id.clone(),
            config,
            role: role_tx,
            peers: RwLock::new(HashMap::new()),
            storage,
            vip_manager,
            health_monitor,
        })
    }
    
    /// Start cluster operations
    pub async fn start(&self) -> Result<(), HaError> {
        // Initialize peers
        for peer in &self.config.peers {
            self.peers.write().await.insert(peer.node_id.clone(), PeerState {
                config: peer.clone(),
                last_heartbeat: Instant::now(),
                role: NodeRole::Standby,
                session_count: 0,
                healthy: false,
            });
        }
        
        // Start heartbeat sender
        let heartbeat_handle = self.start_heartbeat_sender();
        
        // Start heartbeat receiver
        let receiver_handle = self.start_heartbeat_receiver();
        
        // Start peer monitor
        let monitor_handle = self.start_peer_monitor();
        
        // Participate in leader election
        self.run_election().await?;
        
        Ok(())
    }
    
    /// Run leader election
    async fn run_election(&self) -> Result<(), HaError> {
        let lock_name = "forge-primary-lock";
        
        loop {
            // Try to acquire primary lock
            match self.storage.acquire_lock(lock_name, Duration::from_secs(10)).await {
                Ok(true) => {
                    // We are now primary
                    self.role.send(NodeRole::Primary)?;
                    tracing::info!("Node {} became primary", self.node_id);
                    
                    // Acquire VIP if configured
                    if let Some(ref vip) = self.vip_manager {
                        vip.acquire().await?;
                    }
                    
                    // Keep renewing lock while primary
                    self.run_as_primary(lock_name).await?;
                }
                Ok(false) => {
                    // Another node is primary
                    self.role.send(NodeRole::Standby)?;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => {
                    tracing::error!("Lock error: {}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
    
    async fn run_as_primary(&self, lock_name: &str) -> Result<(), HaError> {
        let renewal_interval = Duration::from_secs(3);
        let lock_ttl = Duration::from_secs(10);
        
        loop {
            tokio::time::sleep(renewal_interval).await;
            
            // Check our health
            if !self.health_monitor.is_healthy().await {
                tracing::warn!("Primary node unhealthy, stepping down");
                self.step_down(lock_name).await?;
                return Ok(());
            }
            
            // Renew lock
            if !self.storage.extend_lock(lock_name, lock_ttl).await? {
                tracing::warn!("Lost primary lock");
                self.step_down(lock_name).await?;
                return Ok(());
            }
        }
    }
    
    async fn step_down(&self, lock_name: &str) -> Result<(), HaError> {
        // Release VIP
        if let Some(ref vip) = self.vip_manager {
            vip.release().await?;
        }
        
        // Release lock
        self.storage.release_lock(lock_name).await?;
        
        self.role.send(NodeRole::Standby)?;
        
        Ok(())
    }
    
    fn start_peer_monitor(&self) -> JoinHandle<()> {
        let peers = self.peers.clone();
        let config = self.config.clone();
        let storage = self.storage.clone();
        let node_id = self.node_id.clone();
        
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(config.health.heartbeat_interval).await;
                
                let now = Instant::now();
                let mut peers = peers.write().await;
                
                for (peer_id, peer) in peers.iter_mut() {
                    let elapsed = now - peer.last_heartbeat;
                    
                    if elapsed > config.health.peer_timeout {
                        if peer.healthy {
                            tracing::warn!("Peer {} timed out", peer_id);
                            peer.healthy = false;
                            
                            // If peer was primary, trigger failover
                            if peer.role == NodeRole::Primary {
                                tracing::info!("Primary {} failed, taking over sessions", peer_id);
                                // Sessions will be migrated when we become primary
                            }
                        }
                    } else {
                        peer.healthy = true;
                    }
                }
            }
        })
    }
}
```

### State Synchronization

```rust
/// Manages session state replication
pub struct StateSync {
    storage: Arc<dyn StateStorage>,
    node_id: String,
    key_encryptor: KeyEncryptor,
    sync_interval: Duration,
}

impl StateSync {
    /// Replicate a session's state
    pub async fn sync_session(&self, session: &MediaSession) -> Result<(), HaError> {
        let state = self.session_to_state(session)?;
        self.storage.store_session(&state).await?;
        Ok(())
    }
    
    /// Convert active session to replicated state
    fn session_to_state(&self, session: &MediaSession) -> Result<ReplicatedSessionState, HaError> {
        let mut legs = Vec::new();
        
        for (leg_id, leg) in &session.legs {
            // Encrypt SRTP keys before storing
            let srtp_send_key = self.key_encryptor.encrypt(&leg.srtp_context.send_key)?;
            let srtp_recv_key = self.key_encryptor.encrypt(&leg.srtp_context.recv_key)?;
            
            legs.push(ReplicatedLegState {
                leg_id: *leg_id,
                local_addr: leg.local_addr,
                remote_addr: leg.remote_addr,
                srtp_send_key,
                srtp_recv_key,
                srtp_profile: leg.srtp_context.profile.clone(),
                rtp_seq: leg.rtp_seq,
                rtp_ts: leg.rtp_ts,
                rtp_ssrc: leg.rtp_ssrc,
                rtcp_sr_ts: leg.rtcp_sr_ts,
                rtcp_rr_seq: leg.rtcp_rr_seq,
                jitter_buffer_delay: leg.jitter_buffer.current_delay(),
                codec: leg.codec.clone(),
                payload_type: leg.payload_type,
            });
        }
        
        Ok(ReplicatedSessionState {
            call_id: session.call_id.clone(),
            created_at: session.created_at,
            updated_at: Utc::now(),
            owning_node: self.node_id.clone(),
            legs,
            connections: session.connections.clone(),
        })
    }
    
    /// Restore session from replicated state
    pub async fn restore_session(
        &self,
        state: &ReplicatedSessionState,
        port_pool: &PortPool,
    ) -> Result<MediaSession, HaError> {
        let mut session = MediaSession::new(state.call_id.clone());
        
        for leg_state in &state.legs {
            // Decrypt SRTP keys
            let send_key = self.key_encryptor.decrypt(&leg_state.srtp_send_key)?;
            let recv_key = self.key_encryptor.decrypt(&leg_state.srtp_recv_key)?;
            
            // Bind to same local port if possible, otherwise allocate new
            let socket = match UdpSocket::bind(leg_state.local_addr).await {
                Ok(s) => s,
                Err(_) => {
                    // Port unavailable, allocate new
                    let new_port = port_pool.allocate()?;
                    let new_addr = SocketAddr::new(
                        leg_state.local_addr.ip(),
                        new_port,
                    );
                    UdpSocket::bind(new_addr).await?
                }
            };
            
            // Restore SRTP context
            let srtp_context = SrtpContext::new(
                &send_key,
                &recv_key,
                &leg_state.srtp_profile,
            )?;
            
            // Create leg with restored state
            let mut leg = MediaLeg::new(
                leg_state.leg_id,
                socket,
                leg_state.codec.clone(),
                leg_state.payload_type,
            );
            
            leg.remote_addr = leg_state.remote_addr;
            leg.srtp_context = srtp_context;
            leg.rtp_seq = leg_state.rtp_seq;
            leg.rtp_ts = leg_state.rtp_ts;
            leg.rtp_ssrc = leg_state.rtp_ssrc;
            leg.rtcp_sr_ts = leg_state.rtcp_sr_ts;
            leg.rtcp_rr_seq = leg_state.rtcp_rr_seq;
            leg.jitter_buffer.set_delay(leg_state.jitter_buffer_delay);
            
            session.add_leg(leg);
        }
        
        // Restore connections
        for (leg_a, leg_b) in &state.connections {
            session.connect(*leg_a, *leg_b)?;
        }
        
        Ok(session)
    }
    
    /// Start continuous sync loop
    pub fn start_sync_loop(
        self: Arc<Self>,
        sessions: Arc<DashMap<CallId, MediaSession>>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.sync_interval);
            
            loop {
                interval.tick().await;
                
                for entry in sessions.iter() {
                    if let Err(e) = self.sync_session(entry.value()).await {
                        tracing::error!(
                            "Failed to sync session {}: {}",
                            entry.key(),
                            e
                        );
                    }
                }
            }
        })
    }
}
```

### Failover Coordinator

```rust
/// Coordinates failover between nodes
pub struct FailoverCoordinator {
    config: FailoverConfig,
    cluster: Arc<ClusterManager>,
    state_sync: Arc<StateSync>,
    sessions: Arc<DashMap<CallId, MediaSession>>,
    port_pool: Arc<PortPool>,
}

impl FailoverCoordinator {
    /// Take over sessions from a failed node
    pub async fn takeover_sessions(&self, failed_node: &str) -> Result<TakeoverReport, HaError> {
        let mut report = TakeoverReport::default();
        
        // Get all sessions owned by failed node
        let sessions = self.state_sync.storage.list_sessions(failed_node).await?;
        
        tracing::info!(
            "Taking over {} sessions from {}",
            sessions.len(),
            failed_node
        );
        
        // Rate limit migration
        let mut rate_limiter = RateLimiter::new(self.config.migration_rate);
        
        for state in sessions {
            rate_limiter.wait().await;
            
            match self.restore_single_session(&state).await {
                Ok(session) => {
                    // Update ownership in storage
                    let mut new_state = state.clone();
                    new_state.owning_node = self.cluster.node_id.clone();
                    self.state_sync.storage.store_session(&new_state).await?;
                    
                    // Add to active sessions
                    self.sessions.insert(state.call_id.clone(), session);
                    
                    report.sessions_recovered += 1;
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to restore session {}: {}",
                        state.call_id,
                        e
                    );
                    report.sessions_failed += 1;
                    report.failures.push((state.call_id.clone(), e.to_string()));
                }
            }
        }
        
        // Notify SIP servers if configured
        if self.config.notify_sip_servers {
            self.notify_sip_servers_of_failover(&report).await?;
        }
        
        Ok(report)
    }
    
    async fn restore_single_session(
        &self,
        state: &ReplicatedSessionState,
    ) -> Result<MediaSession, HaError> {
        // Restore session
        let session = self.state_sync.restore_session(state, &self.port_pool).await?;
        
        // Start receive loops for all legs
        for (leg_id, leg) in &session.legs {
            self.start_leg_receiver(*leg_id, leg.clone()).await?;
        }
        
        Ok(session)
    }
    
    /// Gracefully drain sessions for maintenance
    pub async fn drain_sessions(&self, target_node: &str) -> Result<DrainReport, HaError> {
        let mut report = DrainReport::default();
        
        self.cluster.role.send(NodeRole::Draining)?;
        
        // Signal each session to prepare for migration
        for entry in self.sessions.iter() {
            // Notify target node to prepare
            self.notify_migration_start(target_node, entry.key()).await?;
            report.sessions_started += 1;
        }
        
        // Wait for sessions to drain naturally (calls to end)
        // or force migrate after timeout
        let drain_timeout = Duration::from_secs(300);  // 5 minutes
        let start = Instant::now();
        
        while !self.sessions.is_empty() && start.elapsed() < drain_timeout {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        
        // Force migrate remaining sessions
        for entry in self.sessions.iter() {
            let state = self.state_sync.session_to_state(entry.value())?;
            
            // Store with new owner
            let mut new_state = state;
            new_state.owning_node = target_node.to_string();
            self.state_sync.storage.store_session(&new_state).await?;
            
            // Notify target to take over
            self.notify_migration_complete(target_node, entry.key()).await?;
            
            report.sessions_migrated += 1;
        }
        
        // Clear local sessions
        self.sessions.clear();
        
        // Release VIP
        self.cluster.step_down("forge-primary-lock").await?;
        
        Ok(report)
    }
}

#[derive(Debug, Default)]
pub struct TakeoverReport {
    pub sessions_recovered: usize,
    pub sessions_failed: usize,
    pub failures: Vec<(CallId, String)>,
    pub duration: Duration,
}

#[derive(Debug, Default)]
pub struct DrainReport {
    pub sessions_started: usize,
    pub sessions_migrated: usize,
    pub sessions_ended_naturally: usize,
}
```

### Redis State Storage

```rust
use redis::{AsyncCommands, Client, aio::ConnectionManager};

pub struct RedisStorage {
    conn: ConnectionManager,
    prefix: String,
    key_ttl: Duration,
}

impl RedisStorage {
    pub async fn new(url: &str, cluster_mode: bool, prefix: &str) -> Result<Self, HaError> {
        let client = Client::open(url)?;
        let conn = ConnectionManager::new(client).await?;
        
        Ok(Self {
            conn,
            prefix: prefix.to_string(),
            key_ttl: Duration::from_secs(3600),  // 1 hour TTL
        })
    }
    
    fn session_key(&self, call_id: &CallId) -> String {
        format!("{}:session:{}", self.prefix, call_id)
    }
    
    fn node_sessions_key(&self, node_id: &str) -> String {
        format!("{}:node:{}:sessions", self.prefix, node_id)
    }
    
    fn lock_key(&self, name: &str) -> String {
        format!("{}:lock:{}", self.prefix, name)
    }
}

#[async_trait]
impl StateStorage for RedisStorage {
    async fn store_session(&self, state: &ReplicatedSessionState) -> Result<(), HaError> {
        let mut conn = self.conn.clone();
        
        let key = self.session_key(&state.call_id);
        let data = serde_json::to_vec(state)?;
        
        // Store session data with TTL
        conn.set_ex(&key, data, self.key_ttl.as_secs() as u64).await?;
        
        // Add to node's session set
        let node_key = self.node_sessions_key(&state.owning_node);
        conn.sadd(&node_key, state.call_id.to_string()).await?;
        conn.expire(&node_key, self.key_ttl.as_secs() as i64).await?;
        
        Ok(())
    }
    
    async fn get_session(&self, call_id: &CallId) -> Result<Option<ReplicatedSessionState>, HaError> {
        let mut conn = self.conn.clone();
        
        let key = self.session_key(call_id);
        let data: Option<Vec<u8>> = conn.get(&key).await?;
        
        match data {
            Some(bytes) => {
                let state: ReplicatedSessionState = serde_json::from_slice(&bytes)?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }
    
    async fn delete_session(&self, call_id: &CallId) -> Result<(), HaError> {
        let mut conn = self.conn.clone();
        
        // Get session to find owning node
        if let Some(state) = self.get_session(call_id).await? {
            let node_key = self.node_sessions_key(&state.owning_node);
            conn.srem(&node_key, call_id.to_string()).await?;
        }
        
        let key = self.session_key(call_id);
        conn.del(&key).await?;
        
        Ok(())
    }
    
    async fn list_sessions(&self, node_id: &str) -> Result<Vec<ReplicatedSessionState>, HaError> {
        let mut conn = self.conn.clone();
        
        let node_key = self.node_sessions_key(node_id);
        let call_ids: Vec<String> = conn.smembers(&node_key).await?;
        
        let mut sessions = Vec::new();
        for call_id_str in call_ids {
            let call_id = CallId::from_str(&call_id_str)?;
            if let Some(state) = self.get_session(&call_id).await? {
                sessions.push(state);
            }
        }
        
        Ok(sessions)
    }
    
    async fn acquire_lock(&self, lock_name: &str, ttl: Duration) -> Result<bool, HaError> {
        let mut conn = self.conn.clone();
        
        let key = self.lock_key(lock_name);
        let value = uuid::Uuid::new_v4().to_string();
        
        // SET NX EX - set if not exists with expiry
        let result: bool = redis::cmd("SET")
            .arg(&key)
            .arg(&value)
            .arg("NX")
            .arg("PX")
            .arg(ttl.as_millis() as u64)
            .query_async(&mut conn)
            .await
            .unwrap_or(false);
        
        Ok(result)
    }
    
    async fn release_lock(&self, lock_name: &str) -> Result<(), HaError> {
        let mut conn = self.conn.clone();
        let key = self.lock_key(lock_name);
        conn.del(&key).await?;
        Ok(())
    }
    
    async fn extend_lock(&self, lock_name: &str, ttl: Duration) -> Result<bool, HaError> {
        let mut conn = self.conn.clone();
        let key = self.lock_key(lock_name);
        
        // Only extend if we still own it
        let result: bool = conn.expire(&key, ttl.as_secs() as i64).await?;
        Ok(result)
    }
    
    async fn watch_sessions(&self) -> Result<mpsc::Receiver<SessionChange>, HaError> {
        let (tx, rx) = mpsc::channel(100);
        
        // Subscribe to keyspace notifications
        let pattern = format!("__keyspace@*__:{}:session:*", self.prefix);
        
        // Spawn subscriber task
        // ... Redis pub/sub implementation
        
        Ok(rx)
    }
}
```

### Virtual IP Manager

```rust
use std::process::Command;

/// Manages virtual IP for failover
pub struct VipManager {
    config: VipConfig,
    active: AtomicBool,
}

impl VipManager {
    pub fn new(config: VipConfig) -> Self {
        Self {
            config,
            active: AtomicBool::new(false),
        }
    }
    
    /// Acquire the VIP (become active)
    pub async fn acquire(&self) -> Result<(), HaError> {
        if self.active.load(Ordering::SeqCst) {
            return Ok(());
        }
        
        // Add IP to interface
        let output = Command::new("ip")
            .args([
                "addr", "add",
                &format!("{}/32", self.config.address),
                "dev", &self.config.interface,
            ])
            .output()?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore "already exists" error
            if !stderr.contains("File exists") {
                return Err(HaError::VipError(stderr.to_string()));
            }
        }
        
        // Send gratuitous ARP to update neighbor caches
        if self.config.garp_enabled {
            self.send_garp().await?;
        }
        
        // If using BGP anycast, announce route
        if let Some(ref anycast) = self.config.anycast {
            self.announce_bgp_route(anycast).await?;
        }
        
        self.active.store(true, Ordering::SeqCst);
        tracing::info!("VIP {} acquired on {}", self.config.address, self.config.interface);
        
        Ok(())
    }
    
    /// Release the VIP (become standby)
    pub async fn release(&self) -> Result<(), HaError> {
        if !self.active.load(Ordering::SeqCst) {
            return Ok(());
        }
        
        // Withdraw BGP route
        if let Some(ref anycast) = self.config.anycast {
            self.withdraw_bgp_route(anycast).await?;
        }
        
        // Remove IP from interface
        let output = Command::new("ip")
            .args([
                "addr", "del",
                &format!("{}/32", self.config.address),
                "dev", &self.config.interface,
            ])
            .output()?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore "not found" error
            if !stderr.contains("Cannot assign") {
                return Err(HaError::VipError(stderr.to_string()));
            }
        }
        
        self.active.store(false, Ordering::SeqCst);
        tracing::info!("VIP {} released", self.config.address);
        
        Ok(())
    }
    
    async fn send_garp(&self) -> Result<(), HaError> {
        // Use arping to send gratuitous ARP
        let output = Command::new("arping")
            .args([
                "-c", "3",        // 3 packets
                "-U",             // Unsolicited ARP
                "-I", &self.config.interface,
                &self.config.address.to_string(),
            ])
            .output()?;
        
        if !output.status.success() {
            tracing::warn!("GARP failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        
        Ok(())
    }
    
    async fn announce_bgp_route(&self, _config: &AnycastConfig) -> Result<(), HaError> {
        // Integration with BGP daemon (BIRD, FRR, etc.)
        // This would typically involve writing to a config file or using an API
        Ok(())
    }
    
    async fn withdraw_bgp_route(&self, _config: &AnycastConfig) -> Result<(), HaError> {
        Ok(())
    }
}
```

---

## 4. API Additions

### SIPREC API

```rust
// POST /v1/siprec/recordings
#[derive(Debug, Deserialize)]
pub struct StartSiprecRequest {
    pub call_id: CallId,
    pub participants: Vec<ParticipantInfo>,
    pub reason: Option<RecordingReason>,
    pub srs_override: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartSiprecResponse {
    pub recording_id: Uuid,
    pub srs_endpoint: String,
    pub status: RecordingState,
}

// GET /v1/siprec/recordings/:recording_id
// DELETE /v1/siprec/recordings/:recording_id (stop)

// PUT /v1/siprec/recordings/:recording_id/participants
#[derive(Debug, Deserialize)]
pub struct UpdateParticipantsRequest {
    pub action: ParticipantAction,
    pub participant: ParticipantInfo,
}

#[derive(Debug, Deserialize)]
pub enum ParticipantAction {
    Add,
    Remove,
    Update,
}
```

### AI Streaming API

```rust
// POST /v1/ai/sessions
#[derive(Debug, Deserialize)]
pub struct StartAiSessionRequest {
    pub target_type: AiTargetType,
    pub call_id: Option<CallId>,
    pub leg: Option<LegId>,
    pub room_id: Option<RoomId>,
    pub participant_id: Option<Uuid>,
    
    pub provider: AiProviderConfig,
    pub bidirectional: bool,  // Whether AI responses play back to call
    pub webhook_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub enum AiTargetType {
    SessionLeg,
    ConferenceParticipant,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum AiProviderConfig {
    OpenAI {
        model: String,
        voice: Option<String>,
        instructions: Option<String>,
        tools: Option<Vec<AiToolDef>>,
    },
    GoogleSpeech {
        language_code: String,
        model: Option<String>,
    },
    DialogflowCX {
        project_id: String,
        location: String,
        agent_id: String,
        language_code: String,
    },
    AmazonLex {
        bot_id: String,
        bot_alias_id: String,
        locale_id: String,
    },
    AzureSpeech {
        language: String,
        voice: Option<String>,
    },
    Custom {
        websocket_url: String,
        auth_header: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub struct StartAiSessionResponse {
    pub session_id: Uuid,
    pub status: SessionState,
}

// POST /v1/ai/sessions/:session_id/tool-result
#[derive(Debug, Deserialize)]
pub struct ToolResultRequest {
    pub call_id: String,
    pub result: serde_json::Value,
}

// POST /v1/ai/sessions/:session_id/interrupt
// DELETE /v1/ai/sessions/:session_id (stop)

// GET /v1/ai/sessions/:session_id/events (WebSocket)
// Returns stream of AiEvent
```

### HA API

```rust
// GET /v1/ha/status
#[derive(Debug, Serialize)]
pub struct HaStatusResponse {
    pub node_id: String,
    pub role: NodeRole,
    pub cluster_mode: ClusterMode,
    pub peers: Vec<PeerStatus>,
    pub vip: Option<VipStatus>,
    pub sessions_owned: usize,
    pub last_failover: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct PeerStatus {
    pub node_id: String,
    pub address: SocketAddr,
    pub role: NodeRole,
    pub healthy: bool,
    pub last_heartbeat: DateTime<Utc>,
    pub session_count: usize,
}

#[derive(Debug, Serialize)]
pub struct VipStatus {
    pub address: IpAddr,
    pub active: bool,
    pub interface: String,
}

// POST /v1/ha/failover
#[derive(Debug, Deserialize)]
pub struct ForceFailoverRequest {
    pub target_node: Option<String>,  // If not specified, use election
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct FailoverResponse {
    pub old_primary: String,
    pub new_primary: String,
    pub sessions_migrated: usize,
    pub sessions_failed: usize,
}

// POST /v1/ha/drain
#[derive(Debug, Deserialize)]
pub struct DrainRequest {
    pub target_node: String,
    pub timeout_secs: Option<u64>,
}

// GET /v1/ha/sessions
#[derive(Debug, Serialize)]
pub struct ClusterSessionsResponse {
    pub total_sessions: usize,
    pub by_node: HashMap<String, usize>,
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub call_id: CallId,
    pub owning_node: String,
    pub created_at: DateTime<Utc>,
    pub leg_count: usize,
}
```

---

## 5. Configuration Additions

```toml
# SIPREC Configuration
[siprec]
enabled = true
role = "src"  # "src" or "srs" or "both"

[siprec.src]
# SRS servers for sending recordings
[[siprec.src.srs_servers]]
uri = "sip:srs.example.com:5060"
transport = "tcp"
priority = 1
weight = 100

[[siprec.src.srs_servers]]
uri = "sip:srs-backup.example.com:5060"
transport = "tcp"
priority = 2
weight = 100

mode = "early_media"  # "early_media", "answered_only", "on_demand"
failover_enabled = true
max_retries = 3

[siprec.src.policy]
patterns = ["*"]  # Record all calls
exclusions = ["^\\+1800"]  # Except toll-free
direction = "both"

[siprec.srs]
listen_addr = "0.0.0.0:5060"
rtp_port_range = { start = 40000, end = 50000 }
storage_backend = "s3"
format = "opus"
max_sessions = 1000

# AI Streaming Configuration
[ai_stream]
enabled = true
max_concurrent_sessions = 100
default_idle_timeout_secs = 300

[ai_stream.providers.openai]
api_key = "${OPENAI_API_KEY}"
default_model = "gpt-4o-realtime-preview"
default_voice = "alloy"

[ai_stream.providers.google]
credentials_path = "/etc/forge/google-credentials.json"
default_language = "en-US"

[ai_stream.providers.dialogflow]
credentials_path = "/etc/forge/google-credentials.json"

[ai_stream.providers.aws]
region = "us-east-1"

[ai_stream.providers.azure]
subscription_key = "${AZURE_SPEECH_KEY}"
region = "eastus"

# High Availability Configuration
[ha]
enabled = true
node_id = "forge-1"  # Or use hostname
node_addr = "192.168.1.100:8080"
mode = "active-standby"  # "active-standby", "active-active", "n-plus-1"

[ha.storage]
type = "redis"
url = "redis://redis-cluster:6379"
cluster_mode = true
key_prefix = "forge"

[[ha.peers]]
node_id = "forge-2"
addr = "192.168.1.101:8080"
rpc_port = 8090
priority = 2

[[ha.peers]]
node_id = "forge-3"
addr = "192.168.1.102:8080"
rpc_port = 8090
priority = 3

[ha.vip]
address = "192.168.1.200"
interface = "eth0"
garp_enabled = true

# Optional: BGP anycast instead of VIP
# [ha.vip.anycast]
# router_id = "192.168.1.100"
# local_as = 65001
# [[ha.vip.anycast.neighbors]]
# address = "192.168.1.1"
# remote_as = 65000

[ha.health]
heartbeat_interval_ms = 1000
peer_timeout_secs = 3
missed_heartbeats = 3

[[ha.health.check_endpoints]]
type = "http"
url = "http://localhost:8080/health"
interval_secs = 5

[ha.failover]
auto_failover = true
failover_cooldown_secs = 60
migration_rate = 100  # sessions per second
notify_sip_servers = true
graceful = true
```

---

## 6. Updated Crate Structure

```
forge-media/
├── forge-core/
├── forge-rtp/
├── forge-engine/
├── forge-transcoding/
├── forge-kernel/
├── forge-conference/
├── forge-recording/
├── forge-dtmf/
├── forge-transcription/
├── forge-injection/
├── forge-webrtc/
├── forge-sdp/
├── forge-sbc/
├── forge-siprec/          # NEW: SIPREC support
│   ├── src/
│   │   ├── lib.rs
│   │   ├── metadata.rs
│   │   ├── src_client.rs
│   │   ├── srs_server.rs
│   │   └── session.rs
├── forge-ai-stream/       # NEW: AI service streaming
│   ├── src/
│   │   ├── lib.rs
│   │   ├── session.rs
│   │   ├── audio_bridge.rs
│   │   └── providers/
│   │       ├── openai.rs
│   │       ├── google.rs
│   │       ├── dialogflow.rs
│   │       ├── aws.rs
│   │       ├── azure.rs
│   │       └── custom.rs
├── forge-ha/              # NEW: High Availability
│   ├── src/
│   │   ├── lib.rs
│   │   ├── cluster.rs
│   │   ├── state_sync.rs
│   │   ├── failover.rs
│   │   ├── vip.rs
│   │   └── storage/
│   │       ├── redis.rs
│   │       └── etcd.rs
└── forge-api/
```

---

## 7. Additional Dependencies

```toml
# forge-siprec
quick-xml = "0.31"
uuid = { version = "1", features = ["v4", "serde"] }

# forge-ai-stream
tokio-tungstenite = "0.21"
futures-util = "0.3"
base64 = "0.21"

# forge-ha
redis = { version = "0.24", features = ["tokio-comp", "cluster-async"] }
etcd-client = "0.12"
aes-gcm = "0.10"  # For key encryption
```

This completes the design for SIPREC, AI streaming, and media failover features!
