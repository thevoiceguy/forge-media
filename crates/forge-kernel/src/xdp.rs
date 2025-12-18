//! XDP program management and lifecycle

use crate::{Error, Result};
use aya::{
    maps::{HashMap as BpfHashMap},
    programs::{xdp::XdpLinkId, Xdp, XdpFlags},
    Pod,
    Ebpf,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

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

unsafe impl Pod for ForwardKey {}
unsafe impl Pod for ForwardValue {}

/// XDP program manager
pub struct XdpManager {
    interface: String,
    mode: XdpMode,
    bpf: Arc<RwLock<Option<Ebpf>>>,
    loaded: bool,
    link_id: Option<XdpLinkId>,
    load_error: Option<String>,
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
        #[cfg(target_os = "linux")]
        {
            if !std::path::Path::new(&format!("/sys/class/net/{}", interface)).exists() {
                return Err(Error::InvalidConfig(format!(
                    "Interface {} not found",
                    interface
                )));
            }
        }

        info!(
            "Initializing XDP on interface {} with mode {:?}",
            interface, mode
        );

        let mut manager = Self {
            interface: interface.to_string(),
            mode,
            bpf: Arc::new(RwLock::new(None)),
            loaded: false,
            link_id: None,
            load_error: None,
        };

        // Try to load the embedded eBPF bytecode if it was compiled during build
        #[cfg(target_os = "linux")]
        {
            // The build script compiles the eBPF program and embeds it
            // If bpf-linker wasn't available during build, this will be None
            match option_env!("EBPF_OBJECT_PATH") {
                Some(_path) => {
                    // Try loading from file first (may have better ELF parsing)
                    let ebpf_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("ebpf/rtp_forward.o");

                    if ebpf_path.exists() {
                        info!("Loading eBPF program from file: {:?}", ebpf_path);
                        match manager.load_from_file(&ebpf_path).await {
                            Ok(()) => {
                                info!("✓ XDP program loaded successfully");
                            }
                            Err(e) => {
                                warn!("Failed to load XDP program from file: {}", e);
                                warn!("XDP manager will run in stub mode");
                                manager.load_error = Some(e.to_string());
                            }
                        }
                    } else {
                        warn!("XDP program file not found - creating stub manager");
                        warn!("XDP will be configurable but won't load into kernel");
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

    /// Load XDP program from file
    ///
    /// # Arguments
    /// * `path` - Path to compiled eBPF object file
    pub async fn load_from_file(&mut self, path: &std::path::Path) -> Result<()> {
        info!("Loading XDP program from file: {:?}", path);

        // Load BPF program from file
        let mut bpf = Ebpf::load_file(path)
            .map_err(|e| Error::XdpLoadFailed(format!("Failed to load BPF from file: {}", e)))?;

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

        let link_id = program
            .attach(&self.interface, self.mode.to_flags())
            .map_err(|e| Error::XdpAttachFailed(format!("Failed to attach: {}", e)))?;

        info!(
            "XDP program attached to interface {} successfully",
            self.interface
        );

        *self.bpf.write().await = Some(bpf);
        self.loaded = true;
        self.link_id = Some(link_id);

        Ok(())
    }

    /// Load XDP program from bytecode
    ///
    /// # Arguments
    /// * `bytecode` - Compiled eBPF program bytecode
    pub async fn load_from_bytecode(&mut self, bytecode: &[u8]) -> Result<()> {
        info!("Loading XDP program ({} bytes)", bytecode.len());

        // Load BPF program
        let mut bpf = Ebpf::load(bytecode)
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

        let link_id = program
            .attach(&self.interface, self.mode.to_flags())
            .map_err(|e| Error::XdpAttachFailed(format!("Failed to attach: {}", e)))?;

        info!(
            "XDP program attached to interface {} successfully",
            self.interface
        );

        *self.bpf.write().await = Some(bpf);
        self.loaded = true;
        self.link_id = Some(link_id);

        Ok(())
    }

    /// Check if XDP program is loaded
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Retrieve the last load/attach error encountered during initialization
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
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
    /// Validate that the forward map is available.
    pub async fn forward_map(&self) -> Result<()> {
        if !self.loaded {
            return Err(Error::XdpNotAvailable(
                self.load_error
                    .clone()
                    .unwrap_or_else(|| "XDP not loaded".to_string()),
            ));
        }

        let mut guard = self.bpf.write().await;

        let bpf = guard
            .as_mut()
            .ok_or_else(|| Error::XdpNotAvailable("XDP is not loaded".to_string()))?;

        bpf.map_mut("FORWARD_MAP")
            .ok_or_else(|| Error::MapOperationFailed("FORWARD_MAP not available".to_string()))?;

        Ok(())
    }

    /// Insert a forwarding rule
    ///
    /// # Arguments
    /// * `key` - Forward key (5-tuple)
    /// * `value` - Forward destination
    pub async fn insert_forward_rule(&self, key: ForwardKey, value: ForwardValue) -> Result<()> {
        if !self.loaded {
            return Err(Error::XdpNotAvailable(
                self.load_error
                    .clone()
                    .unwrap_or_else(|| "XDP not loaded".to_string()),
            ));
        }

        let mut guard = self.bpf.write().await;
        let bpf = guard
            .as_mut()
            .ok_or_else(|| Error::XdpNotAvailable("XDP is not loaded".to_string()))?;

        let map = bpf
            .map_mut("FORWARD_MAP")
            .ok_or_else(|| Error::MapOperationFailed("FORWARD_MAP not available".to_string()))?;

        let mut map: BpfHashMap<&mut aya::maps::MapData, ForwardKey, ForwardValue> =
            BpfHashMap::try_from(map).map_err(|e| {
                Error::MapOperationFailed(format!("Failed to open FORWARD_MAP: {}", e))
            })?;

        map.insert(&key, &value, 0)
            .map_err(|e| Error::MapOperationFailed(format!("Failed to insert rule: {}", e)))?;

        Ok(())
    }

    /// Remove a forwarding rule
    pub async fn remove_forward_rule(&self, key: &ForwardKey) -> Result<()> {
        if !self.loaded {
            return Err(Error::XdpNotAvailable(
                self.load_error
                    .clone()
                    .unwrap_or_else(|| "XDP not loaded".to_string()),
            ));
        }

        let mut guard = self.bpf.write().await;
        let bpf = guard
            .as_mut()
            .ok_or_else(|| Error::XdpNotAvailable("XDP is not loaded".to_string()))?;

        let map = bpf
            .map_mut("FORWARD_MAP")
            .ok_or_else(|| Error::MapOperationFailed("FORWARD_MAP not available".to_string()))?;

        let mut map: BpfHashMap<&mut aya::maps::MapData, ForwardKey, ForwardValue> =
            BpfHashMap::try_from(map).map_err(|e| {
                Error::MapOperationFailed(format!("Failed to open FORWARD_MAP: {}", e))
            })?;

        map.remove(key)
            .map_err(|e| Error::MapOperationFailed(format!("Failed to remove rule: {}", e)))?;

        Ok(())
    }

    /// Detach and unload the XDP program
    pub async fn detach(&mut self) -> Result<()> {
        if !self.loaded {
            return Ok(());
        }

        info!("Detaching XDP from interface {}", self.interface);

        let mut bpf_lock = self.bpf.write().await;
        if let Some(bpf) = bpf_lock.as_mut() {
            if let Some(link_id) = self.link_id.take() {
                if let Some(program) = bpf.program_mut("rtp_forward") {
                    let program: &mut Xdp = program.try_into().map_err(|e| {
                        Error::XdpDetachFailed(format!("Not an XDP program: {}", e))
                    })?;
                    program
                        .detach(link_id)
                        .map_err(|e| Error::XdpDetachFailed(format!("Failed to detach: {}", e)))?;
                }
            }
        }
        *bpf_lock = None;
        self.loaded = false;

        Ok(())
    }
}

impl Drop for XdpManager {
    fn drop(&mut self) {
        if self.loaded {
            info!("Detaching XDP from interface {} (via Drop)", self.interface);
            if let Ok(mut bpf_lock) = self.bpf.try_write() {
                if let Some(bpf) = bpf_lock.as_mut() {
                    if let Some(link_id) = self.link_id.take() {
                        if let Some(program) = bpf.program_mut("rtp_forward") {
                            let xdp_program: std::result::Result<&mut Xdp, _> = program.try_into();
                            if let Ok(program) = xdp_program {
                                let _ = program.detach(link_id);
                            }
                        }
                    }
                }
                *bpf_lock = None;
            }
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

        // In stub mode we should get an explicit XdpNotAvailable error
        let result = manager.insert_forward_rule(key, value).await;
        assert!(matches!(result, Err(Error::XdpNotAvailable(_))));
    }
}
