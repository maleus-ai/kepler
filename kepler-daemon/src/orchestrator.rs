//! Service orchestration for lifecycle management.
//!
//! This module provides a centralized `ServiceOrchestrator` that handles all service
//! lifecycle operations (start, stop, restart, exit handling). It eliminates duplication
//! by providing unified methods that handle hooks, log retention, and state updates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use std::sync::Arc;
use std::time::Instant;

use crate::config::{resolve_log_retention, DependencyCondition, KeplerConfig, LogRetention, ServiceConfig};
use kepler_protocol::protocol::PrunedConfigInfo;
use crate::config_actor::{ConfigActorHandle, ServiceContext, TaskHandleType};
use crate::config_registry::SharedConfigRegistry;
use crate::deps::{check_dependency_satisfied, get_service_with_deps, get_start_levels, get_start_order, get_stop_order};
use crate::events::{RestartReason, ServiceEvent};
use crate::health::spawn_health_checker;
use crate::hooks::{
    run_global_hook, run_service_hook, GlobalHookType, ServiceHookParams, ServiceHookType,
    GLOBAL_HOOK_PREFIX,
};
use crate::process::{spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams};
use crate::state::ServiceStatus;
use crate::watcher::{spawn_file_watcher, FileChangeEvent};

/// Lifecycle events that trigger different hook/retention combinations
#[derive(Debug, Clone, Copy)]
pub enum LifecycleEvent {
    /// First start - on_init hook
    Init,
    /// Normal start - on_start hook, on_start retention
    Start,
    /// Normal stop - on_stop hook, on_stop retention
    Stop,
    /// Restart - on_restart hook, on_restart retention
    Restart,
    /// Process exit - on_exit hook, on_exit retention + auto-restart logic
    Exit,
}

/// Errors that can occur during service orchestration
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("Service context not found")]
    ServiceContextNotFound,

    #[error("Config not found: {0}")]
    ConfigNotFound(String),

    #[error("Service not found: {0}")]
    ServiceNotFound(String),

    #[error("Failed to stop service: {0}")]
    StopFailed(String),

    #[error("Failed to spawn service: {0}")]
    SpawnFailed(String),

    #[error("Hook failed: {0}")]
    HookFailed(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Dependency timeout: {service} timed out waiting for {dependency} to satisfy condition {condition:?}")]
    DependencyTimeout {
        service: String,
        dependency: String,
        condition: DependencyCondition,
    },

    #[error("Daemon error: {0}")]
    DaemonError(#[from] crate::errors::DaemonError),
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
    /// This method handles:
    /// - Loading/reloading the config (via registry)
    /// - Running global on_init hook (if first time)
    /// - Running global on_start hook
    /// - For each service:
    ///   - Running on_init hook (if first time)
    ///   - Running on_start hook
    ///   - Applying on_start log retention
    ///   - Spawning the process
    ///   - Spawning health check and file watcher tasks
    pub async fn start_services(
        &self,
        config_path: &Path,
        service_filter: Option<&str>,
        sys_env: Option<HashMap<String, String>>,
    ) -> Result<String, OrchestratorError> {
        // Get or create the config actor
        let handle = self
            .registry
            .get_or_create(config_path.to_path_buf(), sys_env.clone())
            .await?;

        let config_dir = handle.get_config_dir().await;

        // Get config and determine services to start
        let config = handle
            .get_config()
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let services_to_start = match service_filter {
            Some(name) => get_service_with_deps(name, &config.services)?,
            None => get_start_order(&config.services)?,
        };

        let log_config = handle
            .get_log_config()
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let global_hooks = config.global_hooks().cloned();
        let global_log_config = config.global_logs().cloned();
        let initialized = handle.is_config_initialized().await;

        // Run global on_init hook if first time
        if !initialized {
            let env = sys_env.clone().unwrap_or_else(|| std::env::vars().collect());
            run_global_hook(
                &global_hooks,
                GlobalHookType::OnInit,
                &config_dir,
                &env,
                Some(&log_config),
                global_log_config.as_ref(),
            )
            .await?;

            handle.mark_config_initialized().await?;
        }

        // Run global on_start hook
        let env = sys_env.clone().unwrap_or_else(|| std::env::vars().collect());
        run_global_hook(
            &global_hooks,
            GlobalHookType::OnStart,
            &config_dir,
            &env,
            Some(&log_config),
            global_log_config.as_ref(),
        )
        .await?;

        let mut started = Vec::new();

        // Get services grouped by dependency level for parallel execution
        // If starting a specific service, use sequential order (simpler)
        if service_filter.is_some() {
            // Sequential start for specific service + deps
            for service_name in &services_to_start {
                if handle.is_service_running(service_name).await {
                    debug!("Service {} is already running", service_name);
                    continue;
                }

                match self.start_single_service(&handle, service_name).await {
                    Ok(()) => started.push(service_name.clone()),
                    Err(e) => {
                        error!("Failed to start service {}: {}", service_name, e);
                        return Err(e);
                    }
                }
            }
        } else {
            // Parallel start: group services by dependency level
            let levels = get_start_levels(&config.services)?;

            for level in levels {
                // Filter to only services that need to be started
                let to_start: Vec<_> = level
                    .into_iter()
                    .filter(|name| services_to_start.contains(name))
                    .collect();

                if to_start.is_empty() {
                    continue;
                }

                // Check which services are already running
                let mut tasks = Vec::new();
                for service_name in to_start {
                    if handle.is_service_running(&service_name).await {
                        debug!("Service {} is already running", service_name);
                        continue;
                    }

                    // Start services at this level in parallel
                    let handle_clone = handle.clone();
                    let service_name_clone = service_name.clone();
                    let self_ref = self;
                    tasks.push(async move {
                        let result = self_ref.start_single_service(&handle_clone, &service_name_clone).await;
                        (service_name_clone, result) // Move instead of clone
                    });
                }

                // Wait for all services at this level to start
                let results = futures::future::join_all(tasks).await;

                // Check results and collect started services
                for (service_name, result) in results {
                    match result {
                        Ok(()) => started.push(service_name),
                        Err(e) => {
                            error!("Failed to start service {}: {}", service_name, e);
                            return Err(e);
                        }
                    }
                }
            }
        }

        // Take config snapshot on first successful service start
        // This captures the expanded environment variables at the time of first start
        if !started.is_empty() {
            if let Err(e) = handle.take_snapshot_if_needed().await {
                warn!("Failed to take config snapshot: {}", e);
            }

            // Check if any service has restart propagation enabled
            let needs_event_handler = config.services.values().any(|svc| {
                !svc.depends_on.dependencies_with_restart().is_empty()
            });

            // Spawn event handler for restart propagation if needed and not already running
            if needs_event_handler && !handle.has_event_handler().await {
                let self_arc = Arc::new(self.clone());
                let config_path_owned = config_path.to_path_buf();
                let handle_clone = handle.clone();
                self_arc.spawn_event_handler(config_path_owned, handle_clone).await;
                handle.set_event_handler_spawned().await;
            }
        }

        if started.is_empty() {
            Ok("All services already running".to_string())
        } else {
            Ok(format!("Started services: {}", started.join(", ")))
        }
    }

    /// Start a single service
    async fn start_single_service(
        &self,
        handle: &ConfigActorHandle,
        service_name: &str,
    ) -> Result<(), OrchestratorError> {
        // Get service context (single round-trip)
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Wait for dependencies to satisfy their conditions
        self.wait_for_dependencies(handle, service_name, &ctx.service_config)
            .await?;

        // Update status to starting
        let _ = handle
            .set_service_status(service_name, ServiceStatus::Starting)
            .await;

        // Run on_init hook if first time for this service
        let service_initialized = handle.is_service_initialized(service_name).await;

        if !service_initialized {
            // Emit Init event
            handle.emit_event(service_name, ServiceEvent::Init).await;

            self.run_service_hook(&ctx, service_name, ServiceHookType::OnInit)
                .await?;

            handle.mark_service_initialized(service_name).await?;
        }

        // Emit Start event
        handle.emit_event(service_name, ServiceEvent::Start).await;

        // Run on_start hook
        self.run_service_hook(&ctx, service_name, ServiceHookType::OnStart)
            .await?;

        // Apply on_start log retention
        self.apply_retention(handle, service_name, &ctx, LifecycleEvent::Start)
            .await;

        // Spawn process
        self.spawn_service(handle, service_name, &ctx).await?;

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
        for (dep_name, dep_config) in service_config.depends_on.iter() {
            let start = Instant::now();

            debug!(
                "Service {} waiting for dependency {} (condition: {:?})",
                service_name, dep_name, dep_config.condition
            );

            loop {
                if check_dependency_satisfied(&dep_name, &dep_config.condition, handle).await {
                    debug!(
                        "Dependency {} satisfied for service {} (took {:?})",
                        dep_name,
                        service_name,
                        start.elapsed()
                    );
                    break;
                }

                // Check timeout
                if let Some(timeout) = dep_config.timeout {
                    if start.elapsed() > timeout {
                        return Err(OrchestratorError::DependencyTimeout {
                            service: service_name.to_string(),
                            dependency: dep_name.clone(),
                            condition: dep_config.condition.clone(),
                        });
                    }
                }

                // Wait before checking again
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }

        Ok(())
    }

    /// Stop services for a config
    ///
    /// If `service_filter` is provided, only stops that service.
    /// Otherwise stops all services in reverse dependency order.
    ///
    /// If `clean` is true, also runs on_cleanup hooks and retention.
    pub async fn stop_services(
        &self,
        config_path: &Path,
        service_filter: Option<&str>,
        clean: bool,
    ) -> Result<String, OrchestratorError> {
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Get or create the actor (needed for cleanup hooks even if not started)
        let handle = if clean {
            Some(self.registry.get_or_create(config_path.to_path_buf(), None).await?)
        } else {
            self.registry.get(&config_path.to_path_buf())
        };

        let handle = match handle {
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

        let mut stopped = Vec::new();

        // Stop services in reverse dependency order
        for service_name in &services_to_stop {
            // Check if running
            let is_running = handle.is_service_running(service_name).await;

            if !is_running {
                continue;
            }

            // Emit Stop event
            handle.emit_event(service_name, ServiceEvent::Stop).await;

            // Get service context
            if let Some(ctx) = handle.get_service_context(service_name).await {
                // Run on_stop hook
                if let Err(e) = self
                    .run_service_hook(&ctx, service_name, ServiceHookType::OnStop)
                    .await
                {
                    warn!("Hook on_stop failed for {}: {}", service_name, e);
                }
            }

            // Stop the service
            stop_service(service_name, handle.clone())
                .await
                .map_err(|e| OrchestratorError::StopFailed(e.to_string()))?;

            stopped.push(service_name.clone());
        }

        // Run global on_stop hook if stopping all and services were stopped
        if service_filter.is_none() && !stopped.is_empty() {
            if let Some(ref log_cfg) = log_config {
                let env = std::env::vars().collect();
                if let Err(e) = run_global_hook(
                    &global_hooks,
                    GlobalHookType::OnStop,
                    &config_dir,
                    &env,
                    Some(log_cfg),
                    global_log_config.as_ref(),
                )
                .await
                {
                    warn!("Hook on_stop failed: {}", e);
                }
            }
        }

        // Run on_cleanup if requested
        if service_filter.is_none() && clean {
            info!("Running cleanup hooks");

            // Emit Cleanup event for all services
            for service_name in &services_to_stop {
                handle.emit_event(service_name, ServiceEvent::Cleanup).await;
            }

            if let Some(ref log_cfg) = log_config {
                let env = std::env::vars().collect();
                if let Err(e) = run_global_hook(
                    &global_hooks,
                    GlobalHookType::OnCleanup,
                    &config_dir,
                    &env,
                    Some(log_cfg),
                    global_log_config.as_ref(),
                )
                .await
                {
                    error!("Cleanup hook failed: {}", e);
                }
            }
        }

        // Apply log retention using LogReader
        if let Some(ref log_cfg) = log_config {
            use crate::logs::LogReader;
            let reader = LogReader::new(log_cfg.logs_dir.clone(), 0);

            for service_name in &stopped {
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
                    reader.clear_service_prefix(&format!("[{}.", service_name));
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
            if state_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&state_dir) {
                    warn!("Failed to remove state directory {:?}: {}", state_dir, e);
                } else {
                    info!("Removed state directory: {:?}", state_dir);
                }
            }

            // Unload config from registry
            self.registry.unload(&config_path.to_path_buf()).await;
        }

        if stopped.is_empty() {
            Ok("No services were running".to_string())
        } else {
            Ok(format!("Stopped services: {}", stopped.join(", ")))
        }
    }

    /// Restart services for a config
    ///
    /// If `service_filter` is provided, only restarts that service.
    /// Otherwise restarts all services with fresh config (re-expands environment variables).
    ///
    /// This method now incorporates the old "recreate" behavior:
    /// 1. Stop services
    /// 2. Clear config snapshot to force re-expansion
    /// 3. Unload config actor
    /// 4. Start services with fresh sys_env
    pub async fn restart_services(
        &self,
        config_path: &Path,
        service_filter: Option<&str>,
        sys_env: Option<HashMap<String, String>>,
    ) -> Result<String, OrchestratorError> {
        info!("Restarting services for {:?}", config_path);

        // Stop services first
        let stop_result = self.stop_services(config_path, service_filter, false).await;
        if let Err(e) = &stop_result {
            warn!("Error stopping services during restart: {}", e);
        }

        // Clear snapshot to force re-expansion with new env
        if let Some(handle) = self.registry.get(&config_path.to_path_buf()) {
            if let Err(e) = handle.clear_snapshot().await {
                warn!("Failed to clear snapshot: {}", e);
            }
        }

        // Unload the config actor completely
        self.registry.unload(&config_path.to_path_buf()).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Start services with fresh config and sys_env
        self.start_services(config_path, service_filter, sys_env).await
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

        // Run on_restart hook
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::OnRestart)
            .await
        {
            warn!("Hook on_restart failed for {}: {}", service_name, e);
        }

        // Apply on_restart log retention
        self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Restart)
            .await;

        // Stop the service
        stop_service(service_name, handle.clone())
            .await
            .map_err(|e| OrchestratorError::StopFailed(e.to_string()))?;

        // Small delay between stop and start
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Refresh context after stop (state may have changed)
        let ctx = handle
            .get_service_context(service_name)
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Emit Start event (restart includes a start)
        handle.emit_event(service_name, ServiceEvent::Start).await;

        // Run on_start hook (runs on every start, including restarts)
        self.run_service_hook(&ctx, service_name, ServiceHookType::OnStart)
            .await?;

        // Apply on_start log retention
        self.apply_retention(&handle, service_name, &ctx, LifecycleEvent::Start)
            .await;

        // Spawn the service
        self.spawn_service(&handle, service_name, &ctx).await?;

        // Spawn auxiliary tasks (health checker, file watcher)
        self.spawn_auxiliary_tasks(&handle, service_name, &ctx).await;

        // Update state
        let _ = handle
            .set_service_status(service_name, ServiceStatus::Running)
            .await;

        let _ = handle.increment_restart_count(service_name).await;

        Ok(())
    }

    /// Handle a process exit event
    ///
    /// This method:
    /// - Records the exit in state
    /// - Runs on_exit hook
    /// - Applies on_exit log retention
    /// - Determines if restart is needed based on policy
    /// - If restarting: runs on_restart hook, applies retention, spawns new process
    pub async fn handle_exit(
        &self,
        config_path: &Path,
        service_name: &str,
        exit_code: Option<i32>,
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
        let _ = handle.record_process_exit(service_name, exit_code).await;

        // Run on_exit hook
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::OnExit)
            .await
        {
            warn!("Hook on_exit failed for {}: {}", service_name, e);
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

            // Run on_restart hook
            if let Err(e) = self
                .run_service_hook(&ctx, service_name, ServiceHookType::OnRestart)
                .await
            {
                warn!("Hook on_restart failed for {}: {}", service_name, e);
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
            // Mark as stopped or failed
            let status = if exit_code == Some(0) {
                ServiceStatus::Stopped
            } else {
                ServiceStatus::Failed
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

    // --- Internal helpers ---

    /// Run a service hook
    async fn run_service_hook(
        &self,
        ctx: &ServiceContext,
        service_name: &str,
        hook_type: ServiceHookType,
    ) -> Result<(), OrchestratorError> {
        let hook_params = ServiceHookParams::from_service_context(
            &ctx.service_config,
            &ctx.working_dir,
            &ctx.env,
            Some(&ctx.log_config),
            ctx.global_log_config.as_ref(),
        );

        run_service_hook(
            &ctx.service_config.hooks,
            hook_type,
            service_name,
            &hook_params,
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
                .clear_service_logs_prefix(&format!("[{}.", service_name))
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
                handle.config_path().clone(),
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

            // Run cleanup hook if config is readable (not orphaned)
            if !is_orphaned {
                let config_file = state_dir.join("config.yaml");
                self.run_cleanup_hook_for_prune(&config_file).await;
            }

            // Unload from registry BEFORE deleting state directory
            // Use the ORIGINAL source path which matches the registry key
            if !is_orphaned {
                if let Ok(canonical) = PathBuf::from(&original_path).canonicalize() {
                    self.registry.unload(&canonical).await;
                }
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

    /// Run cleanup hook before pruning
    async fn run_cleanup_hook_for_prune(&self, config_file: &Path) {
        // Use daemon's current environment for cleanup hooks during prune
        let daemon_env: HashMap<String, String> = std::env::vars().collect();
        if let Ok(config) = KeplerConfig::load(config_file, &daemon_env) {
            let config_dir = config_file.parent().unwrap_or(Path::new("."));
            let env: HashMap<String, String> = std::env::vars().collect();

            if let Err(e) = run_global_hook(
                &config.global_hooks().cloned(),
                GlobalHookType::OnCleanup,
                config_dir,
                &env,
                None, // No log buffer for prune
                config.global_logs(),
            )
            .await
            {
                warn!("Cleanup hook failed: {}", e);
            }
        }
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

/// Tagged message containing service name and event message
#[derive(Debug, Clone)]
pub struct TaggedEventMessage {
    pub service_name: String,
    pub message: crate::events::ServiceEventMessage,
}

/// Handler for service events that manages restart propagation
///
/// The ServiceEventHandler uses a channel to receive events from all services.
/// When a service emits a Restart event, it propagates the restart to dependent
/// services that have `on_dependency_restart` enabled.
pub struct ServiceEventHandler {
    /// Receiver for tagged events from all services
    event_rx: mpsc::Receiver<TaggedEventMessage>,
    /// Reference to the orchestrator for restart operations
    orchestrator: Arc<ServiceOrchestrator>,
    /// Config path for this handler
    config_path: PathBuf,
    /// Config actor handle
    handle: ConfigActorHandle,
}

impl ServiceEventHandler {
    /// Create a new event handler for a config
    pub fn new(
        event_rx: mpsc::Receiver<TaggedEventMessage>,
        orchestrator: Arc<ServiceOrchestrator>,
        config_path: PathBuf,
        handle: ConfigActorHandle,
    ) -> Self {
        Self {
            event_rx,
            orchestrator,
            config_path,
            handle,
        }
    }

    /// Run the event handler loop
    ///
    /// This loop processes events from all services and propagates restarts
    /// to dependent services when appropriate.
    pub async fn run(mut self) {
        info!("ServiceEventHandler started for {:?}", self.config_path);

        while let Some(tagged_msg) = self.event_rx.recv().await {
            let service_name = &tagged_msg.service_name;
            let event = &tagged_msg.message.event;

            debug!(
                "Event received for {}: {:?}",
                service_name, event
            );

            // Handle restart propagation
            if let ServiceEvent::Restart { .. } = event {
                if let Err(e) = self.propagate_restart(service_name).await {
                    error!(
                        "Failed to propagate restart for {}: {}",
                        service_name, e
                    );
                }
            }
        }

        info!("ServiceEventHandler stopped for {:?}", self.config_path);
    }

    /// Propagate restart to dependent services
    ///
    /// When a service restarts, this method:
    /// 1. Finds services that depend on the restarted service with `restart: true`
    /// 2. For each matching service: stops it, waits for the dependency condition, restarts it
    ///
    /// This follows Docker Compose behavior where `restart: true` in depends_on causes
    /// the dependent service to restart when its dependency restarts.
    async fn propagate_restart(&self, restarted_service: &str) -> Result<(), OrchestratorError> {
        let config = match self.handle.get_config().await {
            Some(c) => c,
            None => return Ok(()),
        };

        info!(
            "Checking restart propagation for dependents of {}",
            restarted_service
        );

        for (service_name, service_config) in &config.services {
            // Skip the service that triggered the restart
            if service_name == restarted_service {
                continue;
            }

            // Check if this service depends on the restarted service with restart: true
            // This is the Docker Compose compatible check
            if !service_config
                .depends_on
                .should_restart_on_dependency(restarted_service)
            {
                continue;
            }

            // Check if the service is running
            if !self.handle.is_service_running(service_name).await {
                continue;
            }

            info!(
                "Propagating restart from {} to {}",
                restarted_service, service_name
            );

            // Stop the dependent service
            if let Err(e) = stop_service(service_name, self.handle.clone()).await {
                warn!("Failed to stop {} for restart propagation: {}", service_name, e);
                continue;
            }

            // Wait for the dependency's condition to be met
            let dep_config = service_config
                .depends_on
                .get(restarted_service)
                .unwrap_or_default();

            let start = Instant::now();
            loop {
                if check_dependency_satisfied(restarted_service, &dep_config.condition, &self.handle)
                    .await
                {
                    break;
                }

                // Check timeout if specified
                if let Some(timeout) = dep_config.timeout {
                    if start.elapsed() > timeout {
                        warn!(
                            "Timeout waiting for {} to satisfy condition for {} restart propagation",
                            restarted_service, service_name
                        );
                        break;
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            // Restart the dependent service
            if let Err(e) = self
                .orchestrator
                .restart_single_service_with_reason(
                    &self.config_path,
                    service_name,
                    RestartReason::DependencyRestart {
                        dependency: restarted_service.to_string(),
                    },
                )
                .await
            {
                error!(
                    "Failed to restart {} after {} restart: {}",
                    service_name, restarted_service, e
                );
            }
        }

        Ok(())
    }
}

/// Spawn forwarders that receive events from service channels and forward them to the aggregator
async fn spawn_event_forwarders(
    handle: &ConfigActorHandle,
    aggregate_tx: mpsc::Sender<TaggedEventMessage>,
) {
    let receivers = handle.get_all_event_receivers().await;

    for (service_name, mut rx) in receivers {
        let tx = aggregate_tx.clone();
        let name = service_name.clone();

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let tagged = TaggedEventMessage {
                    service_name: name.clone(),
                    message: msg,
                };
                if tx.send(tagged).await.is_err() {
                    break;
                }
            }
        });
    }
}

impl ServiceOrchestrator {
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
