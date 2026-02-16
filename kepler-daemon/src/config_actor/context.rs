//! Service context and state types

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{LogConfig, ServiceConfig};
use crate::logs::LogWriterConfig;
use crate::state::ServiceStatus;

/// Type of task handle stored in state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskHandleType {
    HealthCheck,
    FileWatcher,
}

/// Context for a service that bundles commonly fetched together data.
/// This reduces the pattern of making multiple separate calls for
/// service_config, config_dir, log_config, and global_log_config.
#[derive(Clone)]
pub struct ServiceContext {
    pub service_config: ServiceConfig,
    pub config_dir: PathBuf,
    pub log_config: LogWriterConfig,
    pub global_log_config: Option<LogConfig>,
    /// Pre-computed environment variables from state
    pub env: HashMap<String, String>,
    /// Pre-computed working directory from state
    pub working_dir: PathBuf,
}

/// Result from updating health check
pub struct HealthCheckUpdate {
    pub previous_status: ServiceStatus,
    pub new_status: ServiceStatus,
    pub failures: u32,
}

/// Event emitted when a service status changes.
/// Delivered to all subscribers via the config actor's subscriber registry.
#[derive(Debug, Clone)]
pub struct ServiceStatusChange {
    pub service: String,
    pub status: ServiceStatus,
}

/// Events emitted by the config actor to global subscribers.
/// Per-dep watchers continue to use `ServiceStatusChange` directly.
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// A service status changed
    StatusChange(ServiceStatusChange),
    /// All services reached their target state (for --wait)
    Ready,
    /// All services settled — nothing more will change (for foreground mode exit)
    Quiescent,
}
