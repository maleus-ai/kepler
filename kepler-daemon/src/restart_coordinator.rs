//! Restart coordination for services.
//!
//! This module handles the orchestration of service restarts triggered by
//! file changes or other events. It encapsulates the restart logic including:
//! - Running on_restart hooks
//! - Applying log retention policies
//! - Stopping the service
//! - Respawning the service

use std::path::Path;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::config::{resolve_log_retention, LogRetention};
use crate::env::build_service_env;
use crate::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use crate::process::{spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams};
use crate::state::ServiceStatus;
use crate::state_actor::StateHandle;
use crate::watcher::FileChangeEvent;

/// Coordinator for service restarts.
///
/// Handles the orchestration of stopping and restarting services
/// in response to file changes or other restart triggers.
pub struct RestartCoordinator {
    state: StateHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
}

impl RestartCoordinator {
    /// Create a new RestartCoordinator
    pub fn new(state: StateHandle, exit_tx: mpsc::Sender<ProcessExitEvent>) -> Self {
        Self { state, exit_tx }
    }

    /// Handle a file change event by restarting the affected service
    pub async fn handle_file_change(&self, event: FileChangeEvent) {
        info!(
            "File change detected for {} in {:?}, restarting",
            event.service_name, event.config_path
        );

        if let Err(e) = self
            .restart_service(&event.config_path, &event.service_name)
            .await
        {
            error!(
                "Failed to restart service {} after file change: {}",
                event.service_name, e
            );
        }
    }

    /// Restart a specific service
    ///
    /// This performs the full restart sequence:
    /// 1. Get service context
    /// 2. Run on_restart hook
    /// 3. Apply log retention policy
    /// 4. Stop the service
    /// 5. Start the service
    /// 6. Update state
    pub async fn restart_service(
        &self,
        config_path: &Path,
        service_name: &str,
    ) -> Result<(), RestartError> {
        // Get service context (bundles service_config, config_dir, logs, global_log_config)
        let ctx = self
            .state
            .get_service_context(config_path.to_path_buf(), service_name.to_string())
            .await
            .ok_or(RestartError::ServiceContextNotFound)?;

        // Determine working directory
        let working_dir = ctx
            .service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| ctx.config_dir.clone());

        // Build environment
        let env = build_service_env(&ctx.service_config, &ctx.config_dir).unwrap_or_default();

        // Run on_restart hook
        let hook_params = ServiceHookParams::from_service_context(
            &ctx.service_config,
            &working_dir,
            &env,
            Some(&ctx.logs),
            ctx.global_log_config.as_ref(),
        );

        if let Err(e) = run_service_hook(
            &ctx.service_config.hooks,
            ServiceHookType::OnRestart,
            service_name,
            &hook_params,
        )
        .await
        {
            warn!("Hook on_restart failed for {}: {}", service_name, e);
            // Continue with restart even if hook fails
        }

        // Apply on_restart log retention policy
        let retention = resolve_log_retention(
            ctx.service_config.logs.as_ref(),
            ctx.global_log_config.as_ref(),
            |l| l.get_on_restart(),
            LogRetention::Retain,
        );

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

        // Stop the service
        stop_service(config_path, service_name, self.state.clone())
            .await
            .map_err(|e| RestartError::StopFailed(e.to_string()))?;

        // Small delay between stop and start
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Restart the service
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
            .map_err(|e| RestartError::SpawnFailed(e.to_string()))?;

        // Update state
        self.state
            .store_process_handle(
                config_path.to_path_buf(),
                service_name.to_string(),
                handle,
            )
            .await;

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

    /// Spawn a task to handle file change events from a receiver
    pub fn spawn_file_change_handler(
        self,
        mut restart_rx: mpsc::Receiver<FileChangeEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = restart_rx.recv().await {
                self.handle_file_change(event).await;
            }
        })
    }
}

/// Errors that can occur during service restart
#[derive(Debug, thiserror::Error)]
pub enum RestartError {
    #[error("Service context not found")]
    ServiceContextNotFound,

    #[error("Failed to stop service: {0}")]
    StopFailed(String),

    #[error("Failed to spawn service: {0}")]
    SpawnFailed(String),
}
