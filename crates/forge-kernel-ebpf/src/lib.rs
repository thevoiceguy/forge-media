#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::xdp_action,
    macros::{map, xdp},
    maps::{HashMap, RingBuf},
    programs::XdpContext,
};
use core::mem;

/// Ethernet header
#[repr(C)]
struct EthHdr {
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    eth_type: u16, // Network byte order
}

/// IPv4 header (simplified)
#[repr(C)]
struct Ipv4Hdr {
    version_ihl: u8,
    tos: u8,
    tot_len: u16,
    id: u16,
    frag_off: u16,
    ttl: u8,
    protocol: u8,
    check: u16,
    saddr: u32,
    daddr: u32,
}

/// UDP header
#[repr(C)]
struct UdpHdr {
    source: u16,
    dest: u16,
    len: u16,
    check: u16,
}

/// Forward map key: UDP 5-tuple
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForwardKey {
    pub src_ip: u32,   // Source IP (network byte order)
    pub src_port: u16, // Source port (network byte order)
    pub dst_port: u16, // Destination port (our RTP port)
    pub dst_ip: u32,   // Destination IP (our IP)
    pub protocol: u8,  // UDP = 17
    pub _padding: [u8; 3],
}

/// Forward map value: destination to forward to
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForwardValue {
    pub dest_ip: u32,   // Where to forward (network byte order)
    pub dest_port: u16, // Destination port
    pub src_ip: u32,    // Our source IP for reply
    pub src_port: u16,  // Our source port for reply
    pub last_seen: u64, // Timestamp (nanoseconds)
}

/// Statistics per session
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SessionStats {
    pub packets_forwarded: u64,
    pub bytes_forwarded: u64,
    pub last_packet_ts: u64,
}

/// Event types
#[repr(u8)]
#[derive(Clone, Copy)]
pub enum EventType {
    UnknownSource = 1,  // Packet from unknown source (needs learning)
    ForwardSuccess = 2, // Packet forwarded successfully
    ParseError = 3,     // Failed to parse packet
}

/// Event sent to userspace via ring buffer
#[repr(C)]
pub struct Event {
    pub event_type: u8,
    pub src_ip: u32,
    pub src_port: u16,
    pub dst_port: u16,
    pub timestamp: u64,
}

/// Forward map: 5-tuple → forward destination
#[map]
static FORWARD_MAP: HashMap<ForwardKey, ForwardValue> = HashMap::with_max_entries(10000, 0);

/// Statistics map: session ID → stats
#[map]
static STATS_MAP: HashMap<u32, SessionStats> = HashMap::with_max_entries(10000, 0);

/// Event ring buffer for userspace communication
#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

/// Constants
const ETH_P_IP: u16 = 0x0800;
const RTP_PORT_MIN: u16 = 30000;
const RTP_PORT_MAX: u16 = 40000;
const IPPROTO_UDP: u8 = 17;

/// XDP program entry point
#[xdp]
pub fn rtp_forward(ctx: XdpContext) -> u32 {
    match try_forward(&ctx) {
        Ok(action) => action,
        Err(_) => xdp_action::XDP_PASS,
    }
}

/// Main forwarding logic
fn try_forward(ctx: &XdpContext) -> Result<u32, ()> {
    let data_start = ctx.data();
    let data_end = ctx.data_end();

    // Parse Ethernet header
    let eth = ptr_at::<EthHdr>(&ctx, 0).map_err(|_| {
        send_event(EventType::ParseError, 0, 0, 0);
    })?;

    // Check if it's IPv4
    if unsafe { (*eth).eth_type } != u16::to_be(ETH_P_IP) {
        return Ok(xdp_action::XDP_PASS);
    }

    // Parse IP header
    let ip_offset = mem::size_of::<EthHdr>();
    let ip = ptr_at::<Ipv4Hdr>(&ctx, ip_offset).map_err(|_| {
        send_event(EventType::ParseError, 0, 0, 0);
    })?;

    // Validate IPv4 version
    let version = (unsafe { (*ip).version_ihl } >> 4) & 0x0f;
    if version != 4 {
        send_event(EventType::ParseError, 0, 0, 0);
        return Ok(xdp_action::XDP_PASS);
    }

    // Validate IHL (Internet Header Length) >= 5 (20 bytes minimum)
    let ihl = (unsafe { (*ip).version_ihl } & 0x0f) as usize;
    if ihl < 5 {
        send_event(EventType::ParseError, 0, 0, 0);
        return Ok(xdp_action::XDP_PASS);
    }

    let ip_hdr_len = ihl * 4;

    // Validate total length doesn't exceed packet bounds
    let tot_len = u16::from_be(unsafe { (*ip).tot_len }) as usize;
    let packet_len = (data_end - data_start) as usize;
    if ip_offset + tot_len > packet_len {
        send_event(EventType::ParseError, 0, 0, 0);
        return Ok(xdp_action::XDP_PASS);
    }

    // Check for IP fragments (drop them - we can't parse UDP header on non-first fragments)
    let frag_off = u16::from_be(unsafe { (*ip).frag_off });
    let is_fragment = (frag_off & 0x1FFF) != 0; // Fragment offset != 0
    let more_fragments = (frag_off & 0x2000) != 0; // More Fragments flag

    if is_fragment || more_fragments {
        // Drop fragments - can't safely parse/rewrite UDP on non-first fragments
        return Ok(xdp_action::XDP_DROP);
    }

    // Check if it's UDP
    if unsafe { (*ip).protocol } != IPPROTO_UDP {
        return Ok(xdp_action::XDP_PASS);
    }

    // Validate UDP header fits within IP payload
    let udp_offset = ip_offset + ip_hdr_len;
    if udp_offset + mem::size_of::<UdpHdr>() > ip_offset + tot_len {
        send_event(EventType::ParseError, 0, 0, 0);
        return Ok(xdp_action::XDP_PASS);
    }

    // Parse UDP header
    let udp = ptr_at::<UdpHdr>(&ctx, udp_offset).map_err(|_| {
        send_event(EventType::ParseError, 0, 0, 0);
    })?;

    // Get port in host byte order for comparison
    let dst_port = u16::from_be(unsafe { (*udp).dest });
    let src_port = u16::from_be(unsafe { (*udp).source });

    // Check if destination port is in RTP range
    if dst_port < RTP_PORT_MIN || dst_port > RTP_PORT_MAX {
        return Ok(xdp_action::XDP_PASS);
    }

    // Skip RTCP (odd ports)
    if dst_port % 2 == 1 {
        return Ok(xdp_action::XDP_PASS);
    }

    // Build forward key (5-tuple)
    let key = ForwardKey {
        src_ip: unsafe { (*ip).saddr },
        src_port: unsafe { (*udp).source },
        dst_port: unsafe { (*udp).dest },
        dst_ip: unsafe { (*ip).daddr },
        protocol: IPPROTO_UDP,
        _padding: [0; 3],
    };

    // Lookup in forward map
    let forward_value = unsafe { FORWARD_MAP.get(&key) };

    if let Some(fwd) = forward_value {
        // Found a forwarding rule - rewrite headers and forward
        // Get mutable pointers for header rewrite
        let ip_mut = ptr_at_mut::<Ipv4Hdr>(&ctx, ip_offset)?;
        let udp_mut = ptr_at_mut::<UdpHdr>(&ctx, udp_offset)?;

        unsafe {
            // Decrement TTL (RFC 1812 Section 5.3.1)
            let old_ttl = (*ip_mut).ttl;
            if old_ttl <= 1 {
                // TTL expired - drop packet to prevent routing loops
                return Ok(xdp_action::XDP_DROP);
            }
            let new_ttl = old_ttl - 1;

            // Save old values for incremental checksum calculation
            let old_saddr = (*ip_mut).saddr;
            let old_daddr = (*ip_mut).daddr;
            let old_src_port = (*udp_mut).source;
            let old_dst_port = (*udp_mut).dest;
            let old_ip_check = (*ip_mut).check;
            let old_udp_check = (*udp_mut).check;

            // Rewrite IP header
            (*ip_mut).ttl = new_ttl;
            (*ip_mut).saddr = fwd.src_ip;
            (*ip_mut).daddr = fwd.dest_ip;

            // Rewrite UDP ports
            (*udp_mut).source = fwd.src_port;
            (*udp_mut).dest = fwd.dest_port;

            // Incremental IP checksum update (RFC 1624)
            (*ip_mut).check = update_ip_checksum(
                old_ip_check,
                old_ttl,
                new_ttl,
                old_saddr,
                fwd.src_ip,
                old_daddr,
                fwd.dest_ip,
            );

            // Incremental UDP checksum update (if non-zero)
            // UDP checksum 0 means "no checksum" in IPv4
            if old_udp_check != 0 {
                (*udp_mut).check = update_udp_checksum(
                    old_udp_check,
                    old_saddr,
                    fwd.src_ip,
                    old_daddr,
                    fwd.dest_ip,
                    old_src_port,
                    fwd.src_port,
                    old_dst_port,
                    fwd.dest_port,
                );
            }
        }

        // Update statistics
        // TODO: Implement atomic increments for packet/byte counters

        // L2 Forwarding Strategy:
        // XDP_TX transmits the packet back out the same interface it arrived on.
        // This works for:
        //   - Hairpin NAT (reflect packet back to same network)
        //   - Local bridge configurations
        //
        // For different L2 forwarding models, consider:
        //   - XDP_REDIRECT: Forward to a different interface (requires bpf_redirect)
        //   - MAC rewrite: Change Ethernet src/dst MACs before XDP_TX
        //   - XDP_PASS: Let kernel routing handle L2/L3 forwarding
        //
        // Current implementation: XDP_TX (same interface loopback)
        // TODO: Make L2 forwarding strategy configurable via map or build-time feature
        return Ok(xdp_action::XDP_TX);
    }

    // No forwarding rule found - send event and pass to userspace for learning
    send_event(
        EventType::UnknownSource,
        unsafe { (*ip).saddr },
        src_port,
        dst_port,
    );

    Ok(xdp_action::XDP_PASS)
}

/// Helper: Send event to userspace via ring buffer
#[inline(always)]
fn send_event(event_type: EventType, src_ip: u32, src_port: u16, dst_port: u16) {
    if let Some(mut entry) = EVENTS.reserve::<Event>(0) {
        let event = Event {
            event_type: event_type as u8,
            src_ip,
            src_port,
            dst_port,
            timestamp: get_timestamp(),
        };
        entry.write(event);
        entry.submit(0);
    }
}

/// Helper: Get current timestamp in nanoseconds
#[inline(always)]
fn get_timestamp() -> u64 {
    unsafe { aya_ebpf::helpers::bpf_ktime_get_ns() }
}

/// Helper: Incremental IP checksum update (RFC 1624)
///
/// Updates checksum when IP header fields change, avoiding full recalculation.
/// Formula: checksum' = ~(~checksum + ~old_data + new_data)
#[inline(always)]
fn update_ip_checksum(
    old_check: u16,
    old_ttl: u8,
    new_ttl: u8,
    old_saddr: u32,
    new_saddr: u32,
    old_daddr: u32,
    new_daddr: u32,
) -> u16 {
    let mut sum = !old_check as u32;

    // Remove old TTL (TTL is in byte 8, need to account for byte position)
    sum = sum.wrapping_sub((old_ttl as u32) << 8);
    sum = sum.wrapping_add((new_ttl as u32) << 8);

    // Remove old source address (2 x 16-bit words)
    sum = sum.wrapping_sub((old_saddr >> 16) & 0xFFFF);
    sum = sum.wrapping_sub(old_saddr & 0xFFFF);

    // Remove old destination address (2 x 16-bit words)
    sum = sum.wrapping_sub((old_daddr >> 16) & 0xFFFF);
    sum = sum.wrapping_sub(old_daddr & 0xFFFF);

    // Add new source address
    sum = sum.wrapping_add((new_saddr >> 16) & 0xFFFF);
    sum = sum.wrapping_add(new_saddr & 0xFFFF);

    // Add new destination address
    sum = sum.wrapping_add((new_daddr >> 16) & 0xFFFF);
    sum = sum.wrapping_add(new_daddr & 0xFFFF);

    // Fold 32-bit sum to 16 bits
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !sum as u16
}

/// Helper: Incremental UDP checksum update
///
/// Updates UDP checksum when pseudo-header or port fields change.
/// UDP checksum covers: pseudo-header (IPs, protocol, length) + UDP header + data
#[inline(always)]
fn update_udp_checksum(
    old_check: u16,
    old_saddr: u32,
    new_saddr: u32,
    old_daddr: u32,
    new_daddr: u32,
    old_src_port: u16,
    new_src_port: u16,
    old_dst_port: u16,
    new_dst_port: u16,
) -> u16 {
    let mut sum = !old_check as u32;

    // Remove old pseudo-header addresses
    sum = sum.wrapping_sub((old_saddr >> 16) & 0xFFFF);
    sum = sum.wrapping_sub(old_saddr & 0xFFFF);
    sum = sum.wrapping_sub((old_daddr >> 16) & 0xFFFF);
    sum = sum.wrapping_sub(old_daddr & 0xFFFF);

    // Add new pseudo-header addresses
    sum = sum.wrapping_add((new_saddr >> 16) & 0xFFFF);
    sum = sum.wrapping_add(new_saddr & 0xFFFF);
    sum = sum.wrapping_add((new_daddr >> 16) & 0xFFFF);
    sum = sum.wrapping_add(new_daddr & 0xFFFF);

    // Remove old ports (already in network byte order)
    sum = sum.wrapping_sub(u16::from_be(old_src_port) as u32);
    sum = sum.wrapping_sub(u16::from_be(old_dst_port) as u32);

    // Add new ports
    sum = sum.wrapping_add(u16::from_be(new_src_port) as u32);
    sum = sum.wrapping_add(u16::from_be(new_dst_port) as u32);

    // Fold 32-bit sum to 16 bits
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    let result = !sum as u16;

    // UDP checksum 0 is special: means "no checksum"
    // If our calculation resulted in 0, use 0xFFFF instead (RFC 768)
    if result == 0 {
        0xFFFF
    } else {
        result
    }
}

/// Helper: Get pointer to struct at offset with bounds checking
/// This is critical for BPF verifier
#[inline(always)]
fn ptr_at<T>(ctx: &XdpContext, offset: usize) -> Result<*const T, ()> {
    let start = ctx.data();
    let end = ctx.data_end();
    let len = mem::size_of::<T>();

    if start + offset + len > end {
        return Err(());
    }

    Ok((start + offset) as *const T)
}

/// Helper: Get mutable pointer to struct at offset with bounds checking
#[inline(always)]
fn ptr_at_mut<T>(ctx: &XdpContext, offset: usize) -> Result<*mut T, ()> {
    let start = ctx.data();
    let end = ctx.data_end();
    let len = mem::size_of::<T>();

    if start + offset + len > end {
        return Err(());
    }

    Ok((start + offset) as *mut T)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
