//! Forge Kernel Module
//!
//! eBPF/XDP integration for high-performance RTP packet forwarding

pub mod error;

#[cfg(target_os = "linux")]
pub mod xdp;

pub use error::{Error, Result};

#[cfg(target_os = "linux")]
pub use xdp::{XdpManager, XdpMode};

// Re-export commonly used types
pub use aya;
