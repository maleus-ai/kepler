use chrono::{DateTime, Utc};
use kepler_protocol::protocol::ServiceInfo;
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
