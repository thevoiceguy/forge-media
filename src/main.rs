//! Forge Media Engine - Binary Entry Point
//!
//! This binary runs Forge as a standalone media server.
//! To use Forge as a library in your project, see the crate documentation.

use anyhow::Result;
use forge_media::{ForgeConfig, ForgeEngine};
use forge_api::{ApiServer, server::ApiServerConfig};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing/logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("🔨 Forge Media Engine v{}", env!("CARGO_PKG_VERSION"));
    info!("Part of the Ferrous Communications Platform");

    // Load configuration
    let config = load_config()?;
    info!("Configuration loaded successfully");

    // Initialize engine
    let _engine = ForgeEngine::new(config.clone()).await?;
    info!("✓ Forge engine initialized");

    // Start API server
    let api_config = ApiServerConfig {
        bind_addr: config.api.http_bind.parse()?,
        enable_cors: config.api.enable_cors,
        port_range_min: config.engine.port_range.start,
        port_range_max: config.engine.port_range.end,
        allowed_origins: config.api.cors_origins.clone(),
        auth_tokens: config.api.auth_tokens.clone(),
        rate_limit_requests_per_window: config.api.rate_limit_requests_per_window,
        rate_limit_window_secs: config.api.rate_limit_window_secs,
        enable_https: config.api.enable_https,
        https_bind: config.api.https_bind.as_ref().map(|s| s.parse()).transpose()?,
        tls_cert: config.api.tls_cert.clone(),
        tls_key: config.api.tls_key.clone(),
        recording_base_dir: config.api.recording_base_dir.clone(),
    };

    let api_server = ApiServer::new(api_config);

    info!("Forge Media Engine is running...");
    info!("Press Ctrl+C to shutdown gracefully");

    // Run server with graceful shutdown
    api_server
        .serve_with_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            info!("Shutdown signal received, stopping gracefully...");
        })
        .await?;

    info!("✓ Forge Media Engine stopped");

    Ok(())
}

fn load_config() -> Result<ForgeConfig> {
    // Try to load from file, fall back to default
    let config_paths = vec![
        "/etc/forge/config.toml",
        "./config/forge.toml",
        "./forge.toml",
    ];

    for path in config_paths {
        if let Ok(contents) = std::fs::read_to_string(path) {
            info!("Loading configuration from: {}", path);
            return Ok(toml::from_str(&contents)?);
        }
    }

    info!("No configuration file found, using defaults");
    Ok(ForgeConfig::default())
}
