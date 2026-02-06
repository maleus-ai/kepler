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
