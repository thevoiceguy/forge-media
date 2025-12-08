#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::xdp_action,
    macros::{map, xdp},
    maps::HashMap,
    programs::XdpContext,
};

/// Forward map key: UDP 5-tuple
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForwardKey {
    pub src_ip: u32,      // Source IP (network byte order)
    pub src_port: u16,    // Source port (network byte order)
    pub dst_port: u16,    // Destination port (our RTP port)
    pub dst_ip: u32,      // Destination IP (our IP)
    pub protocol: u8,     // UDP = 17
    pub _padding: [u8; 3],
}

/// Forward map value: destination to forward to
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForwardValue {
    pub dest_ip: u32,     // Where to forward (network byte order)
    pub dest_port: u16,   // Destination port
    pub src_port: u16,    // Our source port for reply
    pub last_seen: u64,   // Timestamp (nanoseconds)
}

/// Forward map: 5-tuple → forward destination
#[map]
static FORWARD_MAP: HashMap<ForwardKey, ForwardValue> =
    HashMap::with_max_entries(10000, 0);

/// RTP port range constants
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
    // For now, just pass everything to userspace
    // We'll implement parsing and forwarding in next steps
    Ok(xdp_action::XDP_PASS)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
