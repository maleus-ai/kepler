//! Service context and state types

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{LogConfig, RawServiceConfig, ServiceConfig};
use crate::logs::LogStoreHandle;
use crate::state::ServiceStatus;

/// Identifies a claimed startup. Invalidated by a stop or a later lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStartup {
    pub(super) generation: u64,
}

/// Identity of one spawned process, independent of its current lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceInstance {
    pub(super) generation: u64,
}

/// Identity of a checker attached to one service instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthCheckSession {
    pub(super) generation: u64,
}

/// Type of task handle stored in state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskHandleType {
    HealthCheck,
    FileWatcher,
}

/// Context for a service that bundles commonly fetched together data.
/// This reduces the pattern of making multiple separate calls for
/// service_config, config_dir, log_config, and global_log_config.
///
/// The `service_config` is the typed raw config before service start time.
/// The orchestrator resolves it to a typed `ServiceConfig` at service start time
/// (after dependencies are satisfied and expansion context is available).
#[derive(Clone)]
pub struct ServiceContext {
    pub service_config: RawServiceConfig,
    /// Resolved (expanded + deserialized) service config, cached after first start.
    /// None before a service has been started for the first time.
    pub resolved_config: Option<ServiceConfig>,
    /// Guard identity captured with this context, for cleanup of this lifecycle only.
    pub token: Option<crate::token_store::Token>,
    pub config_dir: PathBuf,
    /// State directory for daemon-managed data (outputs, logs, etc.)
    pub state_dir: PathBuf,
    pub log_store: LogStoreHandle,
    pub global_log_config: Option<LogConfig>,
    /// Pre-computed environment variables from state
    pub env: HashMap<String, String>,
    /// Pre-computed working directory from state
    pub working_dir: PathBuf,
    /// Env vars loaded from env_file (before environment entries)
    pub env_file_vars: HashMap<String, String>,
}

/// Diagnostic counts for resource cleanup verification.
/// Used by tests to assert that all resources are properly cleaned up.
#[derive(Debug, Default)]
pub struct DiagnosticCounts {
    pub process_handles: usize,
    pub health_checks: usize,
    pub file_watchers: usize,
    pub event_senders: usize,
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
    /// A service failed and no other service handles the failure
    /// (no `service_failed` or `service_stopped` dependency, and restart won't trigger)
    UnhandledFailure {
        service: String,
        exit_code: Option<i32>,
    },
}
