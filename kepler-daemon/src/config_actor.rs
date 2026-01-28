//! Per-config actor for parallel processing.
//!
//! This module provides a per-config actor pattern that allows parallel processing
//! of commands for different configs. Each loaded config gets its own actor with
//! its own channel, eliminating the single-channel bottleneck.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::info;

use crate::config::{KeplerConfig, LogConfig, ServiceConfig};
use crate::env::build_service_env;
use crate::errors::{DaemonError, Result};
use crate::logs::SharedLogBuffer;
use crate::state::{ProcessHandle, ServiceState, ServiceStatus};
use kepler_protocol::protocol::{LogEntry, ServiceInfo};

/// Type of task handle stored in state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskHandleType {
    HealthCheck,
    FileWatcher,
}

/// Context for a service that bundles commonly fetched together data.
/// This reduces the pattern of making multiple separate calls for
/// service_config, config_dir, logs, and global_log_config.
#[derive(Clone)]
pub struct ServiceContext {
    pub service_config: ServiceConfig,
    pub config_dir: PathBuf,
    pub logs: SharedLogBuffer,
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
        reply: oneshot::Sender<Vec<LogEntry>>,
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
    GetLogsBuffer {
        reply: oneshot::Sender<SharedLogBuffer>,
    },
    GetGlobalLogConfig {
        reply: oneshot::Sender<Option<LogConfig>>,
    },
    IsServiceRunning {
        service_name: String,
        reply: oneshot::Sender<bool>,
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
}

/// Per-config actor state
pub struct ConfigActor {
    config_path: PathBuf,
    config_hash: String,
    config: KeplerConfig,
    config_dir: PathBuf,
    state_dir: PathBuf,
    services: HashMap<String, ServiceState>,
    logs: SharedLogBuffer,
    initialized: bool,

    // Process and task handles (moved from DaemonState)
    processes: HashMap<String, ProcessHandle>,
    watchers: HashMap<String, JoinHandle<()>>,
    health_checks: HashMap<String, JoinHandle<()>>,

    rx: mpsc::Receiver<ConfigCommand>,
}

impl ConfigActor {
    /// Create a new config actor by loading a config file
    pub fn create(config_path: PathBuf) -> Result<(ConfigActorHandle, Self)> {
        // Canonicalize path first
        let canonical_path = std::fs::canonicalize(&config_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(config_path.clone())
            } else {
                DaemonError::Io(e)
            }
        })?;

        // Read original file contents
        let contents = std::fs::read(&canonical_path)?;

        // Compute hash from the canonical path
        let hash = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(canonical_path.to_string_lossy().as_bytes()))
        };

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

        // Get config directory
        let config_dir = canonical_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // Initialize service states with pre-computed environment and working directory
        let services = config
            .services
            .iter()
            .map(|(name, service_config)| {
                let working_dir = service_config
                    .working_dir
                    .clone()
                    .unwrap_or_else(|| config_dir.clone());

                let computed_env = build_service_env(service_config, &config_dir).unwrap_or_default();

                let state = ServiceState {
                    computed_env,
                    working_dir,
                    ..Default::default()
                };
                (name.clone(), state)
            })
            .collect();

        // Create logs buffer
        let logs_dir = state_dir.join("logs");
        let logs = SharedLogBuffer::new(logs_dir);

        // Create channel
        let (tx, rx) = mpsc::channel(256);

        let handle = ConfigActorHandle {
            config_path: canonical_path.clone(),
            config_hash: hash.clone(),
            tx,
        };

        let actor = ConfigActor {
            config_path: canonical_path,
            config_hash: hash,
            config,
            config_dir,
            state_dir,
            services,
            logs,
            initialized: false,
            processes: HashMap::new(),
            watchers: HashMap::new(),
            health_checks: HashMap::new(),
            rx,
        };

        Ok((handle, actor))
    }

    /// Run the actor event loop
    pub async fn run(mut self) {
        info!("ConfigActor started for {:?}", self.config_path);
        while let Some(cmd) = self.rx.recv().await {
            if self.process_command(cmd) {
                break;
            }
        }
        info!("ConfigActor stopped for {:?}", self.config_path);
        self.cleanup();
    }

    /// Process a single command. Returns true if shutdown was requested.
    fn process_command(&mut self, cmd: ConfigCommand) -> bool {
        match cmd {
            // === Query Commands ===
            ConfigCommand::GetServiceContext {
                service_name,
                reply,
            } => {
                let result = self.build_service_context(&service_name);
                let _ = reply.send(result);
            }
            ConfigCommand::GetServiceStatus { service, reply } => {
                let result = self.get_service_status(service.as_deref());
                let _ = reply.send(result);
            }
            ConfigCommand::GetLogs {
                service,
                lines,
                reply,
            } => {
                let result = self.get_logs(service.as_deref(), lines);
                let _ = reply.send(result);
            }
            ConfigCommand::GetServiceConfig {
                service_name,
                reply,
            } => {
                let result = self.config.services.get(&service_name).cloned();
                let _ = reply.send(result);
            }
            ConfigCommand::GetConfig { reply } => {
                let _ = reply.send(self.config.clone());
            }
            ConfigCommand::GetConfigDir { reply } => {
                let _ = reply.send(self.config_dir.clone());
            }
            ConfigCommand::GetLogsBuffer { reply } => {
                let _ = reply.send(self.logs.clone());
            }
            ConfigCommand::GetGlobalLogConfig { reply } => {
                let _ = reply.send(self.config.logs.clone());
            }
            ConfigCommand::IsServiceRunning {
                service_name,
                reply,
            } => {
                let result = self
                    .services
                    .get(&service_name)
                    .map(|s| s.status.is_running())
                    .unwrap_or(false);
                let _ = reply.send(result);
            }
            ConfigCommand::IsConfigInitialized { reply } => {
                let _ = reply.send(self.initialized);
            }
            ConfigCommand::IsServiceInitialized {
                service_name,
                reply,
            } => {
                let result = self
                    .services
                    .get(&service_name)
                    .map(|s| s.initialized)
                    .unwrap_or(false);
                let _ = reply.send(result);
            }
            ConfigCommand::GetServiceState {
                service_name,
                reply,
            } => {
                let result = self.services.get(&service_name).cloned();
                let _ = reply.send(result);
            }
            ConfigCommand::GetConfigHash { reply } => {
                let _ = reply.send(self.config_hash.clone());
            }
            ConfigCommand::GetConfigPath { reply } => {
                let _ = reply.send(self.config_path.clone());
            }
            ConfigCommand::AllServicesStopped { reply } => {
                let all_stopped = self.services.values().all(|s| !s.status.is_running());
                let _ = reply.send(all_stopped);
            }

            // === Mutation Commands ===
            ConfigCommand::ReloadConfig { reply } => {
                let result = self.reload_config();
                let _ = reply.send(result);
            }
            ConfigCommand::SetServiceStatus {
                service_name,
                status,
                reply,
            } => {
                let result = self.set_service_status(&service_name, status);
                let _ = reply.send(result);
            }
            ConfigCommand::SetServicePid {
                service_name,
                pid,
                started_at,
                reply,
            } => {
                let result = self.set_service_pid(&service_name, pid, started_at);
                let _ = reply.send(result);
            }
            ConfigCommand::RecordProcessExit {
                service_name,
                exit_code,
                reply,
            } => {
                let result = self.record_process_exit(&service_name, exit_code);
                let _ = reply.send(result);
            }
            ConfigCommand::UpdateHealthCheck {
                service_name,
                passed,
                retries,
                reply,
            } => {
                let result = self.update_health_check(&service_name, passed, retries);
                let _ = reply.send(result);
            }
            ConfigCommand::MarkConfigInitialized { reply } => {
                self.initialized = true;
                let _ = reply.send(Ok(()));
            }
            ConfigCommand::MarkServiceInitialized {
                service_name,
                reply,
            } => {
                let result = if let Some(ss) = self.services.get_mut(&service_name) {
                    ss.initialized = true;
                    Ok(())
                } else {
                    Err(DaemonError::ServiceNotFound(service_name))
                };
                let _ = reply.send(result);
            }
            ConfigCommand::IncrementRestartCount {
                service_name,
                reply,
            } => {
                let result = if let Some(ss) = self.services.get_mut(&service_name) {
                    ss.restart_count += 1;
                    Ok(())
                } else {
                    Err(DaemonError::ServiceNotFound(service_name))
                };
                let _ = reply.send(result);
            }
            ConfigCommand::ClearServiceLogs { service_name } => {
                self.logs.clear_service(&service_name);
            }
            ConfigCommand::ClearServiceLogsPrefix { prefix } => {
                self.logs.clear_service_prefix(&prefix);
            }

            // === Process Handle Commands ===
            ConfigCommand::StoreProcessHandle {
                service_name,
                handle,
            } => {
                self.processes.insert(service_name, handle);
            }
            ConfigCommand::RemoveProcessHandle {
                service_name,
                reply,
            } => {
                let handle = self.processes.remove(&service_name);
                let _ = reply.send(handle);
            }

            // === Task Handle Commands ===
            ConfigCommand::StoreTaskHandle {
                service_name,
                handle_type,
                handle,
            } => match handle_type {
                TaskHandleType::HealthCheck => {
                    self.health_checks.insert(service_name, handle);
                }
                TaskHandleType::FileWatcher => {
                    self.watchers.insert(service_name, handle);
                }
            },
            ConfigCommand::CancelTaskHandle {
                service_name,
                handle_type,
            } => match handle_type {
                TaskHandleType::HealthCheck => {
                    if let Some(handle) = self.health_checks.remove(&service_name) {
                        handle.abort();
                    }
                }
                TaskHandleType::FileWatcher => {
                    if let Some(handle) = self.watchers.remove(&service_name) {
                        handle.abort();
                    }
                }
            },

            // === Lifecycle ===
            ConfigCommand::Shutdown { reply } => {
                let _ = reply.send(());
                return true;
            }
        }
        false
    }

    /// Build a ServiceContext for a service (single round-trip)
    fn build_service_context(&self, service_name: &str) -> Option<ServiceContext> {
        let service_config = self.config.services.get(service_name)?.clone();
        let service_state = self.services.get(service_name)?;

        Some(ServiceContext {
            service_config,
            config_dir: self.config_dir.clone(),
            logs: self.logs.clone(),
            global_log_config: self.config.logs.clone(),
            env: service_state.computed_env.clone(),
            working_dir: service_state.working_dir.clone(),
        })
    }

    fn get_service_status(&self, service: Option<&str>) -> Result<HashMap<String, ServiceInfo>> {
        match service {
            Some(name) => {
                let state = self
                    .services
                    .get(name)
                    .ok_or_else(|| DaemonError::ServiceNotFound(name.to_string()))?;
                let mut map = HashMap::new();
                map.insert(name.to_string(), ServiceInfo::from(state));
                Ok(map)
            }
            None => Ok(self
                .services
                .iter()
                .map(|(name, state)| (name.clone(), ServiceInfo::from(state)))
                .collect()),
        }
    }

    fn get_logs(&self, service: Option<&str>, lines: usize) -> Vec<LogEntry> {
        self.logs
            .tail(lines, service)
            .into_iter()
            .map(|l| l.into())
            .collect()
    }

    fn reload_config(&mut self) -> Result<()> {
        // Read original file contents
        let contents = std::fs::read(&self.config_path)?;

        // Copy to secure location
        let copied_config_path = self.state_dir.join("config.yaml");
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

        // Parse config
        let new_config = KeplerConfig::load(&copied_config_path)?;

        // Preserve service initialized states
        let old_services = std::mem::take(&mut self.services);

        // Initialize new service states
        self.services = new_config
            .services
            .iter()
            .map(|(name, service_config)| {
                let working_dir = service_config
                    .working_dir
                    .clone()
                    .unwrap_or_else(|| self.config_dir.clone());

                let computed_env =
                    build_service_env(service_config, &self.config_dir).unwrap_or_default();

                let mut state = ServiceState {
                    computed_env,
                    working_dir,
                    ..Default::default()
                };

                // Preserve initialized state if service existed before
                if let Some(old_state) = old_services.get(name) {
                    state.initialized = old_state.initialized;
                }

                (name.clone(), state)
            })
            .collect();

        self.config = new_config;
        Ok(())
    }

    fn set_service_status(&mut self, service_name: &str, status: ServiceStatus) -> Result<()> {
        let service_state = self
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.status = status;
        Ok(())
    }

    fn set_service_pid(
        &mut self,
        service_name: &str,
        pid: Option<u32>,
        started_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let service_state = self
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.pid = pid;
        service_state.started_at = started_at;
        Ok(())
    }

    fn record_process_exit(&mut self, service_name: &str, exit_code: Option<i32>) -> Result<()> {
        // Remove process handle
        self.processes.remove(service_name);

        let service_state = self
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.exit_code = exit_code;
        service_state.pid = None;
        Ok(())
    }

    fn update_health_check(
        &mut self,
        service_name: &str,
        passed: bool,
        retries: u32,
    ) -> Result<HealthCheckUpdate> {
        let service_state = self
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

    /// Cleanup all resources when shutting down
    fn cleanup(&mut self) {
        // Cancel all health checks
        for (_, handle) in self.health_checks.drain() {
            handle.abort();
        }

        // Cancel all watchers
        for (_, handle) in self.watchers.drain() {
            handle.abort();
        }

        // Clear processes (they should have been stopped already)
        self.processes.clear();
    }
}

/// Handle for sending commands to a config actor.
/// This is cheap to clone (just clones the channel sender and path).
#[derive(Clone)]
pub struct ConfigActorHandle {
    config_path: PathBuf,
    config_hash: String,
    tx: mpsc::Sender<ConfigCommand>,
}

impl ConfigActorHandle {
    /// Get the config path this handle is for
    pub fn config_path(&self) -> &PathBuf {
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
        let _ = self
            .tx
            .send(ConfigCommand::GetServiceContext {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await;
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
        let _ = self
            .tx
            .send(ConfigCommand::GetLogs {
                service,
                lines,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or_default()
    }

    /// Get a service configuration
    pub async fn get_service_config(&self, service_name: &str) -> Option<ServiceConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetServiceConfig {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Get the full config
    pub async fn get_config(&self) -> Option<KeplerConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetConfig { reply: reply_tx })
            .await;
        reply_rx.await.ok()
    }

    /// Get config directory
    pub async fn get_config_dir(&self) -> PathBuf {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetConfigDir { reply: reply_tx })
            .await;
        reply_rx.await.unwrap_or_else(|_| PathBuf::from("."))
    }

    /// Get logs buffer
    pub async fn get_logs_buffer(&self) -> Option<SharedLogBuffer> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetLogsBuffer { reply: reply_tx })
            .await;
        reply_rx.await.ok()
    }

    /// Get global log config
    pub async fn get_global_log_config(&self) -> Option<LogConfig> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetGlobalLogConfig { reply: reply_tx })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Check if a service is running
    pub async fn is_service_running(&self, service_name: &str) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::IsServiceRunning {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or(false)
    }

    /// Check if config is initialized
    pub async fn is_config_initialized(&self) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::IsConfigInitialized { reply: reply_tx })
            .await;
        reply_rx.await.unwrap_or(false)
    }

    /// Check if service is initialized
    pub async fn is_service_initialized(&self, service_name: &str) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::IsServiceInitialized {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or(false)
    }

    /// Get service state (clone)
    pub async fn get_service_state(&self, service_name: &str) -> Option<ServiceState> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetServiceState {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await;
        reply_rx.await.ok().flatten()
    }

    /// Check if all services are stopped
    pub async fn all_services_stopped(&self) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::AllServicesStopped { reply: reply_tx })
            .await;
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
    pub async fn record_process_exit(&self, service_name: &str, exit_code: Option<i32>) -> Result<()> {
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
        let _ = self
            .tx
            .send(ConfigCommand::RemoveProcessHandle {
                service_name: service_name.to_string(),
                reply: reply_tx,
            })
            .await;
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
