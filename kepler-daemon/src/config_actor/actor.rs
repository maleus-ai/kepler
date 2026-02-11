//! ConfigActor - per-config actor implementation
//!
//! This module contains the ConfigActor struct and its implementation,
//! which manages state for a single loaded configuration.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::{resolve_log_buffer_size, resolve_log_max_size, KeplerConfig};
use crate::env::build_service_env;
use crate::errors::{DaemonError, Result};
use crate::events::{service_event_channel, ServiceEventMessage, ServiceEventSender};
use crate::logs::{LogReader, LogWriterConfig, DEFAULT_BUFFER_SIZE};
use crate::persistence::{ConfigPersistence, ExpandedConfigSnapshot};
use crate::state::{
    PersistedConfigState, PersistedServiceState, ProcessHandle, ServiceState, ServiceStatus,
};
use kepler_protocol::protocol::{LogEntry, LogMode, ServiceInfo};

use super::command::ConfigCommand;
use super::context::{HealthCheckUpdate, ServiceContext, ServiceStatusChange, TaskHandleType};
use super::handle::ConfigActorHandle;

/// Per-config actor state
pub struct ConfigActor {
    config_path: PathBuf,
    config_hash: String,
    config: KeplerConfig,
    config_dir: PathBuf,
    services: HashMap<String, ServiceState>,
    log_config: LogWriterConfig,
    initialized: bool,

    // Process and task handles (moved from DaemonState)
    processes: HashMap<String, ProcessHandle>,
    watchers: HashMap<String, JoinHandle<()>>,
    health_checks: HashMap<String, JoinHandle<()>>,

    // Event channels per service (service_name -> sender)
    event_senders: HashMap<String, ServiceEventSender>,

    // Persistence
    persistence: ConfigPersistence,
    /// Whether the config snapshot has been taken (happens on first service start)
    snapshot_taken: bool,
    /// Whether this config was restored from a persisted snapshot
    restored_from_snapshot: bool,
    /// Whether an event handler has been spawned for this config
    event_handler_spawned: bool,
    /// System environment variables captured from the CLI
    sys_env: HashMap<String, String>,

    /// Subscriber registry for service status changes (used by Subscribe handler)
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<ServiceStatusChange>>>>,

    rx: mpsc::Receiver<ConfigCommand>,
}

impl ConfigActor {
    /// Create a new config actor by loading a config file.
    ///
    /// This method implements Docker-like immutable config persistence:
    /// - If an expanded config snapshot exists, load from it (don't re-expand env vars)
    /// - If no snapshot exists, parse fresh from source (snapshot taken on first start)
    ///
    /// The `sys_env` parameter provides system environment variables captured from the CLI.
    /// If None and no snapshot exists, defaults to an empty env with a warning.
    pub fn create(
        config_path: PathBuf,
        sys_env: Option<HashMap<String, String>>,
    ) -> Result<(ConfigActorHandle, Self)> {
        // Canonicalize path first
        let canonical_path = std::fs::canonicalize(&config_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(config_path.clone())
            } else {
                DaemonError::Internal(format!("Failed to canonicalize '{}': {}", config_path.display(), e))
            }
        })?;

        // Compute hash from the canonical path
        let hash = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(canonical_path.to_string_lossy().as_bytes()))
        };

        // Create state directory for this config
        let state_dir = crate::global_state_dir()?.join("configs").join(&hash);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o770)
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
        let (config, config_dir, services, initialized, snapshot_taken, restored_from_snapshot, resolved_sys_env) =
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
                    .keys()
                    .map(|name| {
                        let computed_env =
                            snapshot.service_envs.get(name).cloned().unwrap_or_default();
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

                let restored_sys_env = snapshot.sys_env;

                // Recompute wait values (safety net for old snapshots missing resolved wait)
                let mut config = snapshot.config;
                crate::deps::resolve_effective_wait(&mut config.services)?;

                (
                    config,
                    snapshot.config_dir,
                    services,
                    initialized,
                    true,  // snapshot was already taken
                    true,  // restored from snapshot
                    restored_sys_env,
                )
            } else {
                // No snapshot - parse fresh from source
                info!("Loading config fresh from source (no snapshot)");

                let resolved_sys_env = match sys_env {
                    Some(env) => env,
                    None => {
                        warn!("No sys_env provided and no snapshot exists; using empty environment");
                        HashMap::new()
                    }
                };

                // Read original file contents
                let contents = std::fs::read(&canonical_path).map_err(|e| DaemonError::Internal(format!("Failed to read '{}': {}", canonical_path.display(), e)))?;

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

                // Parse config from the secure copy (baking with sys_env)
                let config = KeplerConfig::load(&copied_config_path, &resolved_sys_env)?;

                // Get config directory
                let config_dir = canonical_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));

                // Copy env_files to state directory before building service envs
                // This is needed so build_service_env can find them in the state directory
                let _ = persistence.copy_env_files(&config.services, &config_dir);

                // Initialize service states with pre-computed environment and working directory
                // Note: The config is already "baked" with sys_env applied (if inherit policy)
                let services = config
                    .services
                    .iter()
                    .map(|(name, service_config)| {
                        let working_dir = service_config
                            .working_dir
                            .as_ref()
                            .map(|wd| config_dir.join(wd))
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
                    resolved_sys_env,
                )
            };

        // Create log config (writers are created per-task, no shared state)
        let logs_dir = state_dir.join("logs");
        let log_config = Self::create_log_config(&logs_dir, &config);

        // Create channels
        let (tx, rx) = mpsc::channel(256);
        let subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<ServiceStatusChange>>>> =
            Arc::new(Mutex::new(Vec::new()));

        let handle = ConfigActorHandle::new(canonical_path.clone(), hash.clone(), tx, subscribers.clone());

        let actor = ConfigActor {
            config_path: canonical_path,
            config_hash: hash,
            config,
            config_dir,
            services,
            log_config,
            initialized,
            processes: HashMap::new(),
            watchers: HashMap::new(),
            health_checks: HashMap::new(),
            event_senders: HashMap::new(),
            persistence,
            snapshot_taken,
            restored_from_snapshot,
            event_handler_spawned: false,
            sys_env: resolved_sys_env,
            subscribers,
            rx,
        };

        Ok((handle, actor))
    }

    /// Create a log config with size and buffer settings from config.
    /// Uses truncation if max_size is specified, otherwise unbounded.
    fn create_log_config(logs_dir: &Path, config: &KeplerConfig) -> LogWriterConfig {
        let global_logs = config.global_logs();

        // Get max size from global config (None = unbounded)
        let max_size = resolve_log_max_size(None, global_logs);

        // Get buffer size (default to 8KB for better performance)
        let buffer_size = resolve_log_buffer_size(None, global_logs, DEFAULT_BUFFER_SIZE);

        LogWriterConfig::with_options(logs_dir.to_path_buf(), max_size, buffer_size)
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
                no_hooks,
                reply,
            } => {
                let result = self.get_logs(service.as_deref(), lines, no_hooks);
                let _ = reply.send(result);
            }
            ConfigCommand::GetLogsBounded {
                service,
                lines,
                max_bytes,
                no_hooks,
                reply,
            } => {
                let result = self.get_logs_bounded(service.as_deref(), lines, max_bytes, no_hooks);
                let _ = reply.send(result);
            }
            ConfigCommand::GetLogsWithMode {
                service,
                lines,
                max_bytes,
                mode,
                no_hooks,
                reply,
            } => {
                let result = self.get_logs_with_mode(service.as_deref(), lines, max_bytes, mode, no_hooks);
                let _ = reply.send(result);
            }
            ConfigCommand::GetLogsPaginated {
                service,
                offset,
                limit,
                no_hooks,
                reply,
            } => {
                let result = self.get_logs_paginated(service.as_deref(), offset, limit, no_hooks);
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
            ConfigCommand::GetLogConfig { reply } => {
                let _ = reply.send(self.log_config.clone());
            }
            ConfigCommand::GetGlobalLogConfig { reply } => {
                let _ = reply.send(self.config.global_logs().cloned());
            }
            ConfigCommand::GetGlobalSysEnv { reply } => {
                let _ = reply.send(self.config.global_sys_env().cloned());
            }
            ConfigCommand::GetSysEnv { reply } => {
                let _ = reply.send(self.sys_env.clone());
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
            ConfigCommand::GetRunningServices { reply } => {
                let running: Vec<String> = self
                    .services
                    .iter()
                    .filter(|(_, s)| s.status.is_running())
                    .map(|(name, _)| name.clone())
                    .collect();
                let _ = reply.send(running);
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
            ConfigCommand::ClaimServiceStart { service_name, reply } => {
                let claimed = self.services.get(&service_name)
                    .map(|s| !s.status.is_active())
                    .unwrap_or(false);
                if claimed {
                    let _ = self.set_service_status(&service_name, ServiceStatus::Starting);
                    let _ = self.save_state();
                }
                let _ = reply.send(claimed);
            }
            ConfigCommand::SetServiceStatus {
                service_name,
                status,
                reply,
            } => {
                let result = self.set_service_status(&service_name, status);
                if result.is_ok() {
                    let _ = self.save_state();
                }
                let _ = reply.send(result);
            }
            ConfigCommand::SetServicePid {
                service_name,
                pid,
                started_at,
                reply,
            } => {
                let result = self.set_service_pid(&service_name, pid, started_at);
                if result.is_ok() {
                    let _ = self.save_state();
                }
                let _ = reply.send(result);
            }
            ConfigCommand::RecordProcessExit {
                service_name,
                exit_code,
                signal,
                reply,
            } => {
                let result = self.record_process_exit(&service_name, exit_code, signal);
                if result.is_ok() {
                    let _ = self.save_state();
                }
                let _ = reply.send(result);
            }
            ConfigCommand::UpdateHealthCheck {
                service_name,
                passed,
                retries,
                reply,
            } => {
                let result = self.update_health_check(&service_name, passed, retries);
                if let Ok(ref update) = result
                    && update.previous_status != update.new_status
                {
                    let _ = self.save_state();
                }
                let _ = reply.send(result);
            }
            ConfigCommand::MarkConfigInitialized { reply } => {
                self.initialized = true;
                let _ = self.save_state();
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
                if result.is_ok() {
                    let _ = self.save_state();
                }
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
                if result.is_ok() {
                    let _ = self.save_state();
                }
                let _ = reply.send(result);
            }
            ConfigCommand::ClearServiceLogs { service_name } => {
                let reader = LogReader::new(self.log_config.logs_dir.clone());
                reader.clear_service(&service_name);
            }
            ConfigCommand::ClearServiceLogsPrefix { prefix } => {
                let reader = LogReader::new(self.log_config.logs_dir.clone());
                reader.clear_service_prefix(&prefix);
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

            // === Event Channel Commands ===
            ConfigCommand::CreateEventChannel {
                service_name,
                reply,
            } => {
                let (tx, rx) = service_event_channel();
                self.event_senders.insert(service_name, tx);
                let _ = reply.send(rx);
            }
            ConfigCommand::RemoveEventChannel { service_name } => {
                self.event_senders.remove(&service_name);
            }
            ConfigCommand::EmitEvent {
                service_name,
                event,
            } => {
                if let Some(sender) = self.event_senders.get(&service_name) {
                    let msg = ServiceEventMessage::new(event);
                    if let Err(e) = sender.try_send(msg) {
                        match e {
                            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                warn!("Event channel full for service '{}', dropping event", service_name);
                            }
                            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                warn!("Event channel closed for service '{}'", service_name);
                            }
                        }
                    }
                }
            }
            ConfigCommand::GetAllEventReceivers { reply } => {
                // Create new channels for all services and return the receivers
                let mut receivers = Vec::new();
                for service_name in self.services.keys() {
                    if !self.event_senders.contains_key(service_name) {
                        let (tx, rx) = service_event_channel();
                        self.event_senders.insert(service_name.clone(), tx);
                        receivers.push((service_name.clone(), rx));
                    }
                }
                let _ = reply.send(receivers);
            }
            ConfigCommand::HasEventHandler { reply } => {
                let _ = reply.send(self.event_handler_spawned);
            }
            ConfigCommand::SetEventHandlerSpawned => {
                self.event_handler_spawned = true;
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
            sys_env: self.sys_env.clone(),
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
            log_config: self.log_config.clone(),
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

    fn get_logs(&self, service: Option<&str>, lines: usize, no_hooks: bool) -> Vec<LogEntry> {
        let reader = LogReader::new(self.log_config.logs_dir.clone());
        reader
            .tail(lines, service, no_hooks)
            .into_iter()
            .map(|l| l.into())
            .collect()
    }

    fn get_logs_bounded(
        &self,
        service: Option<&str>,
        lines: usize,
        max_bytes: Option<usize>,
        no_hooks: bool,
    ) -> Vec<LogEntry> {
        let reader = LogReader::new(self.log_config.logs_dir.clone());
        reader
            .tail_bounded(lines, service, max_bytes, no_hooks)
            .into_iter()
            .map(|l| l.into())
            .collect()
    }

    fn get_logs_with_mode(
        &self,
        service: Option<&str>,
        lines: usize,
        max_bytes: Option<usize>,
        mode: LogMode,
        no_hooks: bool,
    ) -> Vec<LogEntry> {
        let reader = LogReader::new(self.log_config.logs_dir.clone());
        match mode {
            LogMode::Head => reader
                .head(lines, service, no_hooks)
                .into_iter()
                .map(|l| l.into())
                .collect(),
            LogMode::Tail => reader
                .tail_bounded(lines, service, max_bytes, no_hooks)
                .into_iter()
                .map(|l| l.into())
                .collect(),
            LogMode::All => reader.iter(service, no_hooks).take(lines).map(|l| l.into()).collect(),
        }
    }

    fn get_logs_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
        no_hooks: bool,
    ) -> (Vec<LogEntry>, bool) {
        let reader = LogReader::new(self.log_config.logs_dir.clone());
        let (logs, has_more) = reader.get_paginated(service, offset, limit, no_hooks);
        (logs.into_iter().map(|l| l.into()).collect(), has_more)
    }

    fn set_service_status(&mut self, service_name: &str, status: ServiceStatus) -> Result<()> {
        let service_state = self
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        if status == ServiceStatus::Starting || status == ServiceStatus::Running {
            service_state.was_healthy = false;
            service_state.health_check_failures = 0;
            service_state.stopped_at = None;
            service_state.signal = None;
            service_state.exit_code = None;
        }
        if matches!(status, ServiceStatus::Stopped | ServiceStatus::Exited | ServiceStatus::Failed | ServiceStatus::Killed) {
            service_state.pid = None;
            service_state.started_at = None;
            service_state.stopped_at = Some(Utc::now());
        }
        service_state.status = status;

        // Notify subscribers of status change
        self.notify_subscribers(ServiceStatusChange {
            service: service_name.to_string(),
            status,
        });

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

    fn record_process_exit(&mut self, service_name: &str, exit_code: Option<i32>, signal: Option<i32>) -> Result<()> {
        // Remove process handle
        self.processes.remove(service_name);

        let service_state = self
            .services
            .get_mut(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.to_string()))?;
        service_state.exit_code = exit_code;
        service_state.signal = signal;
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
                service_state.was_healthy = true;
            }
        } else {
            service_state.health_check_failures += 1;
            if service_state.health_check_failures >= retries
                && service_state.status != ServiceStatus::Unhealthy
            {
                service_state.status = ServiceStatus::Unhealthy;
            }
        }

        let new_status = service_state.status;
        let failures = service_state.health_check_failures;

        // Notify subscribers of status change (only if status actually changed)
        if previous_status != new_status {
            self.notify_subscribers(ServiceStatusChange {
                service: service_name.to_string(),
                status: new_status,
            });
        }

        Ok(HealthCheckUpdate {
            previous_status,
            new_status,
            failures,
        })
    }

    /// Notify all subscribers of a status change, auto-pruning dead senders.
    fn notify_subscribers(&self, change: ServiceStatusChange) {
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|tx| tx.send(change.clone()).is_ok());
    }

    /// Cleanup all resources when shutting down
    fn cleanup(&mut self) {
        // Save state as safety net (in case shutdown wasn't triggered via command)
        if let Err(e) = self.save_state() {
            warn!("Failed to save state during cleanup: {}", e);
        }

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
