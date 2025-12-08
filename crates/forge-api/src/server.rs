//! API server implementation

use crate::middleware;
use crate::routes::{self, sessions::AppState, prometheus::MetricsHandle};
use axum::Router;
use axum::middleware as axum_middleware;
use axum::Extension;
use axum_server::tls_rustls::{RustlsConfig, bind_rustls};
use forge_engine::{SessionManager, SessionManagerConfig};
use forge_rtp::PortPoolConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower::ServiceBuilder;
use tracing::{info, error};

/// API server configuration
#[derive(Debug, Clone)]
pub struct ApiServerConfig {
    pub bind_addr: SocketAddr,
    pub enable_cors: bool,
    pub port_range_min: u16,
    pub port_range_max: u16,
    pub allowed_origins: Vec<String>,
    pub auth_tokens: Vec<String>,
    pub rate_limit_requests_per_window: usize,
    pub rate_limit_window_secs: u64,
    pub enable_https: bool,
    pub https_bind: Option<SocketAddr>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub recording_base_dir: PathBuf,
    pub xdp_enabled: bool,
    pub xdp_interface: String,
    pub xdp_mode: String,
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            enable_cors: true,
            port_range_min: 10000,
            port_range_max: 20000,
            allowed_origins: vec!["http://localhost:3000".to_string()],
            auth_tokens: Vec::new(),
            rate_limit_requests_per_window: 120,
            rate_limit_window_secs: 60,
            enable_https: false,
            https_bind: None,
            tls_cert: None,
            tls_key: None,
            recording_base_dir: PathBuf::from("/var/lib/forge/recordings"),
            xdp_enabled: false,
            xdp_interface: "lo".to_string(),
            xdp_mode: "generic".to_string(),
        }
    }
}

/// API server
pub struct ApiServer {
    config: ApiServerConfig,
    state: Arc<AppState>,
    auth_config: middleware::auth::AuthConfig,
    rate_limiter: middleware::RateLimiter,
}

impl ApiServer {
    /// Create a new API server with the given configuration
    pub async fn new(config: ApiServerConfig) -> Self {
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

        // Create session manager with XDP if enabled
        let session_manager = {
            #[cfg(all(target_os = "linux", feature = "xdp"))]
            {
                if config.xdp_enabled {
                    use forge_core::config::{XdpConfig, XdpMode};

                    let xdp_mode = match config.xdp_mode.to_lowercase().as_str() {
                        "native" => XdpMode::Native,
                        _ => XdpMode::Generic,
                    };

                    let xdp_config = XdpConfig {
                        enabled: true,
                        interface: config.xdp_interface.clone(),
                        mode: xdp_mode,
                        fallback: true,
                    };

                    info!("Initializing XDP on interface {} with mode {:?}",
                          xdp_config.interface, xdp_config.mode);

                    SessionManager::new_with_xdp(session_manager_config, xdp_config, None).await
                } else {
                    SessionManager::new(session_manager_config, None)
                }
            }

            #[cfg(not(all(target_os = "linux", feature = "xdp")))]
            {
                if config.xdp_enabled {
                    info!("XDP requested but not available on this platform or not compiled with 'xdp' feature");
                }
                SessionManager::new(session_manager_config, None)
            }
        };

        // Create conference bridge for media processing
        let conference_bridge = Arc::new(
            forge_media_processor::conference::ConferenceBridge::new(
                forge_media_processor::AudioFormat::pcm_mono(),
                480, // 10ms frame at 48kHz
            ).expect("Failed to create conference bridge")
        );
        info!("✓ Conference bridge initialized");

        // Validate recording base directory
        Self::validate_recording_dir(&config.recording_base_dir)
            .expect("Invalid recording base directory");
        info!("✓ Recording directory validated: {:?}", config.recording_base_dir);

        // Validate CORS origins if CORS is enabled
        if config.enable_cors && !config.allowed_origins.is_empty() {
            Self::validate_cors_origins(&config.allowed_origins)
                .expect("Invalid CORS origin configuration");
            info!("✓ CORS origins validated: {} origins", config.allowed_origins.len());
        }

        let state = Arc::new(AppState::new(
            session_manager,
            metrics_handle,
            conference_bridge,
            config.recording_base_dir.clone(),
        ));
        let auth_config = middleware::auth::AuthConfig::new(config.auth_tokens.clone());
        let rate_limiter = middleware::RateLimiter::new(
            config.rate_limit_requests_per_window,
            Duration::from_secs(config.rate_limit_window_secs),
        );

        Self {
            config,
            state,
            auth_config,
            rate_limiter,
        }
    }

    /// Validate CORS origins are well-formed
    fn validate_cors_origins(origins: &[String]) -> Result<(), std::io::Error> {
        use axum::http::HeaderValue;

        // Wildcard is allowed (though not recommended)
        if origins.iter().any(|o| o == "*") {
            return Ok(());
        }

        // Check each origin is a valid HTTP header value
        let mut invalid_origins = Vec::new();
        for origin in origins {
            if let Err(_) = HeaderValue::from_str(origin) {
                invalid_origins.push(origin.clone());
            }
        }

        if !invalid_origins.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Invalid CORS origins (must be valid HTTP header values): {:?}",
                    invalid_origins
                ),
            ));
        }

        Ok(())
    }

    /// Validate that the recording directory exists and is writable
    fn validate_recording_dir(path: &PathBuf) -> Result<(), std::io::Error> {
        use std::fs;

        // Try to create the directory if it doesn't exist
        if !path.exists() {
            fs::create_dir_all(path)?;
            info!("Created recording directory: {:?}", path);
        }

        // Verify it's a directory
        if !path.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!("Recording path {:?} is not a directory", path),
            ));
        }

        // Check if writable by attempting to create a test file
        let test_file = path.join(".forge_write_test");
        match fs::File::create(&test_file) {
            Ok(_) => {
                // Clean up test file
                let _ = fs::remove_file(&test_file);
                Ok(())
            }
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Recording directory {:?} is not writable: {}", path, e),
            )),
        }
    }

    async fn tls_config(&self) -> Result<Option<RustlsConfig>, std::io::Error> {
        if !self.config.enable_https {
            return Ok(None);
        }

        let cert = self
            .config
            .tls_cert
            .clone()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "TLS cert path not configured"))?;
        let key = self
            .config
            .tls_key
            .clone()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "TLS key path not configured"))?;

        let config = RustlsConfig::from_pem_file(cert, key)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to load TLS config: {}", e)))?;

        Ok(Some(config))
    }

    /// Build the router with all middleware and routes
    fn build_router(&self) -> Router {
        let mut router = routes::create_router()
            .with_state(self.state.clone())
            .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024)); // 10 MB limit

        // Add middleware
        let middleware_stack = ServiceBuilder::new()
            .layer(Extension(self.rate_limiter.clone()))
            .layer(Extension(self.auth_config.clone()))
            .layer(axum_middleware::from_fn(middleware::auth::auth_middleware))
            .layer(axum_middleware::from_fn(middleware::rate_limit::rate_limit_middleware))
            .layer(middleware::tracing_layer());

        router = router.layer(middleware_stack);

        // Add CORS if enabled
        if self.config.enable_cors {
            if self.config.allowed_origins.is_empty() {
                info!("CORS enabled but no allowed origins configured; skipping CORS layer");
            } else {
                router = router.layer(middleware::cors_layer(&self.config.allowed_origins));
            }
        }

        router
    }

    /// Start the API server
    ///
    /// This will bind to the configured address and start serving requests.
    /// The server will run until the process is terminated.
    pub async fn serve(self) -> Result<(), std::io::Error> {
        let router = self.build_router();
        let tls_config = self.tls_config().await?;

        info!("Starting Forge API server on {}", self.config.bind_addr);
        let listener = TcpListener::bind(&self.config.bind_addr).await?;

        // Log available endpoints for HTTP
        info!("✓ HTTP server listening on {}", self.config.bind_addr);
        info!("  Health check: http://{}/health", self.config.bind_addr);
        info!("  Sessions API: http://{}/v1/sessions", self.config.bind_addr);
        info!("  Metrics (JSON): http://{}/v1/metrics", self.config.bind_addr);
        info!("  Metrics (Prometheus): http://{}/metrics/prometheus", self.config.bind_addr);

        let http_server = {
            let router = router.clone();
            async move {
                axum::serve(listener, router.into_make_service())
                    .await
                    .map_err(|e| {
                        error!("HTTP server error: {}", e);
                        std::io::Error::new(std::io::ErrorKind::Other, e)
                    })
            }
        };

        // Optionally start HTTPS
        if let Some(tls) = tls_config {
            let https_addr = self
                .config
                .https_bind
                .unwrap_or(self.config.bind_addr);

            info!("✓ HTTPS server listening on {}", https_addr);
            info!("  Health check: https://{}/health", https_addr);
            info!("  Sessions API: https://{}/v1/sessions", https_addr);
            info!("  Metrics (JSON): https://{}/v1/metrics", https_addr);
            info!("  Metrics (Prometheus): https://{}/metrics/prometheus", https_addr);

            let https = async move {
                bind_rustls(https_addr, tls)
                    .serve(router.into_make_service())
                    .await
                    .map_err(|e| {
                        error!("HTTPS server error: {}", e);
                        std::io::Error::new(std::io::ErrorKind::Other, e)
                    })
            };

            info!("Running both HTTP and HTTPS servers");
            tokio::try_join!(http_server, https).map(|_| ())
        } else {
            info!("TLS disabled; HTTPS listener not started");
            http_server.await
        }
    }

    /// Start the API server with graceful shutdown
    ///
    /// The server will shut down gracefully when the shutdown signal is received.
    pub async fn serve_with_shutdown(
        self,
        shutdown_signal: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), std::io::Error> {
        let router = self.build_router();
        let tls_config = self.tls_config().await?;

        info!("Starting Forge API server on {}", self.config.bind_addr);

        // Start session timeout monitoring
        self.state.session_manager.start_monitoring().await;

        let listener = TcpListener::bind(&self.config.bind_addr).await?;

        info!("✓ API server listening on {}", self.config.bind_addr);
        info!("  Health check: http://{}/health", self.config.bind_addr);
        info!("  Sessions API: http://{}/v1/sessions", self.config.bind_addr);
        info!("  Metrics (JSON): http://{}/v1/metrics", self.config.bind_addr);
        info!("  Metrics (Prometheus): http://{}/metrics", self.config.bind_addr);

        let shutdown_notify = Arc::new(Notify::new());
        let shutdown_task = shutdown_notify.clone();
        tokio::spawn(async move {
            shutdown_signal.await;
            shutdown_task.notify_waiters();
        });

        let http_server = {
            let router = router.clone();
            let shutdown = shutdown_notify.clone();
            async move {
                axum::serve(listener, router.into_make_service())
                    .with_graceful_shutdown(async move {
                        shutdown.notified().await;
                    })
                    .await
                    .map_err(|e| {
                        error!("HTTP server error: {}", e);
                        std::io::Error::new(std::io::ErrorKind::Other, e)
                    })
            }
        };

        let result = if let Some(tls) = tls_config {
            let https_addr = self
                .config
                .https_bind
                .unwrap_or(self.config.bind_addr);

            info!("✓ HTTPS server listening on {}", https_addr);
            let https_router = router.clone().into_make_service();
            let shutdown = shutdown_notify.clone();
            let https_server = async move {
                tokio::select! {
                    res = bind_rustls(https_addr, tls).serve(https_router) => {
                        res.map_err(|e| {
                            error!("HTTPS server error: {}", e);
                            std::io::Error::new(std::io::ErrorKind::Other, e)
                        })
                    }
                    _ = shutdown.notified() => Ok(())
                }
            };

            tokio::try_join!(http_server, https_server).map(|_| ())
        } else {
            info!("TLS disabled; HTTPS listener not started");
            http_server.await
        };

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
