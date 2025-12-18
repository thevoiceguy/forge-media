//! Candidate gathering - enumerates network interfaces and creates ICE candidates

use crate::candidate::{CandidateType, IceCandidate, Protocol};
use crate::stun::StunClient;
use forge_core::Result;
use if_addrs::get_if_addrs;
use std::net::{IpAddr, SocketAddr};
use tokio::net::lookup_host;
use tracing::{debug, warn};

/// Gather host candidates from local network interfaces
///
/// Returns a list of host candidates for each usable network interface
pub async fn gather_host_candidates(component: u16, port: u16) -> Result<Vec<IceCandidate>> {
    let mut candidates = Vec::new();
    let mut foundation_counter = 1;

    // Get all network interfaces
    let interfaces = get_if_addrs().map_err(|e| {
        forge_core::ForgeError::Ice(format!("Failed to enumerate interfaces: {}", e))
    })?;

    debug!("Found {} network interfaces", interfaces.len());

    for iface in interfaces {
        // Filter out unsuitable interfaces
        if !is_usable_interface(&iface) {
            debug!(
                "Skipping interface {} ({}): not usable",
                iface.name,
                iface.addr.ip()
            );
            continue;
        }

        let ip = iface.addr.ip();
        let local_pref = calculate_local_preference(&iface.name, &ip);
        let foundation = foundation_counter.to_string();
        foundation_counter += 1;

        let candidate =
            IceCandidate::new_host(foundation, component, Protocol::Udp, ip, port, local_pref);

        debug!(
            "Gathered host candidate: {} on interface {}",
            candidate, iface.name
        );

        candidates.push(candidate);
    }

    if candidates.is_empty() {
        warn!("No usable network interfaces found for candidate gathering");
    } else {
        debug!("Gathered {} host candidates", candidates.len());
    }

    Ok(candidates)
}

/// Parse STUN server URI and resolve to SocketAddr
///
/// Supports formats:
/// - `stun:hostname:port` (e.g., "stun:stun.l.google.com:19302")
/// - `hostname:port` (e.g., "stun.l.google.com:19302")
/// - `ip:port` (e.g., "192.168.1.1:3478")
///
/// Returns the first resolved SocketAddr
async fn parse_stun_server(uri: &str) -> Result<SocketAddr> {
    // Strip "stun:" prefix if present
    let host_port = uri.strip_prefix("stun:").unwrap_or(uri);

    // Try direct SocketAddr parsing first
    if let Ok(addr) = host_port.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // Try DNS resolution
    let mut addrs = lookup_host(host_port).await.map_err(|e| {
        forge_core::ForgeError::Ice(format!("Failed to resolve STUN server '{}': {}", uri, e))
    })?;

    addrs.next().ok_or_else(|| {
        forge_core::ForgeError::Ice(format!("No addresses found for STUN server '{}'", uri))
    })
}

/// Gather server-reflexive candidates by querying STUN servers
///
/// For each host candidate, queries the configured STUN servers to discover
/// the server-reflexive address (external NAT mapping).
///
/// Returns a list of server-reflexive candidates discovered via STUN.
pub async fn gather_server_reflexive_candidates(
    host_candidates: &[IceCandidate],
    stun_servers: &[String],
    component: u16,
) -> Result<Vec<IceCandidate>> {
    let mut candidates = Vec::new();
    let mut foundation_counter = 1000; // Start at 1000 to avoid conflicts with host candidates

    if stun_servers.is_empty() {
        debug!("No STUN servers configured, skipping server-reflexive gathering");
        return Ok(candidates);
    }

    debug!(
        "Gathering server-reflexive candidates using {} STUN servers",
        stun_servers.len()
    );

    // For each host candidate, try to discover server-reflexive address
    for host in host_candidates {
        // Only process candidates for the requested component
        if host.component != component {
            continue;
        }

        // Bind to the host candidate's address
        let local_addr = SocketAddr::new(host.ip, host.port);

        // Create STUN client
        let stun_client = match StunClient::new_with_reuse(local_addr).await {
            Ok(client) => client,
            Err(e) => {
                warn!("Failed to create STUN client for {}: {}", local_addr, e);
                continue;
            }
        };

        // Query each STUN server
        for stun_server_str in stun_servers {
            // Parse and resolve STUN server URI
            let stun_server_addr = match parse_stun_server(stun_server_str).await {
                Ok(addr) => addr,
                Err(e) => {
                    warn!("Failed to parse STUN server '{}': {}", stun_server_str, e);
                    continue;
                }
            };

            debug!(
                "Querying STUN server {} from local {}",
                stun_server_addr, local_addr
            );

            // Perform STUN binding request
            match stun_client.binding_request(stun_server_addr, None).await {
                Ok(mapped_addr) => {
                    // Check if the mapped address is different from local address
                    // (if same, we're not behind NAT for this interface)
                    if mapped_addr.ip() != host.ip || mapped_addr.port() != host.port {
                        let foundation = foundation_counter.to_string();
                        foundation_counter += 1;

                        // Inherit local preference from host candidate
                        let host_local_pref = host.get_local_preference();

                        // Create server-reflexive candidate
                        let candidate = IceCandidate {
                            foundation,
                            component,
                            protocol: Protocol::Udp,
                            priority: IceCandidate::compute_priority(
                                CandidateType::ServerReflexive,
                                host_local_pref,
                                component,
                            ),
                            ip: mapped_addr.ip(),
                            port: mapped_addr.port(),
                            typ: CandidateType::ServerReflexive,
                            rel_addr: Some(host.ip),
                            rel_port: Some(host.port),
                        };

                        debug!(
                            "Discovered server-reflexive candidate: {} (base: {})",
                            candidate, local_addr
                        );

                        candidates.push(candidate);

                        // Only create one server-reflexive candidate per host candidate
                        // (no need to query multiple STUN servers for the same host)
                        break;
                    } else {
                        debug!(
                            "STUN response shows no NAT (mapped={}, local={})",
                            mapped_addr, local_addr
                        );
                    }
                }
                Err(e) => {
                    debug!("STUN request to {} failed: {}", stun_server_addr, e);
                    // Continue to next STUN server
                }
            }
        }
    }

    if candidates.is_empty() {
        debug!("No server-reflexive candidates discovered");
    } else {
        debug!("Gathered {} server-reflexive candidates", candidates.len());
    }

    Ok(candidates)
}

/// Check if an interface is usable for ICE candidates
fn is_usable_interface(iface: &if_addrs::Interface) -> bool {
    let ip = iface.addr.ip();

    // Skip loopback addresses
    if ip.is_loopback() {
        return false;
    }

    // Skip link-local addresses (169.254.x.x for IPv4, fe80:: for IPv6)
    if is_link_local(&ip) {
        return false;
    }

    // For IPv6, skip temporary/privacy addresses (we prefer stable addresses)
    // This is a heuristic - RFC 4941 privacy extensions
    if let IpAddr::V6(ipv6) = ip {
        // Skip documentation prefix (2001:db8::/32)
        if ipv6.segments()[0] == 0x2001 && ipv6.segments()[1] == 0x0db8 {
            return false;
        }

        // Skip unique local addresses (fc00::/7) for now
        // In production, you might want to include these
        if (ipv6.segments()[0] & 0xfe00) == 0xfc00 {
            return false;
        }
    }

    // Check if interface is up (if available from system)
    // Note: if-addrs doesn't provide interface flags, so we assume it's up

    true
}

/// Check if an IP address is link-local
fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            // 169.254.0.0/16
            let octets = ipv4.octets();
            octets[0] == 169 && octets[1] == 254
        }
        IpAddr::V6(ipv6) => {
            // fe80::/10
            (ipv6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Calculate local preference for an interface
///
/// Higher values = more preferred
/// Typically: ethernet > wifi > vpn > other
pub fn calculate_local_preference(iface_name: &str, ip: &IpAddr) -> u16 {
    let mut pref = 32768u16; // Base preference

    // Prefer IPv4 over IPv6 (for now, configurable in production)
    if ip.is_ipv6() {
        pref = pref.saturating_sub(1000);
    }

    // Heuristics for interface type based on name
    let name_lower = iface_name.to_lowercase();

    if name_lower.contains("eth") || name_lower.contains("en") {
        // Ethernet interfaces
        pref = pref.saturating_add(1000);
    } else if name_lower.contains("wlan")
        || name_lower.contains("wi-fi")
        || name_lower.contains("wifi")
    {
        // WiFi interfaces
        pref = pref.saturating_add(500);
    } else if name_lower.contains("vpn") || name_lower.contains("tun") || name_lower.contains("tap")
    {
        // VPN interfaces (lower priority)
        pref = pref.saturating_sub(1000);
    }

    pref
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_link_local_detection() {
        // IPv4 link-local
        assert!(is_link_local(&IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
        assert!(!is_link_local(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));

        // IPv6 link-local
        let link_local_v6 = "fe80::1".parse::<Ipv6Addr>().unwrap();
        assert!(is_link_local(&IpAddr::V6(link_local_v6)));

        let global_v6 = "2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap();
        assert!(!is_link_local(&IpAddr::V6(global_v6)));
    }

    #[test]
    fn test_local_preference() {
        let ip4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ip6 = IpAddr::V6("2001:db8::1".parse().unwrap());

        let eth_pref = calculate_local_preference("eth0", &ip4);
        let wifi_pref = calculate_local_preference("wlan0", &ip4);
        let vpn_pref = calculate_local_preference("tun0", &ip4);

        // Ethernet should be preferred over WiFi, WiFi over VPN
        assert!(eth_pref > wifi_pref);
        assert!(wifi_pref > vpn_pref);

        // IPv4 should be slightly preferred over IPv6
        let eth_v4_pref = calculate_local_preference("eth0", &ip4);
        let eth_v6_pref = calculate_local_preference("eth0", &ip6);
        assert!(eth_v4_pref > eth_v6_pref);
    }

    #[tokio::test]
    async fn test_gather_host_candidates() {
        // This test will succeed if there are any network interfaces
        let result = gather_host_candidates(1, 50000).await;
        assert!(result.is_ok());

        let candidates = result.unwrap();
        // Should have at least some candidates (unless running in very restricted environment)
        // We don't assert a specific number as it depends on the system
        println!("Gathered {} candidates", candidates.len());

        for candidate in candidates {
            assert_eq!(candidate.component, 1);
            assert_eq!(candidate.port, 50000);
            assert_eq!(candidate.protocol, Protocol::Udp);
            println!("  - {}", candidate);
        }
    }
}
