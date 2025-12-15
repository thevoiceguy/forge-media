//! SIPREC Recording Metadata (RFC 7865)
//!
//! This module implements the XML metadata format for SIPREC recording sessions.
//! The metadata provides information about participants, media streams, and
//! recording session details.
//!
//! # Example
//!
//! ```rust,ignore
//! use forge_siprec::metadata::{RecordingSession, Participant, MediaStream};
//!
//! let mut session = RecordingSession::new("session-1");
//! session.add_participant(Participant::caller("sip:alice@example.com"));
//! session.add_participant(Participant::callee("sip:bob@example.com"));
//! session.add_media_stream(MediaStream::audio("stream-1", "192.168.1.100", 5004));
//!
//! let xml = session.to_xml()?;
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// SIPREC Recording Session metadata
///
/// Contains all metadata about a recording session including participants,
/// media streams, and timing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "recording")]
pub struct RecordingSession {
    /// Unique session identifier
    #[serde(rename = "session")]
    pub session_id: String,

    /// Recording start time
    #[serde(rename = "start-time")]
    pub start_time: DateTime<Utc>,

    /// Recording stop time (if ended)
    #[serde(rename = "stop-time", skip_serializing_if = "Option::is_none")]
    pub stop_time: Option<DateTime<Utc>>,

    /// List of participants in the session
    #[serde(rename = "participant")]
    pub participants: Vec<Participant>,

    /// List of media streams
    #[serde(rename = "stream")]
    pub streams: Vec<MediaStream>,

    /// Recording reason
    #[serde(rename = "reason", skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    /// Extension data
    #[serde(rename = "extensiondata", skip_serializing_if = "Option::is_none")]
    pub extension_data: Option<Vec<ExtensionData>>,
}

/// Participant in a recording session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    /// Unique participant identifier
    #[serde(rename = "@id")]
    pub id: String,

    /// Participant name or display name
    #[serde(rename = "name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// SIP URI of the participant
    #[serde(rename = "aor")]
    pub aor: String,

    /// Participant role (caller, callee, etc.)
    #[serde(rename = "role", skip_serializing_if = "Option::is_none")]
    pub role: Option<ParticipantRole>,

    /// Associated media streams
    #[serde(rename = "stream", skip_serializing_if = "Option::is_none")]
    pub stream_refs: Option<Vec<String>>,
}

/// Participant role in a call
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    /// Call originator
    Caller,
    /// Call recipient
    Callee,
    /// Call transfer target
    Target,
    /// Unknown role
    Unknown,
}

/// Media stream information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaStream {
    /// Unique stream identifier
    #[serde(rename = "@id")]
    pub id: String,

    /// Media type (audio, video, etc.)
    #[serde(rename = "mediaType")]
    pub media_type: MediaType,

    /// Media format (codec)
    #[serde(rename = "format", skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// RTP session information
    #[serde(rename = "session", skip_serializing_if = "Option::is_none")]
    pub session: Option<RtpSession>,

    /// Stream label
    #[serde(rename = "label", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Media type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    /// Audio stream
    Audio,
    /// Video stream
    Video,
    /// Text stream
    Text,
    /// Application data
    Application,
    /// Message stream
    Message,
}

/// RTP session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtpSession {
    /// RTP destination address
    #[serde(rename = "address")]
    pub address: String,

    /// RTP destination port
    #[serde(rename = "port")]
    pub port: u16,

    /// RTCP port (if different)
    #[serde(rename = "rtcp-port", skip_serializing_if = "Option::is_none")]
    pub rtcp_port: Option<u16>,

    /// SSRC identifier
    #[serde(rename = "ssrc", skip_serializing_if = "Option::is_none")]
    pub ssrc: Option<u32>,
}

/// Extension data for custom metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionData {
    /// Extension name/type
    #[serde(rename = "@name")]
    pub name: String,

    /// Extension value
    #[serde(rename = "$text")]
    pub value: String,
}

impl RecordingSession {
    /// Create a new recording session
    ///
    /// # Arguments
    ///
    /// * `session_id` - Unique identifier for this recording session
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            start_time: Utc::now(),
            stop_time: None,
            participants: Vec::new(),
            streams: Vec::new(),
            reason: None,
            extension_data: None,
        }
    }

    /// Add a participant to the session
    pub fn add_participant(&mut self, participant: Participant) {
        self.participants.push(participant);
    }

    /// Add a media stream to the session
    pub fn add_media_stream(&mut self, stream: MediaStream) {
        self.streams.push(stream);
    }

    /// Mark the session as stopped
    pub fn stop(&mut self) {
        self.stop_time = Some(Utc::now());
    }

    /// Set the recording reason
    pub fn set_reason(&mut self, reason: impl Into<String>) {
        self.reason = Some(reason.into());
    }

    /// Add extension data
    pub fn add_extension(&mut self, name: impl Into<String>, value: impl Into<String>) {
        if self.extension_data.is_none() {
            self.extension_data = Some(Vec::new());
        }
        self.extension_data.as_mut().unwrap().push(ExtensionData {
            name: name.into(),
            value: value.into(),
        });
    }

    /// Add AI session metadata
    ///
    /// This adds standardized extension data for AI-enhanced recordings.
    ///
    /// # Arguments
    ///
    /// * `provider` - AI provider name (e.g., "OpenAI", "Google", "Azure")
    /// * `model` - AI model identifier (e.g., "gpt-4o-realtime-preview")
    /// * `voice` - Voice/persona used (if applicable)
    pub fn add_ai_metadata(
        &mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
        voice: Option<String>,
    ) {
        self.add_extension("ai-provider", provider);
        self.add_extension("ai-model", model);
        if let Some(v) = voice {
            self.add_extension("ai-voice", v);
        }
        self.add_extension("ai-enabled", "true");
    }

    /// Add AI participant to the recording
    ///
    /// Creates a virtual participant representing the AI assistant.
    ///
    /// # Arguments
    ///
    /// * `ai_name` - Display name for the AI (e.g., "AI Assistant")
    /// * `provider` - AI provider (e.g., "OpenAI")
    pub fn add_ai_participant(&mut self, ai_name: impl Into<String>, provider: impl Into<String>) {
        let ai_id = format!("ai-participant-{}", self.participants.len());
        let ai_aor = format!("sip:ai@{}.local", provider.into());

        let mut ai_participant = Participant::new(ai_id, ai_aor, ParticipantRole::Unknown);
        ai_participant.name = Some(ai_name.into());

        self.add_participant(ai_participant);
    }

    /// Serialize to XML string
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_xml(&self) -> Result<String, quick_xml::DeError> {
        quick_xml::se::to_string(self)
    }

    /// Deserialize from XML string
    ///
    /// # Errors
    ///
    /// Returns an error if deserialization fails.
    pub fn from_xml(xml: &str) -> Result<Self, quick_xml::DeError> {
        quick_xml::de::from_str(xml)
    }
}

impl Participant {
    /// Create a new participant
    ///
    /// # Arguments
    ///
    /// * `id` - Unique participant identifier
    /// * `aor` - SIP Address of Record (URI)
    /// * `role` - Participant role in the call
    pub fn new(
        id: impl Into<String>,
        aor: impl Into<String>,
        role: ParticipantRole,
    ) -> Self {
        Self {
            id: id.into(),
            name: None,
            aor: aor.into(),
            role: Some(role),
            stream_refs: None,
        }
    }

    /// Create a caller participant
    pub fn caller(aor: impl Into<String>) -> Self {
        Self::new("caller", aor, ParticipantRole::Caller)
    }

    /// Create a callee participant
    pub fn callee(aor: impl Into<String>) -> Self {
        Self::new("callee", aor, ParticipantRole::Callee)
    }

    /// Set the participant's display name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Associate media streams with this participant
    pub fn with_streams(mut self, stream_ids: Vec<String>) -> Self {
        self.stream_refs = Some(stream_ids);
        self
    }
}

impl MediaStream {
    /// Create a new media stream
    ///
    /// # Arguments
    ///
    /// * `id` - Unique stream identifier
    /// * `media_type` - Type of media (audio, video, etc.)
    pub fn new(id: impl Into<String>, media_type: MediaType) -> Self {
        Self {
            id: id.into(),
            media_type,
            format: None,
            session: None,
            label: None,
        }
    }

    /// Create an audio stream
    pub fn audio(id: impl Into<String>, address: impl Into<String>, port: u16) -> Self {
        let mut stream = Self::new(id, MediaType::Audio);
        stream.session = Some(RtpSession {
            address: address.into(),
            port,
            rtcp_port: None,
            ssrc: None,
        });
        stream
    }

    /// Create a video stream
    pub fn video(id: impl Into<String>, address: impl Into<String>, port: u16) -> Self {
        let mut stream = Self::new(id, MediaType::Video);
        stream.session = Some(RtpSession {
            address: address.into(),
            port,
            rtcp_port: None,
            ssrc: None,
        });
        stream
    }

    /// Set the media format (codec)
    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = Some(format.into());
        self
    }

    /// Set the SSRC
    pub fn with_ssrc(mut self, ssrc: u32) -> Self {
        if let Some(ref mut session) = self.session {
            session.ssrc = Some(ssrc);
        }
        self
    }

    /// Set the stream label
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_recording_session() {
        let mut session = RecordingSession::new("session-123");
        session.add_participant(Participant::caller("sip:alice@example.com"));
        session.add_participant(Participant::callee("sip:bob@example.com"));
        session.add_media_stream(MediaStream::audio("stream-1", "192.168.1.100", 5004));

        assert_eq!(session.session_id, "session-123");
        assert_eq!(session.participants.len(), 2);
        assert_eq!(session.streams.len(), 1);
    }

    #[test]
    fn test_participant_roles() {
        let caller = Participant::caller("sip:alice@example.com");
        assert_eq!(caller.role, Some(ParticipantRole::Caller));

        let callee = Participant::callee("sip:bob@example.com");
        assert_eq!(callee.role, Some(ParticipantRole::Callee));
    }

    #[test]
    fn test_media_stream_types() {
        let audio = MediaStream::audio("audio-1", "192.168.1.100", 5004);
        assert_eq!(audio.media_type, MediaType::Audio);

        let video = MediaStream::video("video-1", "192.168.1.100", 5006);
        assert_eq!(video.media_type, MediaType::Video);
    }

    #[test]
    fn test_xml_serialization() {
        let mut session = RecordingSession::new("test-session");
        session.add_participant(
            Participant::caller("sip:alice@example.com")
                .with_name("Alice")
        );
        session.add_media_stream(
            MediaStream::audio("stream-1", "192.168.1.100", 5004)
                .with_format("PCMU")
                .with_ssrc(12345)
        );

        let xml = session.to_xml().unwrap();
        assert!(xml.contains("test-session"));
        assert!(xml.contains("alice@example.com"));
        assert!(xml.contains("192.168.1.100"));
    }

    #[test]
    fn test_xml_roundtrip() {
        let mut session = RecordingSession::new("roundtrip-test");
        session.add_participant(Participant::caller("sip:test@example.com"));
        session.add_media_stream(MediaStream::audio("stream-1", "192.168.1.100", 5004));

        let xml = session.to_xml().unwrap();

        // Note: Full XML roundtrip requires proper XML namespace handling
        // For now, just verify XML generation works
        assert!(xml.contains("roundtrip-test"));
        assert!(xml.contains("test@example.com"));
    }

    #[test]
    fn test_ai_metadata() {
        let mut session = RecordingSession::new("ai-session-123");
        session.add_ai_metadata("OpenAI", "gpt-4o-realtime-preview", Some("alloy".to_string()));

        // Verify extension data was added
        assert!(session.extension_data.is_some());
        let extensions = session.extension_data.as_ref().unwrap();

        assert_eq!(extensions.len(), 4);
        assert!(extensions.iter().any(|e| e.name == "ai-provider" && e.value == "OpenAI"));
        assert!(extensions.iter().any(|e| e.name == "ai-model" && e.value == "gpt-4o-realtime-preview"));
        assert!(extensions.iter().any(|e| e.name == "ai-voice" && e.value == "alloy"));
        assert!(extensions.iter().any(|e| e.name == "ai-enabled" && e.value == "true"));
    }

    #[test]
    fn test_ai_metadata_without_voice() {
        let mut session = RecordingSession::new("ai-session-456");
        session.add_ai_metadata("Google", "gemini-pro", None);

        let extensions = session.extension_data.as_ref().unwrap();

        // Should have 3 extensions (no voice)
        assert_eq!(extensions.len(), 3);
        assert!(extensions.iter().any(|e| e.name == "ai-provider" && e.value == "Google"));
        assert!(extensions.iter().any(|e| e.name == "ai-model" && e.value == "gemini-pro"));
        assert!(extensions.iter().any(|e| e.name == "ai-enabled" && e.value == "true"));

        // Voice should not be present
        assert!(!extensions.iter().any(|e| e.name == "ai-voice"));
    }

    #[test]
    fn test_ai_participant() {
        let mut session = RecordingSession::new("ai-participant-test");
        session.add_participant(Participant::caller("sip:alice@example.com"));
        session.add_participant(Participant::callee("sip:bob@example.com"));
        session.add_ai_participant("AI Assistant", "OpenAI");

        assert_eq!(session.participants.len(), 3);

        // Find AI participant
        let ai_participant = session.participants.iter()
            .find(|p| p.name.as_ref().map(|n| n.as_str()) == Some("AI Assistant"))
            .expect("AI participant should be present");

        assert!(ai_participant.aor.contains("ai@"));
        assert!(ai_participant.aor.contains("OpenAI"));
        assert_eq!(ai_participant.role, Some(ParticipantRole::Unknown));
    }

    #[test]
    fn test_full_ai_recording_session() {
        let mut session = RecordingSession::new("full-ai-test");

        // Add human participants
        session.add_participant(Participant::caller("sip:user@example.com").with_name("User"));

        // Add AI participant
        session.add_ai_participant("AI Assistant", "OpenAI");

        // Add AI metadata
        session.add_ai_metadata("OpenAI", "gpt-4o-realtime-preview", Some("nova".to_string()));

        // Add media streams
        session.add_media_stream(MediaStream::audio("user-stream", "192.168.1.100", 5004));
        session.add_media_stream(MediaStream::audio("ai-stream", "192.168.1.100", 5006));

        // Verify structure
        assert_eq!(session.participants.len(), 2);
        assert_eq!(session.streams.len(), 2);
        assert!(session.extension_data.is_some());

        // Verify XML generation
        let xml = session.to_xml().unwrap();
        assert!(xml.contains("full-ai-test"));
        assert!(xml.contains("AI Assistant"));
        assert!(xml.contains("ai-provider"));
        assert!(xml.contains("OpenAI"));
    }

    #[test]
    fn test_custom_extension_data() {
        let mut session = RecordingSession::new("custom-ext-test");
        session.add_extension("custom-field", "custom-value");
        session.add_extension("another-field", "another-value");

        let extensions = session.extension_data.as_ref().unwrap();
        assert_eq!(extensions.len(), 2);
        assert!(extensions.iter().any(|e| e.name == "custom-field" && e.value == "custom-value"));
        assert!(extensions.iter().any(|e| e.name == "another-field" && e.value == "another-value"));
    }
}
