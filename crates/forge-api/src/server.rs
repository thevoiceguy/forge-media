//! API server implementation

use crate::middleware;
use crate::routes::{self, prometheus::MetricsHandle, sessions::AppState};
use axum::middleware as axum_middleware;
use axum::Extension;
use axum::Router;
use axum_server::tls_rustls::{bind_rustls, RustlsConfig};
use forge_core::EventBus;
use forge_engine::{SessionManager, SessionManagerConfig};
use forge_rtp::PortPoolConfig;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower::ServiceBuilder;
use tracing::{error, info, warn};

/// API server configuration
#[derive(Debug, Clone)]
pub struct ApiServerConfig {
    pub bind_addr: SocketAddr,
    pub enable_cors: bool,
    pub port_range_min: u16,
    pub port_range_max: u16,
    pub allowed_origins: Vec<String>,
    pub disable_auth: bool,
    pub auth_tokens: Vec<String>,
    pub rate_limit_requests_per_window: usize,
    pub rate_limit_window_secs: u64,
    pub trusted_proxies: Vec<IpAddr>, // IPs allowed to set X-Forwarded-For
    pub enable_https: bool,
    pub https_bind: Option<SocketAddr>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub recording_base_dir: PathBuf,
    pub recording_root_jail: PathBuf,
    pub prompts_base_dir: PathBuf,
    pub siprec_enabled: bool,
    pub siprec_output_dir: PathBuf,
    pub siprec_format: forge_core::AudioFormat,
    pub xdp_enabled: bool,
    pub xdp_interface: String,
    pub xdp_mode: String,
    pub ai_allowed_endpoints: Vec<String>,
    /// High availability configuration (optional, requires 'ha' feature)
    #[cfg(feature = "ha")]
    pub ha_config: Option<forge_core::config::HAConfig>,
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            enable_cors: false,
            port_range_min: 10000,
            port_range_max: 20000,
            allowed_origins: vec![],
            disable_auth: false,
            auth_tokens: vec![uuid::Uuid::new_v4().to_string()],
            rate_limit_requests_per_window: 120,
            rate_limit_window_secs: 60,
            trusted_proxies: Vec::new(), // No proxies trusted by default
            enable_https: false,
            https_bind: None,
            tls_cert: None,
            tls_key: None,
            recording_base_dir: PathBuf::from("/var/lib/forge/recordings"),
            recording_root_jail: PathBuf::from("/var/lib/forge"),
            prompts_base_dir: PathBuf::from("/var/lib/forge/prompts"),
            siprec_enabled: false,
            siprec_output_dir: PathBuf::from("/var/lib/forge/siprec"),
            siprec_format: forge_core::AudioFormat::pcm_mono(),
            xdp_enabled: false,
            xdp_interface: "lo".to_string(),
            xdp_mode: "generic".to_string(),
            ai_allowed_endpoints: forge_core::config::default_ai_allowed_endpoints(),
            #[cfg(feature = "ha")]
            ha_config: None, // HA disabled by default
        }
    }
}

/// API server
pub struct ApiServer {
    config: ApiServerConfig,
    state: Arc<AppState>,
    auth_config: middleware::auth::AuthConfig,
    rate_limiter: middleware::RateLimiter,
    #[allow(dead_code)]
    siprec_manager: Option<forge_siprec::SiprecManager>,
    #[cfg(feature = "ha")]
    ha_manager: Option<Arc<crate::ha::HAManager>>,
}

impl ApiServer {
    /// Create a new API server with the given configuration
    pub async fn new(config: ApiServerConfig) -> Self {
        // Initialize Prometheus metrics exporter
        let metrics_handle = Arc::new(MetricsHandle::init());
        info!("✓ Prometheus metrics initialized");

        // Enforce safer defaults: require auth when binding beyond loopback
        let bind_is_loopback = config.bind_addr.ip().is_loopback();
        if config.disable_auth {
            if !bind_is_loopback {
                warn!(
                    "API authentication explicitly disabled while binding to {}. \
                     This will expose control APIs without protection.",
                    config.bind_addr
                );
            }
        } else if config.auth_tokens.is_empty() {
            panic!(
                "API authentication enabled but no auth_tokens configured. \
                 Provide at least one token or set disable_auth=true to run without auth."
            );
        }

        // Create session manager with port pool
        let port_pool_config = PortPoolConfig::new(config.port_range_min, config.port_range_max)
            .expect("Invalid port range configuration");

        let session_manager_config = SessionManagerConfig {
            port_pool_config,
            ..Default::default()
        };

        // Event bus for inter-component notifications
        let event_bus = Arc::new(EventBus::new());

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

                    info!(
                        "Initializing XDP on interface {} with mode {:?}",
                        xdp_config.interface, xdp_config.mode
                    );

                    SessionManager::new_with_xdp(
                        session_manager_config,
                        xdp_config,
                        Some(event_bus.clone()),
                    )
                    .await
                } else {
                    SessionManager::new(session_manager_config, Some(event_bus.clone()))
                }
            }

            #[cfg(not(all(target_os = "linux", feature = "xdp")))]
            {
                if config.xdp_enabled {
                    info!("XDP requested but not available on this platform or not compiled with 'xdp' feature");
                }
                SessionManager::new(session_manager_config, Some(event_bus.clone()))
            }
        };

        // Create conference bridge for media processing
        let conference_bridge = Arc::new(
            forge_conference_processor::ConferenceBridge::new(
                forge_core::AudioFormat::pcm_mono(),
                480, // 10ms frame at 48kHz
            )
            .expect("Failed to create conference bridge"),
        );
        info!("✓ Conference bridge initialized");

        // Start SIPREC manager if enabled
        let siprec_manager = if config.siprec_enabled {
            info!(
                "✓ SIPREC enabled; writing captures to {:?}",
                config.siprec_output_dir
            );
            Some(forge_siprec::SiprecManager::start(
                forge_siprec::SiprecManagerConfig {
                    output_dir: config.siprec_output_dir.clone(),
                    format: config.siprec_format,
                    auth_token: None,
                },
                event_bus.clone(),
            ))
        } else {
            None
        };

        // Validate recording base directory
        let canonical_recording_dir =
            Self::validate_recording_dir(&config.recording_base_dir, &config.recording_root_jail)
                .expect("Invalid recording base directory");
        info!(
            "✓ Recording directory validated: {:?}",
            canonical_recording_dir
        );

        // Validate CORS origins if CORS is enabled
        if config.enable_cors && !config.allowed_origins.is_empty() {
            Self::validate_cors_origins(&config.allowed_origins)
                .expect("Invalid CORS origin configuration");
            info!(
                "✓ CORS origins validated: {} origins",
                config.allowed_origins.len()
            );
        } else if config.enable_cors {
            info!("CORS enabled with empty allowlist; no origins will be permitted");
        }

        // Initialize HA manager if enabled and compiled with 'ha' feature
        #[cfg(feature = "ha")]
        let ha_manager = if let Some(ref ha_cfg) = config.ha_config {
            if ha_cfg.enabled {
                info!("HA enabled, initializing HAManager...");
                match crate::ha::HAManager::new(ha_cfg.clone(), Some(session_manager.clone())).await
                {
                    Ok(mgr) => {
                        // Start HA background tasks
                        if let Err(e) = mgr.clone().start().await {
                            error!("Failed to start HA background tasks: {}", e);
                            None
                        } else {
                            info!("✓ HAManager initialized and started");
                            Some(mgr)
                        }
                    }
                    Err(e) => {
                        error!("Failed to initialize HAManager: {}", e);
                        error!("Server will continue without HA support");
                        None
                    }
                }
            } else {
                info!("HA configuration present but disabled");
                None
            }
        } else {
            None
        };

        let state = Arc::new(AppState::new(
            session_manager,
            metrics_handle,
            conference_bridge,
            config.recording_base_dir.clone(),
            config.prompts_base_dir.clone(),
            config.ai_allowed_endpoints.clone(),
            event_bus.clone(),
            #[cfg(feature = "ha")]
            ha_manager.clone(),
        ));
        let auth_config = middleware::auth::AuthConfig::new(config.auth_tokens.clone());
        let rate_limiter = middleware::RateLimiter::new(
            config.rate_limit_requests_per_window,
            Duration::from_secs(config.rate_limit_window_secs),
            config.trusted_proxies.clone(),
        );

        Self {
            config,
            state,
            auth_config,
            rate_limiter,
            siprec_manager,
            #[cfg(feature = "ha")]
            ha_manager,
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

    /// Validate that the recording directory exists, is within the jail root, and is writable
    fn validate_recording_dir(
        path: &PathBuf,
        jail_root: &PathBuf,
    ) -> Result<PathBuf, std::io::Error> {
        use std::fs;

        // Ensure jail root exists and is not a symlink
        if !jail_root.exists() {
            fs::create_dir_all(jail_root)?;
        }
        if fs::symlink_metadata(jail_root)?.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Recording root {:?} must not be a symlink", jail_root),
            ));
        }

        let jail_canonical = jail_root.canonicalize()?;

        // If target exists and is a symlink, reject
        if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Recording directory {:?} must not be a symlink", path),
            ));
        }

        // Create directory if needed, then canonicalize
        if !path.exists() {
            fs::create_dir_all(path)?;
            info!("Created recording directory: {:?}", path);
        }
        let canonical = path.canonicalize()?;

        // Ensure path is under jail root
        if !canonical.starts_with(&jail_canonical) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Recording directory {:?} must be inside {:?}",
                    canonical, jail_canonical
                ),
            ));
        }

        // Check writeability with temp file inside the canonical path
        let test_file = canonical.join(format!(".forge_write_test-{}", std::process::id()));
        match fs::File::create(&test_file) {
            Ok(_) => {
                let _ = fs::remove_file(&test_file);
                Ok(canonical)
            }
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Recording directory {:?} is not writable: {}", canonical, e),
            )),
        }
    }

    async fn tls_config(&self) -> Result<Option<RustlsConfig>, std::io::Error> {
        if !self.config.enable_https {
            return Ok(None);
        }

        let cert = self.config.tls_cert.clone().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "TLS cert path not configured")
        })?;
        let key = self.config.tls_key.clone().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "TLS key path not configured")
        })?;

        let config = RustlsConfig::from_pem_file(cert, key).await.map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to load TLS config: {}", e),
            )
        })?;

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
            .layer(axum_middleware::from_fn(
                middleware::rate_limit::rate_limit_middleware,
            ))
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

        // Start session timeout monitoring (ensure idle sessions are cleaned up)
        self.state.session_manager.start_monitoring().await;

        let listener = TcpListener::bind(&self.config.bind_addr).await?;

        // Log available endpoints for HTTP
        info!("✓ HTTP server listening on {}", self.config.bind_addr);
        info!("  Health check: http://{}/health", self.config.bind_addr);
        info!(
            "  Sessions API: http://{}/v1/sessions",
            self.config.bind_addr
        );
        info!(
            "  Metrics (JSON): http://{}/v1/metrics",
            self.config.bind_addr
        );
        info!(
            "  Metrics (Prometheus): http://{}/metrics/prometheus",
            self.config.bind_addr
        );

        let http_server = {
            let router = router.clone();
            async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .map_err(|e| {
                    error!("HTTP server error: {}", e);
                    std::io::Error::new(std::io::ErrorKind::Other, e)
                })
            }
        };

        // Optionally start HTTPS
        if let Some(tls) = tls_config {
            let https_addr = self.config.https_bind.unwrap_or(self.config.bind_addr);

            info!("✓ HTTPS server listening on {}", https_addr);
            info!("  Health check: https://{}/health", https_addr);
            info!("  Sessions API: https://{}/v1/sessions", https_addr);
            info!("  Metrics (JSON): https://{}/v1/metrics", https_addr);
            info!(
                "  Metrics (Prometheus): https://{}/metrics/prometheus",
                https_addr
            );

            let https = async move {
                bind_rustls(https_addr, tls)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
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
        info!(
            "  Sessions API: http://{}/v1/sessions",
            self.config.bind_addr
        );
        info!(
            "  Metrics (JSON): http://{}/v1/metrics",
            self.config.bind_addr
        );
        info!(
            "  Metrics (Prometheus): http://{}/metrics",
            self.config.bind_addr
        );

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
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
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
            let https_addr = self.config.https_bind.unwrap_or(self.config.bind_addr);

            info!("✓ HTTPS server listening on {}", https_addr);
            let https_router = router
                .clone()
                .into_make_service_with_connect_info::<SocketAddr>();
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

        // Stop HA manager if running
        #[cfg(feature = "ha")]
        if let Some(ref ha) = self.ha_manager {
            info!("Stopping HA manager...");
            if let Err(e) = ha.stop().await {
                error!("Error stopping HA manager: {}", e);
            } else {
                info!("✓ HA manager stopped");
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_creation() {
        let mut config = ApiServerConfig::default();
        let temp_root = std::env::temp_dir().join("forge-test-root");
        config.recording_root_jail = temp_root.clone();
        config.recording_base_dir = temp_root.join("recordings");
        let _server = ApiServer::new(config);
    }

    #[tokio::test]
    async fn test_router_building() {
        let mut config = ApiServerConfig::default();
        let temp_root = std::env::temp_dir().join("forge-test-root-router");
        config.recording_root_jail = temp_root.clone();
        config.recording_base_dir = temp_root.join("recordings");
        config.prompts_base_dir = std::env::temp_dir().join("forge-test-prompts");
        let server = ApiServer::new(config).await;
        let _router = server.build_router();
    }
}
