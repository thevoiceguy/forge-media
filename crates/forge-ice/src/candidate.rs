//! ICE Candidate types and structures (RFC 8445)

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;

/// ICE candidate type as per RFC 8445 Section 5.1
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateType {
    /// Host candidate - a local IP address
    Host,
    /// Server-reflexive candidate - discovered via STUN
    ServerReflexive,
    /// Peer-reflexive candidate - discovered during connectivity checks
    PeerReflexive,
    /// Relay candidate - obtained from TURN server
    Relay,
}

impl fmt::Display for CandidateType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateType::Host => write!(f, "host"),
            CandidateType::ServerReflexive => write!(f, "srflx"),
            CandidateType::PeerReflexive => write!(f, "prflx"),
            CandidateType::Relay => write!(f, "relay"),
        }
    }
}

/// Transport protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    /// UDP transport
    Udp,
    /// TCP transport (not implemented yet)
    Tcp,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Udp => write!(f, "UDP"),
            Protocol::Tcp => write!(f, "TCP"),
        }
    }
}

/// Component ID for RTP/RTCP multiplexing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Component {
    /// RTP component (component ID 1)
    Rtp = 1,
    /// RTCP component (component ID 2)
    Rtcp = 2,
}

/// ICE Candidate as per RFC 8445
///
/// Represents a transport address that can be used for connectivity checks
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceCandidate {
    /// Foundation - unique identifier for candidates sharing the same base
    pub foundation: String,

    /// Component ID (1 = RTP, 2 = RTCP)
    pub component: u16,

    /// Transport protocol
    pub protocol: Protocol,

    /// Priority (computed per RFC 8445 Section 5.1.2)
    pub priority: u32,

    /// IP address
    pub ip: IpAddr,

    /// Port number
    pub port: u16,

    /// Candidate type
    pub typ: CandidateType,

    /// Related address (for srflx and relay candidates)
    pub rel_addr: Option<IpAddr>,

    /// Related port (for srflx and relay candidates)
    pub rel_port: Option<u16>,
}

impl IceCandidate {
    /// Create a new host candidate
    pub fn new_host(
        foundation: String,
        component: u16,
        protocol: Protocol,
        ip: IpAddr,
        port: u16,
        local_pref: u16,
    ) -> Self {
        let priority = Self::compute_priority(CandidateType::Host, local_pref, component);

        Self {
            foundation,
            component,
            protocol,
            priority,
            ip,
            port,
            typ: CandidateType::Host,
            rel_addr: None,
            rel_port: None,
        }
    }

    /// Create a new server-reflexive candidate
    pub fn new_server_reflexive(
        foundation: String,
        component: u16,
        protocol: Protocol,
        ip: IpAddr,
        port: u16,
        base_ip: IpAddr,
        base_port: u16,
        local_pref: u16,
    ) -> Self {
        let priority =
            Self::compute_priority(CandidateType::ServerReflexive, local_pref, component);

        Self {
            foundation,
            component,
            protocol,
            priority,
            ip,
            port,
            typ: CandidateType::ServerReflexive,
            rel_addr: Some(base_ip),
            rel_port: Some(base_port),
        }
    }

    /// Compute candidate priority per RFC 8445 Section 5.1.2
    ///
    /// priority = (2^24) * (type preference) +
    ///            (2^8)  * (local preference) +
    ///            (2^0)  * (256 - component ID)
    pub fn compute_priority(typ: CandidateType, local_pref: u16, component: u16) -> u32 {
        let type_pref = match typ {
            CandidateType::Host => 126,
            CandidateType::PeerReflexive => 110,
            CandidateType::ServerReflexive => 100,
            CandidateType::Relay => 0,
        };

        // `local_pref` is a u16 whose max is already 65535; no clamp needed.
        let local_pref = local_pref as u32;
        let component_val = (256 - component.min(256)) as u32;

        (type_pref << 24) | (local_pref << 8) | component_val
    }

    /// Extract local preference from candidate priority
    ///
    /// Returns the 16-bit local preference value encoded in the priority field.
    /// Useful for inheriting local preference from host candidates to their
    /// derived server-reflexive candidates.
    pub fn get_local_preference(&self) -> u16 {
        ((self.priority >> 8) & 0xFFFF) as u16
    }

    /// Convert to SDP candidate attribute string
    ///
    /// Format: "candidate:<foundation> <component> <protocol> <priority>
    ///          <ip> <port> typ <type> [raddr <rel-addr> rport <rel-port>]"
    pub fn to_sdp_attribute(&self) -> String {
        let mut attr = format!(
            "candidate:{} {} {} {} {} {} typ {}",
            self.foundation,
            self.component,
            self.protocol,
            self.priority,
            self.ip,
            self.port,
            self.typ
        );

        if let (Some(rel_addr), Some(rel_port)) = (self.rel_addr, self.rel_port) {
            attr.push_str(&format!(" raddr {} rport {}", rel_addr, rel_port));
        }

        attr
    }

    /// Parse from SDP candidate attribute string
    pub fn from_sdp_attribute(attr: &str) -> Result<Self, String> {
        // Remove "candidate:" prefix if present
        let attr = attr.strip_prefix("candidate:").unwrap_or(attr);

        let parts: Vec<&str> = attr.split_whitespace().collect();
        if parts.len() < 8 {
            return Err("Invalid candidate format".to_string());
        }

        let foundation = parts[0].to_string();
        let component = parts[1].parse().map_err(|_| "Invalid component")?;
        let protocol = match parts[2].to_uppercase().as_str() {
            "UDP" => Protocol::Udp,
            "TCP" => Protocol::Tcp,
            _ => return Err("Invalid protocol".to_string()),
        };
        let priority = parts[3].parse().map_err(|_| "Invalid priority")?;
        let ip: IpAddr = parts[4].parse().map_err(|_| "Invalid IP address")?;
        let port = parts[5].parse().map_err(|_| "Invalid port")?;

        if parts[6] != "typ" {
            return Err("Missing 'typ' keyword".to_string());
        }

        let typ = match parts[7] {
            "host" => CandidateType::Host,
            "srflx" => CandidateType::ServerReflexive,
            "prflx" => CandidateType::PeerReflexive,
            "relay" => CandidateType::Relay,
            _ => return Err("Invalid candidate type".to_string()),
        };

        // Parse optional raddr/rport
        let mut rel_addr = None;
        let mut rel_port = None;

        let mut i = 8;
        while i < parts.len() {
            match parts[i] {
                "raddr" if i + 1 < parts.len() => {
                    rel_addr = parts[i + 1].parse().ok();
                    i += 2;
                }
                "rport" if i + 1 < parts.len() => {
                    rel_port = parts[i + 1].parse().ok();
                    i += 2;
                }
                _ => i += 1,
            }
        }

        Ok(Self {
            foundation,
            component,
            protocol,
            priority,
            ip,
            port,
            typ,
            rel_addr,
            rel_port,
        })
    }
}

impl fmt::Display for IceCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}:{}",
            self.typ, self.protocol, self.ip, self.port
        )
    }
}

/// Candidate pair for connectivity checks
#[derive(Debug, Clone)]
pub struct CandidatePair {
    /// Local candidate
    pub local: IceCandidate,

    /// Remote candidate
    pub remote: IceCandidate,

    /// Pair priority (RFC 8445 Section 6.1.2.3)
    pub priority: u64,

    /// Pair state
    pub state: PairState,
}

/// State of a candidate pair
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairState {
    /// Pair is waiting to be checked
    Waiting,
    /// Check is in progress
    InProgress,
    /// Check succeeded
    Succeeded,
    /// Check failed
    Failed,
    /// Pair is frozen (waiting for foundation)
    Frozen,
}

impl CandidatePair {
    /// Create a new candidate pair
    pub fn new(local: IceCandidate, remote: IceCandidate) -> Self {
        let priority = Self::compute_pair_priority(local.priority, remote.priority);

        Self {
            local,
            remote,
            priority,
            state: PairState::Frozen,
        }
    }

    /// Compute pair priority per RFC 8445 Section 6.1.2.3
    ///
    /// pair_priority = 2^32 * MIN(G,D) + 2 * MAX(G,D) + (G>D?1:0)
    fn compute_pair_priority(local_priority: u32, remote_priority: u32) -> u64 {
        let g = local_priority as u64;
        let d = remote_priority as u64;

        let min = g.min(d);
        let max = g.max(d);

        (1u64 << 32) * min + 2 * max + if g > d { 1 } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_candidate_priority() {
        // Host candidate should have highest priority
        let host_priority = IceCandidate::compute_priority(CandidateType::Host, 65535, 1);
        let srflx_priority =
            IceCandidate::compute_priority(CandidateType::ServerReflexive, 65535, 1);
        let relay_priority = IceCandidate::compute_priority(CandidateType::Relay, 65535, 1);

        assert!(host_priority > srflx_priority);
        assert!(srflx_priority > relay_priority);
    }

    #[test]
    fn test_candidate_sdp_roundtrip() {
        let candidate = IceCandidate::new_host(
            "1".to_string(),
            1,
            Protocol::Udp,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            50000,
            65535,
        );

        let sdp = candidate.to_sdp_attribute();
        let parsed = IceCandidate::from_sdp_attribute(&sdp).unwrap();

        assert_eq!(candidate.foundation, parsed.foundation);
        assert_eq!(candidate.component, parsed.component);
        assert_eq!(candidate.ip, parsed.ip);
        assert_eq!(candidate.port, parsed.port);
        assert_eq!(candidate.typ, parsed.typ);
    }

    #[test]
    fn test_pair_priority() {
        let local = IceCandidate::new_host(
            "1".to_string(),
            1,
            Protocol::Udp,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            50000,
            65535,
        );

        let remote = IceCandidate::new_server_reflexive(
            "2".to_string(),
            1,
            Protocol::Udp,
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)),
            50000,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            50000,
            65535,
        );

        let pair = CandidatePair::new(local, remote);
        assert!(pair.priority > 0);
    }
}
