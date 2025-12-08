//! XDP program management and lifecycle

use crate::Result;
use tracing::{info, warn};

/// XDP operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdpMode {
    /// Native XDP mode (XDP_DRV) - fastest, requires driver support
    Native,
    /// Generic XDP mode (XDP_SKB) - software fallback, works everywhere
    Generic,
}

/// XDP program manager
pub struct XdpManager {
    interface: String,
    mode: XdpMode,
}

impl XdpManager {
    /// Create a new XDP manager and attach to the specified interface
    ///
    /// # Arguments
    /// * `interface` - Network interface name (e.g., "eth0", "lo")
    /// * `mode` - XDP mode (Native or Generic)
    pub async fn new(interface: &str, mode: XdpMode) -> Result<Self> {
        info!("Initializing XDP on interface {} with mode {:?}", interface, mode);

        // For now, this is a stub - we'll implement actual XDP loading in Phase 2
        warn!("XDP stub implementation - Phase 2 will implement actual loading");

        Ok(Self {
            interface: interface.to_string(),
            mode,
        })
    }

    /// Get the interface name
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// Get the XDP mode
    pub fn mode(&self) -> XdpMode {
        self.mode
    }
}

impl Drop for XdpManager {
    fn drop(&mut self) {
        info!("Detaching XDP from interface {}", self.interface);
        // Actual detachment will be implemented in Phase 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_xdp_manager_creation() {
        // Test that we can create an XDP manager (stub)
        let manager = XdpManager::new("lo", XdpMode::Generic).await;
        assert!(manager.is_ok());

        let manager = manager.unwrap();
        assert_eq!(manager.interface(), "lo");
        assert_eq!(manager.mode(), XdpMode::Generic);
    }
}
