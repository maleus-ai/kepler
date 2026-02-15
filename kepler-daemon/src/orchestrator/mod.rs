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

use crate::config::{resolve_log_retention, GlobalHooks, KeplerConfig, LogConfig, LogRetention, ServiceConfig};
use crate::config_actor::{ConfigActorHandle, ServiceContext, TaskHandleType};
use crate::config_registry::SharedConfigRegistry;
use crate::deps::{check_dependency_satisfied, get_start_levels, get_start_order, get_stop_order, is_dependency_permanently_unsatisfied};
use crate::events::{RestartReason, ServiceEvent};
use crate::health::spawn_health_checker;
use crate::hooks::{
    run_global_hook, run_service_hook, GlobalHookType, ServiceHookParams, ServiceHookType,
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

        // Check if any services need starting (are in a terminal state).
        // If all are already active, skip global hooks and return early.
        let any_need_starting = {
            let mut found = false;
            for svc in &services_to_start {
                let is_active = handle
                    .get_service_state(svc)
                    .await
                    .map(|s| s.status.is_active())
                    .unwrap_or(false);
                if !is_active {
                    found = true;
                    break;
                }
            }
            found
        };

        if !any_need_starting {
            return Ok("All services already running".to_string());
        }

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
                &config_dir,
                &stored_env,
                Some(&log_config),
                global_log_config.as_ref(),
                &progress,
                Some(&handle),
            )
            .await?;
        }

        // Start services
        let (started, deferred_services) = if is_specific_service {
            // Start only the named service (no transitive deps)
            let mut started = Vec::new();
            for service_name in &services_to_start {
                if handle.is_service_running(service_name).await {
                    debug!("Service {} is already running", service_name);
                    continue;
                }

                match self.start_single_service(&handle, service_name, &progress).await {
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
            (started, Vec::new())
        } else {
            // Use wait field to partition services (always resolved after config load)
            let startup_services: Vec<String> = services_to_start.iter()
                .filter(|s| config.services.get(*s).is_none_or(|c| c.wait.unwrap_or(true)))
                .cloned()
                .collect();
            let deferred_services: Vec<String> = services_to_start.iter()
                .filter(|s| config.services.get(*s).is_some_and(|c| !c.wait.unwrap_or(true)))
                .cloned()
                .collect();

            // Start startup cluster services
            let started = if !startup_services.is_empty() {
                self.start_services_by_level(&startup_services, &config, &handle, true, &progress).await?
            } else {
                Vec::new()
            };

            // Post-startup work (snapshot, event handler, global post_start)
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
                run_global_hooks: true,
                progress: &progress,
            }).await?;

            // Spawn deferred services in background (don't block the response)
            if !deferred_services.is_empty() {
                let self_clone = self.clone();
                let config_clone = config.clone();
                let handle_clone = handle.clone();
                let deferred = deferred_services.clone();
                tokio::spawn(async move {
                    if let Err(e) = self_clone.start_services_by_level(
                        &deferred,
                        &config_clone,
                        &handle_clone,
                        false,
                        &None, // No progress for deferred (background) services
                    ).await {
                        warn!("Error starting deferred services: {}", e);
                    }
                });
            }

            (started, deferred_services)
        };

        // Run post-startup work for specific service start
        if is_specific_service {
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
        }

        if started.is_empty() && deferred_services.is_empty() {
            Ok("All services already running".to_string())
        } else {
            let mut msg = String::new();
            if !started.is_empty() {
                msg.push_str(&format!("Started services: {}", started.join(", ")));
            }
            if !deferred_services.is_empty() {
                if !msg.is_empty() {
                    msg.push_str(" | ");
                }
                msg.push_str(&format!("deferred: {}", deferred_services.join(", ")));
            }
            Ok(msg)
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
            let needs_event_handler = ctx.config.services.values().any(|svc| {
                !svc.depends_on.dependencies_with_restart().is_empty()
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
                    ctx.config_dir,
                    ctx.stored_env,
                    Some(ctx.log_config),
                    ctx.global_log_config,
                    ctx.progress,
                    Some(ctx.handle),
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

    /// Start a single service
    async fn start_single_service(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        progress: &Option<ProgressSender>,
    ) -> Result<(), OrchestratorError> {
        // Atomically claim this service for starting. If another concurrent
        // request already claimed it (or it's already running), skip.
        if !handle.claim_service_start(service_name).await {
            debug!("Service {} already being started or running, skipping", service_name);
            return Ok(());
        }

        // Run the actual startup. If anything fails after claiming, reset
        // status to Failed so the service isn't stuck in Starting forever.
        match self.execute_service_startup(handle, service_name, progress).await {
            Ok(()) => Ok(()),
            Err(OrchestratorError::DependencySkipped { .. }) => {
                let _ = handle.set_service_status(service_name, ServiceStatus::Skipped).await;
                Ok(())
            }
            Err(e) => {
                let _ = handle.set_service_status(service_name, ServiceStatus::Failed).await;
                Err(e)
            }
        }
    }

    /// Execute the actual service startup sequence (after claiming).
    async fn execute_service_startup(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        progress: &Option<ProgressSender>,
    ) -> Result<(), OrchestratorError> {
        // Get service context (single round-trip)
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Wait for dependencies to satisfy their conditions
        self.wait_for_dependencies(handle, service_name, &ctx.service_config)
            .await?;

        // Build dependency state map for if-conditions and hooks
        let mut dep_infos = HashMap::new();
        for (dep_name, _) in ctx.service_config.depends_on.iter() {
            if let Some(dep_state) = handle.get_service_state(&dep_name).await {
                dep_infos.insert(dep_name.clone(), DepInfo {
                    status: dep_state.status.as_str().to_string(),
                    exit_code: dep_state.exit_code,
                    initialized: dep_state.initialized,
                    restart_count: dep_state.restart_count,
                });
            }
        }

        // Evaluate service-level `if` condition
        if let Some(ref condition) = ctx.service_config.condition {
            let state = handle.get_service_state(service_name).await;
            let eval_ctx = crate::lua_eval::EvalContext {
                env: ctx.env.clone(),
                service_name: Some(service_name.to_string()),
                initialized: state.as_ref().map(|s| s.initialized),
                restart_count: state.as_ref().map(|s| s.restart_count),
                exit_code: state.as_ref().and_then(|s| s.exit_code),
                status: state.as_ref().map(|s| s.status.as_str().to_string()),
                deps: dep_infos.clone(),
                ..Default::default()
            };
            match handle.eval_if_condition(condition, eval_ctx).await {
                Ok(true) => {} // condition passed, proceed
                Ok(false) => {
                    tracing::info!("Service {} skipped: if condition '{}' is falsy", service_name, condition);
                    handle.set_service_status(service_name, ServiceStatus::Skipped).await?;
                    return Ok(());
                }
                Err(e) => {
                    tracing::error!("Service {} if-condition error: {}", service_name, e);
                    return Err(OrchestratorError::HookFailed(format!("if condition error: {}", e)));
                }
            }
        }

        // Status already set to Starting by claim_service_start()

        // Run on_init hook if first time for this service
        let service_initialized = handle.is_service_initialized(service_name).await;

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

    /// Wait for all dependencies to satisfy their conditions
    async fn wait_for_dependencies(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        service_config: &ServiceConfig,
    ) -> Result<(), OrchestratorError> {
        // Get the full config for looking up dependency service configs
        let config = handle.get_config().await;

        // Get global timeout from config
        let global_timeout = config.as_ref()
            .and_then(|c| c.kepler.as_ref())
            .and_then(|k| k.timeout);

        for (dep_name, dep_config) in service_config.depends_on.iter() {
            let start = Instant::now();

            debug!(
                "Service {} waiting for dependency {} (condition: {:?})",
                service_name, dep_name, dep_config.condition
            );

            // Use per-dependency timeout, falling back to global timeout
            let effective_timeout = dep_config.timeout.or(global_timeout);
            let deadline = effective_timeout.map(|t| start + t);

            // Subscribe to state changes for instant notification
            let mut status_rx = handle.subscribe_state_changes();

            loop {
                if check_dependency_satisfied(&dep_name, &dep_config, handle).await {
                    debug!(
                        "Dependency {} satisfied for service {} (took {:?})",
                        dep_name,
                        service_name,
                        start.elapsed()
                    );
                    break;
                }

                // Check if dependency was skipped (cascade skip)
                if let Some(ref cfg) = config {
                    if let Some(dep_state) = handle.get_service_state(&dep_name).await {
                        if dep_state.status == ServiceStatus::Skipped {
                            let dep_config_entry = cfg.services.get(&dep_name)
                                .and_then(|_| service_config.depends_on.get(&dep_name));
                            let allow = dep_config_entry.map(|d| d.allow_skipped).unwrap_or(false);
                            if !allow {
                                return Err(OrchestratorError::DependencySkipped {
                                    service: service_name.to_string(),
                                    dependency: dep_name.clone(),
                                });
                            }
                        }
                    }
                }

                // Check if dependency is permanently unsatisfied
                if let Some(ref cfg) = config
                    && let Some(dep_svc_config) = cfg.services.get(&dep_name)
                    && is_dependency_permanently_unsatisfied(
                        &dep_name,
                        &dep_config,
                        handle,
                        dep_svc_config,
                    ).await
                {
                    let state = handle.get_service_state(&dep_name).await;
                    let reason = format!(
                        "dependency {} with exit code {:?}, won't restart (policy: {:?})",
                        state.as_ref().map(|s| s.status.as_str()).unwrap_or("unknown"),
                        state.as_ref().and_then(|s| s.exit_code),
                        dep_svc_config.restart.policy(),
                    );
                    return Err(OrchestratorError::DependencyUnsatisfied {
                        service: service_name.to_string(),
                        dependency: dep_name.clone(),
                        condition: dep_config.condition.clone(),
                        reason,
                    });
                }

                // Check if startup was cancelled during dependency wait
                let state = handle.get_service_state(service_name).await;
                if state.as_ref().map(|s| s.status) != Some(ServiceStatus::Starting) {
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
                            dependency: dep_name.clone(),
                            condition: dep_config.condition.clone(),
                        });
                    }
                    match tokio::time::timeout(remaining, status_rx.recv()).await {
                        Ok(result) => result,
                        Err(_) => {
                            return Err(OrchestratorError::DependencyTimeout {
                                service: service_name.to_string(),
                                dependency: dep_name.clone(),
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
                &config_dir,
                &stored_env,
                Some(log_cfg),
                global_log_config.as_ref(),
                &None,
                Some(&handle),
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
                &config_dir,
                &stored_env,
                Some(log_cfg),
                global_log_config.as_ref(),
                &None,
                Some(&handle),
            )
            .await
        {
            warn!("Global post_stop hook failed: {}", e);
        }

        // Run pre_cleanup if requested
        if service_filter.is_none() && clean {
            info!("Running cleanup hooks");

            // Emit Cleanup event for all services
            for service_name in &services_to_stop {
                handle.emit_event(service_name, ServiceEvent::Cleanup).await;
            }

            // Run service-level pre_cleanup hooks
            for service_name in &services_to_stop {
                let ctx = handle.get_service_context(service_name).await;
                if let Some(ref ctx) = ctx {
                    if let Err(e) = self.run_service_hook(ctx, service_name, ServiceHookType::PreCleanup, &None, Some(&handle)).await {
                        warn!("Hook pre_cleanup failed for {}: {}", service_name, e);
                    }
                }
            }

            // Run global pre_cleanup hook
            if let Some(ref log_cfg) = log_config
                && let Err(e) = run_global_hook(
                    &global_hooks,
                    GlobalHookType::PreCleanup,
                    &config_dir,
                    &stored_env,
                    Some(log_cfg),
                    global_log_config.as_ref(),
                    &None,
                    Some(&handle),
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
                        .and_then(|c| c.logs.as_ref());
                    resolve_log_retention(
                        service_logs,
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

            // Remove entire state directory (logs, config snapshots, env_files, etc.)
            let _cleaned = if state_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&state_dir) {
                    warn!("Failed to remove state directory {:?}: {}", state_dir, e);
                    false
                } else {
                    info!("Removed state directory: {:?}", state_dir);
                    true
                }
            } else {
                false
            };

            // Unload config from registry
            self.registry.unload(&config_path.to_path_buf()).await;

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
        _sys_env: Option<HashMap<String, String>>,
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
        // Stop order: reverse topological sort (dependents first, then dependencies)
        // Start order: forward topological sort (dependencies first, then dependents)
        let stop_order = {
            let filtered: HashMap<_, _> = config.services
                .iter()
                .filter(|(k, _)| services_to_restart.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            get_stop_order(&filtered).unwrap_or_else(|_| services_to_restart.clone())
        };

        let start_order = {
            let filtered: HashMap<_, _> = config.services
                .iter()
                .filter(|(k, _)| services_to_restart.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            get_start_order(&filtered).unwrap_or_else(|_| services_to_restart.clone())
        };

        // Run global pre_restart hook for full restart
        if is_full_restart
            && let Some(ref log_cfg) = log_config
            && let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::PreRestart,
                &config_dir,
                &stored_env,
                Some(log_cfg),
                global_log_config.as_ref(),
                &None,
                Some(&handle),
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
            // Refresh context after stop (state may have changed)
            let ctx = match handle.get_service_context(service_name).await {
                Some(ctx) => ctx,
                None => {
                    warn!("Service context not found for {}", service_name);
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
                    let _ = handle.increment_restart_count(service_name).await;
                }
                Err(e) => {
                    error!("Failed to spawn service {}: {}", service_name, e);
                    let _ = handle.set_service_status(service_name, ServiceStatus::Failed).await;
                }
            }
        }

        // Run global post_restart hook for full restart
        if is_full_restart
            && let Some(ref log_cfg) = log_config
            && let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::PostRestart,
                &config_dir,
                &stored_env,
                Some(log_cfg),
                global_log_config.as_ref(),
                &None,
                Some(&handle),
            )
            .await
        {
            warn!("Global post_restart hook failed: {}", e);
        }

        if restarted.is_empty() {
            Ok("No services were restarted".to_string())
        } else {
            Ok(format!("Restarted services: {}", restarted.join(", ")))
        }
    }

    /// Recreate services for a config (re-bake config only, no start/stop)
    ///
    /// This method only rebakes the config and persists the snapshot:
    /// - Requires all services to be stopped (or config not loaded)
    /// - Clears the config snapshot (forcing re-expansion)
    /// - Unloads the config actor
    /// - Re-loads the config with new sys_env
    /// - Persists the new snapshot
    /// - Does NOT start any services or run any hooks
    ///
    /// The `config_owner` parameter provides the UID/GID of the new CLI user.
    ///
    /// Use this when the config file has changed or you want to pick up
    /// new environment variables without starting services.
    pub async fn recreate_services(
        &self,
        config_path: &Path,
        sys_env: Option<HashMap<String, String>>,
        config_owner: Option<(u32, u32)>,
    ) -> Result<String, OrchestratorError> {
        info!("Recreating config for {:?} (rebake only)", config_path);

        // If config is loaded, check that all services are stopped
        if let Some(handle) = self.registry.get(&config_path.to_path_buf()) {
            if !handle.all_services_stopped().await {
                return Err(OrchestratorError::RecreateWhileRunning(
                    config_path.display().to_string(),
                ));
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
            .get_or_create(config_path.to_path_buf(), sys_env, config_owner)
            .await?;

        // Persist the rebaked config snapshot
        if let Err(e) = handle.take_snapshot_if_needed().await {
            warn!("Failed to take config snapshot: {}", e);
        }

        Ok("Config recreated successfully".to_string())
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

        // Refresh context after stop (state may have changed)
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

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

        let _ = handle.increment_restart_count(service_name).await;

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

        // Record process exit in state
        let _ = handle.record_process_exit(service_name, exit_code, signal).await;

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

        // Determine if we should restart
        let should_restart = ctx.service_config.restart.should_restart_on_exit(exit_code);

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
            let _ = handle.increment_restart_count(service_name).await;
            let _ = handle
                .set_service_status(service_name, ServiceStatus::Starting)
                .await;

            // Small delay before restart
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            info!(
                "Restarting service {} (policy: {:?})",
                service_name,
                ctx.service_config.restart.policy()
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

            // Refresh context (env may have changed if env_file was modified)
            let ctx = handle
                .get_service_context(service_name)
                .await
                .ok_or(OrchestratorError::ServiceContextNotFound)?;

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
            // Mark as killed (signal) or exited (any exit code)
            let status = if signal.is_some() {
                ServiceStatus::Killed
            } else {
                ServiceStatus::Exited
            };
            let _ = handle.set_service_status(service_name, status).await;
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

    /// Spawn a task to handle file change events from a receiver
    pub fn spawn_file_change_handler(
        self: std::sync::Arc<Self>,
        mut restart_rx: mpsc::Receiver<FileChangeEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = restart_rx.recv().await {
                self.handle_file_change(event).await;
            }
        })
    }

    /// Start services level-by-level in parallel within each level.
    ///
    /// If `fail_fast` is true, returns `Err` on the first service failure.
    /// If `fail_fast` is false, logs errors and continues (used for deferred/background startup).
    ///
    /// Returns the list of successfully started service names.
    async fn start_services_by_level(
        &self,
        services_to_start: &[String],
        config: &KeplerConfig,
        handle: &ConfigActorHandle,
        fail_fast: bool,
        progress: &Option<ProgressSender>,
    ) -> Result<Vec<String>, OrchestratorError> {
        let levels = get_start_levels(&config.services)?;
        let mut started = Vec::new();

        for level in levels {
            let to_start: Vec<_> = level
                .into_iter()
                .filter(|name| services_to_start.contains(name))
                .collect();

            if to_start.is_empty() {
                continue;
            }

            let mut tasks = Vec::new();
            for service_name in to_start {
                if handle.is_service_running(&service_name).await {
                    debug!("Service {} is already running", service_name);
                    continue;
                }

                let handle_clone = handle.clone();
                let service_name_clone = service_name.clone();
                let self_ref = self;
                let progress_ref = progress;
                tasks.push(async move {
                    let result = self_ref.start_single_service(&handle_clone, &service_name_clone, progress_ref).await;
                    (service_name_clone, result)
                });
            }

            let results = futures::future::join_all(tasks).await;

            for (service_name, result) in results {
                match result {
                    Ok(()) => started.push(service_name),
                    Err(OrchestratorError::StartupCancelled(_)) => {
                        debug!("Service {} startup was cancelled, skipping", service_name);
                    }
                    Err(OrchestratorError::DependencySkipped { .. }) => {
                        debug!("Service {} skipped due to dependency skip cascade", service_name);
                        let _ = handle.set_service_status(&service_name, ServiceStatus::Skipped).await;
                    }
                    Err(e) => {
                        if fail_fast {
                            error!("Failed to start service {}: {}", service_name, e);
                            return Err(e);
                        } else {
                            // For deferred services, mark as failed and continue
                            if matches!(e, OrchestratorError::DependencyUnsatisfied { .. }) {
                                warn!("{}", e);
                                let _ = handle.set_service_status(&service_name, ServiceStatus::Failed).await;
                            } else {
                                error!("Failed to start deferred service {}: {}", service_name, e);
                            }
                        }
                    }
                }
            }
        }

        Ok(started)
    }

    // --- Internal helpers ---

    /// Run a service hook, forwarding the progress sender for event emission.
    async fn run_service_hook(
        &self,
        ctx: &ServiceContext,
        service_name: &str,
        hook_type: ServiceHookType,
        progress: &Option<ProgressSender>,
        handle: Option<&ConfigActorHandle>,
    ) -> Result<(), OrchestratorError> {
        let mut hook_params = ServiceHookParams::from_service_context(
            &ctx.service_config,
            &ctx.working_dir,
            &ctx.env,
            Some(&ctx.log_config),
            ctx.global_log_config.as_ref(),
        );

        // Populate dependency state for hook conditions
        if let Some(h) = handle {
            for (dep_name, _) in ctx.service_config.depends_on.iter() {
                if let Some(dep_state) = h.get_service_state(&dep_name).await {
                    hook_params.deps.insert(dep_name.clone(), DepInfo {
                        status: dep_state.status.as_str().to_string(),
                        exit_code: dep_state.exit_code,
                        initialized: dep_state.initialized,
                        restart_count: dep_state.restart_count,
                    });
                }
            }
        }

        run_service_hook(
            &ctx.service_config.hooks,
            hook_type,
            service_name,
            &hook_params,
            progress,
            handle,
        )
        .await
        .map_err(|e| OrchestratorError::HookFailed(e.to_string()))
    }

    /// Apply log retention for an event
    async fn apply_retention(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
        ctx: &ServiceContext,
        event: LifecycleEvent,
    ) {
        let retention = match event {
            LifecycleEvent::Start => resolve_log_retention(
                ctx.service_config.logs.as_ref(),
                ctx.global_log_config.as_ref(),
                |l| l.get_on_start(),
                LogRetention::Retain,
            ),
            LifecycleEvent::Stop => resolve_log_retention(
                ctx.service_config.logs.as_ref(),
                ctx.global_log_config.as_ref(),
                |l| l.get_on_stop(),
                LogRetention::Clear,
            ),
            LifecycleEvent::Restart => resolve_log_retention(
                ctx.service_config.logs.as_ref(),
                ctx.global_log_config.as_ref(),
                |l| l.get_on_restart(),
                LogRetention::Retain,
            ),
            LifecycleEvent::Exit => resolve_log_retention(
                ctx.service_config.logs.as_ref(),
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
        let spawn_params = SpawnServiceParams {
            service_name,
            service_config: &ctx.service_config,
            config_dir: &ctx.config_dir,
            log_config: ctx.log_config.clone(),
            handle: handle.clone(),
            exit_tx: self.exit_tx.clone(),
            global_log_config: ctx.global_log_config.as_ref(),
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
        // Start health check if configured
        if let Some(health_config) = &ctx.service_config.healthcheck {
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
        if !ctx.service_config.restart.watch_patterns().is_empty() {
            let task_handle = spawn_file_watcher(
                handle.config_path().to_path_buf(),
                service_name.to_string(),
                ctx.service_config.restart.watch_patterns().to_vec(),
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
            &snapshot.config_dir,
            &snapshot.sys_env,
            None, // No log buffer for prune
            snapshot.config.global_logs(),
            &None,
            None, // No handle available during prune
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
    ) -> tokio::task::JoinHandle<()> {
        // Create an aggregate channel for all service events
        let (aggregate_tx, aggregate_rx) = mpsc::channel(1000);

        // Spawn forwarders for each service
        spawn_event_forwarders(&handle, aggregate_tx).await;

        let event_handler = ServiceEventHandler::new(
            aggregate_rx,
            Arc::clone(self),
            config_path,
            handle,
        );

        tokio::spawn(async move {
            event_handler.run().await;
        })
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
