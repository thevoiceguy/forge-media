//! Recording storage management
//!
//! Manages recording metadata, retention policies, and automatic cleanup

use crate::{StorageError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::fs;
use tracing::{debug, info, warn};

/// Recording metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingInfo {
    /// Unique recording ID
    pub id: String,
    /// File path
    pub path: PathBuf,
    /// Room ID
    pub room_id: String,
    /// Optional participant ID (for participant-level recordings)
    pub participant_id: Option<String>,
    /// Recording start time
    pub started_at: SystemTime,
    /// Recording end time (None if still recording)
    pub ended_at: Option<SystemTime>,
    /// File size in bytes
    pub size_bytes: u64,
    /// Duration in seconds
    pub duration_secs: f64,
}

impl RecordingInfo {
    /// Create a new recording info
    pub fn new<S1: Into<String>, S2: Into<String>>(
        id: S1,
        path: PathBuf,
        room_id: S2,
        participant_id: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            path,
            room_id: room_id.into(),
            participant_id,
            started_at: SystemTime::now(),
            ended_at: None,
            size_bytes: 0,
            duration_secs: 0.0,
        }
    }

    /// Mark recording as completed and update metadata
    pub async fn finalize(&mut self) -> Result<()> {
        self.ended_at = Some(SystemTime::now());

        // Get file size
        if self.path.exists() {
            let metadata = fs::metadata(&self.path).await?;
            self.size_bytes = metadata.len();
        }

        Ok(())
    }

    /// Get recording age
    pub fn age(&self) -> Result<Duration> {
        let end_time = self.ended_at.unwrap_or_else(SystemTime::now);
        end_time
            .duration_since(self.started_at)
            .map_err(|e| StorageError::Internal(format!("Failed to calculate age: {}", e)))
    }

    /// Check if recording file exists
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Delete the recording file
    pub async fn delete(&self) -> Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path).await?;
            info!("Deleted recording file: {:?}", self.path);
        }
        Ok(())
    }
}

/// Storage manager for recordings
pub struct StorageManager {
    /// Recording metadata index
    recordings: HashMap<String, RecordingInfo>,
    /// Base directory for recordings
    _base_dir: PathBuf,
    /// Retention period (recordings older than this will be deleted)
    retention_period: Duration,
    /// Maximum storage size in bytes (0 = unlimited)
    max_storage_bytes: u64,
}

impl StorageManager {
    /// Create a new storage manager
    ///
    /// # Arguments
    /// * `base_dir` - Base directory for storing recordings
    /// * `retention_period` - How long to keep recordings (e.g., Duration::from_secs(7 * 24 * 3600) for 7 days)
    /// * `max_storage_bytes` - Maximum storage size (0 for unlimited)
    pub fn new<P: AsRef<Path>>(
        base_dir: P,
        retention_period: Duration,
        max_storage_bytes: u64,
    ) -> Self {
        Self {
            recordings: HashMap::new(),
            _base_dir: base_dir.as_ref().to_path_buf(),
            retention_period,
            max_storage_bytes,
        }
    }

    /// Register a new recording
    pub fn register_recording(&mut self, info: RecordingInfo) {
        debug!("Registering recording: {}", info.id);
        self.recordings.insert(info.id.clone(), info);
    }

    /// Finalize a recording
    pub async fn finalize_recording(&mut self, id: &str) -> Result<()> {
        let recording = self.recordings.get_mut(id)
            .ok_or_else(|| StorageError::RecordingNotFound(format!("Recording {} not found", id)))?;

        recording.finalize().await?;
        info!("Finalized recording {}: {} bytes, {:.2}s",
              id, recording.size_bytes, recording.duration_secs);
        Ok(())
    }

    /// Get recording info
    pub fn get_recording(&self, id: &str) -> Option<&RecordingInfo> {
        self.recordings.get(id)
    }

    /// List all recordings
    pub fn list_recordings(&self) -> Vec<&RecordingInfo> {
        self.recordings.values().collect()
    }

    /// List recordings for a specific room
    pub fn list_room_recordings(&self, room_id: &str) -> Vec<&RecordingInfo> {
        self.recordings
            .values()
            .filter(|r| r.room_id == room_id)
            .collect()
    }

    /// List recordings for a specific participant
    pub fn list_participant_recordings(&self, participant_id: &str) -> Vec<&RecordingInfo> {
        self.recordings
            .values()
            .filter(|r| r.participant_id.as_deref() == Some(participant_id))
            .collect()
    }

    /// Delete a recording
    pub async fn delete_recording(&mut self, id: &str) -> Result<()> {
        let recording = self.recordings.remove(id)
            .ok_or_else(|| StorageError::RecordingNotFound(format!("Recording {} not found", id)))?;

        recording.delete().await?;
        info!("Deleted recording {}", id);
        Ok(())
    }

    /// Get total storage used
    pub fn total_storage_bytes(&self) -> u64 {
        self.recordings.values().map(|r| r.size_bytes).sum()
    }

    /// Clean up old recordings based on retention policy
    pub async fn cleanup_old_recordings(&mut self) -> Result<usize> {
        let mut deleted_count = 0;

        // Find recordings to delete
        let to_delete: Vec<String> = self.recordings
            .values()
            .filter(|r| {
                // Only delete completed recordings
                if r.ended_at.is_none() {
                    return false;
                }

                // Check if older than retention period
                if let Ok(age) = r.age() {
                    age > self.retention_period
                } else {
                    false
                }
            })
            .map(|r| r.id.clone())
            .collect();

        // Delete recordings
        for id in to_delete {
            if let Err(e) = self.delete_recording(&id).await {
                warn!("Failed to delete old recording {}: {}", id, e);
            } else {
                deleted_count += 1;
            }
        }

        if deleted_count > 0 {
            info!("Cleaned up {} old recordings", deleted_count);
        }

        Ok(deleted_count)
    }

    /// Enforce storage limits by deleting oldest recordings
    pub async fn enforce_storage_limits(&mut self) -> Result<usize> {
        if self.max_storage_bytes == 0 {
            return Ok(0); // Unlimited storage
        }

        let total_storage = self.total_storage_bytes();
        if total_storage <= self.max_storage_bytes {
            return Ok(0); // Within limits
        }

        info!("Storage limit exceeded: {} / {} bytes, cleaning up...",
              total_storage, self.max_storage_bytes);

        // Collect (id, size, end_time) tuples to avoid holding references
        let mut completed: Vec<(String, u64, SystemTime)> = self.recordings
            .values()
            .filter(|r| r.ended_at.is_some())
            .map(|r| (r.id.clone(), r.size_bytes, r.ended_at.unwrap()))
            .collect();

        // Sort by end time (oldest first)
        completed.sort_by_key(|(_, _, end_time)| *end_time);

        let mut deleted_count = 0;
        let mut current_storage = total_storage;

        // Delete oldest recordings until within limit
        for (id, size, _) in completed {
            if current_storage <= self.max_storage_bytes {
                break;
            }

            if let Err(e) = self.delete_recording(&id).await {
                warn!("Failed to delete recording {} during cleanup: {}", id, e);
            } else {
                current_storage -= size;
                deleted_count += 1;
            }
        }

        info!("Deleted {} recordings to enforce storage limits", deleted_count);
        Ok(deleted_count)
    }
}

impl Default for StorageManager {
    fn default() -> Self {
        Self::new(
            "/tmp/forge-recordings",
            Duration::from_secs(7 * 24 * 3600), // 7 days
            0, // Unlimited
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_recording_info() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test.wav");

        let mut info = RecordingInfo::new(
            "rec-1",
            path.clone(),
            "room-1",
            Some("participant-1".to_string()),
        );

        assert_eq!(info.id, "rec-1");
        assert_eq!(info.room_id, "room-1");
        assert_eq!(info.participant_id, Some("participant-1".to_string()));
        assert!(info.ended_at.is_none());
    }

    #[test]
    fn test_storage_manager() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = StorageManager::new(
            temp_dir.path(),
            Duration::from_secs(3600),
            1_000_000,
        );

        let path = temp_dir.path().join("test.wav");
        let info = RecordingInfo::new("rec-1", path, "room-1", None);

        manager.register_recording(info);

        assert_eq!(manager.list_recordings().len(), 1);
        assert!(manager.get_recording("rec-1").is_some());
    }

    #[test]
    fn test_list_room_recordings() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = StorageManager::default();

        let path1 = temp_dir.path().join("rec1.wav");
        let path2 = temp_dir.path().join("rec2.wav");

        manager.register_recording(RecordingInfo::new("rec-1", path1, "room-1", None));
        manager.register_recording(RecordingInfo::new("rec-2", path2, "room-2", None));

        let room1_recordings = manager.list_room_recordings("room-1");
        assert_eq!(room1_recordings.len(), 1);
        assert_eq!(room1_recordings[0].id, "rec-1");
    }
}
