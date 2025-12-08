//! XDP program management and lifecycle

use crate::{Error, Result};
use aya::{
    maps::{HashMap as BpfHashMap, RingBuf as BpfRingBuf},
    programs::{Xdp, XdpFlags},
    Bpf,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};

/// XDP operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdpMode {
    /// Native XDP mode (XDP_DRV) - fastest, requires driver support
    Native,
    /// Generic XDP mode (XDP_SKB) - software fallback, works everywhere
    Generic,
}

impl XdpMode {
    /// Convert to Aya XdpFlags
    fn to_flags(&self) -> XdpFlags {
        match self {
            XdpMode::Native => XdpFlags::DRV_MODE,
            XdpMode::Generic => XdpFlags::SKB_MODE,
        }
    }
}

/// Forward map key: UDP 5-tuple (must match eBPF definition)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ForwardKey {
    pub src_ip: u32,
    pub src_port: u16,
    pub dst_port: u16,
    pub dst_ip: u32,
    pub protocol: u8,
    pub _padding: [u8; 3],
}

/// Forward map value (must match eBPF definition)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ForwardValue {
    pub dest_ip: u32,
    pub dest_port: u16,
    pub src_ip: u32,
    pub src_port: u16,
    pub last_seen: u64,
}

/// XDP program manager
pub struct XdpManager {
    interface: String,
    mode: XdpMode,
    bpf: Arc<RwLock<Option<Bpf>>>,
    loaded: bool,
}

impl XdpManager {
    /// Create a new XDP manager and attach to the specified interface
    ///
    /// # Arguments
    /// * `interface` - Network interface name (e.g., "eth0", "lo")
    /// * `mode` - XDP mode (Native or Generic)
    ///
    /// # Note
    /// This will attempt to load the eBPF program. If the program bytecode is not
    /// available (not compiled yet), it will create a stub manager that can be used
    /// for API compatibility but won't actually load XDP.
    pub async fn new(interface: &str, mode: XdpMode) -> Result<Self> {
        info!("Initializing XDP on interface {} with mode {:?}", interface, mode);

        let mut manager = Self {
            interface: interface.to_string(),
            mode,
            bpf: Arc::new(RwLock::new(None)),
            loaded: false,
        };

        // Try to load the embedded eBPF bytecode if it was compiled during build
        #[cfg(target_os = "linux")]
        {
            // The build script compiles the eBPF program and embeds it
            // If bpf-linker wasn't available during build, this will be None
            match option_env!("EBPF_OBJECT_PATH") {
                Some(path) => {
                    // Embed the compiled eBPF object file
                    let bytecode = include_bytes!(env!("EBPF_OBJECT_PATH"));
                    info!("Loading embedded eBPF program ({} bytes)", bytecode.len());

                    match manager.load_from_bytecode(bytecode).await {
                        Ok(()) => {
                            info!("✓ XDP program loaded successfully");
                        }
                        Err(e) => {
                            warn!("Failed to load XDP program: {}", e);
                            warn!("XDP manager will run in stub mode");
                        }
                    }
                }
                None => {
                    warn!("XDP program bytecode not available - creating stub manager");
                    warn!("eBPF was not compiled during build (bpf-linker not found)");
                    warn!("XDP will be configurable but won't load into kernel");
                }
            }
        }

        Ok(manager)
    }

    /// Load XDP program from bytecode
    ///
    /// # Arguments
    /// * `bytecode` - Compiled eBPF program bytecode
    pub async fn load_from_bytecode(&mut self, bytecode: &[u8]) -> Result<()> {
        info!("Loading XDP program ({} bytes)", bytecode.len());

        // Load BPF program
        let mut bpf = Bpf::load(bytecode)
            .map_err(|e| Error::XdpLoadFailed(format!("Failed to load BPF: {}", e)))?;

        // Get the XDP program
        let program: &mut Xdp = bpf
            .program_mut("rtp_forward")
            .ok_or_else(|| Error::XdpLoadFailed("Program 'rtp_forward' not found".to_string()))?
            .try_into()
            .map_err(|e| Error::XdpLoadFailed(format!("Not an XDP program: {}", e)))?;

        // Attach to interface
        program
            .load()
            .map_err(|e| Error::XdpLoadFailed(format!("Failed to load program: {}", e)))?;

        program
            .attach(&self.interface, self.mode.to_flags())
            .map_err(|e| Error::XdpAttachFailed(format!("Failed to attach: {}", e)))?;

        info!("XDP program attached to interface {} successfully", self.interface);

        *self.bpf.write().await = Some(bpf);
        self.loaded = true;

        Ok(())
    }

    /// Check if XDP program is loaded
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Get the interface name
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// Get the XDP mode
    pub fn mode(&self) -> XdpMode {
        self.mode
    }

    /// Get access to the forward map for inserting/deleting rules
    pub async fn forward_map(&self) -> Result<Option<BpfHashMap<&mut aya::maps::MapData, ForwardKey, ForwardValue>>> {
        let bpf_lock = self.bpf.read().await;

        if let Some(bpf) = bpf_lock.as_ref() {
            // This is a placeholder - actual implementation would require proper map access
            // For now, return None as we don't have the BPF loaded yet
            Ok(None)
        } else {
            Ok(None)
        }
    }

    /// Insert a forwarding rule
    ///
    /// # Arguments
    /// * `key` - Forward key (5-tuple)
    /// * `value` - Forward destination
    pub async fn insert_forward_rule(&self, key: ForwardKey, value: ForwardValue) -> Result<()> {
        if !self.loaded {
            warn!("XDP not loaded - skipping forward rule insertion");
            return Ok(());
        }

        // TODO: Implement actual map insertion when BPF is loaded
        info!("Insert forward rule: {:?} -> {:?}", key, value);

        Ok(())
    }

    /// Remove a forwarding rule
    pub async fn remove_forward_rule(&self, key: &ForwardKey) -> Result<()> {
        if !self.loaded {
            warn!("XDP not loaded - skipping forward rule removal");
            return Ok(());
        }

        // TODO: Implement actual map deletion when BPF is loaded
        info!("Remove forward rule: {:?}", key);

        Ok(())
    }

    /// Detach and unload the XDP program
    pub async fn detach(&mut self) -> Result<()> {
        if !self.loaded {
            return Ok(());
        }

        info!("Detaching XDP from interface {}", self.interface);

        let mut bpf_lock = self.bpf.write().await;
        *bpf_lock = None;
        self.loaded = false;

        Ok(())
    }
}

impl Drop for XdpManager {
    fn drop(&mut self) {
        if self.loaded {
            info!("Detaching XDP from interface {} (via Drop)", self.interface);
            // Note: Can't call async methods in Drop, cleanup happens when Bpf is dropped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_xdp_manager_creation() {
        // Test that we can create an XDP manager (stub mode)
        let manager = XdpManager::new("lo", XdpMode::Generic).await;
        assert!(manager.is_ok());

        let manager = manager.unwrap();
        assert_eq!(manager.interface(), "lo");
        assert_eq!(manager.mode(), XdpMode::Generic);
        assert!(!manager.is_loaded());
    }

    #[tokio::test]
    async fn test_forward_rule_operations() {
        let manager = XdpManager::new("lo", XdpMode::Generic).await.unwrap();

        // Test inserting a forward rule (should not fail even when not loaded)
        let key = ForwardKey {
            src_ip: 0x0100007f, // 127.0.0.1 in network byte order
            src_port: 5060u16.to_be(),
            dst_port: 30000u16.to_be(),
            dst_ip: 0x0100007f,
            protocol: 17,
            _padding: [0; 3],
        };

        let value = ForwardValue {
            dest_ip: 0x0100007f,
            dest_port: 5070u16.to_be(),
            src_ip: 0x0100007f,
            src_port: 30000u16.to_be(),
            last_seen: 0,
        };

        let result = manager.insert_forward_rule(key, value).await;
        assert!(result.is_ok());
    }
}
