use chrono::{DateTime, Utc};
use kepler_protocol::protocol::ServiceInfo;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Child;
use tokio::task::JoinHandle;

use crate::config::KeplerConfig;
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

/// State of a single service
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
        ServiceInfo {
            status: self.status.as_str().to_string(),
            pid: self.pid,
            started_at: self.started_at.map(|dt| dt.timestamp()),
            health_check_failures: self.health_check_failures,
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

/// Process handle for a running service (stored separately due to !Send/!Sync)
pub struct ProcessHandle {
    pub child: Child,
    pub stdout_task: Option<JoinHandle<()>>,
    pub stderr_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessHandle")
            .field("child_pid", &self.child.id())
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
    pub fn load_config(&mut self, path: PathBuf) -> crate::errors::Result<()> {
        let config = KeplerConfig::load(&path)?;
        let hash = KeplerConfig::compute_hash(&path)?;

        if let Some(existing) = self.configs.get(&path) {
            if existing.config_hash == hash {
                // Config hasn't changed
                return Ok(());
            }
        }

        // Preserve initialized state when reloading (logs are now on disk)
        let (existing_initialized, existing_service_states) =
            if let Some(existing) = self.configs.remove(&path) {
                (existing.initialized, Some(existing.services))
            } else {
                (false, None)
            };

        let mut state = ConfigState::new(path.clone(), hash, config);

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

        self.configs.insert(path, state);
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

/// Thread-safe wrapper for daemon state
pub type SharedDaemonState = Arc<RwLock<DaemonState>>;

pub fn new_shared_state() -> SharedDaemonState {
    Arc::new(RwLock::new(DaemonState::new()))
}
