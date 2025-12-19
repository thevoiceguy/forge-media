//! DTLS (Datagram Transport Layer Security) attribute helpers for SDP
//!
//! Provides convenient methods for working with DTLS-related SDP attributes
//! as defined in RFC 8122 (DTLS-SRTP Framework) and RFC 4145 (connection setup).

use crate::{Attribute, MediaDescription, SessionDescription};
use smol_str::SmolStr;

/// DTLS setup role per RFC 4145
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsSetup {
    /// Active: This endpoint will initiate the DTLS handshake
    /// Maps to a=setup:active
    Active,

    /// Passive: This endpoint will wait for the DTLS handshake
    /// Maps to a=setup:passive
    Passive,

    /// Actpass: This endpoint can act as either active or passive
    /// Maps to a=setup:actpass (typically used in offers)
    Actpass,

    /// Holdconn: Connection should be put on hold
    /// Maps to a=setup:holdconn
    Holdconn,
}

impl DtlsSetup {
    /// Convert to SDP attribute value
    pub fn as_str(&self) -> &'static str {
        match self {
            DtlsSetup::Active => "active",
            DtlsSetup::Passive => "passive",
            DtlsSetup::Actpass => "actpass",
            DtlsSetup::Holdconn => "holdconn",
        }
    }

    /// Parse from SDP attribute value
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "active" => Some(DtlsSetup::Active),
            "passive" => Some(DtlsSetup::Passive),
            "actpass" => Some(DtlsSetup::Actpass),
            "holdconn" => Some(DtlsSetup::Holdconn),
            _ => None,
        }
    }
}

/// Extension trait for adding DTLS attributes to session descriptions
pub trait DtlsAttributesExt {
    /// Set DTLS fingerprint at session level
    ///
    /// Adds a=fingerprint:<algorithm> <hash> attribute.
    /// Algorithm is typically "sha-256" per RFC 8122.
    ///
    /// # Example
    /// ```ignore
    /// sdp.set_dtls_fingerprint(
    ///     "sha-256",
    ///     "A1:B2:C3:D4:E5:F6:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23"
    /// );
    /// ```
    fn set_dtls_fingerprint(&mut self, algorithm: &str, hash: &str);

    /// Get DTLS fingerprint from session level
    ///
    /// Returns (algorithm, hash) if present.
    fn get_dtls_fingerprint(&self) -> Option<(String, String)>;

    /// Set DTLS setup role at session level
    ///
    /// Adds a=setup:<role> attribute.
    fn set_dtls_setup(&mut self, setup: DtlsSetup);

    /// Get DTLS setup role from session level
    fn get_dtls_setup(&self) -> Option<DtlsSetup>;
}

/// Extension trait for adding DTLS attributes to media descriptions
pub trait MediaDtlsAttributesExt {
    /// Set DTLS fingerprint at media level (overrides session-level)
    fn set_media_dtls_fingerprint(&mut self, algorithm: &str, hash: &str);

    /// Get media-level DTLS fingerprint
    fn get_media_dtls_fingerprint(&self) -> Option<(String, String)>;

    /// Set DTLS setup role at media level
    fn set_media_dtls_setup(&mut self, setup: DtlsSetup);

    /// Get media-level DTLS setup role
    fn get_media_dtls_setup(&self) -> Option<DtlsSetup>;
}

impl DtlsAttributesExt for SessionDescription {
    fn set_dtls_fingerprint(&mut self, algorithm: &str, hash: &str) {
        if !is_valid_fingerprint_algorithm(algorithm) || !is_valid_fingerprint_hash(hash) {
            return;
        }

        // Remove existing fingerprint
        self.attributes
            .retain(|attr| !matches!(attr, Attribute::Value { name, .. } if name == "fingerprint"));

        // Add new fingerprint
        let fingerprint_value = format!("{} {}", algorithm, hash);
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("fingerprint"),
            value: SmolStr::new(&fingerprint_value),
        });
    }

    fn get_dtls_fingerprint(&self) -> Option<(String, String)> {
        for attr in &self.attributes {
            if let Attribute::Value { name, value } = attr {
                if name == "fingerprint" {
                    // Parse "algorithm hash"
                    let parts: Vec<&str> = value.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        return Some((parts[0].to_string(), parts[1].to_string()));
                    }
                }
            }
        }
        None
    }

    fn set_dtls_setup(&mut self, setup: DtlsSetup) {
        // Remove existing setup
        self.attributes
            .retain(|attr| !matches!(attr, Attribute::Value { name, .. } if name == "setup"));

        // Add new setup
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("setup"),
            value: SmolStr::new(setup.as_str()),
        });
    }

    fn get_dtls_setup(&self) -> Option<DtlsSetup> {
        for attr in &self.attributes {
            if let Attribute::Value { name, value } = attr {
                if name == "setup" {
                    return DtlsSetup::parse(value);
                }
            }
        }
        None
    }
}

impl MediaDtlsAttributesExt for MediaDescription {
    fn set_media_dtls_fingerprint(&mut self, algorithm: &str, hash: &str) {
        if !is_valid_fingerprint_algorithm(algorithm) || !is_valid_fingerprint_hash(hash) {
            return;
        }

        // Remove existing media-level fingerprint
        self.attributes
            .retain(|attr| !matches!(attr, Attribute::Value { name, .. } if name == "fingerprint"));

        // Add new fingerprint
        let fingerprint_value = format!("{} {}", algorithm, hash);
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("fingerprint"),
            value: SmolStr::new(&fingerprint_value),
        });
    }

    fn get_media_dtls_fingerprint(&self) -> Option<(String, String)> {
        for attr in &self.attributes {
            if let Attribute::Value { name, value } = attr {
                if name == "fingerprint" {
                    // Parse "algorithm hash"
                    let parts: Vec<&str> = value.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        return Some((parts[0].to_string(), parts[1].to_string()));
                    }
                }
            }
        }
        None
    }

    fn set_media_dtls_setup(&mut self, setup: DtlsSetup) {
        // Remove existing setup
        self.attributes
            .retain(|attr| !matches!(attr, Attribute::Value { name, .. } if name == "setup"));

        // Add new setup
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("setup"),
            value: SmolStr::new(setup.as_str()),
        });
    }

    fn get_media_dtls_setup(&self) -> Option<DtlsSetup> {
        for attr in &self.attributes {
            if let Attribute::Value { name, value } = attr {
                if name == "setup" {
                    return DtlsSetup::parse(value);
                }
            }
        }
        None
    }
}

fn is_valid_fingerprint_algorithm(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

fn is_valid_fingerprint_hash(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| matches!(b, b'A'..=b'F' | b'a'..=b'f' | b'0'..=b'9' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_sdp::builder::SessionDescriptionBuilder;

    #[test]
    fn test_dtls_setup_as_str() {
        assert_eq!(DtlsSetup::Active.as_str(), "active");
        assert_eq!(DtlsSetup::Passive.as_str(), "passive");
        assert_eq!(DtlsSetup::Actpass.as_str(), "actpass");
        assert_eq!(DtlsSetup::Holdconn.as_str(), "holdconn");
    }

    #[test]
    fn test_dtls_setup_parse() {
        assert_eq!(DtlsSetup::parse("active"), Some(DtlsSetup::Active));
        assert_eq!(DtlsSetup::parse("PASSIVE"), Some(DtlsSetup::Passive));
        assert_eq!(DtlsSetup::parse("ActPass"), Some(DtlsSetup::Actpass));
        assert_eq!(DtlsSetup::parse("holdconn"), Some(DtlsSetup::Holdconn));
        assert_eq!(DtlsSetup::parse("invalid"), None);
    }

    #[test]
    fn test_set_dtls_fingerprint() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .session_name("Test")
            .build();

        sdp.set_dtls_fingerprint(
            "sha-256",
            "A1:B2:C3:D4:E5:F6:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23",
        );

        let (algorithm, hash) = sdp.get_dtls_fingerprint().unwrap();
        assert_eq!(algorithm, "sha-256");
        assert_eq!(
            hash,
            "A1:B2:C3:D4:E5:F6:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23"
        );
    }

    #[test]
    fn test_set_dtls_setup() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .session_name("Test")
            .build();

        sdp.set_dtls_setup(DtlsSetup::Actpass);
        assert_eq!(sdp.get_dtls_setup(), Some(DtlsSetup::Actpass));

        sdp.set_dtls_setup(DtlsSetup::Active);
        assert_eq!(sdp.get_dtls_setup(), Some(DtlsSetup::Active));
    }

    #[test]
    fn test_media_dtls_fingerprint() {
        let mut media = MediaDescription::audio(5000);

        media.set_media_dtls_fingerprint(
            "sha-256",
            "B1:C2:D3:E4:F5:06:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34",
        );

        let (algorithm, hash) = media.get_media_dtls_fingerprint().unwrap();
        assert_eq!(algorithm, "sha-256");
        assert_eq!(
            hash,
            "B1:C2:D3:E4:F5:06:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34:56:78:9A:BC:DE:F0:12:34"
        );
    }

    #[test]
    fn test_media_dtls_setup() {
        let mut media = MediaDescription::audio(5000);

        media.set_media_dtls_setup(DtlsSetup::Passive);
        assert_eq!(media.get_media_dtls_setup(), Some(DtlsSetup::Passive));
    }

    #[test]
    fn test_fingerprint_replaces_existing() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .session_name("Test")
            .build();

        sdp.set_dtls_fingerprint("sha-256", "ABC:DEF");
        sdp.set_dtls_fingerprint("sha-512", "123:456");

        let (algorithm, hash) = sdp.get_dtls_fingerprint().unwrap();
        assert_eq!(algorithm, "sha-512");
        assert_eq!(hash, "123:456");
    }

    #[test]
    fn test_rejects_invalid_fingerprint() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .session_name("Test")
            .build();

        sdp.set_dtls_fingerprint("sha-256\r\n", "ABC");
        assert!(sdp.get_dtls_fingerprint().is_none());

        sdp.set_dtls_fingerprint("sha-256", "ZZ:11");
        assert!(sdp.get_dtls_fingerprint().is_none());

        let mut media = MediaDescription::audio(5000);
        media.set_media_dtls_fingerprint("sha-256", "ABC\r\nDEF");
        assert!(media.get_media_dtls_fingerprint().is_none());
    }
}
