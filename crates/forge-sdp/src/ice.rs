//! ICE (Interactive Connectivity Establishment) attribute helpers for SDP
//!
//! Provides convenient methods for working with ICE-related SDP attributes
//! as defined in RFC 8839 (ICE in SDP).

use crate::{Attribute, MediaDescription, SessionDescription};
use smol_str::SmolStr;

/// Parameters for adding an ICE candidate
#[derive(Debug, Clone)]
pub struct IceCandidateParams<'a> {
    /// Foundation (unique identifier for this candidate)
    pub foundation: &'a str,
    /// Component ID (1 = RTP, 2 = RTCP)
    pub component: u16,
    /// Transport protocol (typically "UDP" or "TCP")
    pub transport: &'a str,
    /// Priority value
    pub priority: u32,
    /// Connection address (IP address)
    pub connection_address: &'a str,
    /// Port number
    pub port: u16,
    /// Candidate type ("host", "srflx", "prflx", "relay")
    pub cand_type: &'a str,
    /// Related address (for non-host candidates)
    pub rel_addr: Option<&'a str>,
    /// Related port (for non-host candidates)
    pub rel_port: Option<u16>,
}

impl<'a> IceCandidateParams<'a> {
    /// Create a new ICE candidate parameters builder
    pub fn new(
        foundation: &'a str,
        component: u16,
        transport: &'a str,
        priority: u32,
        connection_address: &'a str,
        port: u16,
        cand_type: &'a str,
    ) -> Self {
        Self {
            foundation,
            component,
            transport,
            priority,
            connection_address,
            port,
            cand_type,
            rel_addr: None,
            rel_port: None,
        }
    }

    /// Set related address and port (for srflx, prflx, relay candidates)
    pub fn with_related(mut self, addr: &'a str, port: u16) -> Self {
        self.rel_addr = Some(addr);
        self.rel_port = Some(port);
        self
    }
}

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
    /// use forge_sdp::ice::IceCandidateParams;
    ///
    /// let params = IceCandidateParams::new(
    ///     "1", 1, "UDP", 2130706431, "192.168.1.100", 50000, "host"
    /// );
    /// media.add_ice_candidate(&params);
    /// // Produces: a=candidate:1 1 UDP 2130706431 192.168.1.100 50000 typ host
    /// ```
    fn add_ice_candidate(&mut self, params: &IceCandidateParams);

    /// Add ICE candidate from forge-ice IceCandidate
    #[cfg(feature = "forge-ice")]
    fn add_ice_candidate_from_forge(&mut self, candidate: &forge_ice::IceCandidate) {
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

        let ip_str = candidate.ip.to_string();
        let rel_addr_str = candidate.rel_addr.as_ref().map(|ip| ip.to_string());

        let mut params = IceCandidateParams::new(
            &candidate.foundation,
            candidate.component,
            transport,
            candidate.priority,
            &ip_str,
            candidate.port,
            cand_type,
        );

        if let (Some(addr), Some(port)) = (rel_addr_str.as_deref(), candidate.rel_port) {
            params = params.with_related(addr, port);
        }

        self.add_ice_candidate(&params);
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
        if !is_valid_ice_ufrag(ufrag) || !is_valid_ice_pwd(pwd) {
            return;
        }

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
        if !is_valid_token(option) {
            return;
        }

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
    fn add_ice_candidate(&mut self, params: &IceCandidateParams) {
        if !is_valid_token(params.foundation)
            || !is_valid_token(params.transport)
            || !is_valid_token(params.cand_type)
            || !is_valid_connection_address(params.connection_address)
        {
            return;
        }
        if let Some(addr) = params.rel_addr {
            if !is_valid_connection_address(addr) {
                return;
            }
        }
        let mut candidate = format!(
            "{} {} {} {} {} {} typ {}",
            params.foundation,
            params.component,
            params.transport,
            params.priority,
            params.connection_address,
            params.port,
            params.cand_type
        );

        // Add related address and port for non-host candidates
        if let Some(raddr) = params.rel_addr {
            candidate.push_str(&format!(" raddr {}", raddr));
        }
        if let Some(rport) = params.rel_port {
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
        if !is_valid_ice_ufrag(ufrag) || !is_valid_ice_pwd(pwd) {
            return;
        }

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

fn is_valid_ice_ufrag(value: &str) -> bool {
    let len = value.len();
    (4..=256).contains(&len) && is_valid_ice_token(value)
}

fn is_valid_ice_pwd(value: &str) -> bool {
    let len = value.len();
    (22..=256).contains(&len) && is_valid_ice_token(value)
}

fn is_valid_ice_token(value: &str) -> bool {
    value
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'-' | b'_'))
}

fn is_valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && value
            .bytes()
            .all(|b| !(b.is_ascii_whitespace() || b.is_ascii_control()))
}

fn is_valid_connection_address(value: &str) -> bool {
    is_valid_token(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_sdp::builder::SessionDescriptionBuilder;

    #[test]
    fn test_set_ice_credentials() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .unwrap()
            .session_name("Test")
            .unwrap()
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
            .unwrap()
            .session_name("Test")
            .unwrap()
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

        let params =
            IceCandidateParams::new("1", 1, "UDP", 2130706431, "192.168.1.100", 50000, "host");
        media.add_ice_candidate(&params);

        let candidates = media.get_ice_candidates();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].contains("192.168.1.100"));
        assert!(candidates[0].contains("50000"));
        assert!(candidates[0].contains("typ host"));
    }

    #[test]
    fn test_add_srflx_candidate_with_related() {
        let mut media = MediaDescription::audio(5000);

        let params =
            IceCandidateParams::new("2", 1, "UDP", 1694498815, "203.0.113.1", 51000, "srflx")
                .with_related("192.168.1.100", 50000);

        media.add_ice_candidate(&params);

        let candidates = media.get_ice_candidates();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].contains("typ srflx"));
        assert!(candidates[0].contains("raddr 192.168.1.100"));
        assert!(candidates[0].contains("rport 50000"));
    }

    #[test]
    fn test_rejects_invalid_ice_credentials() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .unwrap()
            .session_name("Test")
            .unwrap()
            .build();

        sdp.set_ice_credentials("bad\r\nvalue", "password123456789012345678");
        assert!(sdp.get_ice_credentials().is_none());

        sdp.set_ice_credentials("abcd1234", "short");
        assert!(sdp.get_ice_credentials().is_none());
    }

    #[test]
    fn test_rejects_invalid_ice_option_and_candidate() {
        let mut sdp = SessionDescriptionBuilder::new()
            .origin("test", "12345", "192.168.1.1")
            .unwrap()
            .session_name("Test")
            .unwrap()
            .build();

        sdp.add_ice_option("trickle\r\nfoo");
        assert!(sdp.get_ice_options().is_empty());

        let mut media = MediaDescription::audio(5000);
        let params = IceCandidateParams::new(
            "1",
            1,
            "UDP",
            2130706431,
            "192.168.1.100\r\na=evil",
            50000,
            "host",
        );
        media.add_ice_candidate(&params);
        assert!(media.get_ice_candidates().is_empty());
    }
}
