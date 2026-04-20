//! forge-sdp - SDP (Session Description Protocol) wrapper for Forge Media Engine
//!
//! This crate provides a Forge-specific wrapper around the sip-sdp library,
//! offering simplified APIs for common SDP operations while maintaining
//! full RFC 4566 (SDP) and RFC 3264 (Offer/Answer) compliance.
//!
//! # Features
//!
//! - SDP parsing and serialization
//! - RFC 3264 offer/answer negotiation
//! - Pre-built capability profiles (audio-only, audio+opus, etc.)
//! - Codec negotiation and extraction
//! - Direction negotiation (sendrecv, recvonly, etc.)
//!
//! # Example
//!
//! ```no_run
//! use forge_sdp::{SessionDescription, SessionDescriptionExt, profiles};
//!
//! // Parse an SDP offer
//! let offer_text = "v=0\r\no=alice 123 0 IN IP4 192.168.1.100\r\n...";
//! let offer = SessionDescription::from_str(offer_text)?;
//!
//! // Load local capabilities
//! let profile = profiles::SdpProfile::audio_only();
//! let local_caps = profile.with_local_addr("10.0.0.1", 5000);
//!
//! // Negotiate answer
//! let answer = SessionDescription::negotiate_answer(&offer, &local_caps, "10.0.0.1")?;
//!
//! // Serialize answer
//! let answer_text = sip_sdp::serialize::serialize_sdp(&answer);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod dtls;
pub mod ice;
pub mod profiles;

use thiserror::Error;

// Re-export sip-sdp types for convenience
pub use sip_sdp::{
    negotiate::Direction, serialize, Attribute, Bandwidth, Connection, MediaDescription, MediaType,
    Origin, Protocol, RtpMap, SessionDescription as SipSessionDescription, TimeDescription,
};

// Re-export ICE attribute helpers
pub use ice::{IceAttributesExt, IceCandidateParams, MediaIceAttributesExt};

// Re-export DTLS attribute helpers
pub use dtls::{DtlsAttributesExt, DtlsSetup, MediaDtlsAttributesExt};

/// Forge-specific SDP error types
#[derive(Error, Debug)]
pub enum SdpError {
    /// SDP parsing error
    #[error("SDP parse error: {0}")]
    ParseError(String),

    /// SDP negotiation error
    #[error("SDP negotiation error: {0}")]
    NegotiationError(String),

    /// No common codec found
    #[error("No common codec found between offer and answer")]
    NoCommonCodec,

    /// Invalid media type
    #[error("Invalid media type: {0}")]
    InvalidMediaType(String),

    /// Missing required field
    #[error("Missing required field: {0}")]
    MissingField(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for SDP operations
pub type Result<T> = std::result::Result<T, SdpError>;

/// Forge-specific SessionDescription wrapper
///
/// This is a type alias for the sip-sdp SessionDescription with additional
/// convenience methods provided through extension traits.
pub type SessionDescription = SipSessionDescription;

/// Extension trait for SessionDescription with Forge-specific convenience methods
pub trait SessionDescriptionExt {
    /// Parse SDP from string
    fn from_str(sdp: &str) -> Result<SessionDescription>;

    /// Negotiate an SDP answer from an offer
    ///
    /// # Arguments
    ///
    /// * `offer` - The SDP offer to negotiate against
    /// * `local_caps` - Local capabilities (supported codecs, media types, etc.)
    /// * `local_addr` - Local IP address for the answer
    ///
    /// # Returns
    ///
    /// An SDP answer that represents the negotiated parameters
    fn negotiate_answer(
        offer: &SessionDescription,
        local_caps: &SessionDescription,
        local_addr: &str,
    ) -> Result<SessionDescription>;

    /// Find a media description by type
    fn find_media(&self, media_type: MediaType) -> Option<&MediaDescription>;

    /// Find a mutable media description by type
    fn find_media_mut(&mut self, media_type: MediaType) -> Option<&mut MediaDescription>;

    /// Get negotiated audio codec (payload type and name)
    fn get_audio_codec(&self) -> Option<(u8, String)>;
}

impl SessionDescriptionExt for SessionDescription {
    fn from_str(sdp: &str) -> Result<SessionDescription> {
        sip_sdp::parse::parse_sdp(sdp).map_err(|e| SdpError::ParseError(e.to_string()))
    }

    fn negotiate_answer(
        offer: &SessionDescription,
        local_caps: &SessionDescription,
        local_addr: &str,
    ) -> Result<SessionDescription> {
        sip_sdp::negotiate::negotiate_answer(offer, local_addr, local_caps)
            .map_err(|e| SdpError::NegotiationError(e.to_string()))
    }

    fn find_media(&self, media_type: MediaType) -> Option<&MediaDescription> {
        self.media.iter().find(|m| m.media_type == media_type)
    }

    fn find_media_mut(&mut self, media_type: MediaType) -> Option<&mut MediaDescription> {
        self.media.iter_mut().find(|m| m.media_type == media_type)
    }

    fn get_audio_codec(&self) -> Option<(u8, String)> {
        let audio = self.find_media(MediaType::Audio)?;
        if audio.port == 0 {
            return None;
        }

        // Get first format (primary negotiated codec)
        let pt: u8 = audio.formats.first()?.parse().ok()?;

        // Get codec name from rtpmap
        let rtpmap = audio.rtpmaps.get(&pt)?;
        Some((pt, rtpmap.encoding_name.to_string()))
    }
}

/// Codec information extracted from SDP
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecInfo {
    /// RTP payload type
    pub payload_type: u8,

    /// Codec name (e.g., "PCMU", "PCMA", "Opus")
    pub encoding_name: String,

    /// Clock rate in Hz
    pub clock_rate: u32,

    /// Number of channels (for audio)
    pub channels: Option<u16>,
}

impl CodecInfo {
    /// Extract codec information from an rtpmap
    pub fn from_rtpmap(payload_type: u8, rtpmap: &RtpMap) -> Self {
        let channels = rtpmap.encoding_params.as_ref().and_then(|s| s.parse().ok());

        Self {
            payload_type,
            encoding_name: rtpmap.encoding_name.to_string(),
            clock_rate: rtpmap.clock_rate,
            channels,
        }
    }

    /// Check if this is a DTMF codec (telephone-event)
    pub fn is_dtmf(&self) -> bool {
        self.encoding_name.eq_ignore_ascii_case("telephone-event")
    }

    /// Convert to forge-core AudioCodec enum
    pub fn to_audio_codec(&self) -> Option<forge_core::AudioCodec> {
        match self.encoding_name.to_uppercase().as_str() {
            "PCMU" => Some(forge_core::AudioCodec::PCMU),
            "PCMA" => Some(forge_core::AudioCodec::PCMA),
            "OPUS" => Some(forge_core::AudioCodec::Opus),
            "G722" => Some(forge_core::AudioCodec::G722),
            "G729" => Some(forge_core::AudioCodec::G729),
            "SPEEX" => Some(forge_core::AudioCodec::Speex),
            "ILBC" => Some(forge_core::AudioCodec::ILBC),
            "AMR" => Some(forge_core::AudioCodec::AMR),
            "AMR-WB" => Some(forge_core::AudioCodec::AMRWB),
            _ => None,
        }
    }
}

/// Helper functions for working with SDP
pub mod helpers {
    use super::*;

    /// Extract all codecs from a media description
    pub fn extract_codecs(media: &MediaDescription) -> Vec<CodecInfo> {
        media
            .formats
            .iter()
            .filter_map(|fmt| {
                let pt: u8 = fmt.parse().ok()?;
                media
                    .rtpmaps
                    .get(&pt)
                    .map(|rtpmap| CodecInfo::from_rtpmap(pt, rtpmap))
            })
            .collect()
    }

    /// Extract the primary (first) codec from a media description
    pub fn extract_primary_codec(media: &MediaDescription) -> Option<CodecInfo> {
        let pt: u8 = media.formats.first()?.parse().ok()?;
        let rtpmap = media.rtpmaps.get(&pt)?;
        Some(CodecInfo::from_rtpmap(pt, rtpmap))
    }

    /// Find DTMF codec (telephone-event) if present
    pub fn find_dtmf_codec(media: &MediaDescription) -> Option<CodecInfo> {
        extract_codecs(media)
            .into_iter()
            .find(|codec| codec.is_dtmf())
    }

    /// Check if SDP has a specific codec
    pub fn has_codec(sdp: &SessionDescription, codec_name: &str) -> bool {
        sdp.media.iter().any(|media| {
            media
                .rtpmaps
                .values()
                .any(|rtpmap| rtpmap.encoding_name.eq_ignore_ascii_case(codec_name))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codec_info_from_rtpmap() {
        let rtpmap = RtpMap {
            payload_type: 0,
            encoding_name: "PCMU".into(),
            clock_rate: 8000,
            encoding_params: None,
        };

        let codec = CodecInfo::from_rtpmap(0, &rtpmap);
        assert_eq!(codec.payload_type, 0);
        assert_eq!(codec.encoding_name, "PCMU");
        assert_eq!(codec.clock_rate, 8000);
        assert_eq!(codec.channels, None);
    }

    #[test]
    fn test_codec_info_with_channels() {
        let rtpmap = RtpMap {
            payload_type: 111,
            encoding_name: "opus".into(),
            clock_rate: 48000,
            encoding_params: Some("2".into()),
        };

        let codec = CodecInfo::from_rtpmap(111, &rtpmap);
        assert_eq!(codec.channels, Some(2));
    }

    #[test]
    fn test_is_dtmf() {
        let dtmf_codec = CodecInfo {
            payload_type: 101,
            encoding_name: "telephone-event".into(),
            clock_rate: 8000,
            channels: None,
        };

        assert!(dtmf_codec.is_dtmf());

        let audio_codec = CodecInfo {
            payload_type: 0,
            encoding_name: "PCMU".into(),
            clock_rate: 8000,
            channels: None,
        };

        assert!(!audio_codec.is_dtmf());
    }

    #[test]
    fn test_to_audio_codec() {
        let codecs = vec![
            ("PCMU", forge_core::AudioCodec::PCMU),
            ("PCMA", forge_core::AudioCodec::PCMA),
            ("Opus", forge_core::AudioCodec::Opus),
            ("G722", forge_core::AudioCodec::G722),
        ];

        for (name, expected) in codecs {
            let codec_info = CodecInfo {
                payload_type: 0,
                encoding_name: name.to_string(),
                clock_rate: 8000,
                channels: None,
            };

            assert_eq!(codec_info.to_audio_codec(), Some(expected));
        }
    }

    #[test]
    fn test_unknown_codec() {
        let codec_info = CodecInfo {
            payload_type: 96,
            encoding_name: "UnknownCodec".to_string(),
            clock_rate: 8000,
            channels: None,
        };

        assert_eq!(codec_info.to_audio_codec(), None);
    }

    #[test]
    fn test_get_audio_codec_rejected_media() {
        let media = sip_sdp::MediaDescription::audio(0)
            .add_format(0)
            .unwrap()
            .add_rtpmap(0, "PCMU", 8000, None)
            .unwrap();
        let sdp = sip_sdp::builder::SessionDescriptionBuilder::new()
            .origin("alice", "1", "192.168.1.100")
            .unwrap()
            .session_name("Test")
            .unwrap()
            .connection("192.168.1.100")
            .unwrap()
            .time(0, 0)
            .media(media)
            .unwrap()
            .build();

        assert!(sdp.get_audio_codec().is_none());
    }
}
