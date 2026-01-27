use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::{KeplerConfig, LogConfig, ServiceConfig};
use crate::errors::{DaemonError, Result};
use crate::logs::SharedLogBuffer;
use crate::state::{DaemonState, ProcessHandle, ServiceState, ServiceStatus};
use kepler_protocol::protocol::{ConfigStatus, LoadedConfigInfo, LogEntry, ServiceInfo};

/// Type of task handle stored in state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskHandleType {
    HealthCheck,
    FileWatcher,
}

/// Result from updating health check
pub struct HealthCheckUpdate {
    pub previous_status: ServiceStatus,
    pub new_status: ServiceStatus,
    pub failures: u32,
}

/// Commands that can be sent to the state actor
pub enum StateCommand {
    // === Query Commands ===
    GetServiceStatus {
        config_path: PathBuf,
        service: Option<String>,
        reply: oneshot::Sender<Result<HashMap<String, ServiceInfo>>>,
    },
    GetAllStatus {
        reply: oneshot::Sender<Vec<ConfigStatus>>,
    },
    GetLogs {
        config_path: PathBuf,
        service: Option<String>,
        lines: usize,
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    ListConfigs {
        reply: oneshot::Sender<Vec<LoadedConfigInfo>>,
    },
    GetServiceConfig {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<Option<ServiceConfig>>,
    },
    GetConfig {
        config_path: PathBuf,
        reply: oneshot::Sender<Option<KeplerConfig>>,
    },
    GetConfigDir {
        config_path: PathBuf,
        reply: oneshot::Sender<Option<PathBuf>>,
    },
    GetLogs2 {
        config_path: PathBuf,
        reply: oneshot::Sender<Option<SharedLogBuffer>>,
    },
    GetGlobalLogConfig {
        config_path: PathBuf,
        reply: oneshot::Sender<Option<LogConfig>>,
    },
    IsServiceRunning {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<bool>,
    },
    IsConfigInitialized {
        config_path: PathBuf,
        reply: oneshot::Sender<bool>,
    },
    IsServiceInitialized {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<bool>,
    },
    GetServiceState {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<Option<ServiceState>>,
    },
    GetUptimeSecs {
        reply: oneshot::Sender<u64>,
    },

    // === Mutation Commands ===
    LoadConfig {
        path: PathBuf,
        reply: oneshot::Sender<Result<()>>,
    },
    UnloadConfig {
        path: PathBuf,
        reply: oneshot::Sender<()>,
    },
    SetServiceStatus {
        config_path: PathBuf,
        service_name: String,
        status: ServiceStatus,
        reply: oneshot::Sender<Result<()>>,
    },
    SetServicePid {
        config_path: PathBuf,
        service_name: String,
        pid: Option<u32>,
        started_at: Option<DateTime<Utc>>,
        reply: oneshot::Sender<Result<()>>,
    },
    RecordProcessExit {
        config_path: PathBuf,
        service_name: String,
        exit_code: Option<i32>,
        reply: oneshot::Sender<Result<()>>,
    },
    UpdateHealthCheck {
        config_path: PathBuf,
        service_name: String,
        passed: bool,
        retries: u32,
        reply: oneshot::Sender<Result<HealthCheckUpdate>>,
    },
    MarkConfigInitialized {
        config_path: PathBuf,
        reply: oneshot::Sender<Result<()>>,
    },
    MarkServiceInitialized {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    IncrementRestartCount {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    ClearServiceLogs {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<()>,
    },
    ClearServiceLogsPrefix {
        config_path: PathBuf,
        prefix: String,
        reply: oneshot::Sender<()>,
    },

    // === Process Handle Commands ===
    StoreProcessHandle {
        config_path: PathBuf,
        service_name: String,
        handle: ProcessHandle,
        reply: oneshot::Sender<()>,
    },
    RemoveProcessHandle {
        config_path: PathBuf,
        service_name: String,
        reply: oneshot::Sender<Option<ProcessHandle>>,
    },

    // === Task Handle Commands ===
    StoreTaskHandle {
        config_path: PathBuf,
        service_name: String,
        handle_type: TaskHandleType,
        handle: JoinHandle<()>,
        reply: oneshot::Sender<()>,
    },
    CancelTaskHandle {
        config_path: PathBuf,
        service_name: String,
        handle_type: TaskHandleType,
        reply: oneshot::Sender<()>,
    },

    // === Shutdown ===
    Shutdown {
        reply: oneshot::Sender<Vec<PathBuf>>,
    },
}

/// Handle for sending commands to the state actor.
/// This is cheap to clone (just clones the channel sender).
#[derive(Clone)]
pub struct StateHandle {
    tx: mpsc::Sender<StateCommand>,
}

impl StateHandle {
    /// Create a new StateHandle from a command sender
    pub fn new(tx: mpsc::Sender<StateCommand>) -> Self {
        Self { tx }
    }

    // === Query Methods ===

    /// Get status of services in a config
    pub async fn get_service_status(
        &self,
        config_path: PathBuf,
        service: Option<String>,
    ) -> Result<HashMap<String, ServiceInfo>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::GetServiceStatus {
                config_path,
                service,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Get status of all configs
    pub async fn get_all_status(&self) -> Result<Vec<ConfigStatus>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::GetAllStatus { reply: reply_tx })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))
    }

    /// Get logs for a config/service
    pub async fn get_logs(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        lines: usize,
    ) -> Result<Vec<LogEntry>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::GetLogs {
                config_path,
                service,
                lines,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))
    }

    /// List all loaded configs
    pub async fn list_configs(&self) -> Result<Vec<LoadedConfigInfo>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::ListConfigs { reply: reply_tx })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))
    }

    /// Get a service configuration
    pub async fn get_service_config(
        &self,
        config_path: PathBuf,
        service_name: String,
    ) -> Option<ServiceConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetServiceConfig {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Get the full config
    pub async fn get_config(&self, config_path: PathBuf) -> Option<KeplerConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetConfig {
                config_path,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Get config directory
    pub async fn get_config_dir(&self, config_path: PathBuf) -> Option<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetConfigDir {
                config_path,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Get logs buffer for a config
    pub async fn get_logs_buffer(&self, config_path: PathBuf) -> Option<SharedLogBuffer> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetLogs2 {
                config_path,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Get global log config
    pub async fn get_global_log_config(&self, config_path: PathBuf) -> Option<LogConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetGlobalLogConfig {
                config_path,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Check if a service is running
    pub async fn is_service_running(&self, config_path: PathBuf, service_name: String) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::IsServiceRunning {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or(false)
    }

    /// Check if config is initialized
    pub async fn is_config_initialized(&self, config_path: PathBuf) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::IsConfigInitialized {
                config_path,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or(false)
    }

    /// Check if service is initialized
    pub async fn is_service_initialized(&self, config_path: PathBuf, service_name: String) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::IsServiceInitialized {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or(false)
    }

    /// Get service state (clone)
    pub async fn get_service_state(
        &self,
        config_path: PathBuf,
        service_name: String,
    ) -> Option<ServiceState> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetServiceState {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Get daemon uptime in seconds
    pub async fn get_uptime_secs(&self) -> u64 {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::GetUptimeSecs { reply: reply_tx })
            .await;
        reply_rx.await.unwrap_or(0)
    }

    // === Mutation Methods ===

    /// Load a config file
    pub async fn load_config(&self, path: PathBuf) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::LoadConfig {
                path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Unload a config
    pub async fn unload_config(&self, path: PathBuf) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::UnloadConfig {
                path,
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }

    /// Set service status
    pub async fn set_service_status(
        &self,
        config_path: PathBuf,
        service_name: String,
        status: ServiceStatus,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::SetServiceStatus {
                config_path,
                service_name,
                status,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Set service PID and started_at
    pub async fn set_service_pid(
        &self,
        config_path: PathBuf,
        service_name: String,
        pid: Option<u32>,
        started_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::SetServicePid {
                config_path,
                service_name,
                pid,
                started_at,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Record that a process exited
    pub async fn record_process_exit(
        &self,
        config_path: PathBuf,
        service_name: String,
        exit_code: Option<i32>,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::RecordProcessExit {
                config_path,
                service_name,
                exit_code,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Update health check result
    pub async fn update_health_check(
        &self,
        config_path: PathBuf,
        service_name: String,
        passed: bool,
        retries: u32,
    ) -> Result<HealthCheckUpdate> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::UpdateHealthCheck {
                config_path,
                service_name,
                passed,
                retries,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Mark config as initialized
    pub async fn mark_config_initialized(&self, config_path: PathBuf) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::MarkConfigInitialized {
                config_path,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Mark service as initialized
    pub async fn mark_service_initialized(
        &self,
        config_path: PathBuf,
        service_name: String,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::MarkServiceInitialized {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Increment restart count
    pub async fn increment_restart_count(
        &self,
        config_path: PathBuf,
        service_name: String,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateCommand::IncrementRestartCount {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await
            .map_err(|_| DaemonError::Internal("State actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| DaemonError::Internal("State actor dropped response".into()))?
    }

    /// Clear logs for a service
    pub async fn clear_service_logs(&self, config_path: PathBuf, service_name: String) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::ClearServiceLogs {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }

    /// Clear logs by prefix
    pub async fn clear_service_logs_prefix(&self, config_path: PathBuf, prefix: String) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::ClearServiceLogsPrefix {
                config_path,
                prefix,
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }

    // === Process Handle Methods ===

    /// Store a process handle
    pub async fn store_process_handle(
        &self,
        config_path: PathBuf,
        service_name: String,
        handle: ProcessHandle,
    ) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::StoreProcessHandle {
                config_path,
                service_name,
                handle,
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }

    /// Remove and return a process handle
    pub async fn remove_process_handle(
        &self,
        config_path: PathBuf,
        service_name: String,
    ) -> Option<ProcessHandle> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::RemoveProcessHandle {
                config_path,
                service_name,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    // === Task Handle Methods ===

    /// Store a task handle (health check or file watcher)
    pub async fn store_task_handle(
        &self,
        config_path: PathBuf,
        service_name: String,
        handle_type: TaskHandleType,
        handle: JoinHandle<()>,
    ) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::StoreTaskHandle {
                config_path,
                service_name,
                handle_type,
                handle,
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }

    /// Cancel a task handle
    pub async fn cancel_task_handle(
        &self,
        config_path: PathBuf,
        service_name: String,
        handle_type: TaskHandleType,
    ) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::CancelTaskHandle {
                config_path,
                service_name,
                handle_type,
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }

    // === Shutdown ===

    /// Shutdown and get list of config paths
    pub async fn shutdown(&self) -> Vec<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(StateCommand::Shutdown { reply: reply_tx })
            .await;
        reply_rx.await.unwrap_or_default()
    }
}

/// The state actor - owns all state and processes commands
pub struct StateActor {
    state: DaemonState,
    rx: mpsc::Receiver<StateCommand>,
}

impl StateActor {
    /// Create a new state actor
    pub fn new(rx: mpsc::Receiver<StateCommand>) -> Self {
        Self {
            state: DaemonState::new(),
            rx,
        }
    }

    /// Run the actor event loop
    pub async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            self.process_command(cmd);
        }
    }

    fn process_command(&mut self, cmd: StateCommand) {
        match cmd {
            // === Query Commands ===
            StateCommand::GetServiceStatus {
                config_path,
                service,
                reply,
            } => {
                let result = self.get_service_status(&config_path, service.as_deref());
                let _ = reply.send(result);
            }
            StateCommand::GetAllStatus { reply } => {
                let result = self.get_all_status();
                let _ = reply.send(result);
            }
            StateCommand::GetLogs {
                config_path,
                service,
                lines,
                reply,
            } => {
                let result = self.get_logs(&config_path, service.as_deref(), lines);
                let _ = reply.send(result);
            }
            StateCommand::ListConfigs { reply } => {
                let result = self.list_configs();
                let _ = reply.send(result);
            }
            StateCommand::GetServiceConfig {
                config_path,
                service_name,
                reply,
            } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .and_then(|cs| cs.config.services.get(&service_name).cloned());
                let _ = reply.send(result);
            }
            StateCommand::GetConfig { config_path, reply } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .map(|cs| cs.config.clone());
                let _ = reply.send(result);
            }
            StateCommand::GetConfigDir { config_path, reply } => {
                let result = self.state.configs.get(&config_path).map(|cs| {
                    cs.config_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."))
                });
                let _ = reply.send(result);
            }
            StateCommand::GetLogs2 { config_path, reply } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .map(|cs| cs.logs.clone());
                let _ = reply.send(result);
            }
            StateCommand::GetGlobalLogConfig { config_path, reply } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .and_then(|cs| cs.config.logs.clone());
                let _ = reply.send(result);
            }
            StateCommand::IsServiceRunning {
                config_path,
                service_name,
                reply,
            } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .and_then(|cs| cs.services.get(&service_name))
                    .map(|s| s.status.is_running())
                    .unwrap_or(false);
                let _ = reply.send(result);
            }
            StateCommand::IsConfigInitialized { config_path, reply } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .map(|cs| cs.initialized)
                    .unwrap_or(false);
                let _ = reply.send(result);
            }
            StateCommand::IsServiceInitialized {
                config_path,
                service_name,
                reply,
            } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .and_then(|cs| cs.services.get(&service_name))
                    .map(|s| s.initialized)
                    .unwrap_or(false);
                let _ = reply.send(result);
            }
            StateCommand::GetServiceState {
                config_path,
                service_name,
                reply,
            } => {
                let result = self
                    .state
                    .configs
                    .get(&config_path)
                    .and_then(|cs| cs.services.get(&service_name))
                    .cloned();
                let _ = reply.send(result);
            }
            StateCommand::GetUptimeSecs { reply } => {
                let _ = reply.send(self.state.uptime_secs());
            }

            // === Mutation Commands ===
            StateCommand::LoadConfig { path, reply } => {
                let result = self.state.load_config(path);
                let _ = reply.send(result);
            }
            StateCommand::UnloadConfig { path, reply } => {
                self.state.unload_config(&path);
                let _ = reply.send(());
            }
            StateCommand::SetServiceStatus {
                config_path,
                service_name,
                status,
                reply,
            } => {
                let result = self.set_service_status(&config_path, &service_name, status);
                let _ = reply.send(result);
            }
            StateCommand::SetServicePid {
                config_path,
                service_name,
                pid,
                started_at,
                reply,
            } => {
                let result = self.set_service_pid(&config_path, &service_name, pid, started_at);
                let _ = reply.send(result);
            }
            StateCommand::RecordProcessExit {
                config_path,
                service_name,
                exit_code,
                reply,
            } => {
                let result = self.record_process_exit(&config_path, &service_name, exit_code);
                let _ = reply.send(result);
            }
            StateCommand::UpdateHealthCheck {
                config_path,
                service_name,
                passed,
                retries,
                reply,
            } => {
                let result =
                    self.update_health_check(&config_path, &service_name, passed, retries);
                let _ = reply.send(result);
            }
            StateCommand::MarkConfigInitialized { config_path, reply } => {
                let result = if let Some(cs) = self.state.configs.get_mut(&config_path) {
                    cs.initialized = true;
                    Ok(())
                } else {
                    Err(DaemonError::ConfigNotFound(config_path))
                };
                let _ = reply.send(result);
            }
            StateCommand::MarkServiceInitialized {
                config_path,
                service_name,
                reply,
            } => {
                let result =
                    if let Some(cs) = self.state.configs.get_mut(&config_path) {
                        if let Some(ss) = cs.services.get_mut(&service_name) {
                            ss.initialized = true;
                            Ok(())
                        } else {
                            Err(DaemonError::ServiceNotFound(service_name))
                        }
                    } else {
                        Err(DaemonError::ConfigNotFound(config_path))
                    };
                let _ = reply.send(result);
            }
            StateCommand::IncrementRestartCount {
                config_path,
                service_name,
                reply,
            } => {
                let result =
                    if let Some(cs) = self.state.configs.get_mut(&config_path) {
                        if let Some(ss) = cs.services.get_mut(&service_name) {
                            ss.restart_count += 1;
                            Ok(())
                        } else {
                            Err(DaemonError::ServiceNotFound(service_name))
                        }
                    } else {
                        Err(DaemonError::ConfigNotFound(config_path))
                    };
                let _ = reply.send(result);
            }
            StateCommand::ClearServiceLogs {
                config_path,
                service_name,
                reply,
            } => {
                if let Some(cs) = self.state.configs.get(&config_path) {
                    cs.logs.clear_service(&service_name);
                }
                let _ = reply.send(());
            }
            StateCommand::ClearServiceLogsPrefix {
                config_path,
                prefix,
                reply,
            } => {
                if let Some(cs) = self.state.configs.get(&config_path) {
                    cs.logs.clear_service_prefix(&prefix);
                }
                let _ = reply.send(());
            }

            // === Process Handle Commands ===
            StateCommand::StoreProcessHandle {
                config_path,
                service_name,
                handle,
                reply,
            } => {
                self.state
                    .processes
                    .insert((config_path, service_name), handle);
                let _ = reply.send(());
            }
            StateCommand::RemoveProcessHandle {
                config_path,
                service_name,
                reply,
            } => {
                let handle = self.state.processes.remove(&(config_path, service_name));
                let _ = reply.send(handle);
            }

            // === Task Handle Commands ===
            StateCommand::StoreTaskHandle {
                config_path,
                service_name,
                handle_type,
                handle,
                reply,
            } => {
                let key = (config_path, service_name);
                match handle_type {
                    TaskHandleType::HealthCheck => {
                        self.state.health_checks.insert(key, handle);
                    }
                    TaskHandleType::FileWatcher => {
                        self.state.watchers.insert(key, handle);
                    }
                }
                let _ = reply.send(());
            }
            StateCommand::CancelTaskHandle {
                config_path,
                service_name,
                handle_type,
                reply,
            } => {
                let key = (config_path, service_name);
                match handle_type {
                    TaskHandleType::HealthCheck => {
                        if let Some(handle) = self.state.health_checks.remove(&key) {
                            handle.abort();
                        }
                    }
                    TaskHandleType::FileWatcher => {
                        if let Some(handle) = self.state.watchers.remove(&key) {
                            handle.abort();
                        }
                    }
                }
                let _ = reply.send(());
            }

            // === Shutdown ===
            StateCommand::Shutdown { reply } => {
                let configs = self.state.configs.keys().cloned().collect();
                let _ = reply.send(configs);
            }
        }
    }

    // === Helper methods for processing commands ===

    fn get_service_status(
        &self,
        config_path: &Path,
        service: Option<&str>,
    ) -> Result<HashMap<String, ServiceInfo>> {
        let config_state = self
            .state
            .configs
            .get(config_path)
            .ok_or_else(|| DaemonError::ConfigNotFound(config_path.to_path_buf()))?;

        let services = match service {
            Some(name) => {
                let state = config_state
                    .services
                    .get(name)
                    .ok_or_else(|| DaemonError::ServiceNotFound(name.to_string()))?;
                let mut map = HashMap::new();
                map.insert(name.to_string(), ServiceInfo::from(state));
                map
            }
            None => config_state
                .services
                .iter()
                .map(|(name, state)| (name.clone(), ServiceInfo::from(state)))
                .collect(),
        };
        Ok(services)
    }

    fn get_all_status(&self) -> Vec<ConfigStatus> {
        self.state
            .configs
            .iter()
            .map(|(path, cs)| ConfigStatus {
                config_path: path.to_string_lossy().to_string(),
                config_hash: cs.config_hash.clone(),
                services: cs
                    .services
                    .iter()
                    .map(|(name, state)| (name.clone(), ServiceInfo::from(state)))
                    .collect(),
            })
            .collect()
    }

    fn get_logs(&self, config_path: &Path, service: Option<&str>, lines: usize) -> Vec<LogEntry> {
        self.state
            .configs
            .get(config_path)
            .map(|cs| {
                cs.logs
                    .tail(lines, service)
                    .into_iter()
                    .map(|l| l.into())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn list_configs(&self) -> Vec<LoadedConfigInfo> {
        self.state
            .configs
            .iter()
            .map(|(path, cs)| LoadedConfigInfo {
                config_path: path.to_string_lossy().to_string(),
                config_hash: cs.config_hash.clone(),
                service_count: cs.config.services.len(),
                running_count: cs.services.values().filter(|s| s.status.is_running()).count(),
            })
            .collect()
    }

    fn set_service_status(
        &mut self,
        config_path: &Path,
        service_name: &str,
        status: ServiceStatus,
    ) -> Result<()> {
        let config_state = self
            .state
            .configs
            .get_mut(config_path)
            .ok_or_else(|| DaemonError::ConfigNotFound(config_path.to_path_buf()))?;
        let service_state = config_state
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.status = status;
        Ok(())
    }

    fn set_service_pid(
        &mut self,
        config_path: &Path,
        service_name: &str,
        pid: Option<u32>,
        started_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let config_state = self
            .state
            .configs
            .get_mut(config_path)
            .ok_or_else(|| DaemonError::ConfigNotFound(config_path.to_path_buf()))?;
        let service_state = config_state
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.pid = pid;
        service_state.started_at = started_at;
        Ok(())
    }

    fn record_process_exit(
        &mut self,
        config_path: &Path,
        service_name: &str,
        exit_code: Option<i32>,
    ) -> Result<()> {
        // Remove process handle
        self.state
            .processes
            .remove(&(config_path.to_path_buf(), service_name.to_string()));

        let config_state = self
            .state
            .configs
            .get_mut(config_path)
            .ok_or_else(|| DaemonError::ConfigNotFound(config_path.to_path_buf()))?;
        let service_state = config_state
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.exit_code = exit_code;
        service_state.pid = None;
        Ok(())
    }

    fn update_health_check(
        &mut self,
        config_path: &Path,
        service_name: &str,
        passed: bool,
        retries: u32,
    ) -> Result<HealthCheckUpdate> {
        let config_state = self
            .state
            .configs
            .get_mut(config_path)
            .ok_or_else(|| DaemonError::ConfigNotFound(config_path.to_path_buf()))?;
        let service_state = config_state
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;

        let previous_status = service_state.status;

        if passed {
            service_state.health_check_failures = 0;
            if service_state.status == ServiceStatus::Running
                || service_state.status == ServiceStatus::Unhealthy
            {
                service_state.status = ServiceStatus::Healthy;
            }
        } else {
            service_state.health_check_failures += 1;
            if service_state.health_check_failures >= retries
                && service_state.status != ServiceStatus::Unhealthy
            {
                service_state.status = ServiceStatus::Unhealthy;
            }
        }

        Ok(HealthCheckUpdate {
            previous_status,
            new_status: service_state.status,
            failures: service_state.health_check_failures,
        })
    }
}

/// Create state actor and handle
pub fn create_state_actor() -> (StateHandle, StateActor) {
    let (tx, rx) = mpsc::channel(256); // Bounded channel for backpressure
    let handle = StateHandle::new(tx);
    let actor = StateActor::new(rx);
    (handle, actor)
}
