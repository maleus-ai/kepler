//! Service orchestration for lifecycle management.
//!
//! This module provides a centralized `ServiceOrchestrator` that handles all service
//! lifecycle operations (start, stop, restart, exit handling). It eliminates duplication
//! by providing unified methods that handle hooks, log retention, and state updates.

mod error;
mod events;
mod lifecycle;

pub use error::OrchestratorError;
pub use events::{spawn_event_forwarders, ServiceEventHandler, TaggedEventMessage};
pub use lifecycle::LifecycleEvent;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Delay between stop and start phases during restart.
/// Allows OS resources (ports, file handles) to be fully released.
const RESTART_DELAY: Duration = Duration::from_millis(500);

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{resolve_log_retention, resolve_sys_env, DependsOn, GlobalHooks, KeplerConfig, LogConfig, LogRetention, RawServiceConfig, ServiceHooks, SysEnvPolicy};
use crate::config_actor::{ConfigActorHandle, ServiceContext, TaskHandleType};
use crate::config_registry::SharedConfigRegistry;
use crate::deps::{check_dependency_satisfied, get_start_order, get_stop_order, is_condition_unreachable_by_policy, is_dependency_permanently_unsatisfied, is_transient_satisfaction};
use crate::lua_eval::{EvalContext, LuaEvaluator};
use crate::events::{RestartReason, ServiceEvent};

/// Shared Lua evaluator for cross-service global state within a single config.
/// Created once per `start_services` call and reused across all service starts,
/// ensuring the `global` table persists between services.
type SharedLuaEvaluator = Arc<tokio::sync::Mutex<LuaEvaluator>>;
use crate::health::spawn_health_checker;
use crate::hooks::{
    run_global_hook, run_service_hook, GlobalHookParams, GlobalHookType, ServiceHookParams, ServiceHookType,
    GLOBAL_HOOK_PREFIX,
};
use crate::lua_eval::DepInfo;
use crate::logs::LogWriterConfig;
use crate::process::{spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams};
use crate::state::ServiceStatus;
use crate::watcher::{spawn_file_watcher, FileChangeEvent};
use kepler_protocol::protocol::PrunedConfigInfo;
use kepler_protocol::server::ProgressSender;

/// Context for post-startup work, bundling all parameters into a single struct.
struct StartupContext<'a> {
    config_path: &'a Path,
    config: &'a KeplerConfig,
    handle: &'a ConfigActorHandle,
    started: &'a [String],
    initialized: bool,
    global_hooks: &'a Option<GlobalHooks>,
    global_log_config: Option<&'a LogConfig>,
    config_dir: &'a Path,
    stored_env: &'a HashMap<String, String>,
    log_config: &'a LogWriterConfig,
    run_global_hooks: bool,
    progress: &'a Option<ProgressSender>,
}


/// Coordinator for service lifecycle operations.
///
/// The ServiceOrchestrator provides unified methods for starting, stopping, and restarting
/// services. It handles all the common patterns:
/// - Running appropriate hooks
/// - Applying log retention policies
/// - Updating state
/// - Spawning auxiliary tasks (health checks, file watchers)
#[derive(Clone)]
pub struct ServiceOrchestrator {
    registry: SharedConfigRegistry,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
}

impl ServiceOrchestrator {
    /// Create a new ServiceOrchestrator
    pub fn new(
        registry: SharedConfigRegistry,
        exit_tx: mpsc::Sender<ProcessExitEvent>,
        restart_tx: mpsc::Sender<FileChangeEvent>,
    ) -> Self {
        Self {
            registry,
            exit_tx,
            restart_tx,
        }
    }

    /// Get the registry
    pub fn registry(&self) -> &SharedConfigRegistry {
        &self.registry
    }

    /// Start services for a config
    ///
    /// If `service_filter` is provided, only starts that service and its dependencies.
    /// Otherwise starts all services in dependency order.
    ///
    /// The `config_owner` parameter provides the UID/GID of the CLI user.
    /// On fresh config loads, services without an explicit `user:` will default
    /// to running as this user.
    ///
    /// Runs the full pipeline synchronously and responds when complete.
    /// The CLI decides whether to await the response or fire-and-forget.
    pub async fn start_services(
        &self,
        config_path: &Path,
        service_filter: Option<&str>,
        sys_env: Option<HashMap<String, String>>,
        config_owner: Option<(u32, u32)>,
        progress: Option<ProgressSender>,
    ) -> Result<String, OrchestratorError> {
        // Get or create the config actor
        let handle = self
            .registry
            .get_or_create(config_path.to_path_buf(), sys_env.clone(), config_owner)
            .await?;

        let config_dir = handle.get_config_dir().await;

        // Get config and determine services to start
        let config = handle
            .get_config()
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let services_to_start = match service_filter {
            Some(name) => {
                if !config.services.contains_key(name) {
                    return Err(OrchestratorError::ServiceNotFound(name.to_string()));
                }
                vec![name.to_string()]
            }
            None => get_start_order(&config.services)?,
        };

        // Check if any services need starting.
        // A service needs starting if it's in a non-active state AND either:
        // - Its restart policy says it should restart on its exit code, OR
        // - It has never run (no exit code recorded)
        // Completed one-shots (Exited + restart:no) and Skipped services are "done".
        let any_need_starting = {
            let mut found = false;
            for svc in &services_to_start {
                if self.service_needs_starting(svc, &config, &handle).await {
                    found = true;
                    break;
                }
            }
            found
        };

        if !any_need_starting {
            return Ok("All services already running".to_string());
        }

        // Create a shared Lua evaluator so the `global` table persists across
        // all service starts in this config (enabling cross-service state sharing).
        let shared_evaluator: SharedLuaEvaluator = Arc::new(tokio::sync::Mutex::new(
            config.create_lua_evaluator()
                .map_err(|e| OrchestratorError::ConfigError(e.to_string()))?,
        ));

        let log_config = handle
            .get_log_config()
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let global_hooks = config.global_hooks().cloned();
        let global_log_config = config.global_logs().cloned();
        let initialized = handle.is_config_initialized().await;

        // Fetch stored sys_env once for all global hooks in this method
        let stored_env = handle.get_sys_env().await;

        let is_specific_service = service_filter.is_some();

        // Only run global hooks for full start (not specific service)
        if !is_specific_service {
            // Run global pre_start hook
            run_global_hook(
                &global_hooks,
                GlobalHookType::PreStart,
                &GlobalHookParams {
                    working_dir: &config_dir,
                    env: &stored_env,
                    log_config: Some(&log_config),
                    global_log_config: global_log_config.as_ref(),
                    progress: &progress,
                    handle: Some(&handle),
                    lua_code: config.lua.as_deref(),
                },
            )
            .await?;
        }

        // Start services
        if is_specific_service {
            // Start only the named service (no transitive deps)
            let mut started = Vec::new();
            for service_name in &services_to_start {
                if handle.is_service_running(service_name).await {
                    debug!("Service {} is already running", service_name);
                    continue;
                }

                match self.start_single_service_with_evaluator(&handle, service_name, &progress, Some(shared_evaluator.clone())).await {
                    Ok(()) => started.push(service_name.clone()),
                    Err(OrchestratorError::StartupCancelled(_)) => {
                        debug!("Service {} startup was cancelled, skipping", service_name);
                    }
                    Err(e) => {
                        error!("Failed to start service {}: {}", service_name, e);
                        return Err(e);
                    }
                }
            }

            // Run post-startup work for specific service start
            self.post_startup_work(StartupContext {
                config_path,
                config: &config,
                handle: &handle,
                started: &started,
                initialized,
                global_hooks: &global_hooks,
                global_log_config: global_log_config.as_ref(),
                config_dir: &config_dir,
                stored_env: &stored_env,
                log_config: &log_config,
                run_global_hooks: false, // no global hooks for specific service
                progress: &progress,
            }).await?;

            if started.is_empty() {
                Ok("All services already running".to_string())
            } else {
                Ok(format!("Started services: {}", started.join(", ")))
            }
        } else {
            // Raise startup fence to suppress premature Ready/Quiescent signals
            handle.set_startup_in_progress(true).await;

            // Mark services that need starting as Waiting.
            // Skip: already-active services, completed one-shots, skipped services.
            let mut newly_waiting = Vec::new();
            for service_name in &services_to_start {
                if !self.service_needs_starting(service_name, &config, &handle).await {
                    debug!("Service {} does not need starting, skipping", service_name);
                    continue;
                }
                if let Err(e) = handle.set_service_status(service_name, ServiceStatus::Waiting).await {
                    warn!("Failed to set {} to Waiting: {}", service_name, e);
                }
                newly_waiting.push(service_name.clone());
            }

            // Lower startup fence — all services are now in Waiting state
            handle.set_startup_in_progress(false).await;

            // Spawn only newly-waiting services (not already-active ones or
            // services with an existing blocked task from a previous start call).
            for service_name in &newly_waiting {
                let self_clone = self.clone();
                let handle_clone = handle.clone();
                let progress_clone = progress.clone();
                let service_name_clone = service_name.clone();
                let evaluator_clone = shared_evaluator.clone();
                tokio::spawn(async move {
                    if let Err(e) = self_clone.start_single_service_with_evaluator(
                        &handle_clone, &service_name_clone, &progress_clone, Some(evaluator_clone),
                    ).await {
                        warn!("Failed to start service {}: {}", service_name_clone, e);
                    }
                });
            }

            // Post-startup work (snapshot, event handler, global post_start hook)
            self.post_startup_work(StartupContext {
                config_path,
                config: &config,
                handle: &handle,
                started: &services_to_start,
                initialized,
                global_hooks: &global_hooks,
                global_log_config: global_log_config.as_ref(),
                config_dir: &config_dir,
                stored_env: &stored_env,
                log_config: &log_config,
                run_global_hooks: true,
                progress: &progress,
            }).await?;

            Ok(format!("Services starting: {}", services_to_start.join(", ")))
        }
    }

    /// Run post-startup work: snapshot, event handler, global post_start hook, mark initialized
    ///
    /// When `run_global_hooks` is false (specific service start), the global post_start
    /// hook is skipped — consistent with stop/restart which also skip global hooks
    /// for single-service operations.
    async fn post_startup_work(
        &self,
        ctx: StartupContext<'_>,
    ) -> Result<(), OrchestratorError> {
        if !ctx.started.is_empty() {
            if let Err(e) = ctx.handle.take_snapshot_if_needed().await {
                warn!("Failed to take config snapshot: {}", e);
            }

            // Check if any service has restart propagation enabled
            let needs_event_handler = ctx.config.services.values().any(|raw| {
                !raw.depends_on.dependencies_with_restart().is_empty()
            });

            // Spawn event handler for restart propagation if needed and not already running
            if needs_event_handler && !ctx.handle.has_event_handler().await {
                let self_arc = Arc::new(self.clone());
                let config_path_owned = ctx.config_path.to_path_buf();
                let handle_clone = ctx.handle.clone();
                self_arc.spawn_event_handler(config_path_owned, handle_clone).await;
                ctx.handle.set_event_handler_spawned().await;
            }

            // Run global post_start hook (after all services started)
            // Skipped for specific service start (consistent with stop/restart)
            if ctx.run_global_hooks
                && let Err(e) = run_global_hook(
                    ctx.global_hooks,
                    GlobalHookType::PostStart,
                    &GlobalHookParams {
                        working_dir: ctx.config_dir,
                        env: ctx.stored_env,
                        log_config: Some(ctx.log_config),
                        global_log_config: ctx.global_log_config,
                        progress: ctx.progress,
                        handle: Some(ctx.handle),
                        lua_code: ctx.config.lua.as_deref(),
                    },
                )
                .await
            {
                warn!("Global post_start hook failed: {}", e);
            }

            // Mark config initialized after first start
            if !ctx.initialized {
                ctx.handle.mark_config_initialized().await?;
            }
        }

        Ok(())
    }

    /// Start a single service, optionally sharing a Lua evaluator for cross-service global state.
    async fn start_single_service_with_evaluator(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        progress: &Option<ProgressSender>,
        shared_evaluator: Option<SharedLuaEvaluator>,
    ) -> Result<(), OrchestratorError> {
        // Atomically claim the service for startup
        if !handle.claim_service_start(service_name).await {
            debug!("Service {} already active, skipping", service_name);
            return Ok(());
        }

        // Run the actual startup. If anything fails, mark as Skipped or Failed.
        match self.execute_service_startup(handle, service_name, progress, shared_evaluator).await {
            Ok(()) => Ok(()),
            Err(ref e @ OrchestratorError::DependencySkipped { ref dependency, .. }) => {
                let reason = format!("dependency `{}` was skipped", dependency);
                if let Err(err) = handle.set_service_status_with_reason(
                    service_name, ServiceStatus::Skipped, Some(reason.clone()), None,
                ).await {
                    warn!("Failed to set {} to Skipped: {}", service_name, err);
                }
                info!("Service {} skipped: {}", service_name, e);
                Ok(())
            }
            Err(ref e @ OrchestratorError::DependencyUnsatisfied { ref reason, .. }) => {
                if let Err(err) = handle.set_service_status_with_reason(
                    service_name, ServiceStatus::Skipped, Some(reason.clone()), None,
                ).await {
                    warn!("Failed to set {} to Skipped: {}", service_name, err);
                }
                info!("Service {} skipped: {}", service_name, e);
                Ok(())
            }
            Err(OrchestratorError::StartupCancelled(_)) => {
                // Startup was cancelled (e.g., stop called during dep wait).
                // Don't override the status — stop_service already set it.
                debug!("Service {} startup was cancelled", service_name);
                Ok(())
            }
            Err(e) => {
                if let Err(err) = handle.set_service_status_with_reason(
                    service_name, ServiceStatus::Failed, None, Some(e.to_string()),
                ).await {
                    warn!("Failed to set {} to Failed: {}", service_name, err);
                }
                Err(e)
            }
        }
    }

    /// Respawn a single service during daemon restart recovery.
    ///
    /// Like `start_single_service` but public and without progress reporting.
    /// The caller is responsible for marking the service as Waiting beforehand.
    pub async fn respawn_single_service(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
    ) -> Result<(), OrchestratorError> {
        self.start_single_service_with_evaluator(handle, service_name, &None, None).await
    }

    /// Execute the actual service startup sequence (after claiming).
    ///
    /// This is the lazy resolution point: after dependencies are satisfied,
    /// the raw service config is expanded (${{}} + !lua) and deserialized
    /// into a typed ServiceConfig.
    async fn execute_service_startup(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        progress: &Option<ProgressSender>,
        shared_evaluator: Option<SharedLuaEvaluator>,
    ) -> Result<(), OrchestratorError> {
        // Get service context (single round-trip — raw config + state)
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Extract depends_on from raw config for dependency wait
        let depends_on = ctx.service_config.depends_on.clone();

        // Wait for dependencies to satisfy their conditions (blocks while in Waiting state)
        self.wait_for_dependencies(handle, service_name, &depends_on)
            .await?;

        // Transition: Waiting → Starting (dependencies satisfied)
        handle.set_service_status(service_name, ServiceStatus::Starting).await?;

        // Build evaluation context (sys_env + deps).
        // env_file vars are loaded inside resolve_service (step 0) so that
        // !lua env_file tags are evaluated before environment.
        let sys_env = handle.get_sys_env().await;
        let config = handle.get_config().await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;
        let config_dir = handle.get_config_dir().await;

        // Build dependency state maps for evaluation context
        let ctx_start = std::time::Instant::now();
        let mut dep_infos = HashMap::new();
        for (dep_name, _dep_config) in depends_on.iter() {
            let dep_name_owned = dep_name.to_string();
            if let Some(dep_state) = handle.get_service_state(dep_name).await {
                let dep_outputs = crate::outputs::read_service_outputs(&config_dir, dep_name);
                dep_infos.insert(dep_name_owned, DepInfo {
                    status: dep_state.status.as_str().to_string(),
                    exit_code: dep_state.exit_code,
                    initialized: dep_state.initialized,
                    restart_count: dep_state.restart_count,
                    env: dep_state.computed_env.clone(),
                    outputs: dep_outputs,
                });
            }
        }

        // Pre-load env_file vars for plain string paths (handles state dir copy fallback).
        // For !lua env_file tags, this returns empty — resolve_service will handle them.
        let env_file_vars = self.load_service_env_file(service_name, &ctx.service_config, &config_dir, handle).await;

        let mut full_env = sys_env.clone();
        full_env.extend(env_file_vars.clone());

        let state = handle.get_service_state(service_name).await;
        let mut eval_ctx = EvalContext {
            sys_env: sys_env.clone(),
            env_file: env_file_vars,
            env: full_env,
            service_name: Some(service_name.to_string()),
            initialized: state.as_ref().map(|s| s.initialized),
            restart_count: state.as_ref().map(|s| s.restart_count),
            exit_code: state.as_ref().and_then(|s| s.exit_code),
            status: state.as_ref().map(|s| s.status.as_str().to_string()),
            deps: dep_infos,
            ..Default::default()
        };
        debug!("[timeit] {} eval context built in {:?}", service_name, ctx_start.elapsed());

        // Resolve the service: evaluate ${{ }} + !lua + deserialize to ServiceConfig.
        // resolve_service handles env_file → environment → other fields in the correct order.
        // Uses the shared evaluator (if provided) so the `global` table persists
        // across service starts within the same config.
        //
        // Build default_user from CLI owner uid:gid (skip if root)
        let default_user = match (handle.owner_uid(), handle.owner_gid()) {
            (Some(uid), Some(gid)) if uid != 0 => Some(format!("{}:{}", uid, gid)),
            (Some(uid), None) if uid != 0 => Some(format!("{}", uid)),
            _ => None,
        };
        let resolve_start = std::time::Instant::now();
        let resolved = if let Some(ref shared_eval) = shared_evaluator {
            let evaluator = shared_eval.lock().await;
            config.resolve_service(
                service_name,
                &mut eval_ctx,
                &evaluator,
                handle.config_path(),
                default_user.as_deref(),
            ).map_err(|e| OrchestratorError::ConfigError(e.to_string()))?
        } else {
            let lua_evaluator = config.create_lua_evaluator()
                .map_err(|e| OrchestratorError::ConfigError(e.to_string()))?;
            config.resolve_service(
                service_name,
                &mut eval_ctx,
                &lua_evaluator,
                handle.config_path(),
                default_user.as_deref(),
            ).map_err(|e| OrchestratorError::ConfigError(e.to_string()))?
        };
        debug!("[timeit] {} resolve_service completed in {:?}", service_name, resolve_start.elapsed());

        // Build computed_env respecting sys_env policy.
        // eval_ctx.env has sys_env + env_file + environment (needed for Lua evaluation).
        // But with sys_env: clear, the process should NOT inherit sys_env vars.
        let sys_env_policy = resolve_sys_env(
            resolved.sys_env.as_ref(),
            config.global_sys_env(),
        );
        let computed_env = if sys_env_policy == SysEnvPolicy::Clear {
            // Only env_file + environment entries (no inherited sys_env)
            let mut env = eval_ctx.env_file.clone();
            crate::env::insert_env_entries(&mut env, &resolved.environment);
            env
        } else {
            eval_ctx.env.clone()
        };

        // Resolve working_dir
        let working_dir = resolved
            .working_dir
            .as_ref()
            .map(|wd| config_dir.join(wd))
            .unwrap_or_else(|| config_dir.clone());

        // Store resolved config and computed state in the actor
        handle.store_resolved_config(
            service_name,
            resolved.clone(),
            computed_env.clone(),
            working_dir.clone(),
        ).await;

        // Re-fetch context with updated resolved config, env, and working_dir
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Service-level `if` condition — already resolved to bool by resolve_service
        if resolved.condition == Some(false) {
            let reason = "`if` condition evaluated to false".to_string();
            tracing::info!("Service {} skipped: {}", service_name, reason);
            if let Err(err) = handle.set_service_status_with_reason(
                service_name, ServiceStatus::Skipped, Some(reason), None,
            ).await {
                warn!("Failed to set {} to Skipped: {}", service_name, err);
            }
            return Ok(());
        }

        // Run on_init hook if first time for this service
        let service_initialized = handle.is_service_initialized(service_name).await;

        // Clear previous outputs for a fresh start
        if let Err(e) = crate::outputs::clear_service_outputs(&config_dir, service_name) {
            warn!("Failed to clear outputs for {}: {}", service_name, e);
        }

        // Emit Start event
        handle.emit_event(service_name, ServiceEvent::Start).await;

        // Run pre_start hook
        self.run_service_hook(&ctx, service_name, ServiceHookType::PreStart, progress, Some(handle))
            .await?;

        // Apply on_start log retention
        self.apply_retention(handle, service_name, &ctx, LifecycleEvent::Start)
            .await;

        // Check if startup was cancelled (e.g., concurrent stop)
        let state = handle.get_service_state(service_name).await;
        if state.as_ref().map(|s| s.status) != Some(ServiceStatus::Starting) {
            debug!(
                "Service {} startup cancelled (status: {:?})",
                service_name,
                state.map(|s| s.status)
            );
            return Ok(());
        }

        // Spawn process
        self.spawn_service(handle, service_name, &ctx).await?;

        // Run post_start hook (after process spawned)
        if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostStart, progress, Some(handle)).await {
            warn!("Hook post_start failed for {}: {}", service_name, e);
        }

        // Mark service initialized after first start
        if !service_initialized {
            handle.mark_service_initialized(service_name).await?;
        }

        // Spawn auxiliary tasks
        self.spawn_auxiliary_tasks(handle, service_name, &ctx).await;

        Ok(())
    }

    /// Load env_file variables for a service from the state directory copy.
    async fn load_service_env_file(
        &self,
        service_name: &str,
        raw_config: &RawServiceConfig,
        config_dir: &Path,
        handle: &ConfigActorHandle,
    ) -> HashMap<String, String> {
        // Extract env_file path from typed config (only if static/resolved)
        let env_file_opt = raw_config.env_file.as_static().and_then(|v| v.as_ref());

        if let Some(env_file_path) = env_file_opt {
            let env_file = env_file_path.clone();
            // Try state dir copy first, then original path
            let config_hash = handle.config_hash().to_string();
            let state_dir = match crate::global_state_dir() {
                Ok(d) => d.join("configs").join(&config_hash),
                Err(_) => return HashMap::new(),
            };

            let dest_name = format!(
                "{}_{}",
                service_name,
                env_file.file_name().unwrap_or_default().to_string_lossy()
            );
            let state_copy = state_dir.join("env_files").join(&dest_name);

            let source = if state_copy.exists() {
                state_copy
            } else if env_file.is_relative() {
                config_dir.join(&env_file)
            } else {
                env_file
            };

            if source.exists() {
                match crate::env::load_env_file(&source) {
                    Ok(vars) => return vars,
                    Err(e) => {
                        warn!("Failed to load env_file for {}: {}", service_name, e);
                    }
                }
            }
        }

        HashMap::new()
    }

    /// Re-resolve a service's config at restart time with updated context.
    ///
    /// Builds a fresh `EvalContext` with the current restart_count, exit_code,
    /// and dep states, then evaluates all `${{ }}` / `!lua` fields again.
    /// Stores the new resolved config in the actor and returns a refreshed
    /// `ServiceContext`.
    async fn re_resolve_service(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        exit_code: Option<i32>,
    ) -> Result<ServiceContext, OrchestratorError> {
        let config = handle.get_config().await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;
        let ctx = handle.get_service_context(service_name).await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;
        let config_dir = handle.get_config_dir().await;
        let sys_env = handle.get_sys_env().await;

        // Build dependency info from current service states
        let mut dep_infos = HashMap::new();
        for (dep_name, _) in ctx.service_config.depends_on.iter() {
            if let Some(dep_state) = handle.get_service_state(dep_name).await {
                let dep_outputs = crate::outputs::read_service_outputs(&config_dir, dep_name);
                dep_infos.insert(dep_name.to_string(), DepInfo {
                    status: dep_state.status.as_str().to_string(),
                    exit_code: dep_state.exit_code,
                    initialized: dep_state.initialized,
                    restart_count: dep_state.restart_count,
                    env: dep_state.computed_env.clone(),
                    outputs: dep_outputs,
                });
            }
        }

        // Pre-load env_file vars for plain string paths
        let env_file_vars = self.load_service_env_file(service_name, &ctx.service_config, &config_dir, handle).await;

        let mut full_env = sys_env.clone();
        full_env.extend(env_file_vars.clone());

        // Get current service state for restart_count
        let service_state = handle.get_service_state(service_name).await;

        let mut eval_ctx = EvalContext {
            sys_env: sys_env.clone(),
            env_file: env_file_vars,
            env: full_env,
            service_name: Some(service_name.to_string()),
            deps: dep_infos,
            restart_count: service_state.as_ref().map(|s| s.restart_count),
            exit_code,
            initialized: service_state.as_ref().map(|s| s.initialized),
            status: service_state.as_ref().map(|s| s.status.as_str().to_string()),
            ..Default::default()
        };

        // Create fresh evaluator and resolve
        let lua_evaluator = config.create_lua_evaluator()
            .map_err(|e| OrchestratorError::ConfigError(e.to_string()))?;

        let default_user = match (handle.owner_uid(), handle.owner_gid()) {
            (Some(uid), Some(gid)) if uid != 0 => Some(format!("{}:{}", uid, gid)),
            (Some(uid), None) if uid != 0 => Some(format!("{}", uid)),
            _ => None,
        };

        let resolved = config.resolve_service(
            service_name,
            &mut eval_ctx,
            &lua_evaluator,
            handle.config_path(),
            default_user.as_deref(),
        ).map_err(|e| OrchestratorError::ConfigError(e.to_string()))?;

        // Build computed_env respecting sys_env policy
        let sys_env_policy = resolve_sys_env(
            resolved.sys_env.as_ref(),
            config.global_sys_env(),
        );
        let computed_env = if sys_env_policy == SysEnvPolicy::Clear {
            let mut env = eval_ctx.env_file.clone();
            crate::env::insert_env_entries(&mut env, &resolved.environment);
            env
        } else {
            eval_ctx.env.clone()
        };

        let working_dir = resolved
            .working_dir
            .as_ref()
            .map(|wd| config_dir.join(wd))
            .unwrap_or_else(|| config_dir.clone());

        // Store in actor
        handle.store_resolved_config(
            service_name,
            resolved,
            computed_env,
            working_dir,
        ).await;

        // Return refreshed context
        handle.get_service_context(service_name).await
            .ok_or(OrchestratorError::ServiceContextNotFound)
    }

    /// Wait for all dependencies to satisfy their conditions
    async fn wait_for_dependencies(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        depends_on: &DependsOn,
    ) -> Result<(), OrchestratorError> {
        // Get the full config for looking up dependency service configs (raw values)
        let config = handle.get_config().await;

        for (dep_name, dep_config) in depends_on.iter() {
            let start = Instant::now();

            debug!(
                "Service {} waiting for dependency {} (condition: {:?})",
                service_name, dep_name, dep_config.condition
            );

            // Only use per-dependency timeout (no global fallback — wait indefinitely if unset)
            let deadline = dep_config.timeout.map(|t| start + t);

            // Subscribe to state changes of this specific dependency for instant notification
            let mut status_rx = handle.watch_dep(dep_name);

            loop {
                let dep_satisfied = check_dependency_satisfied(dep_name, &dep_config, handle).await;
                if dep_satisfied {
                    // Check if this is a transient satisfaction (dep exited but will restart).
                    // If so, fall through to recv() — the dep will restart and the condition will no longer hold.
                    let is_transient = if let Some(ref cfg) = config
                        && let Some(dep_raw) = cfg.services.get(dep_name)
                        && let Some(dep_state) = handle.get_service_state(dep_name).await
                    {
                        let dep_restart = dep_raw.restart.as_static().cloned().unwrap_or_default();
                        is_transient_satisfaction(&dep_state, &dep_restart)
                    } else {
                        false
                    };

                    if !is_transient {
                        debug!(
                            "Dependency {} satisfied for service {} (took {:?})",
                            dep_name,
                            service_name,
                            start.elapsed()
                        );
                        break;
                    }

                    debug!(
                        "Dependency {} for {} is transiently satisfied (will restart), continuing to wait",
                        dep_name, service_name
                    );
                    // Fall through to recv() below
                } else {
                    // Check if dependency was skipped (cascade skip)
                    if let Some(dep_state) = handle.get_service_state(dep_name).await
                        && dep_state.status == ServiceStatus::Skipped
                            && !dep_config.allow_skipped {
                                return Err(OrchestratorError::DependencySkipped {
                                    service: service_name.to_string(),
                                    dependency: dep_name.to_string(),
                                });
                            }

                    // Check if dependency is permanently unsatisfied (terminal state, won't restart)
                    if let Some(ref cfg) = config
                        && let Some(dep_raw) = cfg.services.get(dep_name)
                        && is_dependency_permanently_unsatisfied(
                            dep_name,
                            &dep_config,
                            handle,
                            &dep_raw.restart.as_static().cloned().unwrap_or_default(),
                        ).await
                    {
                        let state = handle.get_service_state(dep_name).await;
                        let dep_status = state.as_ref().map(|s| s.status.as_str()).unwrap_or("unknown");
                        let exit_code_str = match state.as_ref().and_then(|s| s.exit_code) {
                            Some(code) => format!(" (exit code {})", code),
                            None => String::new(),
                        };
                        let condition_str = dep_config.condition.as_str();
                        let reason = format!(
                            "dependency `{}` {}{} and won't restart, condition `{}` can never be met",
                            dep_name,
                            dep_status,
                            exit_code_str,
                            condition_str,
                        );
                        return Err(OrchestratorError::DependencyUnsatisfied {
                            service: service_name.to_string(),
                            dependency: dep_name.to_string(),
                            condition: dep_config.condition.clone(),
                            reason,
                        });
                    }

                    // Check if condition is structurally unreachable given dep's restart policy
                    if let Some(ref cfg) = config
                        && let Some(dep_raw) = cfg.services.get(dep_name)
                    {
                        let dep_state = handle.get_service_state(dep_name).await;
                        let dep_running = dep_state.as_ref().is_some_and(|s| s.status.is_running());
                        if dep_running {
                            let dep_restart = dep_raw.restart.as_static().cloned().unwrap_or_default();
                            if let Some(reason) = is_condition_unreachable_by_policy(
                                dep_name,
                                &dep_config.condition,
                                dep_restart.policy(),
                            ) {
                                return Err(OrchestratorError::DependencyUnsatisfied {
                                    service: service_name.to_string(),
                                    dependency: dep_name.to_string(),
                                    condition: dep_config.condition.clone(),
                                    reason,
                                });
                            }
                        }
                    }
                }

                // Check if startup was cancelled during dependency wait
                let state = handle.get_service_state(service_name).await;
                if !matches!(
                    state.as_ref().map(|s| s.status),
                    Some(ServiceStatus::Starting) | Some(ServiceStatus::Waiting)
                ) {
                    return Err(OrchestratorError::StartupCancelled(
                        service_name.to_string(),
                    ));
                }

                // Wait for next status change, with optional deadline
                let recv_result = if let Some(dl) = deadline {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(OrchestratorError::DependencyTimeout {
                            service: service_name.to_string(),
                            dependency: dep_name.to_string(),
                            condition: dep_config.condition.clone(),
                        });
                    }
                    match tokio::time::timeout(remaining, status_rx.recv()).await {
                        Ok(result) => result,
                        Err(_) => {
                            return Err(OrchestratorError::DependencyTimeout {
                                service: service_name.to_string(),
                                dependency: dep_name.to_string(),
                                condition: dep_config.condition.clone(),
                            });
                        }
                    }
                } else {
                    // No timeout — wait indefinitely for next status change
                    status_rx.recv().await
                };

                match recv_result {
                    Some(_) => continue, // status changed somewhere, re-check all conditions
                    None => {
                        return Err(OrchestratorError::StartupCancelled(
                            service_name.to_string(),
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Stop services for a config
    ///
    /// If `service_filter` is provided, only stops that service.
    /// Otherwise stops all services in reverse dependency order.
    ///
    /// If `clean` is true, also runs pre_cleanup hooks and retention.
    /// If `signal` is provided, sends that signal instead of SIGTERM.
    pub async fn stop_services(
        &self,
        config_path: &Path,
        service_filter: Option<&str>,
        clean: bool,
        signal: Option<i32>,
    ) -> Result<String, OrchestratorError> {
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let handle = match self.registry.get(&config_path.to_path_buf()) {
            Some(h) => h,
            None => return Ok("Config not loaded".to_string()),
        };

        // Get services to stop
        let config = match handle.get_config().await {
            Some(c) => c,
            None => return Ok("Config not loaded".to_string()),
        };

        let services_to_stop = match service_filter {
            Some(name) => {
                if !config.services.contains_key(name) {
                    return Err(OrchestratorError::ServiceNotFound(name.to_string()));
                }
                vec![name.to_string()]
            }
            None => get_stop_order(&config.services)?,
        };

        let log_config = handle.get_log_config().await;
        let global_log_config = config.global_logs().cloned();
        let global_hooks = config.global_hooks().cloned();

        // Fetch stored sys_env once for all global hooks in this method
        let stored_env = handle.get_sys_env().await;

        // Run global pre_stop hook if stopping all services
        if service_filter.is_none()
            && let Some(ref log_cfg) = log_config
            && let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::PreStop,
                &GlobalHookParams {
                    working_dir: &config_dir,
                    env: &stored_env,
                    log_config: Some(log_cfg),
                    global_log_config: global_log_config.as_ref(),
                    progress: &None,
                    handle: Some(&handle),
                    lua_code: config.lua.as_deref(),
                },
            )
            .await
        {
            warn!("Global pre_stop hook failed: {}", e);
        }

        let mut stopped = Vec::new();

        // Stop services in reverse dependency order.
        // Loop to catch services that become active during stop (e.g., deferred
        // services triggered by service_stopped conditions).
        for pass in 0..services_to_stop.len() {
            let mut pass_stopped = false;

            for service_name in &services_to_stop {
                // Check if active (includes Starting/Stopping, not just Running)
                let is_active = handle
                    .get_service_state(service_name)
                    .await
                    .map(|s| s.status.is_active())
                    .unwrap_or(false);

                if !is_active {
                    continue;
                }

                // Emit Stop event
                handle.emit_event(service_name, ServiceEvent::Stop).await;

                // Get service context
                let ctx = handle.get_service_context(service_name).await;

                if let Some(ref ctx) = ctx {
                    // Run pre_stop hook
                    if let Err(e) = self
                        .run_service_hook(ctx, service_name, ServiceHookType::PreStop, &None, Some(&handle))
                        .await
                    {
                        warn!("Hook pre_stop failed for {}: {}", service_name, e);
                    }
                }

                // Stop the service
                stop_service(service_name, handle.clone(), signal)
                    .await
                    .map_err(|e| OrchestratorError::StopFailed(e.to_string()))?;

                // Run post_stop hook (after process stopped)
                if let Some(ref ctx) = ctx
                    && let Err(e) = self
                        .run_service_hook(ctx, service_name, ServiceHookType::PostStop, &None, Some(&handle))
                        .await
                    {
                        warn!("Hook post_stop failed for {}: {}", service_name, e);
                    }

                stopped.push(service_name.clone());
                pass_stopped = true;
            }

            if !pass_stopped {
                // No services were stopped in this pass — check if any are still
                // starting up (deferred services may be spawning asynchronously)
                if pass > 0 {
                    // Brief settle time for async service spawning
                    tokio::time::sleep(Duration::from_millis(200)).await;

                    let any_active = {
                        let mut found = false;
                        for service_name in &services_to_stop {
                            let is_active = handle
                                .get_service_state(service_name)
                                .await
                                .map(|s| s.status.is_active())
                                .unwrap_or(false);
                            if is_active {
                                found = true;
                                break;
                            }
                        }
                        found
                    };

                    if !any_active {
                        break;
                    }
                    // Some services became active during the settle — loop again
                } else {
                    break;
                }
            }
        }

        // Run global post_stop hook if stopping all and services were stopped
        if service_filter.is_none() && !stopped.is_empty()
            && let Some(ref log_cfg) = log_config
            && let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::PostStop,
                &GlobalHookParams {
                    working_dir: &config_dir,
                    env: &stored_env,
                    log_config: Some(log_cfg),
                    global_log_config: global_log_config.as_ref(),
                    progress: &None,
                    handle: Some(&handle),
                    lua_code: config.lua.as_deref(),
                },
            )
            .await
        {
            warn!("Global post_stop hook failed: {}", e);
        }

        // Run pre_cleanup if requested
        if service_filter.is_none() && clean {
            info!("Running cleanup hooks");

            // Filter out services that never ran (Skipped/Waiting)
            let cleanable_services: Vec<&String> = {
                let mut result = Vec::new();
                for svc in &services_to_stop {
                    let status = handle.get_service_state(svc).await.map(|s| s.status);
                    if !matches!(status, Some(ServiceStatus::Skipped) | Some(ServiceStatus::Waiting)) {
                        result.push(svc);
                    }
                }
                result
            };

            // Emit Cleanup event for cleanable services only
            for service_name in &cleanable_services {
                handle.emit_event(service_name, ServiceEvent::Cleanup).await;
            }

            // Run service-level pre_cleanup hooks
            for service_name in &cleanable_services {
                let ctx = handle.get_service_context(service_name).await;
                if let Some(ref ctx) = ctx
                    && let Err(e) = self.run_service_hook(ctx, service_name, ServiceHookType::PreCleanup, &None, Some(&handle)).await {
                        warn!("Hook pre_cleanup failed for {}: {}", service_name, e);
                    }
            }

            // Run global pre_cleanup hook
            if let Some(ref log_cfg) = log_config
                && let Err(e) = run_global_hook(
                    &global_hooks,
                    GlobalHookType::PreCleanup,
                    &GlobalHookParams {
                        working_dir: &config_dir,
                        env: &stored_env,
                        log_config: Some(log_cfg),
                        global_log_config: global_log_config.as_ref(),
                        progress: &None,
                        handle: Some(&handle),
                        lua_code: config.lua.as_deref(),
                    },
                )
                .await
            {
                error!("pre_cleanup hook failed: {}", e);
            }
        }

        // Apply log retention using LogReader
        if let Some(ref log_cfg) = log_config {
            use crate::logs::LogReader;
            let reader = LogReader::new(log_cfg.logs_dir.clone());

            // When stopping all services, clear logs for ALL services (including
            // already-exited ones like one-shot tasks), not just those we stopped now.
            let services_to_clear = if service_filter.is_none() {
                &services_to_stop
            } else {
                &stopped
            };

            for service_name in services_to_clear {
                // When clean is true, always clear logs (no retention policy check)
                let should_clear = if clean {
                    true
                } else {
                    let service_logs = config
                        .services
                        .get(service_name)
                        .and_then(|raw| raw.logs.as_static().cloned().flatten());
                    resolve_log_retention(
                        service_logs.as_ref(),
                        global_log_config.as_ref(),
                        |l| l.get_on_stop(),
                        LogRetention::Clear,
                    ) == LogRetention::Clear
                };

                if should_clear {
                    reader.clear_service(service_name);
                    reader.clear_service_prefix(&format!("{}.", service_name));
                }
            }

            // Clear global hook logs if stopping all
            if service_filter.is_none() && (!stopped.is_empty() || clean) {
                // When clean is true, always clear; otherwise check on_stop retention
                let should_clear_global = clean
                    || global_log_config
                        .as_ref()
                        .and_then(|c| c.get_on_stop())
                        .unwrap_or(LogRetention::Clear)
                        == LogRetention::Clear;
                if should_clear_global {
                    reader.clear_service_prefix(GLOBAL_HOOK_PREFIX);
                }
            }
        }

        // When clean=true and stopping all services, remove entire state directory
        // This makes `stop --clean` behave like `prune` - complete cleanup
        if service_filter.is_none() && clean {
            let config_hash = handle.config_hash();
            let state_dir = match crate::global_state_dir() {
                Ok(dir) => dir.join("configs").join(config_hash),
                Err(e) => {
                    warn!("Cannot determine state directory for cleanup: {}", e);
                    // Continue without cleanup - services were still stopped
                    if stopped.is_empty() {
                        return Ok("No services were running".to_string());
                    } else {
                        return Ok(format!("Stopped services: {}", stopped.join(", ")));
                    }
                }
            };

            // Unload config from registry (triggers actor cleanup + save_state)
            self.registry.unload(&config_path.to_path_buf()).await;

            // Remove entire state directory (logs, config snapshots, env_files, etc.)
            // Done AFTER unload so save_state in cleanup() doesn't fail
            if state_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&state_dir) {
                    warn!("Failed to remove state directory {:?}: {}", state_dir, e);
                } else {
                    info!("Removed state directory: {:?}", state_dir);
                }
            }

            // Purge allocator caches to return freed pages to the OS
            crate::allocator::purge_caches();
        }

        if stopped.is_empty() && !clean {
            Ok("No services were running".to_string())
        } else if clean && stopped.is_empty() {
            Ok("Cleaned up (no services were running)".to_string())
        } else if clean {
            Ok(format!("Stopped and cleaned services: {}", stopped.join(", ")))
        } else {
            Ok(format!("Stopped services: {}", stopped.join(", ")))
        }
    }

    /// Restart services for a config
    ///
    /// If `services` is empty, restarts all RUNNING services.
    /// Otherwise restarts only the specified RUNNING services.
    ///
    /// This method PRESERVES the baked config and state:
    /// - Config actor stays loaded
    /// - Environment variables are NOT re-expanded
    /// - Service-level pre_restart/post_restart hooks are called
    ///
    /// For a fresh restart with re-baked config, use `recreate_services()` instead.
    ///
    /// Execution order:
    /// 1. Global pre_restart (full restart only)
    /// 2. STOP PHASE - reverse dependency order (dependents first):
    ///    For each running service: pre_restart, pre_stop, apply retention, stop, post_stop
    /// 3. START PHASE - forward dependency order (dependencies first):
    ///    For each service: pre_start, apply retention, spawn, post_start, post_restart
    /// 4. Global post_restart (full restart only)
    pub async fn restart_services(
        &self,
        config_path: &Path,
        services: &[String],
    ) -> Result<String, OrchestratorError> {
        info!("Restarting services for {:?} (preserving state)", config_path);

        let is_full_restart = services.is_empty();
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Get handle - must already be loaded
        let handle = self.registry.get(&config_path.to_path_buf())
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let config = handle
            .get_config()
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let log_config = handle.get_log_config().await;
        let global_hooks = config.global_hooks().cloned();
        let global_log_config = config.global_logs().cloned();

        // Fetch stored sys_env once for all global hooks in this method
        let stored_env = handle.get_sys_env().await;

        // Get list of running services to restart
        let services_to_restart: Vec<String> = if is_full_restart {
            handle.get_running_services().await
        } else {
            let mut running = Vec::new();
            for s in services {
                if handle.is_service_running(s).await {
                    running.push(s.clone());
                }
            }
            running
        };

        if services_to_restart.is_empty() {
            return Ok("No running services to restart".to_string());
        }

        // Sort services by dependency graph
        // Start order: forward topological sort (dependencies first, then dependents)
        // Stop order: reverse of start order (dependents first, then dependencies)
        let filtered: HashMap<_, _> = config.services
            .iter()
            .filter(|(k, _)| services_to_restart.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let start_order = get_start_order(&filtered).unwrap_or_else(|_| services_to_restart.clone());
        let mut stop_order = start_order.clone();
        stop_order.reverse();

        // Run global pre_restart hook for full restart
        if is_full_restart
            && let Some(ref log_cfg) = log_config
            && let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::PreRestart,
                &GlobalHookParams {
                    working_dir: &config_dir,
                    env: &stored_env,
                    log_config: Some(log_cfg),
                    global_log_config: global_log_config.as_ref(),
                    progress: &None,
                    handle: Some(&handle),
                    lua_code: config.lua.as_deref(),
                },
            )
            .await
        {
            warn!("Global pre_restart hook failed: {}", e);
        }

        // Phase 1: Run pre_restart hooks and stop (reverse dependency order)
        for service_name in &stop_order {
            // Get service context
            let ctx = match handle.get_service_context(service_name).await {
                Some(ctx) => ctx,
                None => {
                    warn!("Service context not found for {}", service_name);
                    continue;
                }
            };

            // Emit Restart event
            handle
                .emit_event(
                    service_name,
                    ServiceEvent::Restart {
                        reason: RestartReason::Manual,
                    },
                )
                .await;

            // Run pre_restart hook
            if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PreRestart, &None, Some(&handle)).await {
                warn!("Hook pre_restart failed for {}: {}", service_name, e);
            }

            // Run pre_stop hook
            if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PreStop, &None, Some(&handle)).await {
                warn!("Hook pre_stop failed for {}: {}", service_name, e);
            }

            // Apply on_restart log retention
            self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Restart).await;

            // Stop process
            if let Err(e) = stop_service(service_name, handle.clone(), None).await {
                warn!("Failed to stop service {}: {}", service_name, e);
            }

            // Run post_stop hook
            if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostStop, &None, Some(&handle)).await {
                warn!("Hook post_stop failed for {}: {}", service_name, e);
            }

        }

        // Small delay between stop and start phases
        tokio::time::sleep(RESTART_DELAY).await;

        let mut restarted = Vec::new();

        // Phase 2: Start services (forward dependency order)
        for service_name in &start_order {
            // Re-resolve service config with updated context (restart_count, deps, etc.)
            let ctx = match self.re_resolve_service(&handle, service_name, None).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    warn!("Failed to re-resolve service {}: {}", service_name, e);
                    continue;
                }
            };

            // Emit Start event (restart includes a start)
            handle.emit_event(service_name, ServiceEvent::Start).await;

            // Run pre_start hook
            if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PreStart, &None, Some(&handle)).await {
                warn!("Hook pre_start failed for {}: {}", service_name, e);
                continue;
            }

            // Apply on_start log retention
            self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Start).await;

            // Spawn process
            match self.spawn_service(&handle, service_name, &ctx).await {
                Ok(()) => {
                    restarted.push(service_name.clone());

                    // Run post_start hook
                    if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostStart, &None, Some(&handle)).await {
                        warn!("Hook post_start failed for {}: {}", service_name, e);
                    }

                    // Run post_restart hook
                    if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostRestart, &None, Some(&handle)).await {
                        warn!("Hook post_restart failed for {}: {}", service_name, e);
                    }

                    // Spawn auxiliary tasks
                    self.spawn_auxiliary_tasks(&handle, service_name, &ctx).await;

                    // Increment restart count
                    if let Err(e) = handle.increment_restart_count(service_name).await {
                        warn!("Failed to increment restart count for {}: {}", service_name, e);
                    }
                }
                Err(e) => {
                    error!("Failed to spawn service {}: {}", service_name, e);
                    if let Err(err) = handle.set_service_status(service_name, ServiceStatus::Failed).await {
                        warn!("Failed to set {} to Failed: {}", service_name, err);
                    }
                }
            }
        }

        // Run global post_restart hook for full restart
        if is_full_restart
            && let Some(ref log_cfg) = log_config
            && let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::PostRestart,
                &GlobalHookParams {
                    working_dir: &config_dir,
                    env: &stored_env,
                    log_config: Some(log_cfg),
                    global_log_config: global_log_config.as_ref(),
                    progress: &None,
                    handle: Some(&handle),
                    lua_code: config.lua.as_deref(),
                },
            )
            .await
        {
            warn!("Global post_restart hook failed: {}", e);
        }

        if restarted.is_empty() {
            Ok("No services were restarted".to_string())
        } else {
            Ok(String::new())
        }
    }

    /// Recreate services for a config: stop --clean, re-bake, then start.
    ///
    /// This method performs a full recreate cycle:
    /// 1. Stop all running services with cleanup
    /// 2. Clear the config snapshot (forcing re-expansion)
    /// 3. Unload the config actor
    /// 4. Re-load the config with new sys_env
    /// 5. Persist the new snapshot
    /// 6. Start all services
    ///
    /// The `config_owner` parameter provides the UID/GID of the new CLI user.
    ///
    /// Use this when the config file has changed or you want to pick up
    /// new environment variables.
    pub async fn recreate_services(
        &self,
        config_path: &Path,
        sys_env: Option<HashMap<String, String>>,
        config_owner: Option<(u32, u32)>,
        progress: Option<ProgressSender>,
    ) -> Result<String, OrchestratorError> {
        info!("Recreating config for {:?}", config_path);

        // Stop all running services with cleanup if config is loaded
        if let Some(handle) = self.registry.get(&config_path.to_path_buf()) {
            if !handle.all_services_stopped().await {
                info!("Stopping all services before recreate for {:?}", config_path);
                self.stop_services(config_path, None, true, None).await?;
            }

            // Clear snapshot to force re-expansion with new env
            if let Err(e) = handle.clear_snapshot().await {
                warn!("Failed to clear snapshot: {}", e);
            }

            // Unload the config actor completely
            self.registry.unload(&config_path.to_path_buf()).await;
        }

        // Re-load config with new sys_env (re-reads source, re-expands env vars)
        let handle = self
            .registry
            .get_or_create(config_path.to_path_buf(), sys_env.clone(), config_owner)
            .await?;

        // Persist the rebaked config snapshot
        if let Err(e) = handle.take_snapshot_if_needed().await {
            warn!("Failed to take config snapshot: {}", e);
        }

        // Start all services
        self.start_services(config_path, None, sys_env, config_owner, progress).await?;

        Ok(String::new())
    }


    /// Restart a single service (used by file watcher)
    pub async fn restart_single_service(
        &self,
        config_path: &Path,
        service_name: &str,
    ) -> Result<(), OrchestratorError> {
        self.restart_single_service_with_reason(config_path, service_name, RestartReason::Watch)
            .await
    }

    /// Restart a single service with a specific reason
    pub async fn restart_single_service_with_reason(
        &self,
        config_path: &Path,
        service_name: &str,
        reason: RestartReason,
    ) -> Result<(), OrchestratorError> {
        info!(
            "Restarting service {} in {:?} (reason: {:?})",
            service_name, config_path, reason
        );

        let handle = self
            .registry
            .get(&config_path.to_path_buf())
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        // Emit Restart event
        handle
            .emit_event(
                service_name,
                ServiceEvent::Restart {
                    reason: reason.clone(),
                },
            )
            .await;

        // Get service context
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Run pre_restart hook
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::PreRestart, &None, Some(&handle))
            .await
        {
            warn!("Hook pre_restart failed for {}: {}", service_name, e);
        }

        // Run pre_stop hook
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::PreStop, &None, Some(&handle))
            .await
        {
            warn!("Hook pre_stop failed for {}: {}", service_name, e);
        }

        // Apply on_restart log retention
        self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Restart)
            .await;

        // Stop the service
        stop_service(service_name, handle.clone(), None)
            .await
            .map_err(|e| OrchestratorError::StopFailed(e.to_string()))?;

        // Run post_stop hook (after process stopped)
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::PostStop, &None, Some(&handle))
            .await
        {
            warn!("Hook post_stop failed for {}: {}", service_name, e);
        }

        // Small delay between stop and start
        tokio::time::sleep(RESTART_DELAY).await;

        // Re-resolve service config with updated context
        let ctx = self.re_resolve_service(&handle, service_name, None).await?;

        // Emit Start event (restart includes a start)
        handle.emit_event(service_name, ServiceEvent::Start).await;

        // Run pre_start hook (runs on every start, including restarts)
        self.run_service_hook(&ctx, service_name, ServiceHookType::PreStart, &None, Some(&handle))
            .await?;

        // Apply on_start log retention
        self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Start)
            .await;

        // Spawn the service
        self.spawn_service(&handle, service_name, &ctx).await?;

        // Run post_start hook (after process spawned)
        if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostStart, &None, Some(&handle)).await {
            warn!("Hook post_start failed for {}: {}", service_name, e);
        }

        // Run post_restart hook (after restart complete)
        if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostRestart, &None, Some(&handle)).await {
            warn!("Hook post_restart failed for {}: {}", service_name, e);
        }

        // Spawn auxiliary tasks (health checker, file watcher)
        self.spawn_auxiliary_tasks(&handle, service_name, &ctx).await;

        if let Err(e) = handle.increment_restart_count(service_name).await {
            warn!("Failed to increment restart count for {}: {}", service_name, e);
        }

        Ok(())
    }

    /// Handle a process exit event
    ///
    /// This method:
    /// - Records the exit in state
    /// - Runs post_exit hook
    /// - Applies on_exit log retention
    /// - Determines if restart is needed based on policy
    /// - If restarting: runs pre_restart/pre_start/post_start/post_restart hooks
    pub async fn handle_exit(
        &self,
        config_path: &Path,
        service_name: &str,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), OrchestratorError> {
        let handle = match self.registry.get(&config_path.to_path_buf()) {
            Some(h) => h,
            None => return Ok(()), // Config no longer loaded
        };

        // Get service context
        let ctx = match handle.get_service_context(service_name).await {
            Some(ctx) => ctx,
            None => return Ok(()), // Service no longer exists
        };

        // Emit Exit event
        handle
            .emit_event(service_name, ServiceEvent::Exit { code: exit_code })
            .await;

        // Take stdout/stderr capture tasks BEFORE record_process_exit,
        // which removes the ProcessHandle (and its tasks) from the actor.
        let output_tasks = handle.take_output_tasks(service_name).await;

        // Record process exit in state
        if let Err(e) = handle.record_process_exit(service_name, exit_code, signal).await {
            warn!("Failed to record process exit for {}: {}", service_name, e);
        }

        // Run post_exit hook
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::PostExit, &None, Some(&handle))
            .await
        {
            warn!("Hook post_exit failed for {}: {}", service_name, e);
        }

        // Apply on_exit log retention
        self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Exit)
            .await;

        // Guard against stale exit events.
        //
        // Exit events arrive asynchronously. By the time we process one, the service
        // may have already been:
        //   - Stopped/Stopping: explicit `kepler stop` — don't restart
        //   - Waiting: a new start cycle re-queued the service — don't interfere
        //   - Exited/Failed/Killed/Skipped: already handled — don't restart again
        //
        // Only restart if the service is still in an actively-running state
        // (Running, Healthy, Unhealthy, Starting) — these are the states where
        // a process exit is expected to trigger the restart policy.
        let current_state = handle.get_service_state(service_name).await;
        let was_actively_running = current_state
            .as_ref()
            .is_some_and(|s| matches!(
                s.status,
                ServiceStatus::Running
                | ServiceStatus::Healthy
                | ServiceStatus::Unhealthy
                | ServiceStatus::Starting
            ));

        // Determine if we should restart (use resolved config or fall back to raw)
        let should_restart = was_actively_running && {
            if let Some(ref resolved) = ctx.resolved_config {
                resolved.restart.should_restart_on_exit(exit_code)
            } else {
                let restart = ctx.service_config.restart.as_static().cloned().unwrap_or_default();
                restart.should_restart_on_exit(exit_code)
            }
        };

        if should_restart {
            // Emit Restart event with Failure reason
            handle
                .emit_event(
                    service_name,
                    ServiceEvent::Restart {
                        reason: RestartReason::Failure { exit_code },
                    },
                )
                .await;

            // Increment restart count and set status to starting
            if let Err(e) = handle.increment_restart_count(service_name).await {
                warn!("Failed to increment restart count for {}: {}", service_name, e);
            }
            if let Err(e) = handle
                .set_service_status(service_name, ServiceStatus::Starting)
                .await
            {
                warn!("Failed to set {} to Starting: {}", service_name, e);
            }

            // Small delay before restart
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let restart_policy = ctx.resolved_config.as_ref()
                .map(|c| c.restart.policy().clone())
                .unwrap_or_else(|| ctx.service_config.restart.as_static().cloned().unwrap_or_default().policy().clone());
            info!(
                "Restarting service {} (policy: {:?})",
                service_name,
                restart_policy
            );

            // Run pre_restart hook
            if let Err(e) = self
                .run_service_hook(&ctx, service_name, ServiceHookType::PreRestart, &None, Some(&handle))
                .await
            {
                warn!("Hook pre_restart failed for {}: {}", service_name, e);
            }

            // Run pre_start hook
            if let Err(e) = self
                .run_service_hook(&ctx, service_name, ServiceHookType::PreStart, &None, Some(&handle))
                .await
            {
                warn!("Hook pre_start failed for {}: {}", service_name, e);
            }

            // Apply on_restart log retention
            self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Restart)
                .await;

            // Re-resolve service config with updated restart_count, exit_code, deps
            let ctx = self.re_resolve_service(&handle, service_name, exit_code).await?;

            // Spawn new process
            match self.spawn_service(&handle, service_name, &ctx).await {
                Ok(()) => {
                    // Run post_start hook (after process spawned)
                    if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostStart, &None, Some(&handle)).await {
                        warn!("Hook post_start failed for {}: {}", service_name, e);
                    }

                    // Run post_restart hook (after restart complete)
                    if let Err(e) = self.run_service_hook(&ctx, service_name, ServiceHookType::PostRestart, &None, Some(&handle)).await {
                        warn!("Hook post_restart failed for {}: {}", service_name, e);
                    }

                    // Spawn auxiliary tasks (health checker, file watcher)
                    self.spawn_auxiliary_tasks(&handle, service_name, &ctx).await;

                    let _ = handle
                        .set_service_status(service_name, ServiceStatus::Running)
                        .await;
                }
                Err(e) => {
                    error!("Failed to restart service {}: {}", service_name, e);
                    let _ = handle
                        .set_service_status(service_name, ServiceStatus::Failed)
                        .await;
                }
            }
        } else {
            // Wait for stdout/stderr capture tasks to finish flushing logs to disk.
            // The process has exited but the capture tasks may still be reading
            // remaining pipe data and writing it to log files.
            let (stdout_task, stderr_task) = output_tasks;
            let mut captured_lines: Option<Vec<String>> = None;
            if let Some(task) = stdout_task {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    task,
                ).await {
                    Ok(Ok(lines)) => { captured_lines = lines; }
                    _ => {}
                }
            }
            if let Some(task) = stderr_task {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    task,
                ).await;
            }

            // Process output capture: write captured KEY=VALUE lines to disk
            let config_dir = handle.get_config_dir().await;
            if let Some(lines) = captured_lines {
                let process_outputs = crate::outputs::parse_capture_lines(&lines);
                if !process_outputs.is_empty() {
                    if let Err(e) = crate::outputs::write_process_outputs(&config_dir, service_name, &process_outputs) {
                        warn!("Failed to write process outputs for {}: {}", service_name, e);
                    }
                }
            }

            // Resolve service `outputs:` declarations (if any)
            // This happens after hooks and process have completed, so ctx.hooks.* is available.
            if let Some(config) = handle.get_config().await {
                if let Some(raw) = config.services.get(service_name) {
                    if !raw.outputs.is_static_none() {
                        // Build eval context with hook outputs for resolving ${{ }} expressions
                        let hook_outputs = crate::outputs::read_all_hook_outputs(&config_dir, service_name);
                        let process_outputs = crate::outputs::read_process_outputs(&config_dir, service_name);

                        let eval_ctx = EvalContext {
                            env: ctx.env.clone(),
                            service_name: Some(service_name.to_string()),
                            hooks: hook_outputs,
                            ..Default::default()
                        };

                        let evaluator = match config.create_lua_evaluator() {
                            Ok(e) => e,
                            Err(e) => {
                                warn!("Failed to create evaluator for outputs resolution: {}", e);
                                // Continue without resolving outputs
                                if was_actively_running {
                                    let status = if signal.is_some() {
                                        ServiceStatus::Killed
                                    } else {
                                        ServiceStatus::Exited
                                    };
                                    if let Err(e) = handle.set_service_status(service_name, status).await {
                                        warn!("Failed to set {} to {:?}: {}", service_name, status, e);
                                    }
                                }
                                return Ok(());
                            }
                        };

                        match raw.outputs.resolve(&evaluator, &eval_ctx, handle.config_path(), &format!("{}.outputs", service_name)) {
                            Ok(Some(declared_outputs)) => {
                                // Merge: process outputs + declared outputs (declared take precedence)
                                let mut final_outputs = process_outputs;
                                final_outputs.extend(declared_outputs);
                                if let Err(e) = crate::outputs::write_resolved_outputs(&config_dir, service_name, &final_outputs) {
                                    warn!("Failed to write resolved outputs for {}: {}", service_name, e);
                                }
                            }
                            Ok(None) => {
                                // No declared outputs, but we may still have process outputs —
                                // write them as resolved so dependents can read them
                                if !crate::outputs::read_process_outputs(&config_dir, service_name).is_empty() {
                                    // Process outputs are already available via read_service_outputs
                                }
                            }
                            Err(e) => {
                                warn!("Failed to resolve outputs for {}: {}", service_name, e);
                            }
                        }
                    }
                }
            }

            // Only update status if the service was actively running.
            // Don't override Stopped, Waiting, or other states set by stop/start cycles.
            if was_actively_running {
                let status = if signal.is_some() {
                    ServiceStatus::Killed
                } else {
                    ServiceStatus::Exited
                };
                if let Err(e) = handle.set_service_status(service_name, status).await {
                    warn!("Failed to set {} to {:?}: {}", service_name, status, e);
                }
            }
        }

        Ok(())
    }

    /// Handle a file change event by restarting the affected service
    pub async fn handle_file_change(&self, event: FileChangeEvent) {
        info!(
            "File change detected for {} in {:?}, restarting",
            event.service_name, event.config_path
        );

        if let Err(e) = self
            .restart_single_service(&event.config_path, &event.service_name)
            .await
        {
            error!(
                "Failed to restart service {} after file change: {}",
                event.service_name, e
            );
        }
    }

    /// Spawn a task to handle file change events from a receiver (each change handled concurrently)
    pub fn spawn_file_change_handler(
        self: std::sync::Arc<Self>,
        mut restart_rx: mpsc::Receiver<FileChangeEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = restart_rx.recv().await {
                let orch = Arc::clone(&self);
                tokio::spawn(async move {
                    orch.handle_file_change(event).await;
                });
            }
        })
    }

    // --- Internal helpers ---

    /// Check if a service needs starting.
    ///
    /// Returns false for:
    /// - Already active services (Running, Healthy, Waiting, Starting, etc.)
    /// - Completed one-shots: Exited/Failed/Killed where restart policy says don't restart
    /// - Skipped services
    ///
    /// Returns true for:
    /// - Stopped services (user explicitly stopped → always re-start on `kepler start`)
    /// - Services whose restart policy says they should restart on their exit code
    async fn service_needs_starting(
        &self,
        service_name: &str,
        config: &KeplerConfig,
        handle: &ConfigActorHandle,
    ) -> bool {
        let state = match handle.get_service_state(service_name).await {
            Some(s) => s,
            None => return false,
        };

        // Already active (Running, Healthy, Waiting, Starting, Stopping, etc.)
        if state.status.is_active() {
            return false;
        }

        // Skipped services are "done"
        if state.status == ServiceStatus::Skipped {
            return false;
        }

        // Stopped by user — always re-start on `kepler start`
        if state.status == ServiceStatus::Stopped {
            return true;
        }

        // Exited/Failed/Killed — check restart policy
        if let Some(raw) = config.services.get(service_name) {
            let restart = raw.restart.as_static().cloned().unwrap_or_default();
            if !restart.should_restart_on_exit(state.exit_code) {
                // Restart policy says don't restart → service is "done"
                return false;
            }
        }

        true
    }

    /// Run a service hook, forwarding the progress sender for event emission.
    /// Requires resolved_config to be available in the ServiceContext.
    ///
    /// Hooks are resolved here (not in `resolve_service`) so that `hook_name`
    /// is available in the `EvalContext` for `!lua` blocks.
    ///
    /// Returns `Ok(())` on success, writing any captured hook step outputs to disk.
    async fn run_service_hook(
        &self,
        ctx: &ServiceContext,
        service_name: &str,
        hook_type: ServiceHookType,
        progress: &Option<ProgressSender>,
        handle: Option<&ConfigActorHandle>,
    ) -> Result<(), OrchestratorError> {
        let resolved = match ctx.resolved_config.as_ref() {
            Some(c) => c,
            None => {
                debug!("No resolved config for {}, skipping hook {:?}", service_name, hook_type);
                return Ok(());
            }
        };

        let mut hook_params = ServiceHookParams::from_service_context(
            resolved,
            &ctx.working_dir,
            &ctx.env,
            Some(&ctx.log_config),
            ctx.global_log_config.as_ref(),
        );

        // Read prior hook outputs from disk and set output_max_size
        hook_params.all_hook_outputs = crate::outputs::read_all_hook_outputs(&ctx.config_dir, service_name);
        if let Some(h) = handle {
            if let Some(config) = h.get_config().await {
                hook_params.output_max_size = config.output_max_size();
            }
        }

        // For static hooks, use resolved.hooks (already resolved in resolve_service).
        // For dynamic hooks (!lua / ${{ }}), resolve here with hook_name in context.
        // lua_code is hoisted so it outlives hook_params.lua_code (a borrowed reference).
        let mut lua_code: Option<String> = None;
        let hooks: Option<ServiceHooks> = if resolved.hooks.is_some() || ctx.service_config.hooks.as_static().is_some() {
            // Static hooks — already resolved, use them directly.
            // Gather deps info and lua_code for ${{ }} evaluation in hook env.
            if let Some(h) = handle {
                let depends_on = &resolved.depends_on;
                for (dep_name, _) in depends_on.iter() {
                    if let Some(dep_state) = h.get_service_state(dep_name).await {
                        hook_params.deps.insert(dep_name.to_string(), DepInfo {
                            status: dep_state.status.as_str().to_string(),
                            exit_code: dep_state.exit_code,
                            initialized: dep_state.initialized,
                            restart_count: dep_state.restart_count,
                            env: dep_state.computed_env.clone(),
                            ..Default::default()
                        });
                    }
                }
                lua_code = h.get_config().await.and_then(|c| c.lua.clone());
            }
            resolved.hooks.clone()
        } else if let Some(h) = handle {
            // Dynamic hooks — need full Lua resolution with hook_name in context
            let depends_on = &resolved.depends_on;
            for (dep_name, _) in depends_on.iter() {
                if let Some(dep_state) = h.get_service_state(dep_name).await {
                    hook_params.deps.insert(dep_name.to_string(), DepInfo {
                        status: dep_state.status.as_str().to_string(),
                        exit_code: dep_state.exit_code,
                        initialized: dep_state.initialized,
                        restart_count: dep_state.restart_count,
                        env: dep_state.computed_env.clone(),
                        ..Default::default()
                    });
                }
            }

            let config = h.get_config().await
                .ok_or(OrchestratorError::ServiceContextNotFound)?;
            let raw = config.services.get(service_name)
                .ok_or(OrchestratorError::ServiceContextNotFound)?;

            let evaluator = config.create_lua_evaluator()
                .map_err(|e| OrchestratorError::ConfigError(e.to_string()))?;

            let state = h.get_service_state(service_name).await;
            let eval_ctx = EvalContext {
                env: ctx.env.clone(),
                service_name: Some(service_name.to_string()),
                hook_name: Some(hook_type.as_str().to_string()),
                initialized: state.as_ref().map(|s| s.initialized),
                restart_count: state.as_ref().map(|s| s.restart_count),
                exit_code: state.as_ref().and_then(|s| s.exit_code),
                status: state.as_ref().map(|s| s.status.as_str().to_string()),
                deps: hook_params.deps.clone(),
                ..Default::default()
            };

            lua_code = config.lua.clone();

            raw.hooks.resolve(&evaluator, &eval_ctx, h.config_path(), &format!("{}.hooks", service_name))
                .map_err(|e| OrchestratorError::ConfigError(e.to_string()))?
        } else {
            // No handle and dynamic hooks — cannot resolve
            None
        };
        hook_params.lua_code = lua_code.as_deref();

        let step_outputs = run_service_hook(
            &hooks,
            hook_type,
            service_name,
            &hook_params,
            progress,
            handle,
        )
        .await
        .map_err(|e| OrchestratorError::HookFailed(e.to_string()))?;

        // Write captured step outputs to disk
        for (step_name, outputs) in &step_outputs {
            if let Err(e) = crate::outputs::write_hook_step_outputs(
                &ctx.config_dir,
                service_name,
                hook_type.as_str(),
                step_name,
                outputs,
            ) {
                warn!("Failed to write hook step outputs for {}/{}/{}: {}", service_name, hook_type.as_str(), step_name, e);
            }
        }

        Ok(())
    }

    /// Apply log retention for an event
    async fn apply_retention(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        ctx: &ServiceContext,
        event: LifecycleEvent,
    ) {
        let service_logs = ctx.resolved_config.as_ref().and_then(|c| c.logs.as_ref());
        let retention = match event {
            LifecycleEvent::Start => resolve_log_retention(
                service_logs,
                ctx.global_log_config.as_ref(),
                |l| l.get_on_start(),
                LogRetention::Retain,
            ),
            LifecycleEvent::Stop => resolve_log_retention(
                service_logs,
                ctx.global_log_config.as_ref(),
                |l| l.get_on_stop(),
                LogRetention::Clear,
            ),
            LifecycleEvent::Restart => resolve_log_retention(
                service_logs,
                ctx.global_log_config.as_ref(),
                |l| l.get_on_restart(),
                LogRetention::Retain,
            ),
            LifecycleEvent::Exit => resolve_log_retention(
                service_logs,
                ctx.global_log_config.as_ref(),
                |l| l.get_on_exit(),
                LogRetention::Retain,
            ),
            LifecycleEvent::Init => return, // No retention for init
        };

        if retention == LogRetention::Clear {
            handle.clear_service_logs(service_name).await;
            handle
                .clear_service_logs_prefix(&format!("{}.", service_name))
                .await;
        }
    }

    /// Spawn a service process
    async fn spawn_service(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        ctx: &ServiceContext,
    ) -> Result<(), OrchestratorError> {
        let resolved = ctx.resolved_config.as_ref()
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Build output capture config if service has `output: true`
        let output_capture = if resolved.output == Some(true) {
            let max_size = handle.get_config().await
                .map(|c| c.output_max_size())
                .unwrap_or(1024 * 1024);
            Some(crate::process::OutputCaptureConfig { max_size })
        } else {
            None
        };

        let spawn_params = SpawnServiceParams {
            service_name,
            service_config: resolved,
            config_dir: &ctx.config_dir,
            log_config: ctx.log_config.clone(),
            handle: handle.clone(),
            exit_tx: self.exit_tx.clone(),
            global_log_config: ctx.global_log_config.as_ref(),
            output_capture,
        };

        let process_handle = spawn_service(spawn_params)
            .await
            .map_err(|e| OrchestratorError::SpawnFailed(e.to_string()))?;

        // Store process handle
        handle.store_process_handle(service_name, process_handle).await;

        // Update status to running
        let _ = handle
            .set_service_status(service_name, ServiceStatus::Running)
            .await;

        Ok(())
    }

    /// Spawn auxiliary tasks (health checker, file watcher)
    async fn spawn_auxiliary_tasks(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        ctx: &ServiceContext,
    ) {
        let resolved = match ctx.resolved_config.as_ref() {
            Some(c) => c,
            None => return,
        };

        // Start health check if configured
        if let Some(health_config) = &resolved.healthcheck {
            let task_handle = spawn_health_checker(
                service_name.to_string(),
                health_config.clone(),
                handle.clone(),
            );
            handle
                .store_task_handle(service_name, TaskHandleType::HealthCheck, task_handle)
                .await;
        }

        // Start file watcher if configured
        if !resolved.restart.watch_patterns().is_empty() {
            let task_handle = spawn_file_watcher(
                handle.config_path().to_path_buf(),
                service_name.to_string(),
                resolved.restart.watch_patterns().to_vec(),
                ctx.working_dir.clone(),
                self.restart_tx.clone(),
            );
            handle
                .store_task_handle(service_name, TaskHandleType::FileWatcher, task_handle)
                .await;
        }
    }

    /// Prune all stopped/orphaned config state directories
    ///
    /// Scans `~/.kepler/configs/` for all config state directories and:
    /// - For each config: verifies all services are stopped (or orphaned)
    /// - Runs the global `on_cleanup` hook (if config readable)
    /// - Deletes the config's state directory entirely
    /// - Reports what was pruned
    pub async fn prune_all(
        &self,
        force: bool,
        dry_run: bool,
    ) -> Result<Vec<PrunedConfigInfo>, OrchestratorError> {
        use crate::persistence::ConfigPersistence;

        let configs_dir = crate::global_state_dir()?.join("configs");

        if !configs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();

        // Scan all config hash directories
        let entries = std::fs::read_dir(&configs_dir)
            .map_err(|e| OrchestratorError::Io(e.to_string()))?;

        for entry in entries.flatten() {
            let hash = entry.file_name().to_string_lossy().to_string();
            let state_dir = entry.path();

            if !state_dir.is_dir() {
                continue;
            }

            // Use ConfigPersistence to get the ORIGINAL source path (not state dir path)
            // This is critical for correctly unloading from the registry
            let persistence = ConfigPersistence::new(state_dir.clone());
            let (original_path, is_orphaned) = match persistence.load_source_path() {
                Ok(Some(source_path)) => {
                    // Check if the original source file still exists
                    let exists = source_path.exists();
                    (source_path.display().to_string(), !exists)
                }
                _ => {
                    // No source_path.txt or failed to read - check if config.yaml is readable
                    let config_file = state_dir.join("config.yaml");
                    let empty_env = HashMap::new();
                    if config_file.exists() && KeplerConfig::load(&config_file, &empty_env).is_ok() {
                        // Config is readable but no source path - treat as orphaned
                        ("unknown (orphaned)".to_string(), true)
                    } else {
                        ("unknown (orphaned)".to_string(), true)
                    }
                }
            };

            // Check if safe to prune
            let can_prune = force || is_orphaned || self.registry.can_prune_config(&hash).await;

            if !can_prune {
                results.push(PrunedConfigInfo {
                    config_path: original_path.clone(),
                    config_hash: hash,
                    bytes_freed: 0,
                    status: "skipped (running)".to_string(),
                });
                continue;
            }

            let size = dir_size(&state_dir);

            if dry_run {
                results.push(PrunedConfigInfo {
                    config_path: original_path,
                    config_hash: hash,
                    bytes_freed: size,
                    status: if is_orphaned {
                        "would_prune (orphaned)".to_string()
                    } else {
                        "would_prune".to_string()
                    },
                });
                continue;
            }

            // Run cleanup hook if snapshot exists (not orphaned)
            if !is_orphaned {
                self.run_cleanup_hook_for_prune(&state_dir).await;
            }

            // Unload from registry BEFORE deleting state directory
            // Use the ORIGINAL source path which matches the registry key
            if !is_orphaned
                && let Ok(canonical) = PathBuf::from(&original_path).canonicalize() {
                    self.registry.unload(&canonical).await;
                }

            // Delete entire state directory
            if let Err(e) = std::fs::remove_dir_all(&state_dir) {
                error!("Failed to remove {}: {}", state_dir.display(), e);
                results.push(PrunedConfigInfo {
                    config_path: original_path,
                    config_hash: hash,
                    bytes_freed: 0,
                    status: format!("failed: {}", e),
                });
                continue;
            }

            results.push(PrunedConfigInfo {
                config_path: original_path,
                config_hash: hash,
                bytes_freed: size,
                status: if is_orphaned {
                    "pruned (orphaned)".to_string()
                } else {
                    "pruned".to_string()
                },
            });

            info!("Pruned config state directory: {}", state_dir.display());
        }

        Ok(results)
    }

    /// Run pre_cleanup hook before pruning
    ///
    /// Loads the baked config and sys_env directly from the persisted snapshot,
    /// avoiding the need to re-parse the raw config.
    async fn run_cleanup_hook_for_prune(&self, state_dir: &Path) {
        use crate::persistence::ConfigPersistence;
        let persistence = ConfigPersistence::new(state_dir.to_path_buf());

        let snapshot = match persistence.load_expanded_config() {
            Ok(Some(s)) => s,
            _ => return,
        };

        if let Err(e) = run_global_hook(
            &snapshot.config.global_hooks().cloned(),
            GlobalHookType::PreCleanup,
            &GlobalHookParams {
                working_dir: &snapshot.config_dir,
                env: &snapshot.sys_env,
                log_config: None,
                global_log_config: snapshot.config.global_logs(),
                progress: &None,
                handle: None,
                lua_code: snapshot.config.lua.as_deref(),
            },
        )
        .await
        {
            warn!("pre_cleanup hook failed: {}", e);
        }
    }

    /// Spawn an event handler for a config
    ///
    /// This creates a ServiceEventHandler that processes events from all services
    /// in the config and handles restart propagation.
    pub async fn spawn_event_handler(
        self: &Arc<Self>,
        config_path: PathBuf,
        handle: ConfigActorHandle,
    ) {
        // Create an aggregate channel for all service events
        let (aggregate_tx, aggregate_rx) = mpsc::channel(1000);

        // Spawn forwarders for each service (returns tracked JoinHandles)
        let forwarder_handles = spawn_event_forwarders(&handle, aggregate_tx).await;

        let event_handler = ServiceEventHandler::new(
            aggregate_rx,
            Arc::clone(self),
            config_path,
            handle.clone(),
        );

        let handler_handle = tokio::spawn(async move {
            event_handler.run().await;
        });

        // Store task handles in ConfigActor for cleanup tracking
        handle.store_event_handler_tasks(handler_handle, forwarder_handles).await;
    }
}

/// Calculate the size of a directory recursively
fn dir_size(path: &Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                size += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if path.is_dir() {
                size += dir_size(&path);
            }
        }
    }
    size
}
