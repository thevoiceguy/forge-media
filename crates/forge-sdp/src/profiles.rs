//! Pre-built SDP capability profiles for common scenarios
//!
//! This module provides ready-to-use SDP profiles for typical media scenarios,
//! simplifying SDP negotiation by offering standard codec configurations.

use crate::SessionDescription;
use sip_sdp::profiles::MediaProfileBuilder;

/// SDP profile with pre-configured media capabilities
#[derive(Debug, Clone)]
pub struct SdpProfile {
    /// Profile name for identification
    pub name: String,

    /// Profile description
    pub description: String,

    /// MediaProfileBuilder for generating SDP
    builder: MediaProfileBuilder,
}

impl SdpProfile {
    /// Create a new SDP profile
    fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        builder: MediaProfileBuilder,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            builder,
        }
    }

    /// Audio-only profile with G.711 (PCMU + PCMA) codecs
    ///
    /// Suitable for basic VoIP calls with maximum compatibility.
    ///
    /// # Codecs
    /// - PCMU (PT 0) @ 8kHz - G.711 μ-law
    /// - PCMA (PT 8) @ 8kHz - G.711 A-law
    /// - telephone-event (PT 101) @ 8kHz - DTMF
    ///
    /// # Example
    /// ```no_run
    /// use forge_sdp::profiles::SdpProfile;
    ///
    /// let profile = SdpProfile::audio_only();
    /// let sdp = profile.with_local_addr("10.0.0.1", 5000);
    /// ```
    pub fn audio_only() -> Self {
        let builder = MediaProfileBuilder::audio_only()
            .telephone_event(true)
            .rtcp_mux(true);

        Self::new(
            "audio-only",
            "G.711 audio with DTMF support (PCMU/PCMA)",
            builder,
        )
    }

    /// Audio-only profile with Opus codec
    ///
    /// High-quality wideband audio for modern VoIP clients.
    ///
    /// # Codecs
    /// - Opus (PT 111) @ 48kHz, 2 channels
    /// - telephone-event (PT 101) @ 8kHz - DTMF
    ///
    /// # Example
    /// ```no_run
    /// use forge_sdp::profiles::SdpProfile;
    ///
    /// let profile = SdpProfile::audio_opus();
    /// let sdp = profile.with_local_addr("10.0.0.1", 5000);
    /// ```
    pub fn audio_opus() -> Self {
        let builder = MediaProfileBuilder::audio_only()
            .add_audio_codec(111, "opus", 48000)
            .telephone_event(true)
            .rtcp_mux(true);

        Self::new(
            "audio-opus",
            "High-quality Opus audio with DTMF support",
            builder,
        )
    }

    /// Audio-only profile with multiple codecs (PCMU, PCMA, Opus)
    ///
    /// Maximum compatibility profile supporting both G.711 and Opus codecs.
    ///
    /// # Codecs (in preference order)
    /// - PCMU (PT 0) @ 8kHz - G.711 μ-law
    /// - PCMA (PT 8) @ 8kHz - G.711 A-law
    /// - Opus (PT 111) @ 48kHz, 2 channels
    /// - telephone-event (PT 101) @ 8kHz - DTMF
    ///
    /// # Example
    /// ```no_run
    /// use forge_sdp::profiles::SdpProfile;
    ///
    /// let profile = SdpProfile::audio_all();
    /// let sdp = profile.with_local_addr("10.0.0.1", 5000);
    /// ```
    pub fn audio_all() -> Self {
        let builder = MediaProfileBuilder::audio_only()
            .add_audio_codec(111, "opus", 48000)
            .telephone_event(true)
            .rtcp_mux(true);

        Self::new(
            "audio-all",
            "Multi-codec audio (PCMU/PCMA/Opus) with maximum compatibility",
            builder,
        )
    }

    /// WebRTC audio profile with Opus codec and DTLS-SRTP
    ///
    /// Modern WebRTC-compatible profile with mandatory encryption.
    ///
    /// # Protocol
    /// - UDP/TLS/RTP/SAVPF (WebRTC standard per RFC 8834)
    ///
    /// # Codecs
    /// - Opus (PT 111) @ 48kHz, 2 channels
    /// - telephone-event (PT 101) @ 48kHz - DTMF
    ///
    /// # WebRTC Features
    /// - DTLS-SRTP key exchange (requires fingerprint and setup attributes)
    /// - ICE candidates (requires ice-ufrag and ice-pwd)
    /// - rtcp-mux (multiplexing RTP and RTCP)
    /// - rtcp-rsize (reduced-size RTCP)
    ///
    /// # Example
    /// ```no_run
    /// use forge_sdp::profiles::SdpProfile;
    /// use forge_sdp::{DtlsSetup, IceAttributesExt, DtlsAttributesExt, MediaIceAttributesExt};
    ///
    /// let profile = SdpProfile::webrtc_audio();
    /// let mut sdp = profile.with_local_addr("10.0.0.1", 5000);
    ///
    /// // Add ICE credentials
    /// sdp.set_ice_credentials("abcd1234", "password12345678901234567890");
    ///
    /// // Add DTLS fingerprint (from certificate)
    /// sdp.set_dtls_fingerprint(
    ///     "sha-256",
    ///     "A1:B2:C3:D4:E5:F6:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23"
    /// );
    /// sdp.set_dtls_setup(DtlsSetup::Actpass);
    /// ```
    pub fn webrtc_audio() -> Self {
        // WebRTC profile needs custom construction because it uses UDP/TLS/RTP/SAVPF
        // We create a builder that generates the SDP with WebRTC-specific attributes
        let builder = MediaProfileBuilder::audio_only()
            .add_audio_codec(111, "opus", 48000)
            .telephone_event(true)
            .rtcp_mux(true);

        Self::new(
            "webrtc-audio",
            "WebRTC audio with Opus, DTLS-SRTP, and ICE",
            builder,
        )
    }

    /// Generate a SessionDescription with the specified local address and port
    ///
    /// This generates a complete SDP offer/answer with the actual local address
    /// and RTP port that will be used for the media session.
    ///
    /// # Arguments
    ///
    /// * `local_addr` - The local IP address to use in the SDP
    /// * `audio_port` - The RTP port for audio
    ///
    /// # Returns
    ///
    /// A SessionDescription ready for negotiation
    pub fn with_local_addr(&self, local_addr: &str, audio_port: u16) -> SessionDescription {
        // Special handling for WebRTC profiles
        if self.name == "webrtc-audio" {
            return self.build_webrtc_sdp(local_addr, audio_port);
        }

        self.builder.build("forge", local_addr, audio_port, None)
    }

    /// Build WebRTC-specific SDP with UDP/TLS/RTP/SAVPF protocol
    fn build_webrtc_sdp(&self, local_addr: &str, audio_port: u16) -> SessionDescription {
        use crate::{Attribute, MediaDescription, Protocol};
        use sip_sdp::builder::SessionDescriptionBuilder;
        use smol_str::SmolStr;

        let session_id = chrono::Utc::now().timestamp().to_string();

        let mut media = MediaDescription::audio(audio_port);
        media.protocol = Protocol::UdpTlsRtpSavpf;

        // Add Opus codec
        media.formats.push(111);
        media.rtpmaps.insert(
            111,
            crate::RtpMap {
                payload_type: 111,
                encoding_name: SmolStr::new("opus"),
                clock_rate: 48000,
                encoding_params: Some(SmolStr::new("2")),
            },
        );
        media.attributes.push(Attribute::Value {
            name: SmolStr::new("rtpmap"),
            value: SmolStr::new("111 opus/48000/2"),
        });
        media.attributes.push(Attribute::Value {
            name: SmolStr::new("fmtp"),
            value: SmolStr::new("111 minptime=10;useinbandfec=1"),
        });

        // Add telephone-event (DTMF)
        media.formats.push(101);
        media.rtpmaps.insert(
            101,
            crate::RtpMap {
                payload_type: 101,
                encoding_name: SmolStr::new("telephone-event"),
                clock_rate: 48000,
                encoding_params: None,
            },
        );
        media.attributes.push(Attribute::Value {
            name: SmolStr::new("rtpmap"),
            value: SmolStr::new("101 telephone-event/48000"),
        });
        media.attributes.push(Attribute::Value {
            name: SmolStr::new("fmtp"),
            value: SmolStr::new("101 0-16"),
        });

        // Add WebRTC-specific attributes
        media
            .attributes
            .push(Attribute::Property(SmolStr::new("rtcp-mux")));
        media
            .attributes
            .push(Attribute::Property(SmolStr::new("rtcp-rsize")));
        media
            .attributes
            .push(Attribute::Property(SmolStr::new("sendrecv")));

        SessionDescriptionBuilder::new()
            .origin("forge", &session_id, local_addr)
            .session_name("WebRTC Audio Session")
            .connection(local_addr)
            .time(0, 0)
            .media(media)
            .build()
    }

    /// Get a reference to the underlying MediaProfileBuilder
    pub fn builder(&self) -> &MediaProfileBuilder {
        &self.builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaType;

    #[test]
    fn test_audio_only_profile() {
        let profile = SdpProfile::audio_only();
        assert_eq!(profile.name, "audio-only");

        let sdp = profile.with_local_addr("192.168.1.100", 5000);
        assert_eq!(sdp.origin.unicast_address.as_str(), "192.168.1.100");
        assert_eq!(sdp.media.len(), 1);
        assert_eq!(sdp.media[0].media_type, MediaType::Audio);
        assert_eq!(sdp.media[0].port, 5000);

        // Should have PCMU (0), PCMA (8), and telephone-event (101)
        assert!(sdp.media[0].formats.contains(&0));
        assert!(sdp.media[0].formats.contains(&8));
        assert!(sdp.media[0].formats.contains(&101));
    }

    #[test]
    fn test_audio_opus_profile() {
        let profile = SdpProfile::audio_opus();
        assert_eq!(profile.name, "audio-opus");

        let sdp = profile.with_local_addr("10.0.0.1", 6000);
        assert_eq!(sdp.origin.unicast_address.as_str(), "10.0.0.1");
        assert_eq!(sdp.media.len(), 1);
        assert_eq!(sdp.media[0].port, 6000);

        // Should have PCMU, PCMA, Opus (111), and telephone-event (101)
        assert!(sdp.media[0].formats.contains(&0));
        assert!(sdp.media[0].formats.contains(&8));
        assert!(sdp.media[0].formats.contains(&111));
        assert!(sdp.media[0].formats.contains(&101));

        // Check Opus rtpmap
        let opus = sdp.media[0].rtpmaps.get(&111).unwrap();
        assert_eq!(opus.encoding_name.as_str(), "opus");
        assert_eq!(opus.clock_rate, 48000);
    }

    #[test]
    fn test_audio_all_profile() {
        let profile = SdpProfile::audio_all();
        assert_eq!(profile.name, "audio-all");

        let sdp = profile.with_local_addr("10.0.0.1", 7000);

        // Should have PCMU (0), PCMA (8), Opus (111), and telephone-event (101)
        assert!(sdp.media[0].formats.contains(&0));
        assert!(sdp.media[0].formats.contains(&8));
        assert!(sdp.media[0].formats.contains(&111));
        assert!(sdp.media[0].formats.contains(&101));

        // Verify all rtpmaps are present
        assert!(sdp.media[0].rtpmaps.contains_key(&0));
        assert!(sdp.media[0].rtpmaps.contains_key(&8));
        assert!(sdp.media[0].rtpmaps.contains_key(&111));
        assert!(sdp.media[0].rtpmaps.contains_key(&101));
    }

    #[test]
    fn test_with_local_addr_different_ports() {
        let profile = SdpProfile::audio_only();
        let sdp1 = profile.with_local_addr("192.168.1.1", 5000);
        let sdp2 = profile.with_local_addr("10.0.0.1", 6000);

        assert_eq!(sdp1.origin.unicast_address.as_str(), "192.168.1.1");
        assert_eq!(sdp1.media[0].port, 5000);

        assert_eq!(sdp2.origin.unicast_address.as_str(), "10.0.0.1");
        assert_eq!(sdp2.media[0].port, 6000);

        // Connection should also be updated
        if let Some(ref conn) = sdp1.connection {
            assert_eq!(conn.connection_address.as_str(), "192.168.1.1");
        }
        if let Some(ref conn) = sdp2.connection {
            assert_eq!(conn.connection_address.as_str(), "10.0.0.1");
        }
    }

    #[test]
    fn test_profile_has_rtcp_mux() {
        let profile = SdpProfile::audio_only();
        let sdp = profile.with_local_addr("10.0.0.1", 5000);

        // Check for rtcp-mux attribute
        let has_rtcp_mux = sdp.media[0]
            .attributes
            .iter()
            .any(|attr| matches!(attr, crate::Attribute::Property(name) if name == "rtcp-mux"));

        assert!(has_rtcp_mux);
    }

    #[test]
    fn test_webrtc_audio_profile() {
        let profile = SdpProfile::webrtc_audio();
        assert_eq!(profile.name, "webrtc-audio");

        let sdp = profile.with_local_addr("10.0.0.1", 9000);
        assert_eq!(sdp.origin.unicast_address.as_str(), "10.0.0.1");
        assert_eq!(sdp.media.len(), 1);
        assert_eq!(sdp.media[0].media_type, MediaType::Audio);
        assert_eq!(sdp.media[0].port, 9000);

        // Should use UDP/TLS/RTP/SAVPF protocol
        assert_eq!(sdp.media[0].protocol, crate::Protocol::UdpTlsRtpSavpf);

        // Should have Opus (111) and telephone-event (101)
        assert!(sdp.media[0].formats.contains(&111));
        assert!(sdp.media[0].formats.contains(&101));

        // Check Opus rtpmap
        let opus = sdp.media[0].rtpmaps.get(&111).unwrap();
        assert_eq!(opus.encoding_name.as_str(), "opus");
        assert_eq!(opus.clock_rate, 48000);
        assert_eq!(opus.encoding_params.as_ref().map(|s| s.as_str()), Some("2"));

        // Check telephone-event rtpmap
        let dtmf = sdp.media[0].rtpmaps.get(&101).unwrap();
        assert_eq!(dtmf.encoding_name.as_str(), "telephone-event");
        assert_eq!(dtmf.clock_rate, 48000);

        // Check for WebRTC-specific attributes
        let has_rtcp_mux = sdp.media[0]
            .attributes
            .iter()
            .any(|attr| matches!(attr, crate::Attribute::Property(name) if name == "rtcp-mux"));
        assert!(has_rtcp_mux);

        let has_rtcp_rsize = sdp.media[0]
            .attributes
            .iter()
            .any(|attr| matches!(attr, crate::Attribute::Property(name) if name == "rtcp-rsize"));
        assert!(has_rtcp_rsize);

        let has_sendrecv = sdp.media[0]
            .attributes
            .iter()
            .any(|attr| matches!(attr, crate::Attribute::Property(name) if name == "sendrecv"));
        assert!(has_sendrecv);

        // Check for Opus fmtp (useinbandfec)
        let has_opus_fmtp = sdp.media[0]
            .attributes
            .iter()
            .any(|attr| {
                matches!(attr, crate::Attribute::Value { name, value }
                    if name == "fmtp" && value.starts_with("111") && value.contains("useinbandfec=1"))
            });
        assert!(has_opus_fmtp);
    }
}
