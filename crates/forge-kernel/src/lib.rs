//! Forge Kernel Module
//!
//! eBPF/XDP integration for high-performance RTP packet forwarding

// Pre-existing style lints; tracked as tech debt.
#![allow(clippy::wrong_self_convention)]
#![allow(clippy::needless_borrows_for_generic_args)]

pub mod error;

#[cfg(target_os = "linux")]
pub mod xdp;

pub use error::{Error, Result};

#[cfg(target_os = "linux")]
pub use xdp::{ForwardKey, ForwardValue, XdpManager, XdpMode};

// Re-export commonly used types
pub use aya;
