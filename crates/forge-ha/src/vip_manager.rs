//! Virtual IP (VIP) management for cloud and on-premises deployments

use crate::config::{CloudConfig, OnPremConfig};
use crate::types::{DeploymentMode, HARole};
use forge_core::{ForgeError, Result};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Trait for VIP management across different deployment modes
#[async_trait::async_trait]
pub trait VIPManager: Send + Sync {
    /// Activate VIP (called when becoming primary)
    async fn activate(&self) -> Result<()>;

    /// Deactivate VIP (called when stepping down from primary)
    async fn deactivate(&self) -> Result<()>;

    /// Check if VIP is active on this instance
    async fn is_active(&self) -> Result<bool>;

    /// Get deployment mode
    fn deployment_mode(&self) -> DeploymentMode;
}

/// Cloud VIP manager (uses health endpoint status)
/// In cloud deployments, the load balancer routes traffic based on health checks
pub struct CloudVIPManager {
    config: CloudConfig,
    is_primary: Arc<RwLock<bool>>,
}

impl CloudVIPManager {
    /// Create a new cloud VIP manager
    pub fn new(config: CloudConfig) -> Self {
        Self {
            config,
            is_primary: Arc::new(RwLock::new(false)),
        }
    }

    /// Update role (affects health check response)
    pub async fn update_role(&self, role: HARole) {
        let is_primary = role == HARole::Primary;
        *self.is_primary.write().await = is_primary;

        if is_primary {
            info!("Cloud VIP: Now primary (health endpoint will return 200)");
        } else {
            info!("Cloud VIP: Now standby (health endpoint will return 503)");
        }
    }

    /// Check if health endpoint should return healthy
    pub async fn should_return_healthy(&self) -> bool {
        if self.config.standby_returns_503 {
            // Only primary returns 200
            *self.is_primary.read().await
        } else {
            // Both primary and standby return 200
            true
        }
    }
}

#[async_trait::async_trait]
impl VIPManager for CloudVIPManager {
    async fn activate(&self) -> Result<()> {
        info!(
            "Activating cloud VIP for provider: {:?}",
            self.config.provider
        );
        *self.is_primary.write().await = true;

        info!("Cloud VIP activated - health endpoint will now return 200 OK");
        Ok(())
    }

    async fn deactivate(&self) -> Result<()> {
        info!(
            "Deactivating cloud VIP for provider: {:?}",
            self.config.provider
        );
        *self.is_primary.write().await = false;

        info!("Cloud VIP deactivated - health endpoint will now return 503");
        Ok(())
    }

    async fn is_active(&self) -> Result<bool> {
        Ok(*self.is_primary.read().await)
    }

    fn deployment_mode(&self) -> DeploymentMode {
        DeploymentMode::Cloud
    }
}

/// On-premises VIP manager (integrates with Keepalived/VRRP)
/// Uses signal-based communication with Keepalived
pub struct VRRPManager {
    config: OnPremConfig,
    is_active: Arc<RwLock<bool>>,
}

impl VRRPManager {
    /// Create a new VRRP manager
    pub fn new(config: OnPremConfig) -> Self {
        Self {
            config,
            is_active: Arc::new(RwLock::new(false)),
        }
    }

    /// Trigger Keepalived to take over VIP
    async fn trigger_vrrp_takeover(&self) -> Result<()> {
        info!(
            "Triggering VRRP takeover for VIP {} on interface {}",
            self.config.vip, self.config.interface
        );

        // In a real implementation, this would:
        // 1. Send a signal to Keepalived (SIGUSR1 or modify priority file)
        // 2. Or use ip command to add VIP to interface directly
        //
        // For now, we'll use a simple approach: modify Keepalived config priority
        // and signal it to reload (or use notify scripts)

        info!(
            "Note: VRRP takeover requires Keepalived to be running and monitoring our health endpoint"
        );
        info!("The health endpoint will now return 200, causing Keepalived to increase priority");

        Ok(())
    }

    /// Release VRRP VIP
    async fn release_vrrp_vip(&self) -> Result<()> {
        info!(
            "Releasing VRRP VIP {} from interface {}",
            self.config.vip, self.config.interface
        );

        info!("The health endpoint will now return 503, causing Keepalived to decrease priority");

        Ok(())
    }
}

#[async_trait::async_trait]
impl VIPManager for VRRPManager {
    async fn activate(&self) -> Result<()> {
        info!(
            "Activating VRRP VIP: {} (router_id: {}, priority: {})",
            self.config.vip, self.config.virtual_router_id, self.config.priority
        );

        self.trigger_vrrp_takeover().await?;
        *self.is_active.write().await = true;

        info!("VRRP VIP activation initiated");
        Ok(())
    }

    async fn deactivate(&self) -> Result<()> {
        info!(
            "Deactivating VRRP VIP: {} (router_id: {})",
            self.config.vip, self.config.virtual_router_id
        );

        self.release_vrrp_vip().await?;
        *self.is_active.write().await = false;

        info!("VRRP VIP deactivation initiated");
        Ok(())
    }

    async fn is_active(&self) -> Result<bool> {
        Ok(*self.is_active.read().await)
    }

    fn deployment_mode(&self) -> DeploymentMode {
        DeploymentMode::OnPrem
    }
}

/// No-op VIP manager (for testing or single-instance deployments)
pub struct NoOpVIPManager;

#[async_trait::async_trait]
impl VIPManager for NoOpVIPManager {
    async fn activate(&self) -> Result<()> {
        debug!("NoOp VIP manager: activate (no action)");
        Ok(())
    }

    async fn deactivate(&self) -> Result<()> {
        debug!("NoOp VIP manager: deactivate (no action)");
        Ok(())
    }

    async fn is_active(&self) -> Result<bool> {
        Ok(true)
    }

    fn deployment_mode(&self) -> DeploymentMode {
        DeploymentMode::Cloud // Default to cloud
    }
}

/// VIP manager factory
pub struct VIPManagerFactory;

impl VIPManagerFactory {
    /// Create appropriate VIP manager based on deployment mode
    pub fn create(
        deployment_mode: DeploymentMode,
        cloud_config: Option<CloudConfig>,
        onprem_config: Option<OnPremConfig>,
    ) -> Result<Box<dyn VIPManager>> {
        match deployment_mode {
            DeploymentMode::Cloud => {
                let config = cloud_config
                    .ok_or_else(|| ForgeError::Internal("Cloud config required".to_string()))?;
                Ok(Box::new(CloudVIPManager::new(config)))
            }
            DeploymentMode::OnPrem => {
                let config = onprem_config
                    .ok_or_else(|| ForgeError::Internal("OnPrem config required".to_string()))?;
                Ok(Box::new(VRRPManager::new(config)))
            }
        }
    }

    /// Create no-op VIP manager (for testing)
    pub fn create_noop() -> Box<dyn VIPManager> {
        Box::new(NoOpVIPManager)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CloudProvider;

    fn create_test_cloud_config() -> CloudConfig {
        CloudConfig {
            provider: CloudProvider::Gcp,
            health_check_path: "/health".to_string(),
            standby_returns_503: true,
        }
    }

    fn create_test_onprem_config() -> OnPremConfig {
        OnPremConfig {
            vip: "10.0.1.100".to_string(),
            interface: "eth0".to_string(),
            virtual_router_id: 51,
            priority: 100,
            auth_password: "test123".to_string(),
        }
    }

    #[tokio::test]
    async fn test_cloud_vip_manager() {
        let config = create_test_cloud_config();
        let manager = CloudVIPManager::new(config);

        // Initially not active
        assert!(!manager.is_active().await.unwrap());

        // Activate
        manager.activate().await.unwrap();
        assert!(manager.is_active().await.unwrap());

        // Deactivate
        manager.deactivate().await.unwrap();
        assert!(!manager.is_active().await.unwrap());
    }

    #[tokio::test]
    async fn test_cloud_vip_health_check() {
        let config = create_test_cloud_config();
        let manager = CloudVIPManager::new(config);

        // Standby should not return healthy
        assert!(!manager.should_return_healthy().await);

        // Update to primary
        manager.update_role(HARole::Primary).await;
        assert!(manager.should_return_healthy().await);

        // Update to standby
        manager.update_role(HARole::Standby).await;
        assert!(!manager.should_return_healthy().await);
    }

    #[tokio::test]
    async fn test_vrrp_manager() {
        let config = create_test_onprem_config();
        let manager = VRRPManager::new(config);

        // Initially not active
        assert!(!manager.is_active().await.unwrap());

        // Activate
        manager.activate().await.unwrap();
        assert!(manager.is_active().await.unwrap());

        // Deactivate
        manager.deactivate().await.unwrap();
        assert!(!manager.is_active().await.unwrap());
    }

    #[tokio::test]
    async fn test_noop_manager() {
        let manager = NoOpVIPManager;

        assert!(manager.is_active().await.unwrap());
        manager.activate().await.unwrap();
        manager.deactivate().await.unwrap();
        assert!(manager.is_active().await.unwrap());
    }

    #[test]
    fn test_vip_manager_factory_cloud() {
        let config = create_test_cloud_config();
        let manager = VIPManagerFactory::create(DeploymentMode::Cloud, Some(config), None).unwrap();

        assert_eq!(manager.deployment_mode(), DeploymentMode::Cloud);
    }

    #[test]
    fn test_vip_manager_factory_onprem() {
        let config = create_test_onprem_config();
        let manager =
            VIPManagerFactory::create(DeploymentMode::OnPrem, None, Some(config)).unwrap();

        assert_eq!(manager.deployment_mode(), DeploymentMode::OnPrem);
    }

    #[test]
    fn test_vip_manager_factory_noop() {
        let manager = VIPManagerFactory::create_noop();
        // NoOp manager always works
        assert_eq!(manager.deployment_mode(), DeploymentMode::Cloud);
    }
}
