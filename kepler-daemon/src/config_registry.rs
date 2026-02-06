//! Registry for config actors.
//!
//! This module provides a registry that manages config actors, allowing
//! multiple CLI clients to share actors and enabling parallel processing
//! of commands for different configs.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config_actor::{ConfigActor, ConfigActorHandle};
use crate::errors::Result;
use kepler_protocol::protocol::ConfigStatus;

/// Registry for config actors - allows multiple CLI clients to share actors
pub struct ConfigRegistry {
    /// Map from config path (canonicalized) to actor handle
    actors: DashMap<PathBuf, ConfigActorHandle>,
    /// Daemon start time for uptime calculation
    started_at: DateTime<Utc>,
}

impl ConfigRegistry {
    /// Create a new registry
    pub fn new() -> Self {
        Self {
            actors: DashMap::new(),
            started_at: Utc::now(),
        }
    }

    /// Get daemon uptime in seconds
    pub fn uptime_secs(&self) -> u64 {
        (Utc::now() - self.started_at).num_seconds().max(0) as u64
    }

    /// Get or create a config actor for a path.
    /// If the actor already exists, returns the existing handle.
    /// Otherwise, creates a new actor, spawns it, and returns the handle.
    ///
    /// The `sys_env` parameter provides the system environment variables captured
    /// from the CLI. If None, the daemon's current environment is used.
    pub async fn get_or_create(
        &self,
        config_path: PathBuf,
        sys_env: Option<HashMap<String, String>>,
    ) -> Result<ConfigActorHandle> {
        // Canonicalize path first
        let canonical_path = std::fs::canonicalize(&config_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                crate::errors::DaemonError::ConfigNotFound(config_path.clone())
            } else {
                crate::errors::DaemonError::Io(e)
            }
        })?;

        // Return existing if loaded
        if let Some(handle) = self.actors.get(&canonical_path) {
            // Try to reload config to pick up any changes
            let _ = handle.reload_config().await;
            return Ok(handle.clone());
        }

        // Create new actor
        let (handle, actor) = ConfigActor::create(config_path, sys_env)?;

        // Spawn the actor
        tokio::spawn(actor.run());

        // Store the handle
        self.actors.insert(canonical_path, handle.clone());

        Ok(handle)
    }

    /// Get existing actor (None if not loaded)
    pub fn get(&self, config_path: &PathBuf) -> Option<ConfigActorHandle> {
        // Try to canonicalize the path first
        let canonical_path = std::fs::canonicalize(config_path).ok()?;
        self.actors.get(&canonical_path).map(|h| h.clone())
    }

    /// Unload a config - stops the actor
    pub async fn unload(&self, config_path: &PathBuf) {
        // Canonicalize the path
        if let Ok(canonical_path) = std::fs::canonicalize(config_path) {
            if let Some((_, handle)) = self.actors.remove(&canonical_path) {
                handle.shutdown().await;
            }
        }
    }

    /// List all loaded config paths
    pub fn list_paths(&self) -> Vec<PathBuf> {
        self.actors.iter().map(|e| e.key().clone()).collect()
    }

    /// Get all actor handles for cross-config operations
    pub fn all_handles(&self) -> Vec<ConfigActorHandle> {
        self.actors.iter().map(|e| e.value().clone()).collect()
    }

    /// Shutdown all actors and return the list of config paths
    pub async fn shutdown_all(&self) -> Vec<PathBuf> {
        let paths: Vec<_> = self.list_paths();
        let handles: Vec<_> = self.all_handles();

        // Shutdown all in parallel
        futures::future::join_all(handles.into_iter().map(|h| async move { h.shutdown().await })).await;
        self.actors.clear();

        paths
    }

    /// Get status of all configs (for cross-config status queries)
    pub async fn get_all_status(&self) -> Vec<ConfigStatus> {
        let handles = self.all_handles();

        futures::future::join_all(handles.iter().map(|h| async {
            let services = h.get_service_status(None).await.unwrap_or_default();
            ConfigStatus {
                config_path: h.config_path().to_string_lossy().to_string(),
                config_hash: h.config_hash().to_string(),
                services,
            }
        }))
        .await
    }

    /// Check if config can be pruned (for prune command)
    /// Returns true if config is not loaded or all services are stopped
    pub async fn can_prune_config(&self, config_hash: &str) -> bool {
        for entry in self.actors.iter() {
            let handle = entry.value();
            if handle.config_hash() == config_hash {
                // Config is loaded - check if all services stopped
                return handle.all_services_stopped().await;
            }
        }
        // Config not loaded - can prune
        true
    }

    /// Get loaded config info for listing
    pub async fn list_configs(&self) -> Vec<crate::LoadedConfigInfo> {
        let handles = self.all_handles();

        futures::future::join_all(handles.iter().map(|h| async {
            let config = h.get_config().await;
            let services = h.get_service_status(None).await.unwrap_or_default();
            let running_count = services.values().filter(|s| {
                s.status == "running" || s.status == "healthy" || s.status == "unhealthy"
            }).count();

            crate::LoadedConfigInfo {
                config_path: h.config_path().to_string_lossy().to_string(),
                config_hash: h.config_hash().to_string(),
                service_count: config.map(|c| c.services.len()).unwrap_or(0),
                running_count,
            }
        }))
        .await
    }
}

impl Default for ConfigRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared reference to the config registry
pub type SharedConfigRegistry = Arc<ConfigRegistry>;

/// Loaded config info for listing
#[derive(Debug, Clone)]
pub struct LoadedConfigInfo {
    pub config_path: String,
    pub config_hash: String,
    pub service_count: usize,
    pub running_count: usize,
}

impl From<LoadedConfigInfo> for kepler_protocol::protocol::LoadedConfigInfo {
    fn from(info: LoadedConfigInfo) -> Self {
        kepler_protocol::protocol::LoadedConfigInfo {
            config_path: info.config_path,
            config_hash: info.config_hash,
            service_count: info.service_count,
            running_count: info.running_count,
        }
    }
}
