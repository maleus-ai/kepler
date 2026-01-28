use chrono::{DateTime, Utc};
use kepler_protocol::protocol::ServiceInfo;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::task::JoinHandle;

use crate::config::KeplerConfig;
use crate::errors::DaemonError;
use crate::logs::SharedLogBuffer;

/// Status of a service
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    Healthy,   // Running + health checks passing
    Unhealthy, // Running but health checks failing
}

impl ServiceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceStatus::Stopped => "stopped",
            ServiceStatus::Starting => "starting",
            ServiceStatus::Running => "running",
            ServiceStatus::Stopping => "stopping",
            ServiceStatus::Failed => "failed",
            ServiceStatus::Healthy => "healthy",
            ServiceStatus::Unhealthy => "unhealthy",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self,
            ServiceStatus::Running | ServiceStatus::Healthy | ServiceStatus::Unhealthy
        )
    }
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// State of a single service
#[derive(Clone)]
pub struct ServiceState {
    pub status: ServiceStatus,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub health_check_failures: u32,
    pub restart_count: u32,
    /// Whether on_init hook has been run for this service
    pub initialized: bool,
}

impl Default for ServiceState {
    fn default() -> Self {
        Self {
            status: ServiceStatus::Stopped,
            pid: None,
            started_at: None,
            exit_code: None,
            health_check_failures: 0,
            restart_count: 0,
            initialized: false,
        }
    }
}

impl ServiceState {
    pub fn to_service_info(&self) -> ServiceInfo {
        ServiceInfo::from(self)
    }
}

impl From<&ServiceState> for ServiceInfo {
    fn from(state: &ServiceState) -> Self {
        ServiceInfo {
            status: state.status.as_str().to_string(),
            pid: state.pid,
            started_at: state.started_at.map(|dt| dt.timestamp()),
            health_check_failures: state.health_check_failures,
        }
    }
}

/// State for a loaded configuration
pub struct ConfigState {
    pub config_path: PathBuf,
    pub config_hash: String,
    pub config: KeplerConfig,
    pub state_dir: PathBuf,
    pub services: HashMap<String, ServiceState>,
    pub logs: SharedLogBuffer,
    /// Whether global on_init hook has been run
    pub initialized: bool,
}

impl ConfigState {
    pub fn new(config_path: PathBuf, config_hash: String, config: KeplerConfig) -> Self {
        let state_dir = crate::global_state_dir().join("configs").join(&config_hash);
        let logs_dir = state_dir.join("logs");

        // Initialize service states
        let services = config
            .services
            .keys()
            .map(|name| (name.clone(), ServiceState::default()))
            .collect();

        Self {
            config_path,
            config_hash,
            config,
            state_dir,
            services,
            logs: SharedLogBuffer::new(logs_dir),
            initialized: false,
        }
    }

    pub fn get_service_state(&self, name: &str) -> Option<&ServiceState> {
        self.services.get(name)
    }

    pub fn get_service_state_mut(&mut self, name: &str) -> Option<&mut ServiceState> {
        self.services.get_mut(name)
    }
}

/// Process handle for a running service
/// Note: Child is not stored here - it's owned by the monitor task
pub struct ProcessHandle {
    /// Channel to signal shutdown to the monitor task
    pub shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    pub stdout_task: Option<JoinHandle<()>>,
    pub stderr_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessHandle")
            .field("shutdown_tx", &self.shutdown_tx.is_some())
            .field("stdout_task", &self.stdout_task.is_some())
            .field("stderr_task", &self.stderr_task.is_some())
            .finish()
    }
}

/// Global daemon state
pub struct DaemonState {
    /// Map from config path (canonicalized) to config state
    pub configs: HashMap<PathBuf, ConfigState>,
    /// Map from (config_path, service_name) to process handle
    pub processes: HashMap<(PathBuf, String), ProcessHandle>,
    /// Watcher handles: (config_path, service_name) -> watcher task
    pub watchers: HashMap<(PathBuf, String), JoinHandle<()>>,
    /// Health check handles: (config_path, service_name) -> health check task
    pub health_checks: HashMap<(PathBuf, String), JoinHandle<()>>,
    /// Daemon start time
    pub started_at: DateTime<Utc>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            processes: HashMap::new(),
            watchers: HashMap::new(),
            health_checks: HashMap::new(),
            started_at: Utc::now(),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        (Utc::now() - self.started_at).num_seconds() as u64
    }

    pub fn get_config(&self, path: &PathBuf) -> Option<&ConfigState> {
        self.configs.get(path)
    }

    pub fn get_config_mut(&mut self, path: &PathBuf) -> Option<&mut ConfigState> {
        self.configs.get_mut(path)
    }

    /// Load or reload a config file
    ///
    /// Security model: The config file is copied to a secure location (~/.kepler/configs/<hash>/config.yaml)
    /// before being parsed. This prevents privilege escalation even if someone modifies the original
    /// file after loading, since the daemon uses the secure copy.
    ///
    /// The hash is computed from the canonicalized path, ensuring stable directory names
    /// regardless of how the path is specified (relative, symlinks, etc.).
    pub fn load_config(&mut self, path: PathBuf) -> crate::errors::Result<()> {
        // Canonicalize path first (resolves symlinks and relative paths)
        let canonical_path = std::fs::canonicalize(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(path.clone())
            } else {
                DaemonError::Io(e)
            }
        })?;

        // Read original file contents
        let contents = std::fs::read(&canonical_path)?;

        // Compute hash from the canonical path (not content) for stable directory names
        let hash = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(canonical_path.to_string_lossy().as_bytes()))
        };

        // Check if config already loaded with same hash
        // Use canonical_path as the key for consistent lookups
        if let Some(existing) = self.configs.get(&canonical_path) {
            if existing.config_hash == hash {
                return Ok(());
            }
        }

        // Create state directory for this config
        let state_dir = crate::global_state_dir().join("configs").join(&hash);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&state_dir)
                .map_err(DaemonError::ConfigCopy)?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&state_dir).map_err(DaemonError::ConfigCopy)?;
        }

        // Copy config to secure location
        let copied_config_path = state_dir.join("config.yaml");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&copied_config_path)
                .map_err(DaemonError::ConfigCopy)?;
            file.write_all(&contents).map_err(DaemonError::ConfigCopy)?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&copied_config_path, &contents).map_err(DaemonError::ConfigCopy)?;
        }

        // Parse config from the secure copy
        let config = KeplerConfig::load(&copied_config_path)?;

        // Preserve initialized state when reloading (logs are now on disk)
        let (existing_initialized, existing_service_states) =
            if let Some(existing) = self.configs.remove(&canonical_path) {
                (existing.initialized, Some(existing.services))
            } else {
                (false, None)
            };

        let mut state = ConfigState::new(canonical_path.clone(), hash, config);

        // Restore initialized flag
        state.initialized = existing_initialized;

        // Restore service initialized states for services that still exist
        if let Some(old_services) = existing_service_states {
            for (name, old_state) in old_services {
                if let Some(new_state) = state.services.get_mut(&name) {
                    new_state.initialized = old_state.initialized;
                }
            }
        }

        self.configs.insert(canonical_path, state);
        Ok(())
    }

    /// Unload a config and clean up
    pub fn unload_config(&mut self, path: &PathBuf) {
        // Remove process handles (they should be stopped first)
        let keys_to_remove: Vec<_> = self
            .processes
            .keys()
            .filter(|(p, _)| p == path)
            .cloned()
            .collect();
        for key in keys_to_remove {
            self.processes.remove(&key);
        }

        // Cancel watcher tasks
        let watcher_keys: Vec<_> = self
            .watchers
            .keys()
            .filter(|(p, _)| p == path)
            .cloned()
            .collect();
        for key in watcher_keys {
            if let Some(handle) = self.watchers.remove(&key) {
                handle.abort();
            }
        }

        // Cancel health check tasks
        let health_keys: Vec<_> = self
            .health_checks
            .keys()
            .filter(|(p, _)| p == path)
            .cloned()
            .collect();
        for key in health_keys {
            if let Some(handle) = self.health_checks.remove(&key) {
                handle.abort();
            }
        }

        // Remove config state
        self.configs.remove(path);
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
