//! Forge Media Engine HTTP API Client

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Forge API client
#[derive(Debug, Clone)]
pub struct ForgeClient {
    base_url: String,
    client: reqwest::Client,
}

/// Health check response
#[derive(Debug, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
}

/// Create session request
#[derive(Debug, Serialize)]
pub struct CreateSessionRequest {
    pub call_id: String,
}

/// Session response
#[derive(Debug, Clone, Deserialize)]
pub struct SessionResponse {
    pub call_id: String,
    pub state: String,
    pub rtp_port: u16,
    pub rtcp_port: u16,
}

/// API success wrapper
#[derive(Debug, Deserialize)]
pub struct ApiSuccess<T> {
    pub status: String,
    pub data: T,
}

impl ForgeClient {
    /// Create a new Forge API client
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Check Forge API health
    pub async fn health_check(&self) -> Result<HealthResponse> {
        let url = format!("{}/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send health check request")?;

        if !response.status().is_success() {
            anyhow::bail!("Health check failed with status: {}", response.status());
        }

        response
            .json()
            .await
            .context("Failed to parse health check response")
    }

    /// Create a new Forge session
    pub async fn create_session(&self, call_id: &str) -> Result<SessionResponse> {
        let url = format!("{}/v1/sessions", self.base_url);
        let request = CreateSessionRequest {
            call_id: call_id.to_string(),
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send create session request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Create session failed with status {}: {}", status, body);
        }

        let api_response: ApiSuccess<SessionResponse> = response
            .json()
            .await
            .context("Failed to parse create session response")?;

        Ok(api_response.data)
    }

    /// Get session information
    pub async fn get_session(&self, call_id: &str) -> Result<SessionResponse> {
        let url = format!("{}/v1/sessions/{}", self.base_url, call_id);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send get session request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Get session failed with status {}: {}", status, body);
        }

        let api_response: ApiSuccess<SessionResponse> = response
            .json()
            .await
            .context("Failed to parse get session response")?;

        Ok(api_response.data)
    }

    /// Start a Forge session (activate RTP forwarding)
    pub async fn start_session(&self, call_id: &str) -> Result<SessionResponse> {
        let url = format!("{}/v1/sessions/{}/start", self.base_url, call_id);
        let response = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to send start session request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Start session failed with status {}: {}", status, body);
        }

        let api_response: ApiSuccess<SessionResponse> = response
            .json()
            .await
            .context("Failed to parse start session response")?;

        Ok(api_response.data)
    }

    /// Delete a Forge session (stop RTP forwarding and deallocate ports)
    pub async fn delete_session(&self, call_id: &str) -> Result<()> {
        let url = format!("{}/v1/sessions/{}", self.base_url, call_id);
        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .context("Failed to send delete session request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Delete session failed with status {}: {}", status, body);
        }

        Ok(())
    }

    /// List all active sessions
    pub async fn list_sessions(&self) -> Result<Vec<SessionResponse>> {
        let url = format!("{}/v1/sessions", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to send list sessions request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("List sessions failed with status {}: {}", status, body);
        }

        #[derive(Debug, Deserialize)]
        struct ListResponse {
            sessions: Vec<SessionResponse>,
            count: usize,
        }

        let api_response: ApiSuccess<ListResponse> = response
            .json()
            .await
            .context("Failed to parse list sessions response")?;

        Ok(api_response.data.sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires running Forge server
    async fn test_forge_client() {
        let client = ForgeClient::new("http://localhost:8081");

        // Health check
        let health = client.health_check().await.unwrap();
        assert_eq!(health.status, "healthy");

        // Create session
        let session = client.create_session("test-call-123").await.unwrap();
        assert_eq!(session.call_id, "test-call-123");
        assert!(session.rtp_port > 0);

        // Get session
        let session = client.get_session("test-call-123").await.unwrap();
        assert_eq!(session.call_id, "test-call-123");

        // Start session
        let session = client.start_session("test-call-123").await.unwrap();
        assert_eq!(session.state, "Active");

        // List sessions
        let sessions = client.list_sessions().await.unwrap();
        assert!(sessions.iter().any(|s| s.call_id == "test-call-123"));

        // Delete session
        client.delete_session("test-call-123").await.unwrap();
    }
}
