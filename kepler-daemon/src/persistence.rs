//! Configuration persistence layer for Docker-like immutable config snapshots.
//!
//! This module provides persistence for configuration snapshots, allowing the daemon to:
//! - Snapshot expanded configs (with resolved env vars) at first service start
//! - Restore configs on daemon restart without re-expanding environment variables
//! - Track source config paths for existence checks
//! - Persist service runtime state across daemon restarts

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::config::{KeplerConfig, ServiceConfig};
use crate::errors::{DaemonError, Result};
use crate::state::PersistedConfigState;

/// Expanded configuration snapshot with resolved environment variables.
///
/// This snapshot captures the complete resolved state of a configuration
/// at the time of first service start, including all computed environment
/// variables and working directories for each service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandedConfigSnapshot {
    /// The expanded configuration (with env vars already resolved in the env fields)
    pub config: KeplerConfig,
    /// Pre-computed environment variables for each service
    pub service_envs: HashMap<String, HashMap<String, String>>,
    /// Pre-computed working directories for each service
    pub service_working_dirs: HashMap<String, PathBuf>,
    /// Config directory (where the original config file was located)
    pub config_dir: PathBuf,
    /// Unix timestamp of when the snapshot was taken
    pub snapshot_time: i64,
}

/// Persistence layer for a single config's state directory.
///
/// Handles reading and writing:
/// - `expanded_config.yaml`: Fully-expanded config with resolved env vars
/// - `state.json`: Service runtime state (internal, not user-facing)
/// - `source_path.txt`: Original config path for existence check
pub struct ConfigPersistence {
    state_dir: PathBuf,
}

impl ConfigPersistence {
    /// Create a new ConfigPersistence for the given state directory.
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Get the path to the expanded config file.
    fn expanded_config_path(&self) -> PathBuf {
        self.state_dir.join("expanded_config.yaml")
    }

    /// Get the path to the state file.
    fn state_path(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    /// Get the path to the source path file.
    fn source_path_path(&self) -> PathBuf {
        self.state_dir.join("source_path.txt")
    }

    // =========================================================================
    // Expanded config snapshot
    // =========================================================================

    /// Check if an expanded config snapshot exists.
    pub fn has_expanded_config(&self) -> bool {
        self.expanded_config_path().exists()
    }

    /// Save the expanded config snapshot to disk.
    ///
    /// This captures the configuration with all environment variables resolved,
    /// along with the pre-computed environment and working directory for each service.
    pub fn save_expanded_config(&self, snapshot: &ExpandedConfigSnapshot) -> Result<()> {
        let path = self.expanded_config_path();
        let content = serde_yaml::to_string(snapshot).map_err(|e| {
            DaemonError::Internal(format!("Failed to serialize expanded config: {}", e))
        })?;

        self.write_secure_file(&path, content.as_bytes())?;
        debug!("Saved expanded config snapshot to {:?}", path);
        Ok(())
    }

    /// Load the expanded config snapshot from disk.
    pub fn load_expanded_config(&self) -> Result<Option<ExpandedConfigSnapshot>> {
        let path = self.expanded_config_path();
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path).map_err(|e| DaemonError::Internal(format!("Failed to read '{}': {}", path.display(), e)))?;
        let snapshot: ExpandedConfigSnapshot = serde_yaml::from_str(&content).map_err(|e| {
            DaemonError::Internal(format!("Failed to parse expanded config: {}", e))
        })?;

        debug!("Loaded expanded config snapshot from {:?}", path);
        Ok(Some(snapshot))
    }

    // =========================================================================
    // Service state persistence
    // =========================================================================

    /// Save the service state to disk.
    ///
    /// This persists runtime state (status, PID, etc.) for restoration
    /// after daemon restart.
    pub fn save_state(&self, state: &PersistedConfigState) -> Result<()> {
        let path = self.state_path();
        let content = serde_json::to_string_pretty(state).map_err(|e| {
            DaemonError::Internal(format!("Failed to serialize state: {}", e))
        })?;

        self.write_secure_file(&path, content.as_bytes())?;
        debug!("Saved config state to {:?}", path);
        Ok(())
    }

    /// Load the service state from disk.
    pub fn load_state(&self) -> Result<Option<PersistedConfigState>> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path).map_err(|e| DaemonError::Internal(format!("Failed to read '{}': {}", path.display(), e)))?;
        match serde_json::from_str(&content) {
            Ok(state) => {
                debug!("Loaded config state from {:?}", path);
                Ok(Some(state))
            }
            Err(e) => {
                warn!("Failed to parse state.json, ignoring: {}", e);
                Ok(None)
            }
        }
    }

    // =========================================================================
    // Source path tracking
    // =========================================================================

    /// Save the original source config path to disk.
    ///
    /// This is used to check if the source config still exists when
    /// discovering configs on daemon startup.
    pub fn save_source_path(&self, path: &Path) -> Result<()> {
        let dest = self.source_path_path();
        let content = path.to_string_lossy();
        self.write_secure_file(&dest, content.as_bytes())?;
        debug!("Saved source path to {:?}", dest);
        Ok(())
    }

    /// Load the original source config path from disk.
    pub fn load_source_path(&self) -> Result<Option<PathBuf>> {
        let path = self.source_path_path();
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path).map_err(|e| DaemonError::Internal(format!("Failed to read '{}': {}", path.display(), e)))?;
        let source_path = PathBuf::from(content.trim());
        debug!("Loaded source path from {:?}: {:?}", path, source_path);
        Ok(Some(source_path))
    }

    // =========================================================================
    // Env file management
    // =========================================================================

    /// Get the path to the env_files directory.
    fn env_files_dir(&self) -> PathBuf {
        self.state_dir.join("env_files")
    }

    /// Copy service env_files to the state directory.
    ///
    /// This copies each service's env_file to the state directory so that
    /// the env_file values are preserved even if the original file is deleted.
    /// The env_file is used both for:
    /// 1. Expansion context (values used to expand ${VAR} in config fields)
    /// 2. Runtime injection (all values injected into process environment)
    pub fn copy_env_files(
        &self,
        services: &HashMap<String, ServiceConfig>,
        config_dir: &Path,
    ) -> Result<()> {
        let env_files_dir = self.env_files_dir();

        // Create env_files directory with secure permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&env_files_dir)
                .map_err(|e| DaemonError::Internal(format!("Failed to create directory '{}': {}", env_files_dir.display(), e)))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&env_files_dir).map_err(|e| DaemonError::Internal(format!("Failed to create directory '{}': {}", env_files_dir.display(), e)))?;
        }

        for (service_name, config) in services {
            if let Some(ref env_file) = config.env_file {
                // Resolve the env_file path (relative to config dir, or absolute as-is)
                let source = if env_file.is_relative() {
                    config_dir.join(env_file)
                } else {
                    env_file.clone()
                };

                if source.exists() {
                    debug!(
                        "Service '{}': Copying env_file {:?} to state directory",
                        service_name, source
                    );
                    // Use service name as prefix to avoid conflicts
                    let dest_name = format!(
                        "{}_{}",
                        service_name,
                        env_file.file_name().unwrap_or_default().to_string_lossy()
                    );
                    let dest = env_files_dir.join(&dest_name);

                    // Copy with secure permissions
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        let contents = std::fs::read(&source).map_err(|e| DaemonError::Internal(format!("Failed to read '{}': {}", source.display(), e)))?;
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .mode(0o600)
                            .open(&dest)
                            .map_err(|e| DaemonError::Internal(format!("Failed to open '{}': {}", dest.display(), e)))?;
                        file.write_all(&contents).map_err(|e| DaemonError::Internal(format!("Failed to write '{}': {}", dest.display(), e)))?;
                    }
                    #[cfg(not(unix))]
                    {
                        std::fs::copy(&source, &dest).map_err(|e| DaemonError::Internal(format!("Failed to copy '{}' to '{}': {}", source.display(), dest.display(), e)))?;
                    }

                    info!(
                        "Copied env_file for {}: {:?} -> {:?}",
                        service_name, source, dest
                    );
                } else {
                    debug!(
                        "Env file for {} does not exist, skipping: {:?}",
                        service_name, source
                    );
                }
            }
        }
        Ok(())
    }

    // =========================================================================
    // Snapshot management
    // =========================================================================

    /// Clear the snapshot files to force re-expansion on next start.
    ///
    /// This is called by the `recreate` command to force the config
    /// to be re-read and environment variables to be re-expanded.
    pub fn clear_snapshot(&self) -> Result<()> {
        // Remove expanded config
        let expanded_path = self.expanded_config_path();
        if expanded_path.exists() {
            std::fs::remove_file(&expanded_path).map_err(|e| DaemonError::Internal(format!("Failed to remove '{}': {}", expanded_path.display(), e)))?;
            debug!("Removed expanded config: {:?}", expanded_path);
        }

        // Remove state (optional, could preserve some fields)
        let state_path = self.state_path();
        if state_path.exists() {
            std::fs::remove_file(&state_path).map_err(|e| DaemonError::Internal(format!("Failed to remove '{}': {}", state_path.display(), e)))?;
            debug!("Removed state: {:?}", state_path);
        }

        // Remove env_files directory
        let env_files_dir = self.env_files_dir();
        if env_files_dir.exists() {
            std::fs::remove_dir_all(&env_files_dir).map_err(|e| DaemonError::Internal(format!("Failed to remove directory '{}': {}", env_files_dir.display(), e)))?;
            debug!("Removed env_files directory: {:?}", env_files_dir);
        }

        Ok(())
    }

    /// Get the state directory path.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    // =========================================================================
    // Internal helpers
    // =========================================================================

    /// Write a file atomically with secure permissions (0o600 on Unix).
    ///
    /// Writes to a temporary file in the same directory, then atomically
    /// renames it to the target path. This prevents corruption if the
    /// process crashes mid-write.
    fn write_secure_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        let parent = path.parent().ok_or_else(|| {
            DaemonError::Internal(format!("No parent directory for '{}'", path.display()))
        })?;

        let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| {
            DaemonError::Internal(format!("Failed to create temp file in '{}': {}", parent.display(), e))
        })?;

        // Set secure permissions before writing content
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| {
                DaemonError::Internal(format!("Failed to set permissions on temp file: {}", e))
            })?;
        }

        tmp.write_all(content).map_err(|e| {
            DaemonError::Internal(format!("Failed to write temp file: {}", e))
        })?;

        tmp.persist(path).map_err(|e| {
            DaemonError::Internal(format!("Failed to persist '{}': {}", path.display(), e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_persistence_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

        // Test source path
        let source_path = PathBuf::from("/home/user/project/kepler.yaml");
        persistence.save_source_path(&source_path).unwrap();
        let loaded = persistence.load_source_path().unwrap().unwrap();
        assert_eq!(loaded, source_path);

        // Test has_expanded_config
        assert!(!persistence.has_expanded_config());
    }

    #[test]
    fn test_clear_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

        // Create dummy files
        std::fs::write(persistence.expanded_config_path(), "test").unwrap();
        std::fs::write(persistence.state_path(), "test").unwrap();

        assert!(persistence.has_expanded_config());

        // Clear
        persistence.clear_snapshot().unwrap();

        assert!(!persistence.has_expanded_config());
        assert!(!persistence.state_path().exists());
    }
}
