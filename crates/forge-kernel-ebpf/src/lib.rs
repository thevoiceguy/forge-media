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
    let eth = ptr_at::<EthHdr>(&ctx, 0)?;

    // Check if it's IPv4
    if unsafe { (*eth).eth_type } != u16::to_be(ETH_P_IP) {
        return Ok(xdp_action::XDP_PASS);
    }

    // Parse IP header
    let ip_offset = mem::size_of::<EthHdr>();
    let ip = ptr_at::<Ipv4Hdr>(&ctx, ip_offset)?;

    // Check if it's UDP
    if unsafe { (*ip).protocol } != IPPROTO_UDP {
        return Ok(xdp_action::XDP_PASS);
    }

    // Parse UDP header
    let ip_hdr_len = ((unsafe { (*ip).version_ihl } & 0x0f) * 4) as usize;
    let udp_offset = ip_offset + ip_hdr_len;
    let udp = ptr_at::<UdpHdr>(&ctx, udp_offset)?;

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
            // Rewrite IP addresses
            (*ip_mut).saddr = fwd.src_ip;
            (*ip_mut).daddr = fwd.dest_ip;

            // Rewrite UDP ports
            (*udp_mut).source = fwd.src_port;
            (*udp_mut).dest = fwd.dest_port;

            // TODO: Recalculate checksums
            // For now, rely on hardware offload or disable checksum validation
            // In production, we should calculate incremental checksum updates
            (*ip_mut).check = 0; // Let hardware recalculate
            (*udp_mut).check = 0; // Let hardware recalculate
        }

        // Update statistics
        // TODO: Implement atomic increments for packet/byte counters

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
            timestamp: 0, // TODO: Get actual timestamp from bpf_ktime_get_ns()
        };
        unsafe {
            entry.write(event);
        }
        entry.submit(0);
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
