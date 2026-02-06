//! ConfigActorHandle - handle for communicating with ConfigActor
//!
//! This module contains the ConfigActorHandle struct which provides
//! a cheap-to-clone interface for sending commands to a ConfigActor.

use chrono::{DateTime, Utc};
use tracing::warn;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::{KeplerConfig, LogConfig, ServiceConfig, SysEnvPolicy};
use crate::errors::{DaemonError, Result};
use crate::events::{ServiceEvent, ServiceEventReceiver};
use crate::logs::LogWriterConfig;
use crate::state::{ProcessHandle, ServiceState, ServiceStatus};
use kepler_protocol::protocol::{LogEntry, LogMode, ServiceInfo};

use super::command::ConfigCommand;
use super::context::{HealthCheckUpdate, ServiceContext, TaskHandleType};

/// Handle for sending commands to a config actor.
/// This is cheap to clone (just clones the channel sender and path).
#[derive(Clone)]
pub struct ConfigActorHandle {
    config_path: PathBuf,
    config_hash: String,
    tx: mpsc::Sender<ConfigCommand>,
}

impl ConfigActorHandle {
    /// Create a new handle (called by ConfigActor::create)
    pub(super) fn new(
        config_path: PathBuf,
        config_hash: String,
        tx: mpsc::Sender<ConfigCommand>,
    ) -> Self {
        Self {
            config_path,
            config_hash,
            tx,
        }
    }

    /// Get the config path this handle is for
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Get the config hash
    pub fn config_hash(&self) -> &str {
        &self.config_hash
    }

    // === Query Methods ===

    /// Get service context - bundles service_config, config_dir, logs, global_log_config,
    /// and pre-computed environment/working_dir from state.
    /// This is a single round-trip instead of 5.
    pub async fn get_service_context(&self, service_name: &str) -> Option<ServiceContext> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetServiceContext {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetServiceContext");
        }
        reply_rx.await.ok().flatten()
    }

    /// Get status of services
    pub async fn get_service_status(
        &self,
        service: Option<String>,
    ) -> Result<HashMap<String, ServiceInfo>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::GetServiceStatus {
                service,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Get logs
    pub async fn get_logs(&self, service: Option<String>, lines: usize) -> Vec<LogEntry> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetLogs {
                service,
                lines,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetLogs");
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Get logs with bounded reading (prevents OOM with large log files)
    pub async fn get_logs_bounded(
        &self,
        service: Option<String>,
        lines: usize,
        max_bytes: Option<usize>,
    ) -> Vec<LogEntry> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetLogsBounded {
                service,
                lines,
                max_bytes,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetLogsBounded");
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Get logs with mode support (head, tail, or all)
    pub async fn get_logs_with_mode(
        &self,
        service: Option<String>,
        lines: usize,
        max_bytes: Option<usize>,
        mode: LogMode,
    ) -> Vec<LogEntry> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetLogsWithMode {
                service,
                lines,
                max_bytes,
                mode,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetLogsWithMode");
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Get logs with true pagination (reads efficiently from disk with offset/limit)
    pub async fn get_logs_paginated(
        &self,
        service: Option<String>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogEntry>, bool) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetLogsPaginated {
                service,
                offset,
                limit,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetLogsPaginated");
        }
        reply_rx.await.unwrap_or_else(|_| (Vec::new(), false))
    }

    /// Get a service configuration
    pub async fn get_service_config(&self, service_name: &str) -> Option<ServiceConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetServiceConfig {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetServiceConfig");
        }
        reply_rx.await.ok().flatten()
    }

    /// Get the full config
    pub async fn get_config(&self) -> Option<KeplerConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetConfig { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetConfig");
        }
        reply_rx.await.ok()
    }

    /// Get config directory
    pub async fn get_config_dir(&self) -> PathBuf {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetConfigDir { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetConfigDir");
        }
        reply_rx.await.unwrap_or_else(|_| PathBuf::from("."))
    }

    /// Get log config
    pub async fn get_log_config(&self) -> Option<LogWriterConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetLogConfig { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetLogConfig");
        }
        reply_rx.await.ok()
    }

    /// Get global log config
    pub async fn get_global_log_config(&self) -> Option<LogConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetGlobalLogConfig { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetGlobalLogConfig");
        }
        reply_rx.await.ok().flatten()
    }

    /// Get global sys_env policy
    pub async fn get_global_sys_env(&self) -> Option<SysEnvPolicy> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetGlobalSysEnv { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetGlobalSysEnv");
        }
        reply_rx.await.ok().flatten()
    }

    /// Check if a service is running
    pub async fn is_service_running(&self, service_name: &str) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::IsServiceRunning {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send IsServiceRunning");
        }
        reply_rx.await.unwrap_or(false)
    }

    /// Get list of running services
    pub async fn get_running_services(&self) -> Vec<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetRunningServices { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetRunningServices");
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Check if config is initialized
    pub async fn is_config_initialized(&self) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::IsConfigInitialized { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send IsConfigInitialized");
        }
        reply_rx.await.unwrap_or(false)
    }

    /// Check if service is initialized
    pub async fn is_service_initialized(&self, service_name: &str) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::IsServiceInitialized {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send IsServiceInitialized");
        }
        reply_rx.await.unwrap_or(false)
    }

    /// Get service state (clone)
    pub async fn get_service_state(&self, service_name: &str) -> Option<ServiceState> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetServiceState {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetServiceState");
        }
        reply_rx.await.ok().flatten()
    }

    /// Check if all services are stopped
    pub async fn all_services_stopped(&self) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::AllServicesStopped { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send AllServicesStopped");
        }
        reply_rx.await.unwrap_or(true)
    }

    // === Mutation Methods ===

    /// Reload the config file
    pub async fn reload_config(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::ReloadConfig { reply: reply_tx })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Set service status
    pub async fn set_service_status(
        &self,
        service_name: &str,
        status: ServiceStatus,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::SetServiceStatus {
                service_name: service_name.to_string(),
                status,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Set service PID and started_at
    pub async fn set_service_pid(
        &self,
        service_name: &str,
        pid: Option<u32>,
        started_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::SetServicePid {
                service_name: service_name.to_string(),
                pid,
                started_at,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Record that a process exited
    pub async fn record_process_exit(
        &self,
        service_name: &str,
        exit_code: Option<i32>,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::RecordProcessExit {
                service_name: service_name.to_string(),
                exit_code,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Update health check result
    pub async fn update_health_check(
        &self,
        service_name: &str,
        passed: bool,
        retries: u32,
    ) -> Result<HealthCheckUpdate> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::UpdateHealthCheck {
                service_name: service_name.to_string(),
                passed,
                retries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Mark config as initialized
    pub async fn mark_config_initialized(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::MarkConfigInitialized { reply: reply_tx })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Mark service as initialized
    pub async fn mark_service_initialized(&self, service_name: &str) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::MarkServiceInitialized {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Increment restart count
    pub async fn increment_restart_count(&self, service_name: &str) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::IncrementRestartCount {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Clear logs for a service
    pub async fn clear_service_logs(&self, service_name: &str) {
        let _ = self
            .tx
            .send(ConfigCommand::ClearServiceLogs {
                service_name: service_name.to_string(),
            })
            .await;
    }

    /// Clear logs by prefix
    pub async fn clear_service_logs_prefix(&self, prefix: &str) {
        let _ = self
            .tx
            .send(ConfigCommand::ClearServiceLogsPrefix {
                prefix: prefix.to_string(),
            })
            .await;
    }

    // === Process Handle Methods ===

    /// Store a process handle
    pub async fn store_process_handle(&self, service_name: &str, handle: ProcessHandle) {
        let _ = self
            .tx
            .send(ConfigCommand::StoreProcessHandle {
                service_name: service_name.to_string(),
                handle,
            })
            .await;
    }

    /// Remove and return a process handle
    pub async fn remove_process_handle(&self, service_name: &str) -> Option<ProcessHandle> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::RemoveProcessHandle {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send RemoveProcessHandle");
        }
        reply_rx.await.ok().flatten()
    }

    // === Task Handle Methods ===

    /// Store a task handle (health check or file watcher)
    pub async fn store_task_handle(
        &self,
        service_name: &str,
        handle_type: TaskHandleType,
        handle: JoinHandle<()>,
    ) {
        let _ = self
            .tx
            .send(ConfigCommand::StoreTaskHandle {
                service_name: service_name.to_string(),
                handle_type,
                handle,
            })
            .await;
    }

    /// Cancel a task handle
    pub async fn cancel_task_handle(&self, service_name: &str, handle_type: TaskHandleType) {
        let _ = self
            .tx
            .send(ConfigCommand::CancelTaskHandle {
                service_name: service_name.to_string(),
                handle_type,
            })
            .await;
    }

    // === Persistence Methods ===

    /// Take a snapshot of the expanded config if not already taken.
    /// Returns Ok(true) if snapshot was taken, Ok(false) if already existed.
    pub async fn take_snapshot_if_needed(&self) -> Result<bool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::TakeSnapshotIfNeeded { reply: reply_tx })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Clear the snapshot to force re-expansion on next start.
    pub async fn clear_snapshot(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ConfigCommand::ClearSnapshot { reply: reply_tx })
            .await
            .map_err(|_| DaemonError::Internal("Config actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("Config actor dropped response".into()))?
    }

    /// Check if this config was restored from a snapshot.
    pub async fn is_restored_from_snapshot(&self) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::IsRestoredFromSnapshot { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send IsRestoredFromSnapshot");
        }
        reply_rx.await.unwrap_or(false)
    }

    // === Event Channel Methods ===

    /// Create an event channel for a service, returns receiver for orchestrator
    pub async fn create_event_channel(&self, service_name: &str) -> Option<ServiceEventReceiver> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::CreateEventChannel {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send CreateEventChannel");
        }
        reply_rx.await.ok()
    }

    /// Remove event channel when service is removed
    pub async fn remove_event_channel(&self, service_name: &str) {
        let _ = self
            .tx
            .send(ConfigCommand::RemoveEventChannel {
                service_name: service_name.to_string(),
            })
            .await;
    }

    /// Emit an event for a service
    pub async fn emit_event(&self, service_name: &str, event: ServiceEvent) {
        let _ = self
            .tx
            .send(ConfigCommand::EmitEvent {
                service_name: service_name.to_string(),
                event,
            })
            .await;
    }

    /// Get all event receivers for the orchestrator to poll
    pub async fn get_all_event_receivers(&self) -> Vec<(String, ServiceEventReceiver)> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::GetAllEventReceivers { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send GetAllEventReceivers");
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Check if an event handler has been spawned for this config
    pub async fn has_event_handler(&self) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(ConfigCommand::HasEventHandler { reply: reply_tx })
            .await
            .is_err()
        {
            warn!("Config actor closed, cannot send HasEventHandler");
        }
        reply_rx.await.unwrap_or(false)
    }

    /// Mark event handler as spawned for this config
    pub async fn set_event_handler_spawned(&self) {
        let _ = self.tx.send(ConfigCommand::SetEventHandlerSpawned).await;
    }

    // === Lifecycle ===

    /// Shutdown the actor
    pub async fn shutdown(&self) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::Shutdown { reply: reply_tx })
            .await;
        let _ = reply_rx.await;
    }
}
