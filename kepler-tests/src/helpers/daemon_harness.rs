//! Test harness that manages daemon state without full binary

use kepler_daemon::config::{KeplerConfig, LogRetention};
use kepler_daemon::env::build_service_env;
use kepler_daemon::health::spawn_health_checker;
use kepler_daemon::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use kepler_daemon::logs::SharedLogBuffer;
use kepler_daemon::process::{spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams};
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::state_actor::{create_state_actor, StateHandle, TaskHandleType};
use kepler_daemon::watcher::{spawn_file_watcher, FileChangeEvent};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

/// Test harness for managing daemon state
pub struct TestDaemonHarness {
    pub state: StateHandle,
    pub config_path: PathBuf,
    pub config_dir: PathBuf,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    exit_rx: Option<mpsc::Receiver<ProcessExitEvent>>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
    restart_rx: Option<mpsc::Receiver<FileChangeEvent>>,
}

impl TestDaemonHarness {
    /// Create a new test harness with the given config
    pub async fn new(config: KeplerConfig, config_dir: &Path) -> std::io::Result<Self> {
        // Write config to file
        let config_path = config_dir.join("kepler.yaml");
        let config_yaml = serde_yaml::to_string(&config)
            .map_err(std::io::Error::other)?;
        std::fs::write(&config_path, config_yaml)?;

        let (state, actor) = create_state_actor();
        tokio::spawn(actor.run());

        let (exit_tx, exit_rx) = mpsc::channel(32);
        let (restart_tx, restart_rx) = mpsc::channel(32);

        // Load the config into state
        state.load_config(config_path.clone()).await.map_err(|e| {
            std::io::Error::other(e.to_string())
        })?;

        Ok(Self {
            state,
            config_path: config_path.clone(),
            config_dir: config_dir.to_path_buf(),
            exit_tx,
            exit_rx: Some(exit_rx),
            restart_tx,
            restart_rx: Some(restart_rx),
        })
    }

    /// Create a harness from an existing config file
    pub async fn from_file(config_path: &Path) -> std::io::Result<Self> {
        let (state, actor) = create_state_actor();
        tokio::spawn(actor.run());

        let (exit_tx, exit_rx) = mpsc::channel(32);
        let (restart_tx, restart_rx) = mpsc::channel(32);

        let config_path = config_path.canonicalize()?;
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // Load the config into state
        state.load_config(config_path.clone()).await.map_err(|e| {
            std::io::Error::other(e.to_string())
        })?;

        Ok(Self {
            state,
            config_path,
            config_dir,
            exit_tx,
            exit_rx: Some(exit_rx),
            restart_tx,
            restart_rx: Some(restart_rx),
        })
    }

    /// Take the exit event receiver (can only be taken once)
    pub fn take_exit_rx(&mut self) -> Option<mpsc::Receiver<ProcessExitEvent>> {
        self.exit_rx.take()
    }

    /// Get a clone of the exit event sender
    pub fn exit_tx(&self) -> mpsc::Sender<ProcessExitEvent> {
        self.exit_tx.clone()
    }

    /// Take the restart event receiver (can only be taken once)
    pub fn take_restart_rx(&mut self) -> Option<mpsc::Receiver<FileChangeEvent>> {
        self.restart_rx.take()
    }

    /// Get a clone of the restart event sender
    pub fn restart_tx(&self) -> mpsc::Sender<FileChangeEvent> {
        self.restart_tx.clone()
    }

    /// Get the state handle
    pub fn state(&self) -> &StateHandle {
        &self.state
    }

    /// Get the config path
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Start a specific service
    pub async fn start_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let service_config = self.state
            .get_service_config(self.config_path.clone(), service_name.to_string())
            .await
            .ok_or("Service not found")?;

        let logs = self.state
            .get_logs_buffer(self.config_path.clone())
            .await
            .ok_or("Logs not found")?;

        let global_log_config = self.state
            .get_global_log_config(self.config_path.clone())
            .await;

        // Update status to starting
        let _ = self.state
            .set_service_status(
                self.config_path.clone(),
                service_name.to_string(),
                ServiceStatus::Starting,
            )
            .await;

        // Run on_init hook if not initialized
        let should_run_init = !self.state
            .is_service_initialized(self.config_path.clone(), service_name.to_string())
            .await;

        let working_dir = service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.clone());
        let env = build_service_env(&service_config, &self.config_dir)?;
        let hook_params = ServiceHookParams {
            working_dir: &working_dir,
            env: &env,
            logs: Some(&logs),
            service_user: service_config.user.as_deref(),
            service_group: service_config.group.as_deref(),
            service_log_config: service_config.logs.as_ref(),
            global_log_config: global_log_config.as_ref(),
        };

        if should_run_init {
            run_service_hook(
                &service_config.hooks,
                ServiceHookType::OnInit,
                service_name,
                &hook_params,
            )
            .await?;

            // Mark as initialized
            let _ = self.state
                .mark_service_initialized(self.config_path.clone(), service_name.to_string())
                .await;
        }

        // Run on_start hook
        run_service_hook(
            &service_config.hooks,
            ServiceHookType::OnStart,
            service_name,
            &hook_params,
        )
        .await?;

        // Spawn the process
        let spawn_params = SpawnServiceParams {
            config_path: &self.config_path,
            service_name,
            service_config: &service_config,
            config_dir: &self.config_dir,
            logs: logs.clone(),
            state: self.state.clone(),
            exit_tx: self.exit_tx.clone(),
            global_log_config: global_log_config.as_ref(),
        };
        let handle = spawn_service(spawn_params).await?;

        let working_dir = service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.clone());

        // Store the handle
        self.state
            .store_process_handle(self.config_path.clone(), service_name.to_string(), handle)
            .await;

        // Update status to Running (PID is already set by spawn_service)
        let _ = self.state
            .set_service_status(
                self.config_path.clone(),
                service_name.to_string(),
                ServiceStatus::Running,
            )
            .await;

        // Start file watcher if configured
        if !service_config.restart.watch_patterns().is_empty() {
            let handle = spawn_file_watcher(
                self.config_path.clone(),
                service_name.to_string(),
                service_config.restart.watch_patterns().to_vec(),
                working_dir,
                self.restart_tx.clone(),
            );
            self.state
                .store_task_handle(
                    self.config_path.clone(),
                    service_name.to_string(),
                    TaskHandleType::FileWatcher,
                    handle,
                )
                .await;
        }

        Ok(())
    }

    /// Start the health checker for a service
    pub async fn start_health_checker(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let service_config = self.state
            .get_service_config(self.config_path.clone(), service_name.to_string())
            .await
            .ok_or("Service not found")?;

        if let Some(health_config) = service_config.healthcheck {
            let handle = spawn_health_checker(
                self.config_path.clone(),
                service_name.to_string(),
                health_config,
                self.state.clone(),
            );

            // Store the health check handle
            self.state
                .store_task_handle(
                    self.config_path.clone(),
                    service_name.to_string(),
                    TaskHandleType::HealthCheck,
                    handle,
                )
                .await;
        }

        Ok(())
    }

    /// Stop a specific service
    pub async fn stop_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Get service config for hooks
        let service_config = self.state
            .get_service_config(self.config_path.clone(), service_name.to_string())
            .await
            .ok_or("Service not found")?;

        let logs = self.state
            .get_logs_buffer(self.config_path.clone())
            .await
            .ok_or("Logs not found")?;

        let global_log_config = self.state
            .get_global_log_config(self.config_path.clone())
            .await;

        // Run on_stop hook
        let working_dir = service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.clone());
        let env = build_service_env(&service_config, &self.config_dir)?;
        let hook_params = ServiceHookParams {
            working_dir: &working_dir,
            env: &env,
            logs: Some(&logs),
            service_user: service_config.user.as_deref(),
            service_group: service_config.group.as_deref(),
            service_log_config: service_config.logs.as_ref(),
            global_log_config: global_log_config.as_ref(),
        };

        run_service_hook(
            &service_config.hooks,
            ServiceHookType::OnStop,
            service_name,
            &hook_params,
        )
        .await?;

        // Stop the service
        stop_service(&self.config_path, service_name, self.state.clone()).await?;

        Ok(())
    }

    /// Stop all services
    pub async fn stop_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        let configs = self.state.list_configs().await.unwrap_or_default();

        for config_info in configs {
            if config_info.config_path == self.config_path.to_string_lossy() {
                // Get all service names from the config
                if let Some(config) = self.state.get_config(self.config_path.clone()).await {
                    let service_names: Vec<String> = config.services.keys().cloned().collect();
                    for name in service_names {
                        let _ = self.stop_service(&name).await;
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the current status of a service
    pub async fn get_status(&self, service_name: &str) -> Option<ServiceStatus> {
        self.state
            .get_service_state(self.config_path.clone(), service_name.to_string())
            .await
            .map(|s| s.status)
    }

    /// Check if a service has a health check configured
    pub async fn has_healthcheck(&self, service_name: &str) -> bool {
        self.state
            .get_service_config(self.config_path.clone(), service_name.to_string())
            .await
            .map(|c| c.healthcheck.is_some())
            .unwrap_or(false)
    }

    /// Get the logs buffer
    pub async fn logs(&self) -> Option<SharedLogBuffer> {
        self.state.get_logs_buffer(self.config_path.clone()).await
    }

    /// Spawn a file change handler that restarts services on file changes
    /// Must be called after taking restart_rx with take_restart_rx()
    pub fn spawn_file_change_handler(&self, mut restart_rx: mpsc::Receiver<FileChangeEvent>) {
        let state = self.state.clone();
        let exit_tx = self.exit_tx.clone();

        tokio::spawn(async move {
            while let Some(event) = restart_rx.recv().await {
                // Get service config and global log config
                let service_config = state
                    .get_service_config(event.config_path.clone(), event.service_name.clone())
                    .await;
                let global_log_config = state
                    .get_global_log_config(event.config_path.clone())
                    .await;

                if let Some(config) = service_config {
                    // Get config_dir and logs
                    let config_dir = state
                        .get_config_dir(event.config_path.clone())
                        .await
                        .unwrap_or_else(|| PathBuf::from("."));

                    let logs = match state.get_logs_buffer(event.config_path.clone()).await {
                        Some(l) => l,
                        None => continue,
                    };

                    // Run on_restart hook
                    let working_dir = config
                        .working_dir
                        .clone()
                        .unwrap_or_else(|| config_dir.clone());
                    let env = build_service_env(&config, &config_dir).unwrap_or_default();
                    let hook_params = ServiceHookParams {
                        working_dir: &working_dir,
                        env: &env,
                        logs: Some(&logs),
                        service_user: config.user.as_deref(),
                        service_group: config.group.as_deref(),
                        service_log_config: config.logs.as_ref(),
                        global_log_config: global_log_config.as_ref(),
                    };

                    let _ = run_service_hook(
                        &config.hooks,
                        ServiceHookType::OnRestart,
                        &event.service_name,
                        &hook_params,
                    )
                    .await;

                    // Apply on_restart log retention policy
                    {
                        use kepler_daemon::config::resolve_log_retention;

                        let retention = resolve_log_retention(
                            config.logs.as_ref(),
                            global_log_config.as_ref(),
                            |l| l.get_on_restart(),
                            LogRetention::Retain,
                        );
                        let should_clear = retention == LogRetention::Clear;

                        if should_clear {
                            logs.clear_service(&event.service_name);
                            logs.clear_service_prefix(&format!("[{}.", event.service_name));
                        }
                    }

                    // Stop the service
                    if stop_service(&event.config_path, &event.service_name, state.clone()).await.is_err() {
                        continue;
                    }

                    // Small delay
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                    // Restart the service
                    let spawn_params = SpawnServiceParams {
                        config_path: &event.config_path,
                        service_name: &event.service_name,
                        service_config: &config,
                        config_dir: &config_dir,
                        logs,
                        state: state.clone(),
                        exit_tx: exit_tx.clone(),
                        global_log_config: global_log_config.as_ref(),
                    };
                    if let Ok(handle) = spawn_service(spawn_params).await {
                        state
                            .store_process_handle(
                                event.config_path.clone(),
                                event.service_name.clone(),
                                handle,
                            )
                            .await;

                        let _ = state
                            .set_service_status(
                                event.config_path.clone(),
                                event.service_name.clone(),
                                ServiceStatus::Running,
                            )
                            .await;
                        // Note: PID is already set by spawn_service
                        let _ = state
                            .increment_restart_count(
                                event.config_path.clone(),
                                event.service_name.clone(),
                            )
                            .await;
                    }
                }
            }
        });
    }

    /// Reload the config from disk
    pub async fn reload_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.state.load_config(self.config_path.clone()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_harness_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = TestConfigBuilder::new()
            .add_service("test", TestServiceBuilder::long_running().build())
            .build();

        let harness = TestDaemonHarness::new(config, temp_dir.path())
            .await
            .unwrap();

        assert!(harness.config_path.exists());
    }

    #[tokio::test]
    async fn test_start_stop_service() {
        let temp_dir = TempDir::new().unwrap();
        let config = TestConfigBuilder::new()
            .add_service("test", TestServiceBuilder::long_running().build())
            .build();

        let harness = TestDaemonHarness::new(config, temp_dir.path())
            .await
            .unwrap();

        // Start the service
        harness.start_service("test").await.unwrap();
        assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Running));

        // Stop the service
        harness.stop_service("test").await.unwrap();
        assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Stopped));
    }
}
