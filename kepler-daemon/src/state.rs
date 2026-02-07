use chrono::{DateTime, Utc};
use kepler_protocol::protocol::ServiceInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::task::JoinHandle;

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
    /// Pre-computed environment variables for this service
    pub computed_env: HashMap<String, String>,
    /// Pre-computed working directory for this service
    pub working_dir: PathBuf,
    /// Whether the service was healthy at some point (for service_unhealthy condition)
    pub was_healthy: bool,
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
            computed_env: HashMap::new(),
            working_dir: PathBuf::new(),
            was_healthy: false,
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
            exit_code: state.exit_code,
        }
    }
}

/// Process handle for a running service
/// Note: Child is not stored here - it's owned by the monitor task
pub struct ProcessHandle {
    /// Channel to signal shutdown to the monitor task (carries signal number, 15=SIGTERM)
    pub shutdown_tx: Option<tokio::sync::oneshot::Sender<i32>>,
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

// ============================================================================
// Persisted state structures for disk serialization
// ============================================================================

/// Serializable version of ServiceStatus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    Healthy,
    Unhealthy,
}

impl From<ServiceStatus> for PersistedServiceStatus {
    fn from(status: ServiceStatus) -> Self {
        match status {
            ServiceStatus::Stopped => PersistedServiceStatus::Stopped,
            ServiceStatus::Starting => PersistedServiceStatus::Starting,
            ServiceStatus::Running => PersistedServiceStatus::Running,
            ServiceStatus::Stopping => PersistedServiceStatus::Stopping,
            ServiceStatus::Failed => PersistedServiceStatus::Failed,
            ServiceStatus::Healthy => PersistedServiceStatus::Healthy,
            ServiceStatus::Unhealthy => PersistedServiceStatus::Unhealthy,
        }
    }
}

impl From<PersistedServiceStatus> for ServiceStatus {
    fn from(status: PersistedServiceStatus) -> Self {
        match status {
            PersistedServiceStatus::Stopped => ServiceStatus::Stopped,
            PersistedServiceStatus::Starting => ServiceStatus::Starting,
            PersistedServiceStatus::Running => ServiceStatus::Running,
            PersistedServiceStatus::Stopping => ServiceStatus::Stopping,
            PersistedServiceStatus::Failed => ServiceStatus::Failed,
            PersistedServiceStatus::Healthy => ServiceStatus::Healthy,
            PersistedServiceStatus::Unhealthy => ServiceStatus::Unhealthy,
        }
    }
}

/// Persisted state of a single service (serializable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedServiceState {
    pub status: PersistedServiceStatus,
    pub pid: Option<u32>,
    /// Unix timestamp of when the service started
    pub started_at: Option<i64>,
    pub exit_code: Option<i32>,
    pub health_check_failures: u32,
    pub restart_count: u32,
    pub initialized: bool,
    #[serde(default)]
    pub was_healthy: bool,
}

impl From<&ServiceState> for PersistedServiceState {
    fn from(state: &ServiceState) -> Self {
        PersistedServiceState {
            status: state.status.into(),
            pid: state.pid,
            started_at: state.started_at.map(|dt| dt.timestamp()),
            exit_code: state.exit_code,
            health_check_failures: state.health_check_failures,
            restart_count: state.restart_count,
            initialized: state.initialized,
            was_healthy: state.was_healthy,
        }
    }
}

impl PersistedServiceState {
    /// Convert to ServiceState, using provided computed_env and working_dir
    pub fn to_service_state(
        &self,
        computed_env: HashMap<String, String>,
        working_dir: PathBuf,
    ) -> ServiceState {
        ServiceState {
            status: self.status.into(),
            pid: self.pid,
            started_at: self
                .started_at
                .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0)),
            exit_code: self.exit_code,
            health_check_failures: self.health_check_failures,
            restart_count: self.restart_count,
            initialized: self.initialized,
            computed_env,
            working_dir,
            was_healthy: self.was_healthy,
        }
    }
}

/// Persisted config state containing all service states (serializable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedConfigState {
    /// Service states keyed by service name
    pub services: HashMap<String, PersistedServiceState>,
    /// Whether the config has been initialized (on_init hook run)
    pub config_initialized: bool,
    /// Unix timestamp of when the snapshot was taken
    pub snapshot_time: i64,
}
