//! ICE (Interactive Connectivity Establishment) attribute helpers for SDP
//!
//! Provides convenient methods for working with ICE-related SDP attributes
//! as defined in RFC 8839 (ICE in SDP).

use crate::{Attribute, MediaDescription, SessionDescription};
use smol_str::SmolStr;

/// Extension trait for adding ICE attributes to SDP
pub trait IceAttributesExt {
    /// Set ICE credentials (username fragment and password)
    ///
    /// Adds a=ice-ufrag and a=ice-pwd attributes at the session level.
    fn set_ice_credentials(&mut self, ufrag: &str, pwd: &str);

    /// Get ICE credentials from session-level attributes
    ///
    /// Returns (ufrag, pwd) if both are present.
    fn get_ice_credentials(&self) -> Option<(String, String)>;

    /// Add ICE options (e.g., "trickle" for trickle ICE)
    ///
    /// Adds a=ice-options:<option> attribute.
    fn add_ice_option(&mut self, option: &str);

    /// Get all ICE options
    fn get_ice_options(&self) -> Vec<String>;

    /// Check if trickle ICE is enabled
    fn has_trickle_ice(&self) -> bool {
        self.get_ice_options()
            .iter()
            .any(|opt| opt.eq_ignore_ascii_case("trickle"))
    }
}

/// Extension trait for adding ICE attributes to media descriptions
pub trait MediaIceAttributesExt {
    /// Add an ICE candidate to this media description
    ///
    /// Format: a=candidate:<foundation> <component-id> <transport> <priority> <connection-address> <port> typ <cand-type> [raddr <rel-addr>] [rport <rel-port>]
    ///
    /// # Example
    /// ```ignore
    /// media.add_ice_candidate("1", 1, "UDP", 2130706431, "192.168.1.100", 50000, "host", None, None);
    /// // Produces: a=candidate:1 1 UDP 2130706431 192.168.1.100 50000 typ host
    /// ```
    fn add_ice_candidate(
        &mut self,
        foundation: &str,
        component: u16,
        transport: &str,
        priority: u32,
        connection_address: &str,
        port: u16,
        cand_type: &str,
        rel_addr: Option<&str>,
        rel_port: Option<u16>,
    );

    /// Add ICE candidate from forge-ice IceCandidate
    #[cfg(feature = "forge-ice")]
    fn add_ice_candidate_from_forge(&mut self, candidate: &forge_ice::IceCandidate) {
        let rel_addr = candidate.rel_addr.as_ref().map(|ip| ip.to_string());
        let rel_port = candidate.rel_port;

        let cand_type = match candidate.typ {
            forge_ice::CandidateType::Host => "host",
            forge_ice::CandidateType::ServerReflexive => "srflx",
            forge_ice::CandidateType::PeerReflexive => "prflx",
            forge_ice::CandidateType::Relay => "relay",
        };

        let transport = match candidate.protocol {
            forge_ice::Protocol::Udp => "UDP",
            forge_ice::Protocol::Tcp => "TCP",
        };

        self.add_ice_candidate(
            &candidate.foundation,
            candidate.component,
            transport,
            candidate.priority,
            &candidate.ip.to_string(),
            candidate.port,
            cand_type,
            rel_addr.as_deref(),
            rel_port,
        );
    }

    /// Get all ICE candidates from this media description
    ///
    /// Returns a vector of candidate attribute values.
    fn get_ice_candidates(&self) -> Vec<String>;

    /// Set ICE credentials at media level (overrides session-level)
    fn set_media_ice_credentials(&mut self, ufrag: &str, pwd: &str);

    /// Get media-level ICE credentials
    fn get_media_ice_credentials(&self) -> Option<(String, String)>;
}

impl IceAttributesExt for SessionDescription {
    fn set_ice_credentials(&mut self, ufrag: &str, pwd: &str) {
        // Remove existing ice-ufrag and ice-pwd
        self.attributes
            .retain(|attr| !matches!(attr, Attribute::Value { name, .. } if name == "ice-ufrag" || name == "ice-pwd"));

        // Add new credentials
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("ice-ufrag"),
            value: SmolStr::new(ufrag),
        });
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("ice-pwd"),
            value: SmolStr::new(pwd),
        });
    }

    fn get_ice_credentials(&self) -> Option<(String, String)> {
        let mut ufrag = None;
        let mut pwd = None;

        for attr in &self.attributes {
            if let Attribute::Value { name, value } = attr {
                match name.as_str() {
                    "ice-ufrag" => ufrag = Some(value.to_string()),
                    "ice-pwd" => pwd = Some(value.to_string()),
                    _ => {}
                }
            }
        }

        match (ufrag, pwd) {
            (Some(u), Some(p)) => Some((u, p)),
            _ => None,
        }
    }

    fn add_ice_option(&mut self, option: &str) {
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("ice-options"),
            value: SmolStr::new(option),
        });
    }

    fn get_ice_options(&self) -> Vec<String> {
        self.attributes
            .iter()
            .filter_map(|attr| {
                if let Attribute::Value { name, value } = attr {
                    if name == "ice-options" {
                        return Some(value.to_string());
                    }
                }
                None
            })
            .collect()
    }
}

impl MediaIceAttributesExt for MediaDescription {
    fn add_ice_candidate(
        &mut self,
        foundation: &str,
        component: u16,
        transport: &str,
        priority: u32,
        connection_address: &str,
        port: u16,
        cand_type: &str,
        rel_addr: Option<&str>,
        rel_port: Option<u16>,
    ) {
        let mut candidate = format!(
            "{} {} {} {} {} {} typ {}",
            foundation, component, transport, priority, connection_address, port, cand_type
        );

        // Add related address and port for non-host candidates
        if let Some(raddr) = rel_addr {
            candidate.push_str(&format!(" raddr {}", raddr));
        }
        if let Some(rport) = rel_port {
            candidate.push_str(&format!(" rport {}", rport));
        }

        self.attributes.push(Attribute::Value {
            name: SmolStr::new("candidate"),
            value: SmolStr::new(candidate),
        });
    }

    fn get_ice_candidates(&self) -> Vec<String> {
        self.attributes
            .iter()
            .filter_map(|attr| {
                if let Attribute::Value { name, value } = attr {
                    if name == "candidate" {
                        return Some(value.to_string());
                    }
                }
                None
            })
            .collect()
    }

    fn set_media_ice_credentials(&mut self, ufrag: &str, pwd: &str) {
        // Remove existing media-level ice credentials
        self.attributes
            .retain(|attr| !matches!(attr, Attribute::Value { name, .. } if name == "ice-ufrag" || name == "ice-pwd"));

        // Add new credentials
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("ice-ufrag"),
            value: SmolStr::new(ufrag),
        });
        self.attributes.push(Attribute::Value {
            name: SmolStr::new("ice-pwd"),
            value: SmolStr::new(pwd),
        });
    }

    fn get_media_ice_credentials(&self) -> Option<(String, String)> {
        let mut ufrag = None;
        let mut pwd = None;

        for attr in &self.attributes {
            if let Attribute::Value { name, value } = attr {
                match name.as_str() {
                    "ice-ufrag" => ufrag = Some(value.to_string()),
                    "ice-pwd" => pwd = Some(value.to_string()),
                    _ => {}
                }
            }
        }

        match (ufrag, pwd) {
            (Some(u), Some(p)) => Some((u, p)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_sdp::builder::SessionDescriptionBuilder;

    #[test]
    fn test_set_ice_credentials() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .session_name("Test")
            .build();

        sdp.set_ice_credentials("abcd1234", "password123456789012345678");

        let (ufrag, pwd) = sdp.get_ice_credentials().unwrap();
        assert_eq!(ufrag, "abcd1234");
        assert_eq!(pwd, "password123456789012345678");
    }

    #[test]
    fn test_ice_options() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .session_name("Test")
            .build();

        sdp.add_ice_option("trickle");
        sdp.add_ice_option("ice2");

        let options = sdp.get_ice_options();
        assert_eq!(options.len(), 2);
        assert!(options.contains(&"trickle".to_string()));
        assert!(options.contains(&"ice2".to_string()));
        assert!(sdp.has_trickle_ice());
    }

    #[test]
    fn test_add_ice_candidate() {
        let mut media = MediaDescription::audio(5000);

        media.add_ice_candidate(
            "1",
            1,
            "UDP",
            2130706431,
            "192.168.1.100",
            50000,
            "host",
            None,
            None,
        );

        let candidates = media.get_ice_candidates();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].contains("192.168.1.100"));
        assert!(candidates[0].contains("50000"));
        assert!(candidates[0].contains("typ host"));
    }

    #[test]
    fn test_add_srflx_candidate_with_related() {
        let mut media = MediaDescription::audio(5000);

        media.add_ice_candidate(
            "2",
            1,
            "UDP",
            1694498815,
            "203.0.113.1",
            51000,
            "srflx",
            Some("192.168.1.100"),
            Some(50000),
        );

        let candidates = media.get_ice_candidates();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].contains("typ srflx"));
        assert!(candidates[0].contains("raddr 192.168.1.100"));
        assert!(candidates[0].contains("rport 50000"));
    }
}
