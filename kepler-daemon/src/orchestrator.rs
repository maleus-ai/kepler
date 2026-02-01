//! Service orchestration for lifecycle management.
//!
//! This module provides a centralized `ServiceOrchestrator` that handles all service
//! lifecycle operations (start, stop, restart, exit handling). It eliminates duplication
//! by providing unified methods that handle hooks, log retention, and state updates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{resolve_log_retention, KeplerConfig, LogRetention};
use kepler_protocol::protocol::PrunedConfigInfo;
use crate::config_actor::{ConfigActorHandle, ServiceContext, TaskHandleType};
use crate::config_registry::SharedConfigRegistry;
use crate::deps::{get_service_with_deps, get_start_levels, get_start_order, get_stop_order};
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

        let logs = handle
            .get_logs_buffer()
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
                Some(&logs),
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
            Some(&logs),
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

        // Update status to starting
        let _ = handle
            .set_service_status(service_name, ServiceStatus::Starting)
            .await;

        // Run on_init hook if first time for this service
        let service_initialized = handle.is_service_initialized(service_name).await;

        if !service_initialized {
            self.run_service_hook(&ctx, service_name, ServiceHookType::OnInit)
                .await?;

            handle.mark_service_initialized(service_name).await?;
        }

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

        let logs = handle.get_logs_buffer().await;
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
            if let Some(ref logs) = logs {
                let env = std::env::vars().collect();
                if let Err(e) = run_global_hook(
                    &global_hooks,
                    GlobalHookType::OnStop,
                    &config_dir,
                    &env,
                    Some(logs),
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
            if let Some(ref logs) = logs {
                let env = std::env::vars().collect();
                if let Err(e) = run_global_hook(
                    &global_hooks,
                    GlobalHookType::OnCleanup,
                    &config_dir,
                    &env,
                    Some(logs),
                    global_log_config.as_ref(),
                )
                .await
                {
                    error!("Cleanup hook failed: {}", e);
                }
            }
        }

        // Apply log retention
        if let Some(ref logs_buffer) = logs {
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
                    logs_buffer.clear_service(service_name);
                    logs_buffer.clear_service_prefix(&format!("[{}.", service_name));
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
                    logs_buffer.clear_service_prefix(GLOBAL_HOOK_PREFIX);
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
        info!(
            "Restarting service {} in {:?}",
            service_name, config_path
        );

        let handle = self
            .registry
            .get(&config_path.to_path_buf())
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

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

        // Spawn the service
        self.spawn_service(&handle, service_name, &ctx).await?;

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
            Some(&ctx.logs),
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
            logs: ctx.logs.clone(),
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
