//! Service orchestration for lifecycle management.
//!
//! This module provides a centralized `ServiceOrchestrator` that handles all service
//! lifecycle operations (start, stop, restart, exit handling). It eliminates duplication
//! by providing unified methods that handle hooks, log retention, and state updates.

use std::path::Path;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{resolve_log_retention, LogRetention};
use crate::deps::{get_service_with_deps, get_start_order, get_stop_order};
use crate::health::spawn_health_checker;
use crate::hooks::{
    run_global_hook, run_service_hook, GlobalHookType, ServiceHookParams, ServiceHookType,
    GLOBAL_HOOK_PREFIX,
};
use crate::process::{spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams};
use crate::state::ServiceStatus;
use crate::state_actor::{ServiceContext, StateHandle, TaskHandleType};
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
    /// Cleanup - on_cleanup retention only
    Cleanup,
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
    state: StateHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
}

impl ServiceOrchestrator {
    /// Create a new ServiceOrchestrator
    pub fn new(
        state: StateHandle,
        exit_tx: mpsc::Sender<ProcessExitEvent>,
        restart_tx: mpsc::Sender<FileChangeEvent>,
    ) -> Self {
        Self {
            state,
            exit_tx,
            restart_tx,
        }
    }

    /// Get the state handle
    pub fn state(&self) -> &StateHandle {
        &self.state
    }

    /// Start services for a config
    ///
    /// If `service_filter` is provided, only starts that service and its dependencies.
    /// Otherwise starts all services in dependency order.
    ///
    /// This method handles:
    /// - Loading/reloading the config
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
    ) -> Result<String, OrchestratorError> {
        // Load or reload config
        self.state.load_config(config_path.to_path_buf()).await?;

        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Get config and determine services to start
        let config = self
            .state
            .get_config(config_path.to_path_buf())
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let services_to_start = match service_filter {
            Some(name) => get_service_with_deps(name, &config.services)?,
            None => get_start_order(&config.services)?,
        };

        let logs = self
            .state
            .get_logs_buffer(config_path.to_path_buf())
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let global_hooks = config.hooks.clone();
        let global_log_config = config.logs.clone();
        let initialized = self
            .state
            .is_config_initialized(config_path.to_path_buf())
            .await;

        // Run global on_init hook if first time
        if !initialized {
            let env = std::env::vars().collect();
            run_global_hook(
                &global_hooks,
                GlobalHookType::OnInit,
                &config_dir,
                &env,
                Some(&logs),
                global_log_config.as_ref(),
            )
            .await?;

            self.state
                .mark_config_initialized(config_path.to_path_buf())
                .await?;
        }

        // Run global on_start hook
        let env = std::env::vars().collect();
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

        // Start services in order
        for service_name in &services_to_start {
            // Check if already running
            if self
                .state
                .is_service_running(config_path.to_path_buf(), service_name.clone())
                .await
            {
                debug!("Service {} is already running", service_name);
                continue;
            }

            match self
                .start_single_service(config_path, service_name)
                .await
            {
                Ok(()) => started.push(service_name.clone()),
                Err(e) => {
                    error!("Failed to start service {}: {}", service_name, e);
                    return Err(e);
                }
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
        config_path: &Path,
        service_name: &str,
    ) -> Result<(), OrchestratorError> {
        // Get service context (includes cached env and working_dir)
        let ctx = self
            .state
            .get_service_context(config_path.to_path_buf(), service_name.to_string())
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Update status to starting
        let _ = self
            .state
            .set_service_status(
                config_path.to_path_buf(),
                service_name.to_string(),
                ServiceStatus::Starting,
            )
            .await;

        // Run on_init hook if first time for this service
        let service_initialized = self
            .state
            .is_service_initialized(config_path.to_path_buf(), service_name.to_string())
            .await;

        if !service_initialized {
            self.run_service_hook(&ctx, service_name, ServiceHookType::OnInit)
                .await?;

            self.state
                .mark_service_initialized(config_path.to_path_buf(), service_name.to_string())
                .await?;
        }

        // Run on_start hook
        self.run_service_hook(&ctx, service_name, ServiceHookType::OnStart)
            .await?;

        // Apply on_start log retention
        self.apply_retention(config_path, service_name, &ctx, LifecycleEvent::Start)
            .await;

        // Spawn process
        self.spawn_service(config_path, service_name, &ctx).await?;

        // Spawn auxiliary tasks
        self.spawn_auxiliary_tasks(config_path, service_name, &ctx)
            .await;

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

        // Load config if clean is requested (so cleanup hooks can run even if not started)
        if clean {
            let _ = self.state.load_config(config_path.to_path_buf()).await;
        }

        // Get services to stop
        let config = match self.state.get_config(config_path.to_path_buf()).await {
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

        let logs = self.state.get_logs_buffer(config_path.to_path_buf()).await;
        let global_log_config = config.logs.clone();
        let global_hooks = config.hooks.clone();

        let mut stopped = Vec::new();

        // Stop services in reverse dependency order
        for service_name in &services_to_stop {
            // Check if running
            let is_running = self
                .state
                .is_service_running(config_path.to_path_buf(), service_name.clone())
                .await;

            if !is_running {
                continue;
            }

            // Get service context
            if let Some(ctx) = self
                .state
                .get_service_context(config_path.to_path_buf(), service_name.clone())
                .await
            {
                // Run on_stop hook
                if let Err(e) = self
                    .run_service_hook(&ctx, service_name, ServiceHookType::OnStop)
                    .await
                {
                    warn!("Hook on_stop failed for {}: {}", service_name, e);
                }
            }

            // Stop the service
            stop_service(config_path, service_name, self.state.clone())
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
                let service_logs = config
                    .services
                    .get(service_name)
                    .and_then(|c| c.logs.as_ref());

                let retention = if clean {
                    resolve_log_retention(
                        service_logs,
                        global_log_config.as_ref(),
                        |l| l.get_on_cleanup(),
                        LogRetention::Clear,
                    )
                } else {
                    resolve_log_retention(
                        service_logs,
                        global_log_config.as_ref(),
                        |l| l.get_on_stop(),
                        LogRetention::Clear,
                    )
                };

                if retention == LogRetention::Clear {
                    logs_buffer.clear_service(service_name);
                    logs_buffer.clear_service_prefix(&format!("[{}.", service_name));
                }
            }

            // Clear global hook logs if stopping all
            if service_filter.is_none() && (!stopped.is_empty() || clean) {
                let should_clear_global = if clean {
                    global_log_config
                        .as_ref()
                        .and_then(|c| c.get_on_cleanup())
                        .unwrap_or(LogRetention::Clear)
                        == LogRetention::Clear
                } else {
                    global_log_config
                        .as_ref()
                        .and_then(|c| c.get_on_stop())
                        .unwrap_or(LogRetention::Clear)
                        == LogRetention::Clear
                };
                if should_clear_global {
                    logs_buffer.clear_service_prefix(GLOBAL_HOOK_PREFIX);
                }
            }
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
    /// Otherwise restarts all services.
    pub async fn restart_services(
        &self,
        config_path: &Path,
        service_filter: Option<&str>,
    ) -> Result<String, OrchestratorError> {
        // Load or reload config
        self.state.load_config(config_path.to_path_buf()).await?;

        // Get services to restart
        let config = self
            .state
            .get_config(config_path.to_path_buf())
            .await
            .ok_or_else(|| OrchestratorError::ConfigNotFound(config_path.display().to_string()))?;

        let services_to_restart = match service_filter {
            Some(name) => {
                if !config.services.contains_key(name) {
                    return Err(OrchestratorError::ServiceNotFound(name.to_string()));
                }
                vec![name.to_string()]
            }
            None => get_start_order(&config.services)?,
        };

        // Run on_restart hooks and apply retention
        for service_name in &services_to_restart {
            if let Some(ctx) = self
                .state
                .get_service_context(config_path.to_path_buf(), service_name.clone())
                .await
            {
                // Run on_restart hook
                if let Err(e) = self
                    .run_service_hook(&ctx, service_name, ServiceHookType::OnRestart)
                    .await
                {
                    warn!("Hook on_restart failed for {}: {}", service_name, e);
                }

                // Apply on_restart log retention
                self.apply_retention(config_path, service_name, &ctx, LifecycleEvent::Restart)
                    .await;
            }
        }

        // Stop services
        self.stop_services(config_path, service_filter, false).await?;

        // Small delay between stop and start
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Start services
        self.start_services(config_path, service_filter).await
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

        // Get service context
        let ctx = self
            .state
            .get_service_context(config_path.to_path_buf(), service_name.to_string())
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
        self.apply_retention(config_path, service_name, &ctx, LifecycleEvent::Restart)
            .await;

        // Stop the service
        stop_service(config_path, service_name, self.state.clone())
            .await
            .map_err(|e| OrchestratorError::StopFailed(e.to_string()))?;

        // Small delay between stop and start
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Refresh context after stop (state may have changed)
        let ctx = self
            .state
            .get_service_context(config_path.to_path_buf(), service_name.to_string())
            .await
            .ok_or(OrchestratorError::ServiceContextNotFound)?;

        // Spawn the service
        self.spawn_service(config_path, service_name, &ctx).await?;

        // Update state
        let _ = self
            .state
            .set_service_status(
                config_path.to_path_buf(),
                service_name.to_string(),
                ServiceStatus::Running,
            )
            .await;

        let _ = self
            .state
            .increment_restart_count(config_path.to_path_buf(), service_name.to_string())
            .await;

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
        // Get service context
        let ctx = match self
            .state
            .get_service_context(config_path.to_path_buf(), service_name.to_string())
            .await
        {
            Some(ctx) => ctx,
            None => return Ok(()), // Service no longer exists
        };

        // Record process exit in state
        let _ = self
            .state
            .record_process_exit(
                config_path.to_path_buf(),
                service_name.to_string(),
                exit_code,
            )
            .await;

        // Run on_exit hook
        if let Err(e) = self
            .run_service_hook(&ctx, service_name, ServiceHookType::OnExit)
            .await
        {
            warn!("Hook on_exit failed for {}: {}", service_name, e);
        }

        // Apply on_exit log retention
        self.apply_retention(config_path, service_name, &ctx, LifecycleEvent::Exit)
            .await;

        // Determine if we should restart
        let should_restart = ctx.service_config.restart.should_restart_on_exit(exit_code);

        if should_restart {
            // Increment restart count and set status to starting
            let _ = self
                .state
                .increment_restart_count(config_path.to_path_buf(), service_name.to_string())
                .await;
            let _ = self
                .state
                .set_service_status(
                    config_path.to_path_buf(),
                    service_name.to_string(),
                    ServiceStatus::Starting,
                )
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
            self.apply_retention(config_path, service_name, &ctx, LifecycleEvent::Restart)
                .await;

            // Refresh context (env may have changed if env_file was modified)
            let ctx = self
                .state
                .get_service_context(config_path.to_path_buf(), service_name.to_string())
                .await
                .ok_or(OrchestratorError::ServiceContextNotFound)?;

            // Spawn new process
            match self.spawn_service(config_path, service_name, &ctx).await {
                Ok(()) => {
                    let _ = self
                        .state
                        .set_service_status(
                            config_path.to_path_buf(),
                            service_name.to_string(),
                            ServiceStatus::Running,
                        )
                        .await;
                }
                Err(e) => {
                    error!("Failed to restart service {}: {}", service_name, e);
                    let _ = self
                        .state
                        .set_service_status(
                            config_path.to_path_buf(),
                            service_name.to_string(),
                            ServiceStatus::Failed,
                        )
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
            let _ = self
                .state
                .set_service_status(
                    config_path.to_path_buf(),
                    service_name.to_string(),
                    status,
                )
                .await;
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
        config_path: &Path,
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
            LifecycleEvent::Cleanup => resolve_log_retention(
                ctx.service_config.logs.as_ref(),
                ctx.global_log_config.as_ref(),
                |l| l.get_on_cleanup(),
                LogRetention::Clear,
            ),
            LifecycleEvent::Init => return, // No retention for init
        };

        if retention == LogRetention::Clear {
            self.state
                .clear_service_logs(config_path.to_path_buf(), service_name.to_string())
                .await;
            self.state
                .clear_service_logs_prefix(
                    config_path.to_path_buf(),
                    format!("[{}.", service_name),
                )
                .await;
        }
    }

    /// Spawn a service process
    async fn spawn_service(
        &self,
        config_path: &Path,
        service_name: &str,
        ctx: &ServiceContext,
    ) -> Result<(), OrchestratorError> {
        let spawn_params = SpawnServiceParams {
            config_path,
            service_name,
            service_config: &ctx.service_config,
            config_dir: &ctx.config_dir,
            logs: ctx.logs.clone(),
            state: self.state.clone(),
            exit_tx: self.exit_tx.clone(),
            global_log_config: ctx.global_log_config.as_ref(),
        };

        let handle = spawn_service(spawn_params)
            .await
            .map_err(|e| OrchestratorError::SpawnFailed(e.to_string()))?;

        // Store process handle
        self.state
            .store_process_handle(
                config_path.to_path_buf(),
                service_name.to_string(),
                handle,
            )
            .await;

        // Update status to running
        let _ = self
            .state
            .set_service_status(
                config_path.to_path_buf(),
                service_name.to_string(),
                ServiceStatus::Running,
            )
            .await;

        Ok(())
    }

    /// Spawn auxiliary tasks (health checker, file watcher)
    async fn spawn_auxiliary_tasks(
        &self,
        config_path: &Path,
        service_name: &str,
        ctx: &ServiceContext,
    ) {
        // Start health check if configured
        if let Some(health_config) = &ctx.service_config.healthcheck {
            let handle = spawn_health_checker(
                config_path.to_path_buf(),
                service_name.to_string(),
                health_config.clone(),
                self.state.clone(),
            );
            self.state
                .store_task_handle(
                    config_path.to_path_buf(),
                    service_name.to_string(),
                    TaskHandleType::HealthCheck,
                    handle,
                )
                .await;
        }

        // Start file watcher if configured
        if !ctx.service_config.restart.watch_patterns().is_empty() {
            let handle = spawn_file_watcher(
                config_path.to_path_buf(),
                service_name.to_string(),
                ctx.service_config.restart.watch_patterns().to_vec(),
                ctx.working_dir.clone(),
                self.restart_tx.clone(),
            );
            self.state
                .store_task_handle(
                    config_path.to_path_buf(),
                    service_name.to_string(),
                    TaskHandleType::FileWatcher,
                    handle,
                )
                .await;
        }
    }
}
