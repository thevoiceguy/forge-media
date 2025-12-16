//! Disk-based persistence backend
//!
//! Stores AI session state as JSON files in a directory.

use super::{PersistenceBackend, PersistedAISession};
use async_trait::async_trait;
use forge_core::{CallId, ForgeError, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, error, info, warn};

/// Disk-based persistence backend
pub struct DiskBackend {
    base_dir: PathBuf,
}

impl DiskBackend {
    /// Create a new disk backend
    pub async fn new(base_dir: PathBuf) -> Result<Self> {
        // Ensure base directory exists
        if !base_dir.exists() {
            fs::create_dir_all(&base_dir).await.map_err(|e| {
                ForgeError::Internal(format!(
                    "Failed to create persistence directory {:?}: {}",
                    base_dir, e
                ))
            })?;
            info!("Created AI session persistence directory: {:?}", base_dir);
        }

        // Verify directory is writable
        if !base_dir.is_dir() {
            return Err(ForgeError::Internal(format!(
                "Persistence path {:?} is not a directory",
                base_dir
            )));
        }

        Ok(Self { base_dir })
    }

    /// Get file path for a call ID
    fn session_path(&self, call_id: &CallId) -> PathBuf {
        // Use call_id as filename with .json extension
        // Sanitize call_id to be filesystem-safe
        let safe_id = call_id
            .0
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>();
        self.base_dir.join(format!("{}.json", safe_id))
    }
}

#[async_trait]
impl PersistenceBackend for DiskBackend {
    async fn save(&self, session: &PersistedAISession) -> Result<()> {
        let path = self.session_path(&session.call_id);

        // Serialize to JSON
        let json = serde_json::to_string_pretty(session).map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to serialize AI session for call {}: {}",
                session.call_id.0, e
            ))
        })?;

        // Write atomically using temp file + rename
        let temp_path = path.with_extension("json.tmp");
        fs::write(&temp_path, json).await.map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to write AI session state for call {}: {}",
                session.call_id.0, e
            ))
        })?;

        fs::rename(&temp_path, &path).await.map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to persist AI session state for call {}: {}",
                session.call_id.0, e
            ))
        })?;

        debug!(
            "Saved AI session state for call {} to {:?}",
            session.call_id.0, path
        );

        Ok(())
    }

    async fn load(&self, call_id: &CallId) -> Result<Option<PersistedAISession>> {
        let path = self.session_path(call_id);

        // Check if file exists
        if !path.exists() {
            return Ok(None);
        }

        // Read and deserialize
        let contents = fs::read_to_string(&path).await.map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to read AI session state for call {}: {}",
                call_id.0, e
            ))
        })?;

        let session: PersistedAISession = serde_json::from_str(&contents).map_err(|e| {
            error!(
                "Failed to deserialize AI session for call {}: {}. File may be corrupted.",
                call_id.0, e
            );
            ForgeError::Internal(format!(
                "Failed to deserialize AI session for call {}: {}",
                call_id.0, e
            ))
        })?;

        debug!("Loaded AI session state for call {} from {:?}", call_id.0, path);

        Ok(Some(session))
    }

    async fn delete(&self, call_id: &CallId) -> Result<()> {
        let path = self.session_path(call_id);

        if path.exists() {
            fs::remove_file(&path).await.map_err(|e| {
                ForgeError::Internal(format!(
                    "Failed to delete AI session state for call {}: {}",
                    call_id.0, e
                ))
            })?;

            debug!("Deleted AI session state for call {}", call_id.0);
        }

        Ok(())
    }

    async fn list_all(&self) -> Result<HashMap<CallId, PersistedAISession>> {
        let mut sessions = HashMap::new();

        // Read directory entries
        let mut entries = fs::read_dir(&self.base_dir).await.map_err(|e| {
            ForgeError::Internal(format!(
                "Failed to read persistence directory {:?}: {}",
                self.base_dir, e
            ))
        })?;

        // Load each JSON file
        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            ForgeError::Internal(format!("Failed to read directory entry: {}", e))
        })? {
            let path = entry.path();

            // Skip non-JSON files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Read and deserialize
            match fs::read_to_string(&path).await {
                Ok(contents) => match serde_json::from_str::<PersistedAISession>(&contents) {
                    Ok(session) => {
                        sessions.insert(session.call_id.clone(), session);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to deserialize AI session from {:?}: {}. Skipping.",
                            path, e
                        );
                    }
                },
                Err(e) => {
                    warn!("Failed to read AI session file {:?}: {}. Skipping.", path, e);
                }
            }
        }

        info!(
            "Loaded {} AI sessions from disk persistence",
            sessions.len()
        );

        Ok(sessions)
    }

    async fn health_check(&self) -> Result<bool> {
        // Check if directory exists and is writable
        if !self.base_dir.exists() {
            return Ok(false);
        }

        // Try to write a test file
        let test_path = self.base_dir.join(".health_check");
        match fs::write(&test_path, b"ok").await {
            Ok(_) => {
                // Clean up test file
                let _ = fs::remove_file(&test_path).await;
                Ok(true)
            }
            Err(e) => {
                error!(
                    "Disk persistence health check failed for {:?}: {}",
                    self.base_dir, e
                );
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::CallId;

    #[tokio::test]
    async fn test_disk_backend_save_load() {
        let temp_dir = std::env::temp_dir().join(format!("forge-test-{}", uuid::Uuid::new_v4()));
        let backend = DiskBackend::new(temp_dir.clone()).await.unwrap();

        let call_id = CallId("test-call-123".to_string());
        let config = crate::ai_integration::AISessionConfig::default();
        let session = PersistedAISession::new(call_id.clone(), config);

        // Save
        backend.save(&session).await.unwrap();

        // Load
        let loaded = backend.load(&call_id).await.unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.call_id, call_id);

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_disk_backend_delete() {
        let temp_dir = std::env::temp_dir().join(format!("forge-test-{}", uuid::Uuid::new_v4()));
        let backend = DiskBackend::new(temp_dir.clone()).await.unwrap();

        let call_id = CallId("test-call-456".to_string());
        let config = crate::ai_integration::AISessionConfig::default();
        let session = PersistedAISession::new(call_id.clone(), config);

        // Save and delete
        backend.save(&session).await.unwrap();
        backend.delete(&call_id).await.unwrap();

        // Verify deleted
        let loaded = backend.load(&call_id).await.unwrap();
        assert!(loaded.is_none());

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_disk_backend_health_check() {
        let temp_dir = std::env::temp_dir().join(format!("forge-test-{}", uuid::Uuid::new_v4()));
        let backend = DiskBackend::new(temp_dir.clone()).await.unwrap();

        // Health check should pass
        let healthy = backend.health_check().await.unwrap();
        assert!(healthy);

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
}
