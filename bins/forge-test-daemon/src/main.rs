//! Forge Test Daemon - Simple Integration Demo
//!
//! This is a minimal daemon that demonstrates how to integrate the Forge Media Engine
//! HTTP API with a SIP application. It provides a simple test harness for end-to-end
//! RTP forwarding testing.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

/// Forge API client for session management
use forge_test_daemon::forge_client::ForgeClient;

/// Simple session tracker
type SessionMap = Arc<RwLock<HashMap<String, SessionInfo>>>;

#[derive(Debug, Clone)]
struct SessionInfo {
    call_id: String,
    rtp_port: u16,
    rtcp_port: u16,
    state: String,
}

/// Configuration for the test daemon
#[derive(Debug, Clone)]
struct Config {
    /// Forge API base URL
    forge_api_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            forge_api_url: "http://localhost:8081".to_string(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "forge_test_daemon=info".into()),
        )
        .init();

    info!("🔨 Forge Test Daemon - API Integration Test");
    info!("This daemon demonstrates Forge Media Engine integration");

    let config = Config::default();
    info!("Forge API: {}", config.forge_api_url);

    // Create Forge client
    let forge_client = Arc::new(ForgeClient::new(&config.forge_api_url));

    // Test Forge connection
    info!("Testing Forge API connection...");
    match forge_client.health_check().await {
        Ok(health) => {
            info!("✓ Forge API is healthy");
            info!("  Version: {}", health.version);
            info!("  Uptime: {}s", health.uptime_seconds);
        }
        Err(e) => {
            error!("✗ Failed to connect to Forge API: {}", e);
            error!(
                "Make sure Forge Media Engine is running on {}",
                config.forge_api_url
            );
            return Err(e.into());
        }
    }

    // Create session tracker
    let sessions: SessionMap = Arc::new(RwLock::new(HashMap::new()));

    info!("");
    info!("=== Running Integration Tests ===");
    info!("");

    // Test 1: Create a session
    info!("Test 1: Creating session...");
    let test_call_id = "test-integration-call-001";
    match forge_client.create_session(test_call_id).await {
        Ok(session) => {
            info!("✓ Session created successfully");
            info!("  Call-ID: {}", session.call_id);
            info!("  State: {}", session.state);
            info!("  RTP Port: {}", session.rtp_port);
            info!("  RTCP Port: {}", session.rtcp_port);

            sessions.write().await.insert(
                test_call_id.to_string(),
                SessionInfo {
                    call_id: session.call_id.clone(),
                    rtp_port: session.rtp_port,
                    rtcp_port: session.rtcp_port,
                    state: session.state.clone(),
                },
            );
        }
        Err(e) => {
            error!("✗ Failed to create session: {}", e);
            return Err(e.into());
        }
    }

    // Test 2: Get session info
    info!("");
    info!("Test 2: Retrieving session info...");
    match forge_client.get_session(test_call_id).await {
        Ok(session) => {
            info!("✓ Session info retrieved");
            info!("  Call-ID: {}", session.call_id);
            info!("  State: {}", session.state);
        }
        Err(e) => {
            error!("✗ Failed to get session: {}", e);
        }
    }

    // Test 3: Start session (activate RTP forwarding)
    info!("");
    info!("Test 3: Starting RTP forwarding...");
    match forge_client.start_session(test_call_id).await {
        Ok(session) => {
            info!("✓ RTP forwarding started");
            info!("  Call-ID: {}", session.call_id);
            info!("  State: {}", session.state);

            // Update session state
            if let Some(sess) = sessions.write().await.get_mut(test_call_id) {
                sess.state = session.state.clone();
            }
        }
        Err(e) => {
            error!("✗ Failed to start session: {}", e);
        }
    }

    // Test 4: List all sessions
    info!("");
    info!("Test 4: Listing all active sessions...");
    match forge_client.list_sessions().await {
        Ok(session_list) => {
            info!("✓ Active sessions: {}", session_list.len());
            for session in session_list {
                info!(
                    "  - {}: RTP={}, state={}",
                    session.call_id, session.rtp_port, session.state
                );
            }
        }
        Err(e) => {
            error!("✗ Failed to list sessions: {}", e);
        }
    }

    // Display session info
    info!("");
    info!("=== Session Information ===");
    info!("");
    let sessions_guard = sessions.read().await;
    if let Some(sess) = sessions_guard.get(test_call_id) {
        info!("Active Session:");
        info!("  Call-ID: {}", sess.call_id);
        info!("  State: {}", sess.state);
        info!("  RTP Port: {}", sess.rtp_port);
        info!("  RTCP Port: {}", sess.rtcp_port);
        info!("");
        info!("To test RTP forwarding:");
        info!("  1. Send RTP packets to localhost:{}", sess.rtp_port);
        info!("  2. Forge will automatically learn participant endpoints");
        info!("  3. RTP will be forwarded bidirectionally between participants");
        info!("");
        info!("Example using netcat:");
        info!("  # From one terminal:");
        info!("  echo 'RTP test' | nc -u localhost {}", sess.rtp_port);
        info!("  # Forge will relay between participants once both endpoints are known");
    }

    drop(sessions_guard);

    info!("");
    info!("Press Ctrl+C to cleanup and exit");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .context("Failed to listen for Ctrl+C")?;

    info!("");
    info!("Shutting down...");

    // Test 5: Cleanup - delete session
    info!("Test 5: Deleting session...");
    match forge_client.delete_session(test_call_id).await {
        Ok(()) => {
            info!("✓ Session deleted successfully");
            info!("  Ports deallocated");
            info!("  RTP forwarding stopped");
        }
        Err(e) => {
            error!("✗ Failed to delete session: {}", e);
        }
    }

    info!("");
    info!("✓ Integration tests complete");
    info!("Forge Test Daemon stopped");

    Ok(())
}
