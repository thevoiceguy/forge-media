//! Core type definitions for Forge

use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

/// Unique call identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallId(pub String);

impl CallId {
    /// Generate a new random CallId
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create from a string
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for CallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for CallId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Conference room identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomId(pub String);

impl RoomId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Participant identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantId(pub String);

impl ParticipantId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Leg identifier for point-to-point sessions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegIdentifier {
    /// Leg A (typically caller)
    LegA,
    /// Leg B (typically callee)
    LegB,
    /// Identified by tag
    ByTag(u32),
}

/// Media direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaDirection {
    /// Send and receive
    SendRecv,
    /// Send only
    SendOnly,
    /// Receive only
    RecvOnly,
    /// Inactive
    Inactive,
}

/// Media type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaType {
    Audio,
    Video,
    Application,
}

/// IP version configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpVersionConfig {
    /// IPv4 only
    V4Only,
    /// IPv6 only
    V6Only,
    /// Dual stack (IPv4 and IPv6)
    DualStack,
    /// Bridge IPv4 to IPv6
    Bridge4to6,
    /// Bridge IPv6 to IPv4
    Bridge6to4,
}

impl Default for IpVersionConfig {
    fn default() -> Self {
        Self::DualStack
    }
}

/// Transport protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    UDP,
    TCP,
    TLS,
    DTLS,
}

/// Audio codec enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    /// G.711 μ-law
    PCMU,
    /// G.711 A-law
    PCMA,
    /// G.722
    G722,
    /// G.729
    G729,
    /// Opus
    Opus,
    /// Speex
    Speex,
    /// iLBC
    ILBC,
    /// AMR
    AMR,
    /// AMR-WB
    AMRWB,
    /// Raw PCM (for WAV files, etc.)
    PCM,
}

impl AudioCodec {
    /// Get the standard RTP payload type for this codec
    pub fn payload_type(&self) -> Option<u8> {
        match self {
            Self::PCMU => Some(0),
            Self::PCMA => Some(8),
            Self::G722 => Some(9),
            Self::G729 => Some(18),
            _ => None, // Dynamic payload types
        }
    }

    /// Get the sample rate for this codec
    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::PCMU | Self::PCMA | Self::G729 => 8000,
            Self::G722 => 16000,
            Self::Opus => 48000,
            Self::Speex => 8000,
            Self::ILBC => 8000,
            Self::AMR => 8000,
            Self::AMRWB => 16000,
            Self::PCM => 48000, // Default, can vary
        }
    }

    /// Get typical bitrate in bits per second
    pub fn bitrate(&self) -> u32 {
        match self {
            Self::PCMU | Self::PCMA | Self::G722 => 64000,
            Self::G729 => 8000,
            Self::Opus => 24000, // Variable, this is a typical value
            Self::Speex => 24000,
            Self::ILBC => 15200,
            Self::AMR => 12200,
            Self::AMRWB => 23850,
            Self::PCM => 1536000, // 48kHz * 16bit * 2 channels
        }
    }

    /// Parse codec from string (case-insensitive)
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "opus" => Some(Self::Opus),
            "pcmu" => Some(Self::PCMU),
            "pcma" => Some(Self::PCMA),
            "pcm" | "wav" => Some(Self::PCM),
            "g722" => Some(Self::G722),
            "g729" => Some(Self::G729),
            "speex" => Some(Self::Speex),
            "ilbc" => Some(Self::ILBC),
            "amr" => Some(Self::AMR),
            "amrwb" | "amr-wb" => Some(Self::AMRWB),
            _ => None,
        }
    }

    /// Get the recommended file extension for this codec
    pub fn file_extension(&self) -> &str {
        match self {
            Self::Opus => "opus",
            Self::PCM => "wav",
            Self::PCMU | Self::PCMA | Self::G722 | Self::G729 => "wav",
            Self::Speex => "spx",
            Self::ILBC => "wav",
            Self::AMR => "amr",
            Self::AMRWB => "awb",
        }
    }
}

/// Video codec enumeration.
///
/// Identifiers and RTP conventions only: forge carries video as opaque
/// RTP payloads (forwarding, keyframe detection, stream switching) and
/// does not encode or decode it. Every listed codec uses a 90 kHz RTP
/// clock and a dynamic payload type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    /// H.264 / AVC (RFC 6184)
    H264,
    /// H.265 / HEVC (RFC 7798)
    H265,
    /// VP8 (RFC 7741)
    VP8,
    /// VP9 (RFC 9628)
    VP9,
    /// AV1 (AV1 RTP payload specification)
    AV1,
}

impl VideoCodec {
    /// Every codec, in the order forge prefers them when nothing else
    /// decides: H.264 first for SIP interoperability, then the royalty-free
    /// codecs browsers ship.
    pub const ALL: [VideoCodec; 5] = [
        VideoCodec::H264,
        VideoCodec::VP8,
        VideoCodec::VP9,
        VideoCodec::AV1,
        VideoCodec::H265,
    ];

    /// The RTP clock rate every video codec uses.
    pub const CLOCK_RATE: u32 = 90_000;

    /// Encoding name as it appears in `a=rtpmap`.
    pub fn sdp_name(&self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
            Self::VP8 => "VP8",
            Self::VP9 => "VP9",
            Self::AV1 => "AV1",
        }
    }

    /// Parse an `a=rtpmap` encoding name (case-insensitive).
    pub fn from_sdp_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "H264" => Some(Self::H264),
            "H265" | "HEVC" => Some(Self::H265),
            "VP8" => Some(Self::VP8),
            "VP9" => Some(Self::VP9),
            "AV1" | "AV1X" => Some(Self::AV1),
            _ => None,
        }
    }

    /// The dynamic payload type forge offers this codec with when it makes
    /// an offer. Answers always take the offerer's number. Chosen not to
    /// collide with the audio conventions (Opus 111, telephone-event 101).
    pub fn default_payload_type(&self) -> u8 {
        match self {
            Self::H264 => 96,
            Self::VP8 => 97,
            Self::VP9 => 98,
            Self::AV1 => 99,
            Self::H265 => 100,
        }
    }

    /// RTP clock rate (always 90 kHz).
    pub fn clock_rate(&self) -> u32 {
        Self::CLOCK_RATE
    }
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.sdp_name())
    }
}

/// Audio sample format
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Sample rate in Hz (e.g., 48000, 16000, 8000)
    pub sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo)
    pub channels: u16,
    /// Codec used for encoding
    pub codec: AudioCodec,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            codec: AudioCodec::Opus,
        }
    }
}

impl AudioFormat {
    /// Create a new audio format
    pub fn new(sample_rate: u32, channels: u16, codec: AudioCodec) -> Self {
        Self {
            sample_rate,
            channels,
            codec,
        }
    }

    /// Create Opus format (48kHz mono)
    pub fn opus_mono() -> Self {
        Self::new(48000, 1, AudioCodec::Opus)
    }

    /// Create PCM format for WAV files (48kHz mono)
    pub fn pcm_mono() -> Self {
        Self::new(48000, 1, AudioCodec::PCM)
    }
}

/// Codec configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecConfig {
    pub codec: AudioCodec,
    pub payload_type: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

impl CodecConfig {
    pub fn pcmu() -> Self {
        Self {
            codec: AudioCodec::PCMU,
            payload_type: 0,
            sample_rate: 8000,
            channels: 1,
        }
    }

    pub fn pcma() -> Self {
        Self {
            codec: AudioCodec::PCMA,
            payload_type: 8,
            sample_rate: 8000,
            channels: 1,
        }
    }

    pub fn opus() -> Self {
        Self {
            codec: AudioCodec::Opus,
            payload_type: 111,
            sample_rate: 48000,
            channels: 2,
        }
    }
}

/// Session state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Session is being created
    Creating,
    /// Session is active
    Active,
    /// Session is on hold
    OnHold,
    /// Session is being torn down
    Terminating,
    /// Session has ended
    Terminated,
}

/// Recording identifier
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingId(pub String);

impl RecordingId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl fmt::Display for RecordingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Secrets that should be redacted when formatted for logs, JSON, and serialization
#[derive(Clone, PartialEq, Eq)]
pub struct SecureString(Zeroizing<String>);

impl Default for SecureString {
    fn default() -> Self {
        Self(Zeroizing::new(String::new()))
    }
}

impl SecureString {
    /// Create a new secure string
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// Reveal the underlying secret
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }

    /// Check if the secret is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecureString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Display for SecureString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl From<String> for SecureString {
    fn from(value: String) -> Self {
        SecureString::new(value)
    }
}

impl From<&str> for SecureString {
    fn from(value: &str) -> Self {
        SecureString::new(value)
    }
}

impl AsRef<str> for SecureString {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl serde::Serialize for SecureString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Always serialize as [REDACTED] to prevent leakage in JSON logs/responses
        serializer.serialize_str("[REDACTED]")
    }
}

impl<'de> serde::Deserialize<'de> for SecureString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Allow deserialization from strings (needed for config loading)
        // but log a warning if the value looks like it might be [REDACTED]
        let value = String::deserialize(deserializer)?;

        if value == "[REDACTED]" {
            return Err(serde::de::Error::custom(
                "Cannot deserialize [REDACTED] placeholder as a secret. \
                 Use environment variables or secret management systems instead.",
            ));
        }

        Ok(SecureString::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_codecs_round_trip_sdp_names_and_use_the_90khz_clock() {
        for codec in VideoCodec::ALL {
            assert_eq!(VideoCodec::from_sdp_name(codec.sdp_name()), Some(codec));
            assert_eq!(codec.clock_rate(), 90_000);
            assert!(codec.default_payload_type() >= 96);
            assert_ne!(codec.default_payload_type(), 101, "telephone-event clash");
            assert_ne!(codec.default_payload_type(), 111, "opus clash");
        }
        assert_eq!(VideoCodec::from_sdp_name("h264"), Some(VideoCodec::H264));
        assert_eq!(VideoCodec::from_sdp_name("HEVC"), Some(VideoCodec::H265));
        assert_eq!(VideoCodec::from_sdp_name("AV1X"), Some(VideoCodec::AV1));
        assert_eq!(VideoCodec::from_sdp_name("opus"), None);
        assert_eq!(VideoCodec::VP8.to_string(), "VP8");
    }

    #[test]
    fn test_secure_string_debug_redaction() {
        let secret = SecureString::new("sk-super-secret-api-key-12345");
        let debug_output = format!("{:?}", secret);

        assert_eq!(debug_output, "[REDACTED]");
        assert!(!debug_output.contains("sk-super-secret"));
    }

    #[test]
    fn test_secure_string_display_redaction() {
        let secret = SecureString::new("password123");
        let display_output = format!("{}", secret);

        assert_eq!(display_output, "[REDACTED]");
        assert!(!display_output.contains("password"));
    }

    #[test]
    fn test_secure_string_serialization_redaction() {
        let secret = SecureString::new("sk-openai-key-abc123xyz");
        let json = serde_json::to_string(&secret).unwrap();

        // Should serialize as [REDACTED], not the actual secret
        assert_eq!(json, "\"[REDACTED]\"");
        assert!(!json.contains("sk-openai-key"));
        assert!(!json.contains("abc123xyz"));
    }

    #[test]
    fn test_secure_string_deserialization() {
        // Should successfully deserialize a real secret
        let json = "\"my-secret-key\"";
        let secret: SecureString = serde_json::from_str(json).unwrap();
        assert_eq!(secret.expose_secret(), "my-secret-key");
    }

    #[test]
    fn test_secure_string_reject_redacted_placeholder() {
        // Should reject [REDACTED] as a value
        let json = "\"[REDACTED]\"";
        let result: Result<SecureString, _> = serde_json::from_str(json);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Cannot deserialize [REDACTED]"));
    }

    #[test]
    fn test_secure_string_expose_secret() {
        let secret = SecureString::new("actual-secret-value");

        // expose_secret() should reveal the actual value
        assert_eq!(secret.expose_secret(), "actual-secret-value");
    }

    #[test]
    fn test_secure_string_is_empty() {
        let empty = SecureString::new("");
        let non_empty = SecureString::new("value");

        assert!(empty.is_empty());
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_secure_string_from_conversions() {
        let from_string = SecureString::from("test".to_string());
        let from_str = SecureString::from("test");

        assert_eq!(from_string.expose_secret(), "test");
        assert_eq!(from_str.expose_secret(), "test");
    }

    #[test]
    fn test_secure_string_as_ref() {
        let secret = SecureString::new("mykey");
        let as_ref: &str = secret.as_ref();

        assert_eq!(as_ref, "mykey");
    }

    #[test]
    fn test_secure_string_clone() {
        let original = SecureString::new("original");
        let cloned = original.clone();

        assert_eq!(original.expose_secret(), cloned.expose_secret());
    }

    #[test]
    fn test_secure_string_equality() {
        let secret1 = SecureString::new("same");
        let secret2 = SecureString::new("same");
        let secret3 = SecureString::new("different");

        assert_eq!(secret1, secret2);
        assert_ne!(secret1, secret3);
    }
}
