//! Commands for the ConfigActor

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::context::{HealthCheckUpdate, ServiceContext, TaskHandleType};
use crate::config::{KeplerConfig, LogConfig, ServiceConfig, SysEnvPolicy};
use crate::errors::Result;
use crate::events::{ServiceEvent, ServiceEventReceiver};
use crate::logs::LogWriterConfig;
use crate::state::{ProcessHandle, ServiceState, ServiceStatus};
use kepler_protocol::protocol::{LogEntry, LogMode, ServiceInfo};

/// Commands for a single config's actor
pub enum ConfigCommand {
    // === Query Commands ===
    GetServiceContext {
        service_name: String,
        reply: oneshot::Sender<Option<ServiceContext>>,
    },
    GetServiceStatus {
        service: Option<String>,
        reply: oneshot::Sender<Result<HashMap<String, ServiceInfo>>>,
    },
    GetLogs {
        service: Option<String>,
        lines: usize,
        no_hooks: bool,
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    GetLogsBounded {
        service: Option<String>,
        lines: usize,
        max_bytes: Option<usize>,
        no_hooks: bool,
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    GetLogsWithMode {
        service: Option<String>,
        lines: usize,
        max_bytes: Option<usize>,
        mode: LogMode,
        no_hooks: bool,
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    GetLogsPaginated {
        service: Option<String>,
        offset: usize,
        limit: usize,
        no_hooks: bool,
        reply: oneshot::Sender<(Vec<LogEntry>, bool)>,
    },
    GetServiceConfig {
        service_name: String,
        reply: oneshot::Sender<Option<ServiceConfig>>,
    },
    GetConfig {
        reply: oneshot::Sender<KeplerConfig>,
    },
    GetConfigDir {
        reply: oneshot::Sender<PathBuf>,
    },
    GetLogConfig {
        reply: oneshot::Sender<LogWriterConfig>,
    },
    GetGlobalLogConfig {
        reply: oneshot::Sender<Option<LogConfig>>,
    },
    GetGlobalSysEnv {
        reply: oneshot::Sender<Option<SysEnvPolicy>>,
    },
    GetSysEnv {
        reply: oneshot::Sender<HashMap<String, String>>,
    },
    IsServiceRunning {
        service_name: String,
        reply: oneshot::Sender<bool>,
    },
    GetRunningServices {
        reply: oneshot::Sender<Vec<String>>,
    },
    IsConfigInitialized {
        reply: oneshot::Sender<bool>,
    },
    IsServiceInitialized {
        service_name: String,
        reply: oneshot::Sender<bool>,
    },
    GetServiceState {
        service_name: String,
        reply: oneshot::Sender<Option<ServiceState>>,
    },
    GetConfigHash {
        reply: oneshot::Sender<String>,
    },
    GetConfigPath {
        reply: oneshot::Sender<PathBuf>,
    },
    AllServicesStopped {
        reply: oneshot::Sender<bool>,
    },

    // === Mutation Commands ===
    ReloadConfig {
        reply: oneshot::Sender<Result<()>>,
    },
    SetServiceStatus {
        service_name: String,
        status: ServiceStatus,
        reply: oneshot::Sender<Result<()>>,
    },
    SetServicePid {
        service_name: String,
        pid: Option<u32>,
        started_at: Option<DateTime<Utc>>,
        reply: oneshot::Sender<Result<()>>,
    },
    RecordProcessExit {
        service_name: String,
        exit_code: Option<i32>,
        reply: oneshot::Sender<Result<()>>,
    },
    UpdateHealthCheck {
        service_name: String,
        passed: bool,
        retries: u32,
        reply: oneshot::Sender<Result<HealthCheckUpdate>>,
    },
    MarkConfigInitialized {
        reply: oneshot::Sender<Result<()>>,
    },
    MarkServiceInitialized {
        service_name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    IncrementRestartCount {
        service_name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    ClearServiceLogs {
        service_name: String,
    },
    ClearServiceLogsPrefix {
        prefix: String,
    },

    // === Process Handle Commands ===
    StoreProcessHandle {
        service_name: String,
        handle: ProcessHandle,
    },
    RemoveProcessHandle {
        service_name: String,
        reply: oneshot::Sender<Option<ProcessHandle>>,
    },

    // === Task Handle Commands ===
    StoreTaskHandle {
        service_name: String,
        handle_type: TaskHandleType,
        handle: JoinHandle<()>,
    },
    CancelTaskHandle {
        service_name: String,
        handle_type: TaskHandleType,
    },

    // === Lifecycle ===
    Shutdown {
        reply: oneshot::Sender<()>,
    },

    // === Persistence Commands ===
    /// Take a snapshot of the expanded config if not already taken
    TakeSnapshotIfNeeded {
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Clear the snapshot (for recreate command)
    ClearSnapshot {
        reply: oneshot::Sender<Result<()>>,
    },
    /// Check if this config was restored from a snapshot
    IsRestoredFromSnapshot {
        reply: oneshot::Sender<bool>,
    },

    // === Event Channel Commands ===
    /// Create an event channel for a service, returns receiver
    CreateEventChannel {
        service_name: String,
        reply: oneshot::Sender<ServiceEventReceiver>,
    },
    /// Remove event channel for a service
    RemoveEventChannel {
        service_name: String,
    },
    /// Emit an event for a service
    EmitEvent {
        service_name: String,
        event: ServiceEvent,
    },
    /// Get all event receivers (for orchestrator to poll)
    GetAllEventReceivers {
        reply: oneshot::Sender<Vec<(String, ServiceEventReceiver)>>,
    },
    /// Check if event handler has been spawned
    HasEventHandler {
        reply: oneshot::Sender<bool>,
    },
    /// Mark event handler as spawned
    SetEventHandlerSpawned,
}
