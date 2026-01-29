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

use crate::config::{KeplerConfig, LogConfig, ServiceConfig, SysEnvPolicy};
use crate::env::build_service_env;
use crate::errors::{DaemonError, Result};
use crate::logs::SharedLogBuffer;
use crate::persistence::{ConfigPersistence, ExpandedConfigSnapshot};
use crate::state::{
    PersistedConfigState, PersistedServiceState, ProcessHandle, ServiceState, ServiceStatus,
};
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
    GetLogsBounded {
        service: Option<String>,
        lines: usize,
        max_bytes: Option<usize>,
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    GetLogsPaginated {
        service: Option<String>,
        offset: usize,
        limit: usize,
        reply: oneshot::Sender<(Vec<LogEntry>, usize)>,
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
    GetGlobalSysEnv {
        reply: oneshot::Sender<Option<SysEnvPolicy>>,
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

    // Persistence
    persistence: ConfigPersistence,
    /// Whether the config snapshot has been taken (happens on first service start)
    snapshot_taken: bool,
    /// Whether this config was restored from a persisted snapshot
    restored_from_snapshot: bool,

    rx: mpsc::Receiver<ConfigCommand>,
}

impl ConfigActor {
    /// Create a new config actor by loading a config file.
    ///
    /// This method implements Docker-like immutable config persistence:
    /// - If an expanded config snapshot exists, load from it (don't re-expand env vars)
    /// - If no snapshot exists, parse fresh from source (snapshot taken on first start)
    pub fn create(config_path: PathBuf) -> Result<(ConfigActorHandle, Self)> {
        // Canonicalize path first
        let canonical_path = std::fs::canonicalize(&config_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(config_path.clone())
            } else {
                DaemonError::Io(e)
            }
        })?;

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

        // Create persistence layer
        let persistence = ConfigPersistence::new(state_dir.clone());

        // Save source path for discovery on daemon restart
        let _ = persistence.save_source_path(&canonical_path);

        // Check if we have an existing expanded config snapshot
        let (config, config_dir, services, initialized, snapshot_taken, restored_from_snapshot) =
            if let Ok(Some(snapshot)) = persistence.load_expanded_config() {
                info!(
                    "Restoring config from snapshot (taken at {})",
                    snapshot.snapshot_time
                );

                // Load persisted state if available
                let persisted_state = persistence.load_state().ok().flatten();

                // Restore service states from snapshot + persisted state
                let services = snapshot
                    .config
                    .services
                    .iter()
                    .map(|(name, _service_config)| {
                        let computed_env = snapshot
                            .service_envs
                            .get(name)
                            .cloned()
                            .unwrap_or_default();
                        let working_dir = snapshot
                            .service_working_dirs
                            .get(name)
                            .cloned()
                            .unwrap_or_else(|| snapshot.config_dir.clone());

                        // Restore runtime state if available
                        let state = if let Some(ref ps) = persisted_state {
                            if let Some(ss) = ps.services.get(name) {
                                ss.to_service_state(computed_env, working_dir)
                            } else {
                                ServiceState {
                                    computed_env,
                                    working_dir,
                                    ..Default::default()
                                }
                            }
                        } else {
                            ServiceState {
                                computed_env,
                                working_dir,
                                ..Default::default()
                            }
                        };

                        (name.clone(), state)
                    })
                    .collect();

                let initialized = persisted_state
                    .as_ref()
                    .map(|ps| ps.config_initialized)
                    .unwrap_or(false);

                (
                    snapshot.config,
                    snapshot.config_dir,
                    services,
                    initialized,
                    true,  // snapshot was already taken
                    true,  // restored from snapshot
                )
            } else {
                // No snapshot - parse fresh from source
                info!("Loading config fresh from source (no snapshot)");

                // Read original file contents
                let contents = std::fs::read(&canonical_path)?;

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
                    std::fs::write(&copied_config_path, &contents)
                        .map_err(DaemonError::ConfigCopy)?;
                }

                // Parse config from the secure copy
                let config = KeplerConfig::load(&copied_config_path)?;

                // Get config directory
                let config_dir = canonical_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));

                // Copy env_files to state directory before building service envs
                // This is needed so build_service_env can find them in the state directory
                let _ = persistence.copy_env_files(&config.services, &config_dir);

                // Initialize service states with pre-computed environment and working directory
                let services = config
                    .services
                    .iter()
                    .map(|(name, service_config)| {
                        let working_dir = service_config
                            .working_dir
                            .clone()
                            .unwrap_or_else(|| config_dir.clone());

                        let computed_env =
                            build_service_env(service_config, name, &state_dir).unwrap_or_default();

                        let state = ServiceState {
                            computed_env,
                            working_dir,
                            ..Default::default()
                        };
                        (name.clone(), state)
                    })
                    .collect();

                (
                    config,
                    config_dir,
                    services,
                    false, // not initialized
                    false, // snapshot not yet taken
                    false, // not restored from snapshot
                )
            };

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
            initialized,
            processes: HashMap::new(),
            watchers: HashMap::new(),
            health_checks: HashMap::new(),
            persistence,
            snapshot_taken,
            restored_from_snapshot,
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
            ConfigCommand::GetLogsBounded {
                service,
                lines,
                max_bytes,
                reply,
            } => {
                let result = self.get_logs_bounded(service.as_deref(), lines, max_bytes);
                let _ = reply.send(result);
            }
            ConfigCommand::GetLogsPaginated {
                service,
                offset,
                limit,
                reply,
            } => {
                let result = self.get_logs_paginated(service.as_deref(), offset, limit);
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
                let _ = reply.send(self.config.global_logs().cloned());
            }
            ConfigCommand::GetGlobalSysEnv { reply } => {
                let _ = reply.send(self.config.global_sys_env().cloned());
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
                // Save state before shutdown
                let _ = self.save_state();
                let _ = reply.send(());
                return true;
            }

            // === Persistence Commands ===
            ConfigCommand::TakeSnapshotIfNeeded { reply } => {
                let result = self.take_snapshot_if_needed();
                let _ = reply.send(result);
            }
            ConfigCommand::ClearSnapshot { reply } => {
                let result = self.persistence.clear_snapshot();
                if result.is_ok() {
                    self.snapshot_taken = false;
                    self.restored_from_snapshot = false;
                }
                let _ = reply.send(result);
            }
            ConfigCommand::IsRestoredFromSnapshot { reply } => {
                let _ = reply.send(self.restored_from_snapshot);
            }
        }
        false
    }

    /// Take a snapshot of the expanded config if not already taken.
    /// Returns Ok(true) if snapshot was taken, Ok(false) if already existed.
    fn take_snapshot_if_needed(&mut self) -> Result<bool> {
        if self.snapshot_taken {
            return Ok(false);
        }

        info!("Taking config snapshot");

        // Copy env_files to state directory before taking snapshot
        self.persistence
            .copy_env_files(&self.config.services, &self.config_dir)?;

        // Build the snapshot
        let service_envs: HashMap<String, HashMap<String, String>> = self
            .services
            .iter()
            .map(|(name, state)| (name.clone(), state.computed_env.clone()))
            .collect();

        let service_working_dirs: HashMap<String, PathBuf> = self
            .services
            .iter()
            .map(|(name, state)| (name.clone(), state.working_dir.clone()))
            .collect();

        let snapshot = ExpandedConfigSnapshot {
            config: self.config.clone(),
            service_envs,
            service_working_dirs,
            config_dir: self.config_dir.clone(),
            snapshot_time: chrono::Utc::now().timestamp(),
        };

        // Save the snapshot
        self.persistence.save_expanded_config(&snapshot)?;
        self.snapshot_taken = true;

        // Also save current state
        self.save_state()?;

        Ok(true)
    }

    /// Save the current service state to disk.
    fn save_state(&self) -> Result<()> {
        let services: HashMap<String, PersistedServiceState> = self
            .services
            .iter()
            .map(|(name, state)| (name.clone(), PersistedServiceState::from(state)))
            .collect();

        let state = PersistedConfigState {
            services,
            config_initialized: self.initialized,
            snapshot_time: chrono::Utc::now().timestamp(),
        };

        self.persistence.save_state(&state)
    }

    /// Build a ServiceContext for a service (single round-trip)
    fn build_service_context(&self, service_name: &str) -> Option<ServiceContext> {
        let service_config = self.config.services.get(service_name)?.clone();
        let service_state = self.services.get(service_name)?;

        Some(ServiceContext {
            service_config,
            config_dir: self.config_dir.clone(),
            logs: self.logs.clone(),
            global_log_config: self.config.global_logs().cloned(),
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

    fn get_logs_bounded(
        &self,
        service: Option<&str>,
        lines: usize,
        max_bytes: Option<usize>,
    ) -> Vec<LogEntry> {
        self.logs
            .tail_bounded(lines, service, max_bytes)
            .into_iter()
            .map(|l| l.into())
            .collect()
    }

    fn get_logs_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogEntry>, usize) {
        let (logs, total) = self.logs.get_paginated(service, offset, limit);
        (logs.into_iter().map(|l| l.into()).collect(), total)
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

        // Re-copy env_files to state directory (they may have changed)
        let _ = self
            .persistence
            .copy_env_files(&new_config.services, &self.config_dir);

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
                    build_service_env(service_config, name, &self.state_dir).unwrap_or_default();

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

    /// Get logs with bounded reading (prevents OOM with large log files)
    pub async fn get_logs_bounded(
        &self,
        service: Option<String>,
        lines: usize,
        max_bytes: Option<usize>,
    ) -> Vec<LogEntry> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetLogsBounded {
                service,
                lines,
                max_bytes,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or_default()
    }

    /// Get logs with true pagination (reads efficiently from disk with offset/limit)
    pub async fn get_logs_paginated(
        &self,
        service: Option<String>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogEntry>, usize) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetLogsPaginated {
                service,
                offset,
                limit,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap_or_else(|_| (Vec::new(), 0))
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

    /// Get global sys_env policy
    pub async fn get_global_sys_env(&self) -> Option<SysEnvPolicy> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .tx
            .send(ConfigCommand::GetGlobalSysEnv { reply: reply_tx })
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
        let _ = self
            .tx
            .send(ConfigCommand::IsRestoredFromSnapshot { reply: reply_tx })
            .await;
        reply_rx.await.unwrap_or(false)
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
