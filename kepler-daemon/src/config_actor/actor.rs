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
use tracing::{debug, info, warn};

use crate::config::{resolve_log_buffer_size, resolve_log_max_size, DependencyConfig, KeplerConfig, ServiceConfig};
use crate::deps::{get_start_order, is_condition_met, is_failure_handled};
use crate::lua_eval::{ConditionResult, EvalContext};
use crate::errors::{DaemonError, Result};
use crate::events::{service_event_channel, ServiceEventMessage, ServiceEventSender};
use crate::logs::{LogReader, LogWriterConfig, DEFAULT_BUFFER_SIZE};
use crate::persistence::{ConfigPersistence, ExpandedConfigSnapshot};
use crate::state::{
    PersistedConfigState, PersistedServiceState, ProcessHandle, ServiceState, ServiceStatus,
};
use kepler_protocol::protocol::{LogEntry, LogMode, ServiceInfo};

use super::command::ConfigCommand;
use super::context::{ConfigEvent, DiagnosticCounts, HealthCheckUpdate, ServiceContext, ServiceStatusChange, TaskHandleType};
use super::handle::ConfigActorHandle;

use crate::config::DynamicExpr;

/// Request sent to the dedicated Lua worker thread.
struct LuaEvalRequest {
    expr: DynamicExpr,
    context: EvalContext,
    reply: tokio::sync::oneshot::Sender<Result<ConditionResult>>,
}

/// Per-config actor state
pub struct ConfigActor {
    config_path: PathBuf,
    config_hash: String,
    config: KeplerConfig,
    config_dir: PathBuf,
    services: HashMap<String, ServiceState>,
    log_config: LogWriterConfig,
    initialized: bool,

    /// Cached resolved (expanded) ServiceConfigs per service.
    /// Populated after first service start via `StoreResolvedConfig`.
    resolved_configs: HashMap<String, ServiceConfig>,

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
    /// UID of the CLI user who loaded this config
    owner_uid: Option<u32>,
    /// GID of the CLI user who loaded this config
    owner_gid: Option<u32>,

    /// Subscriber registry for config events (used by Subscribe handler)
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<ConfigEvent>>>>,

    /// Per-service dependency watchers.
    /// Maps service_name -> list of channels to notify when that service's state changes.
    dep_watchers: Arc<Mutex<HashMap<String, Vec<mpsc::UnboundedSender<ServiceStatusChange>>>>>,

    /// Channel to the dedicated Lua worker thread (lazily spawned)
    lua_eval_tx: Option<mpsc::UnboundedSender<LuaEvalRequest>>,

    /// JoinHandle for the event handler task (for cleanup)
    event_handler_task: Option<JoinHandle<()>>,
    /// JoinHandles for event forwarder tasks (for cleanup)
    event_forwarder_tasks: Vec<JoinHandle<()>>,

    /// Whether the Ready signal has been emitted for the current cycle
    last_ready: bool,
    /// Whether the Quiescent signal has been emitted for the current cycle
    last_quiescent: bool,
    /// Startup fence: when true, Ready/Quiescent signals are suppressed.
    /// Set to true before marking services as Waiting, set to false after all are marked.
    startup_in_progress: bool,
    /// Cached topological order for quiescence computation (dep graph is immutable)
    cached_topo_order: Option<Vec<String>>,

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
    ///
    /// The `config_owner` parameter provides the UID/GID of the CLI user that loaded
    /// this config. If provided and uid != 0, services without an explicit `user:`
    /// field will default to running as this user instead of root.
    /// This is only applied on fresh loads — snapshot restoration already has user baked.
    pub fn create(
        config_path: PathBuf,
        sys_env: Option<HashMap<String, String>>,
        config_owner: Option<(u32, u32)>,
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
        let (config, config_dir, services, initialized, snapshot_taken, restored_from_snapshot, resolved_sys_env, owner_uid, owner_gid) =
            if let Ok(Some(snapshot)) = persistence.load_expanded_config() {
                info!(
                    "Restoring config from snapshot (taken at {})",
                    snapshot.snapshot_time
                );

                // Load persisted state if available
                let persisted_state = persistence.load_state().ok().flatten();

                // Restore service states from snapshot + persisted state
                // computed_env and working_dir start empty — they are built lazily
                // at service start time via ${{}}$ expansion.
                let services = snapshot
                    .config
                    .services
                    .keys()
                    .map(|name| {
                        let computed_env = HashMap::new();
                        let working_dir = snapshot.config_dir.clone();

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
                let owner_uid = snapshot.owner_uid;
                let owner_gid = snapshot.owner_gid;

                let config = snapshot.config;

                (
                    config,
                    snapshot.config_dir,
                    services,
                    initialized,
                    true,  // snapshot was already taken
                    true,  // restored from snapshot
                    restored_sys_env,
                    owner_uid,
                    owner_gid,
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
                // Map error to show the original config path, not the internal copy
                let mut config = KeplerConfig::load(&copied_config_path, &resolved_sys_env)
                    .map_err(|e| match e {
                        DaemonError::ConfigParse { source, .. } => DaemonError::ConfigParse {
                            path: canonical_path.clone(),
                            source,
                        },
                        other => other,
                    })?;

                // Bake default user into services based on CLI user
                if let Some((uid, gid)) = config_owner {
                    config.resolve_default_user(uid, gid);
                }

                // Get config directory
                let config_dir = canonical_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));

                // Copy env_files to state directory for snapshot self-containment
                let _ = persistence.copy_env_files(&config.services, &config_dir);

                // Initialize service states with empty computed_env and working_dir.
                // These are built lazily at service start time via ${{}}$ expansion.
                let services = config
                    .services
                    .keys()
                    .map(|name| {
                        let state = ServiceState {
                            computed_env: HashMap::new(),
                            working_dir: config_dir.clone(),
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
                    config_owner.map(|(uid, _)| uid),
                    config_owner.map(|(_, gid)| gid),
                )
            };

        // Create log config (writers are created per-task, no shared state)
        let logs_dir = state_dir.join("logs");
        let log_config = Self::create_log_config(&logs_dir, &config);

        // Create channels
        let (tx, rx) = mpsc::channel(256);
        let subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<ConfigEvent>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let dep_watchers: Arc<Mutex<HashMap<String, Vec<mpsc::UnboundedSender<ServiceStatusChange>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let handle = ConfigActorHandle::new(canonical_path.clone(), hash.clone(), tx, subscribers.clone(), dep_watchers.clone(), owner_uid, owner_gid);

        let actor = ConfigActor {
            config_path: canonical_path,
            config_hash: hash,
            config,
            config_dir,
            services,
            log_config,
            initialized,
            resolved_configs: HashMap::new(),
            processes: HashMap::new(),
            watchers: HashMap::new(),
            health_checks: HashMap::new(),
            event_senders: HashMap::new(),
            persistence,
            snapshot_taken,
            restored_from_snapshot,
            event_handler_spawned: false,
            sys_env: resolved_sys_env,
            owner_uid,
            owner_gid,
            subscribers,
            dep_watchers,
            lua_eval_tx: None,
            event_handler_task: None,
            event_forwarder_tasks: Vec::new(),
            last_ready: false,
            last_quiescent: false,
            startup_in_progress: false,
            cached_topo_order: None,
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
            // Handle EvalIfCondition directly — forwards to the worker, never blocks
            if let ConfigCommand::EvalIfCondition { expr, context, reply } = cmd {
                self.handle_lua_eval(*expr, *context, reply);
                continue;
            }
            if self.process_command(cmd) {
                break;
            }
        }
        info!("ConfigActor stopped for {:?}", self.config_path);
        self.cleanup();
    }

    /// Forward a Lua eval request to the dedicated worker thread.
    /// Lazily spawns the worker on first call.
    fn handle_lua_eval(
        &mut self,
        expr: DynamicExpr,
        context: EvalContext,
        reply: tokio::sync::oneshot::Sender<Result<ConditionResult>>,
    ) {
        if self.lua_eval_tx.is_none() {
            let (tx, mut rx) = mpsc::unbounded_channel::<LuaEvalRequest>();
            let config_lua = self.config.lua.clone();
            tokio::task::spawn_blocking(move || {
                use crate::lua_eval::LuaEvaluator;

                let evaluator = match LuaEvaluator::new() {
                    Ok(e) => e,
                    Err(e) => {
                        // Drain and fail all pending requests
                        let err_msg = format!("Failed to create Lua evaluator: {}", e);
                        let _ = rx; // drop rx after draining
                        // Can't drain an already-moved rx, so just log the error.
                        // The channel will be dropped, causing all future sends to fail.
                        tracing::error!("{}", err_msg);
                        return;
                    }
                };

                if let Some(ref code) = config_lua
                    && let Err(e) = evaluator.load_inline(code) {
                        tracing::error!("Failed to load Lua code: {}", e);
                        // Drain remaining requests with error
                        while let Ok(req) = rx.try_recv() {
                            let _ = req.reply.send(Err(DaemonError::Internal(
                                format!("Lua initialization failed: {}", e),
                            )));
                        }
                        return;
                    }

                while let Some(req) = rx.blocking_recv() {
                    // Set 10s interrupt watchdog for runtime conditions
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    evaluator.set_interrupt(move |_| {
                        if std::time::Instant::now() > deadline {
                            Err(mlua::Error::RuntimeError(
                                "Lua condition timed out (10s limit)".into(),
                            ))
                        } else {
                            Ok(mlua::VmState::Continue)
                        }
                    });
                    let result = evaluator.eval_condition_expr(&req.expr, &req.context)
                        .map_err(|e| DaemonError::Internal(format!("Lua eval failed: {}", e)));
                    evaluator.remove_interrupt();
                    let _ = req.reply.send(result);
                }

            });
            self.lua_eval_tx = Some(tx);
        }

        if let Some(ref tx) = self.lua_eval_tx {
            let _ = tx.send(LuaEvalRequest { expr, context, reply });
        }
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
            ConfigCommand::GetStateDir { reply } => {
                let _ = reply.send(self.persistence.state_dir().to_path_buf());
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

            ConfigCommand::RecheckReadyQuiescent => {
                // Reset flags and re-check — ensures new subscribers get the signal
                // if the system is already in a ready/quiescent state.
                self.last_ready = false;
                self.last_quiescent = false;
                self.check_and_notify_ready();
                self.check_and_notify_quiescent();
            }
            ConfigCommand::SetStartupInProgress { in_progress, reply } => {
                self.startup_in_progress = in_progress;
                if !in_progress {
                    // Fence lifted — immediately check signals
                    self.check_and_notify_ready();
                    self.check_and_notify_quiescent();
                }
                let _ = reply.send(());
            }

            // === Mutation Commands ===
            ConfigCommand::ClaimServiceStart {
                service_name,
                reply,
            } => {
                let claimed = if let Some(state) = self.services.get(&service_name) {
                    match state.status {
                        ServiceStatus::Waiting => true, // Already Waiting (set by spawn-all)
                        s if !s.is_active() => {
                            // Terminal state — set to Waiting atomically
                            let _ = self.set_service_status(&service_name, ServiceStatus::Waiting);
                            true
                        }
                        _ => false, // Already active — skip
                    }
                } else {
                    false
                };
                let _ = reply.send(claimed);
            }
            ConfigCommand::SetServiceStatusWithReason {
                service_name,
                status,
                skip_reason,
                fail_reason,
                reply,
            } => {
                // Set reason(s) first, then status — all in one atomic operation
                if let Some(state) = self.services.get_mut(&service_name) {
                    if let Some(reason) = skip_reason {
                        state.skip_reason = Some(reason);
                    }
                    if let Some(reason) = fail_reason {
                        state.fail_reason = Some(reason);
                    }
                }
                let result = self.set_service_status(&service_name, status);
                if result.is_ok() {
                    let _ = self.save_state();
                }
                let _ = reply.send(result);
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
            ConfigCommand::SetSkipReason {
                service_name,
                reason,
            } => {
                if let Some(state) = self.services.get_mut(&service_name) {
                    state.skip_reason = Some(reason);
                }
            }
            ConfigCommand::SetFailReason {
                service_name,
                reason,
            } => {
                if let Some(state) = self.services.get_mut(&service_name) {
                    state.fail_reason = Some(reason);
                }
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
            ConfigCommand::StoreResolvedConfig {
                service_name,
                config,
                computed_env,
                working_dir,
                env_file_vars,
            } => {
                // Update service state with computed env, working dir, and env_file vars
                if let Some(state) = self.services.get_mut(&service_name) {
                    state.computed_env = computed_env;
                    state.working_dir = working_dir;
                    state.env_file_vars = env_file_vars;
                }
                // Cache the resolved config
                self.resolved_configs.insert(service_name, *config);
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
            ConfigCommand::TakeOutputTasks {
                service_name,
                reply,
            } => {
                let tasks = if let Some(ph) = self.processes.get_mut(&service_name) {
                    (ph.stdout_task.take(), ph.stderr_task.take())
                } else {
                    (None, None)
                };
                let _ = reply.send(tasks);
            }

            // === Task Handle Commands ===
            ConfigCommand::StoreTaskHandle {
                service_name,
                handle_type,
                handle,
            } => match handle_type {
                TaskHandleType::HealthCheck => {
                    if let Some(old) = self.health_checks.insert(service_name, handle) {
                        old.abort();
                    }
                }
                TaskHandleType::FileWatcher => {
                    if let Some(old) = self.watchers.insert(service_name, handle) {
                        old.abort();
                    }
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
                    self.resolved_configs.clear();
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
            ConfigCommand::StoreEventHandlerTasks { handler, forwarders } => {
                // Abort previous event handler/forwarders if any
                if let Some(old) = self.event_handler_task.take() {
                    old.abort();
                }
                for old in self.event_forwarder_tasks.drain(..) {
                    old.abort();
                }
                self.event_handler_task = Some(handler);
                self.event_forwarder_tasks = forwarders;
            }
            ConfigCommand::GetDiagnosticCounts { reply } => {
                let _ = reply.send(DiagnosticCounts {
                    process_handles: self.processes.len(),
                    health_checks: self.health_checks.len(),
                    file_watchers: self.watchers.len(),
                    event_senders: self.event_senders.len(),
                });
            }
            ConfigCommand::EvalIfCondition { .. } => {
                // Handled in run() before process_command — should never reach here
                unreachable!("EvalIfCondition handled in run() loop");
            }
            ConfigCommand::MergeSysEnv { overrides, reply } => {
                // Merge overrides into sys_env
                self.sys_env.extend(overrides);

                // Re-save snapshot with updated sys_env
                if self.snapshot_taken {
                    let snapshot = ExpandedConfigSnapshot {
                        config: self.config.clone(),
                        service_envs: Default::default(),
                        service_working_dirs: Default::default(),
                        config_dir: self.config_dir.clone(),
                        snapshot_time: chrono::Utc::now().timestamp(),
                        sys_env: self.sys_env.clone(),
                        owner_uid: self.owner_uid,
                        owner_gid: self.owner_gid,
                    };
                    if let Err(e) = self.persistence.save_expanded_config(&snapshot) {
                        warn!("Failed to re-save snapshot after MergeSysEnv: {}", e);
                    }
                }

                // Clear resolved config cache so services re-resolve with new env
                self.resolved_configs.clear();

                let _ = reply.send(());
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

        // Build the snapshot — stores only inputs to expansion (raw config + sys_env).
        // computed_env and working_dir are built lazily at each service start.
        let snapshot = ExpandedConfigSnapshot {
            config: self.config.clone(),
            service_envs: Default::default(),
            service_working_dirs: Default::default(),
            config_dir: self.config_dir.clone(),
            snapshot_time: chrono::Utc::now().timestamp(),
            sys_env: self.sys_env.clone(),
            owner_uid: self.owner_uid,
            owner_gid: self.owner_gid,
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
        let resolved_config = self.resolved_configs.get(service_name).cloned();
        let service_state = self.services.get(service_name)?;

        Some(ServiceContext {
            service_config,
            resolved_config,
            config_dir: self.config_dir.clone(),
            state_dir: self.persistence.state_dir().to_path_buf(),
            log_config: self.log_config.clone(),
            global_log_config: self.config.global_logs().cloned(),
            env: service_state.computed_env.clone(),
            working_dir: service_state.working_dir.clone(),
            env_file_vars: service_state.env_file_vars.clone(),
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

        if status == ServiceStatus::Waiting || status == ServiceStatus::Starting {
            // Clear stale reasons from previous cycles
            service_state.fail_reason = None;
            service_state.skip_reason = None;
        }
        if status == ServiceStatus::Starting || status == ServiceStatus::Running {
            service_state.was_healthy = false;
            service_state.health_check_failures = 0;
            service_state.stopped_at = None;
            service_state.signal = None;
            service_state.exit_code = None;
        }
        if matches!(status, ServiceStatus::Stopped | ServiceStatus::Exited | ServiceStatus::Failed | ServiceStatus::Killed | ServiceStatus::Skipped) {
            service_state.pid = None;
            service_state.started_at = None;
            service_state.stopped_at = Some(Utc::now());
        }
        service_state.status = status;

        let change = ServiceStatusChange {
            service: service_name.to_string(),
            status,
        };

        // Notify per-dep watchers (targeted — only dependents of this service)
        self.notify_dep_watchers(service_name, &change);

        // Notify global subscribers (CLI progress bars / Subscribe handler)
        self.notify_subscribers(ConfigEvent::StatusChange(change));

        // Check for unhandled failure.
        // Mirror the ServiceFailed dependency condition logic: Failed/Killed are always
        // failures; Exited with non-zero exit code is also a failure.
        let exit_code = self.services.get(service_name).and_then(|s| s.exit_code);
        let is_failure = match status {
            ServiceStatus::Failed | ServiceStatus::Killed => true,
            ServiceStatus::Exited => exit_code.is_some_and(|c| c != 0),
            _ => false,
        };

        if is_failure {
            let would_restart = self.resolved_configs.get(service_name)
                .map(|rc| rc.restart.should_restart_on_exit(exit_code))
                .or_else(|| self.config.services.get(service_name)
                    .map(|raw| raw.restart.as_static().cloned().unwrap_or_default()
                        .should_restart_on_exit(exit_code)))
                .unwrap_or(false);

            if !would_restart && !is_failure_handled(service_name, &self.config.services) {
                self.notify_subscribers(ConfigEvent::UnhandledFailure {
                    service: service_name.to_string(),
                    exit_code,
                });
            }
        }

        // Reset Ready signal when a service enters Starting (no longer at target state)
        if status == ServiceStatus::Starting {
            self.last_ready = false;
        }

        // Reset Quiescent signal when a service enters an active non-Waiting state
        if status.is_active() && status != ServiceStatus::Waiting {
            self.last_quiescent = false;
        }

        // Check Ready (can trigger on any state change)
        self.check_and_notify_ready();

        // Check Quiescent (on terminal transitions and Waiting services that may be settled)
        if !status.is_active() || status == ServiceStatus::Waiting {
            self.check_and_notify_quiescent();
        }

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
            let change = ServiceStatusChange {
                service: service_name.to_string(),
                status: new_status,
            };
            self.notify_dep_watchers(service_name, &change);
            self.notify_subscribers(ConfigEvent::StatusChange(change));

            // Health status changes affect readiness (Running→Healthy means ready)
            self.check_and_notify_ready();
        }

        Ok(HealthCheckUpdate {
            previous_status,
            new_status,
            failures,
        })
    }

    /// Notify all global subscribers of a config event, auto-pruning dead senders.
    fn notify_subscribers(&self, event: ConfigEvent) {
        let mut subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Notify per-dep watchers for a specific service, auto-pruning dead senders.
    fn notify_dep_watchers(&self, service_name: &str, change: &ServiceStatusChange) {
        let mut watchers = self.dep_watchers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(txs) = watchers.get_mut(service_name) {
            txs.retain(|tx| tx.send(change.clone()).is_ok());
        }
    }

    // ========================================================================
    // Ready / Quiescent computation
    // ========================================================================

    /// Check and emit Ready signal if all services reached their target state.
    fn check_and_notify_ready(&mut self) {
        if self.startup_in_progress { return; }
        let ready = self.compute_ready();
        if ready && !self.last_ready {
            self.last_ready = true;
            self.notify_subscribers(ConfigEvent::Ready);
        }
    }

    /// Check and emit Quiescent signal if all services are settled.
    fn check_and_notify_quiescent(&mut self) {
        if self.startup_in_progress { return; }
        let q = self.compute_quiescence();
        if q && !self.last_quiescent {
            self.last_quiescent = true;
            self.notify_subscribers(ConfigEvent::Quiescent);
        }
    }

    /// Returns true when all services have reached their target state or are permanently blocked.
    /// This implements the `--wait` semantic (like `docker compose up -d --wait`).
    fn compute_ready(&self) -> bool {
        self.config.services.keys().all(|s| self.is_service_ready(s))
    }

    /// A service is "ready" when it reached its target state, completed, or is a deferred wait.
    /// `Stopped` is NOT ready — it means explicitly stopped by the user, not completed.
    fn is_service_ready(&self, service_name: &str) -> bool {
        let status = match self.services.get(service_name) {
            Some(s) => s.status,
            None => return true,
        };

        // Stopped = explicitly stopped (not completed) → not ready
        // This prevents stale Ready signals after `kepler stop`.
        if status == ServiceStatus::Stopped { return false; }

        // Other terminal states (Exited, Failed, Killed, Skipped) → ready
        if !status.is_active() { return true; }

        // Running states: check if healthcheck resolved (if applicable)
        if status.is_running() {
            if let Some(raw) = self.config.services.get(service_name)
                && raw.has_healthcheck() {
                    // Healthy or Unhealthy means the healthcheck has resolved
                    return status == ServiceStatus::Healthy || status == ServiceStatus::Unhealthy;
                }
            return true; // no healthcheck, Running is enough
        }

        // Starting → actively starting up (deps already satisfied) → NOT ready yet
        if status == ServiceStatus::Starting { return false; }

        // Waiting → check if this is a deferred wait (unsatisfied deps that are all stable)
        // vs "about to start" (all deps satisfied or no deps — will transition to Starting momentarily)
        if status == ServiceStatus::Waiting {
            if let Some(raw) = self.config.services.get(service_name) {
                let mut has_unsatisfied_stable_dep = false;
                for (dep_name, dep_config) in raw.depends_on.iter() {
                    if self.is_condition_satisfied_sync(dep_name, &dep_config) { continue; }
                    // Condition not met. Is dep still settling?
                    let dep_state = self.services.get(dep_name);
                    let dep_status = dep_state.map(|s| s.status);
                    if matches!(dep_status, Some(ServiceStatus::Starting) | Some(ServiceStatus::Waiting) | Some(ServiceStatus::Stopping)) {
                        return false; // dep still settling → not ready yet
                    }
                    // Check if dep is terminal but will restart — still settling
                    if let Some(ds) = dep_state
                        && matches!(ds.status, ServiceStatus::Exited | ServiceStatus::Killed | ServiceStatus::Failed)
                            && let Some(dep_raw) = self.config.services.get(dep_name) {
                                let dep_restart = self.resolved_configs.get(dep_name)
                                    .map(|rc| rc.restart.clone())
                                    .unwrap_or_else(|| dep_raw.restart.as_static().cloned().unwrap_or_default());
                                if dep_restart.should_restart_on_exit(ds.exit_code) {
                                    return false; // dep will restart → still settling
                                }
                            }
                    // dep is in a stable state (Running/Healthy/Unhealthy/terminal-no-restart)
                    has_unsatisfied_stable_dep = true;
                }
                if has_unsatisfied_stable_dep {
                    return true; // deferred wait → ready
                }
            }
            // All deps satisfied (or no deps) → about to start → not ready yet
            return false;
        }

        // Stopping → not ready
        false
    }

    /// Returns true when all services are settled — nothing more will change.
    /// Uses topological traversal so deps are evaluated before dependents.
    fn compute_quiescence(&mut self) -> bool {
        // Lazily compute and cache topological order (dep graph is immutable)
        if self.cached_topo_order.is_none() {
            self.cached_topo_order = get_start_order(&self.config.services).ok();
        }
        let topo_order = match self.cached_topo_order {
            Some(ref order) => order,
            None => return false,
        };

        let mut settled: HashMap<String, bool> = HashMap::new();
        for service_name in topo_order {
            let is_settled = self.is_service_settled(service_name, &settled);
            settled.insert(service_name.to_string(), is_settled);
        }

        settled.values().all(|&s| s)
    }

    /// A service is "settled" when it's in a permanent terminal state or permanently blocked.
    fn is_service_settled(&self, service_name: &str, settled_cache: &HashMap<String, bool>) -> bool {
        let state = match self.services.get(service_name) {
            Some(s) => s,
            None => return true,
        };
        let status = state.status;

        if status == ServiceStatus::Skipped { return true; }

        // Waiting → check if this has unsatisfied deps that are all settled (condition will never be met)
        // vs "about to start" (all deps satisfied or no deps — will transition to Starting momentarily)
        if status == ServiceStatus::Waiting {
            if let Some(raw) = self.config.services.get(service_name) {
                let mut has_unsatisfied_settled_dep = false;
                for (dep_name, dep_config) in raw.depends_on.iter() {
                    if self.is_condition_satisfied_sync(dep_name, &dep_config) { continue; }
                    // Condition not met. Is dep settled?
                    if !settled_cache.get(dep_name).copied().unwrap_or(false) {
                        return false; // dep still settling
                    }
                    has_unsatisfied_settled_dep = true;
                }
                if has_unsatisfied_settled_dep {
                    return true; // condition will never be met → settled
                }
            }
            // All deps satisfied (or no deps) → about to start → not settled
            return false;
        }

        // Any other active state (Starting, Running, Stopping, Healthy, Unhealthy) → not settled
        if status.is_active() { return false; }

        // Terminal (Stopped, Failed, Exited, Killed)
        let would_restart = self.resolved_configs.get(service_name)
            .map(|rc| rc.restart.should_restart_on_exit(state.exit_code))
            .or_else(|| self.config.services.get(service_name)
                .map(|raw| raw.restart.as_static().cloned().unwrap_or_default().should_restart_on_exit(state.exit_code)))
            .unwrap_or(false);

        if !would_restart { return true; }

        // Would restart — but only if deps are still satisfiable
        !self.are_deps_satisfiable(service_name, settled_cache)
    }

    /// Check if all dependencies for a service can still be satisfied.
    /// Returns false if any dep is settled but its condition is not met.
    fn are_deps_satisfiable(&self, service_name: &str, settled_cache: &HashMap<String, bool>) -> bool {
        let raw = match self.config.services.get(service_name) {
            Some(v) => v,
            None => return false,
        };

        for (dep_name, dep_config) in raw.depends_on.iter() {
            if self.is_condition_satisfied_sync(dep_name, &dep_config) { continue; }
            // Condition not met. If dep is settled, it can never reach the required state.
            let dep_state = match self.services.get(dep_name) {
                Some(s) => s,
                None => return false,
            };
            if dep_state.status == ServiceStatus::Skipped && !dep_config.allow_skipped {
                return false;
            }
            if settled_cache.get(dep_name).copied().unwrap_or(false) {
                return false;
            }
        }
        true
    }

    /// Synchronous version of `check_dependency_satisfied` for use inside the actor.
    /// Checks whether a dependency condition is currently satisfied, including transient exit filtering.
    fn is_condition_satisfied_sync(&self, dep_name: &str, dep_config: &DependencyConfig) -> bool {
        let state = match self.services.get(dep_name) {
            Some(s) => s,
            None => return false,
        };

        let dep_restart = self.resolved_configs.get(dep_name)
            .map(|rc| rc.restart.clone())
            .or_else(|| self.config.services.get(dep_name)
                .map(|raw| raw.restart.as_static().cloned().unwrap_or_default()));
        is_condition_met(state, dep_config, dep_restart.as_ref())
    }

    /// Cleanup all resources when shutting down
    fn cleanup(&mut self) {
        if let Some(fd_count) = crate::fd_count::count_open_fds() {
            debug!("FD count before cleanup: {}", fd_count);
        }

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

        // Abort output tasks before clearing process handles.
        // Dropping a JoinHandle does NOT cancel the underlying task, so we must
        // explicitly abort to release pipe FDs and log file FDs.
        for (_, process_handle) in self.processes.drain() {
            if let Some(task) = process_handle.stdout_task {
                task.abort();
            }
            if let Some(task) = process_handle.stderr_task {
                task.abort();
            }
        }

        // Clear all channel-based resources to avoid dead sender accumulation
        self.subscribers.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.dep_watchers.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.event_senders.clear();

        // Drop Lua worker channel sender so the spawn_blocking thread exits promptly
        // (otherwise it only exits when the entire ConfigActor struct drops)
        self.lua_eval_tx = None;

        // Abort event handler and forwarder tasks
        if let Some(handler) = self.event_handler_task.take() {
            handler.abort();
        }
        for task in self.event_forwarder_tasks.drain(..) {
            task.abort();
        }

        if let Some(fd_count) = crate::fd_count::count_open_fds() {
            debug!("FD count after cleanup: {}", fd_count);
        }
    }
}
