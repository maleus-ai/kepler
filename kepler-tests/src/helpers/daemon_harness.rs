//! Test harness that manages daemon state without full binary

use kepler_daemon::config::{KeplerConfig, LogRetention};
use kepler_daemon::config_actor::{ConfigActor, ConfigActorHandle, TaskHandleType};
use kepler_daemon::health::spawn_health_checker;
use kepler_daemon::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use kepler_daemon::logs::{BufferedLogWriter, LogReader, LogStream, LogWriterConfig, LogLine};
use kepler_daemon::process::{spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams};
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::{spawn_file_watcher, FileChangeEvent};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Mutex to synchronize environment variable setting and ConfigActor creation.
/// This ensures that parallel tests don't interfere with each other's KEPLER_DAEMON_PATH.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Test harness for managing daemon state
pub struct TestDaemonHarness {
    pub handle: ConfigActorHandle,
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
        // Write config to file first (outside the lock)
        let config_path = config_dir.join("kepler.yaml");
        let config_yaml = serde_yaml::to_string(&config)
            .map_err(std::io::Error::other)?;
        std::fs::write(&config_path, config_yaml)?;

        // Lock to ensure atomic env var setting + ConfigActor creation
        // This prevents race conditions when tests run in parallel
        let (handle, actor) = {
            let _guard = ENV_LOCK.lock().unwrap();

            // Set KEPLER_DAEMON_PATH to isolate state within this test's directory
            // SAFETY: We hold ENV_LOCK, so no other test can interfere between
            // setting the env var and creating the ConfigActor.
            let kepler_state_dir = config_dir.join(".kepler");
            unsafe {
                std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
            }

            ConfigActor::create(config_path.clone(), None)
                .map_err(|e| std::io::Error::other(e.to_string()))?
        };
        tokio::spawn(actor.run());

        let (exit_tx, exit_rx) = mpsc::channel(32);
        let (restart_tx, restart_rx) = mpsc::channel(32);

        Ok(Self {
            handle,
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
        let config_path = config_path.canonicalize()?;
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // Lock to ensure atomic env var setting + ConfigActor creation
        // This prevents race conditions when tests run in parallel
        let (handle, actor) = {
            let _guard = ENV_LOCK.lock().unwrap();

            // Set KEPLER_DAEMON_PATH to isolate state within this test's directory
            // SAFETY: We hold ENV_LOCK, so no other test can interfere between
            // setting the env var and creating the ConfigActor.
            let kepler_state_dir = config_dir.join(".kepler");
            unsafe {
                std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
            }

            ConfigActor::create(config_path.clone(), None)
                .map_err(|e| std::io::Error::other(e.to_string()))?
        };
        tokio::spawn(actor.run());

        let (exit_tx, exit_rx) = mpsc::channel(32);
        let (restart_tx, restart_rx) = mpsc::channel(32);

        Ok(Self {
            handle,
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

    /// Get the config actor handle
    pub fn handle(&self) -> &ConfigActorHandle {
        &self.handle
    }

    /// Get the config path
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Start a specific service
    pub async fn start_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Get service context (single round-trip)
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        // Update status to starting
        let _ = self.handle
            .set_service_status(service_name, ServiceStatus::Starting)
            .await;

        // Run on_init hook if not initialized
        let should_run_init = !self.handle
            .is_service_initialized(service_name)
            .await;

        let hook_params = ServiceHookParams::from_service_context(
            &ctx.service_config,
            &ctx.working_dir,
            &ctx.env,
            Some(&ctx.log_config),
            ctx.global_log_config.as_ref(),
        );

        if should_run_init {
            run_service_hook(
                &ctx.service_config.hooks,
                ServiceHookType::OnInit,
                service_name,
                &hook_params,
            )
            .await?;

            // Mark as initialized
            let _ = self.handle
                .mark_service_initialized(service_name)
                .await;
        }

        // Run on_start hook
        run_service_hook(
            &ctx.service_config.hooks,
            ServiceHookType::OnStart,
            service_name,
            &hook_params,
        )
        .await?;

        // Spawn the process
        let spawn_params = SpawnServiceParams {
            service_name,
            service_config: &ctx.service_config,
            config_dir: &ctx.config_dir,
            log_config: ctx.log_config.clone(),
            handle: self.handle.clone(),
            exit_tx: self.exit_tx.clone(),
            global_log_config: ctx.global_log_config.as_ref(),
        };
        let process_handle = spawn_service(spawn_params).await?;

        // Store the handle
        self.handle
            .store_process_handle(service_name, process_handle)
            .await;

        // Update status to Running (PID is already set by spawn_service)
        let _ = self.handle
            .set_service_status(service_name, ServiceStatus::Running)
            .await;

        // Start file watcher if configured
        if !ctx.service_config.restart.watch_patterns().is_empty() {
            let watcher_handle = spawn_file_watcher(
                self.config_path.clone(),
                service_name.to_string(),
                ctx.service_config.restart.watch_patterns().to_vec(),
                ctx.working_dir.clone(),
                self.restart_tx.clone(),
            );
            self.handle
                .store_task_handle(
                    service_name,
                    TaskHandleType::FileWatcher,
                    watcher_handle,
                )
                .await;
        }

        Ok(())
    }

    /// Start the health checker for a service
    pub async fn start_health_checker(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        if let Some(health_config) = ctx.service_config.healthcheck {
            let task_handle = spawn_health_checker(
                service_name.to_string(),
                health_config,
                self.handle.clone(),
            );

            // Store the health check handle
            self.handle
                .store_task_handle(
                    service_name,
                    TaskHandleType::HealthCheck,
                    task_handle,
                )
                .await;
        }

        Ok(())
    }

    /// Stop a specific service
    pub async fn stop_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Get service context for hooks
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        // Run on_stop hook
        let hook_params = ServiceHookParams::from_service_context(
            &ctx.service_config,
            &ctx.working_dir,
            &ctx.env,
            Some(&ctx.log_config),
            ctx.global_log_config.as_ref(),
        );

        run_service_hook(
            &ctx.service_config.hooks,
            ServiceHookType::OnStop,
            service_name,
            &hook_params,
        )
        .await?;

        // Stop the service
        stop_service(service_name, self.handle.clone()).await?;

        Ok(())
    }

    /// Stop all services
    pub async fn stop_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Get all service names from the config
        if let Some(config) = self.handle.get_config().await {
            let service_names: Vec<String> = config.services.keys().cloned().collect();
            for name in service_names {
                let _ = self.stop_service(&name).await;
            }
        }

        Ok(())
    }

    /// Get the current status of a service
    pub async fn get_status(&self, service_name: &str) -> Option<ServiceStatus> {
        self.handle
            .get_service_state(service_name)
            .await
            .map(|s| s.status)
    }

    /// Check if a service has a health check configured
    pub async fn has_healthcheck(&self, service_name: &str) -> bool {
        self.handle
            .get_service_config(service_name)
            .await
            .map(|c| c.healthcheck.is_some())
            .unwrap_or(false)
    }

    /// Get the logs helper for test operations
    pub async fn logs(&self) -> Option<TestLogHelper> {
        self.handle.get_log_config().await.map(TestLogHelper::new)
    }

    /// Spawn a file change handler that restarts services on file changes
    /// Must be called after taking restart_rx with take_restart_rx()
    pub fn spawn_file_change_handler(&self, mut restart_rx: mpsc::Receiver<FileChangeEvent>) {
        let handle = self.handle.clone();
        let exit_tx = self.exit_tx.clone();

        tokio::spawn(async move {
            while let Some(event) = restart_rx.recv().await {
                // Get service context
                let ctx = match handle.get_service_context(&event.service_name).await {
                    Some(ctx) => ctx,
                    None => continue,
                };

                // Run on_restart hook
                let hook_params = ServiceHookParams::from_service_context(
                    &ctx.service_config,
                    &ctx.working_dir,
                    &ctx.env,
                    Some(&ctx.log_config),
                    ctx.global_log_config.as_ref(),
                );

                let _ = run_service_hook(
                    &ctx.service_config.hooks,
                    ServiceHookType::OnRestart,
                    &event.service_name,
                    &hook_params,
                )
                .await;

                // Apply on_restart log retention policy
                {
                    use kepler_daemon::config::resolve_log_retention;

                    let retention = resolve_log_retention(
                        ctx.service_config.logs.as_ref(),
                        ctx.global_log_config.as_ref(),
                        |l| l.get_on_restart(),
                        LogRetention::Retain,
                    );
                    let should_clear = retention == LogRetention::Clear;

                    if should_clear {
                        let reader = LogReader::new(
                            ctx.log_config.logs_dir.clone(),
                            0,
                        );
                        reader.clear_service(&event.service_name);
                        reader.clear_service_prefix(&format!("[{}.", event.service_name));
                    }
                }

                // Stop the service
                if stop_service(&event.service_name, handle.clone()).await.is_err() {
                    continue;
                }

                // Small delay
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Restart the service
                let spawn_params = SpawnServiceParams {
                    service_name: &event.service_name,
                    service_config: &ctx.service_config,
                    config_dir: &ctx.config_dir,
                    log_config: ctx.log_config.clone(),
                    handle: handle.clone(),
                    exit_tx: exit_tx.clone(),
                    global_log_config: ctx.global_log_config.as_ref(),
                };
                if let Ok(process_handle) = spawn_service(spawn_params).await {
                    handle
                        .store_process_handle(&event.service_name, process_handle)
                        .await;

                    let _ = handle
                        .set_service_status(&event.service_name, ServiceStatus::Running)
                        .await;
                    // Note: PID is already set by spawn_service
                    let _ = handle
                        .increment_restart_count(&event.service_name)
                        .await;
                }
            }
        });
    }

    /// Spawn a process exit handler that handles restarts based on restart policy
    /// Must be called after taking exit_rx with take_exit_rx()
    pub fn spawn_process_exit_handler(&self, mut exit_rx: mpsc::Receiver<ProcessExitEvent>) {
        let handle = self.handle.clone();
        let exit_tx = self.exit_tx.clone();

        tokio::spawn(async move {
            while let Some(event) = exit_rx.recv().await {
                // Get service context
                let ctx = match handle.get_service_context(&event.service_name).await {
                    Some(ctx) => ctx,
                    None => continue,
                };

                // Record process exit in state
                let _ = handle.record_process_exit(&event.service_name, event.exit_code).await;

                // Run on_exit hook
                let hook_params = ServiceHookParams::from_service_context(
                    &ctx.service_config,
                    &ctx.working_dir,
                    &ctx.env,
                    Some(&ctx.log_config),
                    ctx.global_log_config.as_ref(),
                );

                let _ = run_service_hook(
                    &ctx.service_config.hooks,
                    ServiceHookType::OnExit,
                    &event.service_name,
                    &hook_params,
                )
                .await;

                // Apply on_exit log retention
                {
                    use kepler_daemon::config::resolve_log_retention;

                    let retention = resolve_log_retention(
                        ctx.service_config.logs.as_ref(),
                        ctx.global_log_config.as_ref(),
                        |l| l.get_on_exit(),
                        LogRetention::Retain,
                    );
                    let should_clear = retention == LogRetention::Clear;

                    if should_clear {
                        let reader = LogReader::new(
                            ctx.log_config.logs_dir.clone(),
                            0,
                        );
                        reader.clear_service(&event.service_name);
                        reader.clear_service_prefix(&format!("[{}.", event.service_name));
                    }
                }

                // Determine if we should restart
                let should_restart = ctx.service_config.restart.should_restart_on_exit(event.exit_code);

                if should_restart {
                    // Small delay before restart
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    // Run on_restart hook
                    let _ = run_service_hook(
                        &ctx.service_config.hooks,
                        ServiceHookType::OnRestart,
                        &event.service_name,
                        &hook_params,
                    )
                    .await;

                    // Apply on_restart log retention
                    {
                        use kepler_daemon::config::resolve_log_retention;

                        let retention = resolve_log_retention(
                            ctx.service_config.logs.as_ref(),
                            ctx.global_log_config.as_ref(),
                            |l| l.get_on_restart(),
                            LogRetention::Retain,
                        );
                        let should_clear = retention == LogRetention::Clear;

                        if should_clear {
                            let reader = LogReader::new(
                                ctx.log_config.logs_dir.clone(),
                                0,
                            );
                            reader.clear_service(&event.service_name);
                            reader.clear_service_prefix(&format!("[{}.", event.service_name));
                        }
                    }

                    // Restart the service
                    let spawn_params = SpawnServiceParams {
                        service_name: &event.service_name,
                        service_config: &ctx.service_config,
                        config_dir: &ctx.config_dir,
                        log_config: ctx.log_config.clone(),
                        handle: handle.clone(),
                        exit_tx: exit_tx.clone(),
                        global_log_config: ctx.global_log_config.as_ref(),
                    };
                    if let Ok(process_handle) = spawn_service(spawn_params).await {
                        handle
                            .store_process_handle(&event.service_name, process_handle)
                            .await;

                        let _ = handle
                            .set_service_status(&event.service_name, ServiceStatus::Running)
                            .await;
                        let _ = handle
                            .increment_restart_count(&event.service_name)
                            .await;
                    }
                } else {
                    // Mark as stopped or failed
                    let status = if event.exit_code == Some(0) {
                        ServiceStatus::Stopped
                    } else {
                        ServiceStatus::Failed
                    };
                    let _ = handle.set_service_status(&event.service_name, status).await;
                }
            }
        });
    }

    /// Reload the config from disk
    pub async fn reload_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.handle.reload_config().await?;
        Ok(())
    }
}

/// Test helper for log operations that provides the same interface as the old SharedLogBuffer.
/// This allows tests to push log entries directly for testing purposes.
pub struct TestLogHelper {
    config: LogWriterConfig,
    sequence: std::sync::atomic::AtomicU64,
}

impl TestLogHelper {
    pub fn new(config: LogWriterConfig) -> Self {
        Self {
            config,
            sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Push a log entry to disk (creates a BufferedLogWriter, writes, and flushes)
    pub fn push(&self, service: &str, line: String, stream: LogStream) {
        let mut writer = BufferedLogWriter::from_config(&self.config, service, stream);
        writer.write(&line);
        // writer.flush() is called by drop
        self.sequence.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Get the last N log entries, optionally filtered by service
    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
            0,
        );
        reader.tail(count, service)
    }

    /// Clear all logs
    pub fn clear(&self) {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
            0,
        );
        reader.clear();
        self.sequence.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Clear logs for a specific service
    pub fn clear_service(&self, service: &str) {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
            0,
        );
        reader.clear_service(service);
    }

    /// Clear logs for services matching a prefix
    pub fn clear_service_prefix(&self, prefix: &str) {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
            0,
        );
        reader.clear_service_prefix(prefix);
    }

    /// Get current sequence number
    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get entries since a sequence number
    pub fn entries_since(&self, since: u64, service: Option<&str>) -> Vec<LogLine> {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
            0,
        );
        // Get all entries and filter by index
        // Note: this is less efficient than the old approach but maintains API compatibility
        let all = reader.tail(10000, service);
        let current = self.current_sequence();
        if since >= current {
            return Vec::new();
        }
        let skip = (since as usize).saturating_sub(all.len().saturating_sub(current as usize));
        all.into_iter().skip(skip).collect()
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
