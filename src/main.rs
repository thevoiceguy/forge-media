//! Forge Media Engine - Binary Entry Point
//!
//! This binary runs Forge as a standalone media server.
//! To use Forge as a library in your project, see the crate documentation.

use anyhow::Result;
use forge_media::{ForgeConfig, ForgeEngine};
use forge_api::{ApiServer, server::ApiServerConfig};
use tracing::{info, error};
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
