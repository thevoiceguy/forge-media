//! Forge Media Engine - Binary Entry Point
//!
//! This binary runs Forge as a standalone media server.
//! To use Forge as a library in your project, see the crate documentation.

use anyhow::Result;
use forge_media::{ForgeConfig, ForgeEngine};
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
    let _engine = ForgeEngine::new(config).await?;
    info!("✓ Forge engine initialized");

    // TODO: Start API server
    info!("API server placeholder - will bind to configured address");

    // Keep running
    info!("Forge Media Engine is running...");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down gracefully...");

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
