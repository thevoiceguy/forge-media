//! API server implementation

use crate::middleware;
use crate::routes::{self, sessions::AppState, prometheus::MetricsHandle};
use axum::Router;
use forge_engine::{SessionManager, SessionManagerConfig};
use forge_rtp::PortPoolConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tracing::{info, error};

/// API server configuration
#[derive(Debug, Clone)]
pub struct ApiServerConfig {
    pub bind_addr: SocketAddr,
    pub enable_cors: bool,
    pub port_range_min: u16,
    pub port_range_max: u16,
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            enable_cors: true,
            port_range_min: 10000,
            port_range_max: 20000,
        }
    }
}

/// API server
pub struct ApiServer {
    config: ApiServerConfig,
    state: Arc<AppState>,
}

impl ApiServer {
    /// Create a new API server with the given configuration
    pub fn new(config: ApiServerConfig) -> Self {
        // Initialize Prometheus metrics exporter
        let metrics_handle = Arc::new(MetricsHandle::init());
        info!("✓ Prometheus metrics initialized");

        // Create session manager with port pool
        let port_pool_config = PortPoolConfig::new(config.port_range_min, config.port_range_max)
            .expect("Invalid port range configuration");

        let session_manager_config = SessionManagerConfig {
            port_pool_config,
            ..Default::default()
        };

        let session_manager = SessionManager::new(session_manager_config, None);

        // Create conference bridge for media processing
        let conference_bridge = Arc::new(
            forge_media_processor::conference::ConferenceBridge::new(
                forge_media_processor::AudioFormat::pcm_mono(),
                480, // 10ms frame at 48kHz
            ).expect("Failed to create conference bridge")
        );
        info!("✓ Conference bridge initialized");

        let state = Arc::new(AppState::new(session_manager, metrics_handle, conference_bridge));

        Self { config, state }
    }

    /// Build the router with all middleware and routes
    fn build_router(&self) -> Router {
        let mut router = routes::create_router()
            .with_state(self.state.clone())
            .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024)); // 10 MB limit

        // Add middleware
        let middleware_stack = ServiceBuilder::new()
            .layer(middleware::tracing_layer());

        router = router.layer(middleware_stack);

        // Add CORS if enabled
        if self.config.enable_cors {
            router = router.layer(middleware::cors_layer());
        }

        router
    }

    /// Start the API server
    ///
    /// This will bind to the configured address and start serving requests.
    /// The server will run until the process is terminated.
    pub async fn serve(self) -> Result<(), std::io::Error> {
        let router = self.build_router();

        info!("Starting Forge API server on {}", self.config.bind_addr);

        let listener = TcpListener::bind(&self.config.bind_addr).await?;

        info!("✓ API server listening on {}", self.config.bind_addr);
        info!("  Health check: http://{}/health", self.config.bind_addr);
        info!("  Sessions API: http://{}/v1/sessions", self.config.bind_addr);
        info!("  Metrics (JSON): http://{}/v1/metrics", self.config.bind_addr);
        info!("  Metrics (Prometheus): http://{}/metrics", self.config.bind_addr);

        axum::serve(listener, router)
            .await
            .map_err(|e| {
                error!("Server error: {}", e);
                std::io::Error::new(std::io::ErrorKind::Other, e)
            })
    }

    /// Start the API server with graceful shutdown
    ///
    /// The server will shut down gracefully when the shutdown signal is received.
    pub async fn serve_with_shutdown(
        self,
        shutdown_signal: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), std::io::Error> {
        let router = self.build_router();

        info!("Starting Forge API server on {}", self.config.bind_addr);

        // Start session timeout monitoring
        self.state.session_manager.start_monitoring().await;

        let listener = TcpListener::bind(&self.config.bind_addr).await?;

        info!("✓ API server listening on {}", self.config.bind_addr);
        info!("  Health check: http://{}/health", self.config.bind_addr);
        info!("  Sessions API: http://{}/v1/sessions", self.config.bind_addr);
        info!("  Metrics (JSON): http://{}/v1/metrics", self.config.bind_addr);
        info!("  Metrics (Prometheus): http://{}/metrics", self.config.bind_addr);

        let result = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal)
            .await
            .map_err(|e| {
                error!("Server error: {}", e);
                std::io::Error::new(std::io::ErrorKind::Other, e)
            });

        // Stop session timeout monitoring on shutdown
        self.state.session_manager.stop_monitoring().await;

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_creation() {
        let config = ApiServerConfig::default();
        let _server = ApiServer::new(config);
    }

    #[test]
    fn test_router_building() {
        let config = ApiServerConfig::default();
        let server = ApiServer::new(config);
        let _router = server.build_router();
    }
}
