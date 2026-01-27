//! Test harness that manages daemon state without full binary

use chrono::Utc;
use kepler_daemon::config::KeplerConfig;
use kepler_daemon::env::build_service_env;
use kepler_daemon::health::spawn_health_checker;
use kepler_daemon::hooks::{run_service_hook, ServiceHookType};
use kepler_daemon::logs::SharedLogBuffer;
use kepler_daemon::process::{spawn_service, stop_service, ProcessExitEvent};
use kepler_daemon::state::{new_shared_state, ServiceStatus, SharedDaemonState};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

/// Test harness for managing daemon state
pub struct TestDaemonHarness {
    pub state: SharedDaemonState,
    pub config_path: PathBuf,
    pub config_dir: PathBuf,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    exit_rx: Option<mpsc::Receiver<ProcessExitEvent>>,
}

impl TestDaemonHarness {
    /// Create a new test harness with the given config
    pub async fn new(config: KeplerConfig, config_dir: &Path) -> std::io::Result<Self> {
        // Write config to file
        let config_path = config_dir.join("kepler.yaml");
        let config_yaml = serde_yaml::to_string(&config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&config_path, config_yaml)?;

        let state = new_shared_state();
        let (exit_tx, exit_rx) = mpsc::channel(32);

        // Load the config into state
        {
            let mut state = state.write();
            state.load_config(config_path.clone()).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            })?;
        }

        Ok(Self {
            state,
            config_path: config_path.clone(),
            config_dir: config_dir.to_path_buf(),
            exit_tx,
            exit_rx: Some(exit_rx),
        })
    }

    /// Create a harness from an existing config file
    pub async fn from_file(config_path: &Path) -> std::io::Result<Self> {
        let state = new_shared_state();
        let (exit_tx, exit_rx) = mpsc::channel(32);

        let config_path = config_path.canonicalize()?;
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // Load the config into state
        {
            let mut state = state.write();
            state.load_config(config_path.clone()).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            })?;
        }

        Ok(Self {
            state,
            config_path,
            config_dir,
            exit_tx,
            exit_rx: Some(exit_rx),
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

    /// Get the shared state
    pub fn state(&self) -> &SharedDaemonState {
        &self.state
    }

    /// Get the config path
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Start a specific service
    pub async fn start_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let (service_config, logs) = {
            let state = self.state.read();
            let config_state = state
                .configs
                .get(&self.config_path)
                .ok_or("Config not found")?;
            let service_config = config_state
                .config
                .services
                .get(service_name)
                .ok_or("Service not found")?
                .clone();
            (service_config, config_state.logs.clone())
        };

        // Update status to starting
        {
            let mut state = self.state.write();
            if let Some(config_state) = state.configs.get_mut(&self.config_path) {
                if let Some(service_state) = config_state.services.get_mut(service_name) {
                    service_state.status = ServiceStatus::Starting;
                }
            }
        }

        // Run on_init hook if not initialized
        let should_run_init = {
            let state = self.state.read();
            if let Some(config_state) = state.configs.get(&self.config_path) {
                if let Some(service_state) = config_state.services.get(service_name) {
                    !service_state.initialized
                } else {
                    false
                }
            } else {
                false
            }
        };

        if should_run_init {
            let working_dir = service_config
                .working_dir
                .clone()
                .unwrap_or_else(|| self.config_dir.clone());
            let env = build_service_env(&service_config, &self.config_dir)?;

            run_service_hook(
                &service_config.hooks,
                ServiceHookType::OnInit,
                service_name,
                &working_dir,
                &env,
                Some(&logs),
            )
            .await?;

            // Mark as initialized
            let mut state = self.state.write();
            if let Some(config_state) = state.configs.get_mut(&self.config_path) {
                if let Some(service_state) = config_state.services.get_mut(service_name) {
                    service_state.initialized = true;
                }
            }
        }

        // Run on_start hook
        {
            let working_dir = service_config
                .working_dir
                .clone()
                .unwrap_or_else(|| self.config_dir.clone());
            let env = build_service_env(&service_config, &self.config_dir)?;

            run_service_hook(
                &service_config.hooks,
                ServiceHookType::OnStart,
                service_name,
                &working_dir,
                &env,
                Some(&logs),
            )
            .await?;
        }

        // Spawn the process
        let handle = spawn_service(
            &self.config_path,
            service_name,
            &service_config,
            &self.config_dir,
            logs.clone(),
            self.state.clone(),
            self.exit_tx.clone(),
        )
        .await?;

        let pid = handle.child.id();

        // Store the handle and update state
        {
            let mut state = self.state.write();
            state
                .processes
                .insert((self.config_path.clone(), service_name.to_string()), handle);

            if let Some(config_state) = state.configs.get_mut(&self.config_path) {
                if let Some(service_state) = config_state.services.get_mut(service_name) {
                    service_state.status = ServiceStatus::Running;
                    service_state.pid = pid;
                    service_state.started_at = Some(Utc::now());
                }
            }
        }

        Ok(())
    }

    /// Start the health checker for a service
    pub fn start_health_checker(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let health_config = {
            let state = self.state.read();
            let config_state = state
                .configs
                .get(&self.config_path)
                .ok_or("Config not found")?;
            let service_config = config_state
                .config
                .services
                .get(service_name)
                .ok_or("Service not found")?;
            service_config.healthcheck.clone()
        };

        if let Some(health_config) = health_config {
            let handle = spawn_health_checker(
                self.config_path.clone(),
                service_name.to_string(),
                health_config,
                self.state.clone(),
            );

            // Store the health check handle
            let mut state = self.state.write();
            state.health_checks.insert(
                (self.config_path.clone(), service_name.to_string()),
                handle,
            );
        }

        Ok(())
    }

    /// Stop a specific service
    pub async fn stop_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Get service config for hooks
        let (service_config, logs) = {
            let state = self.state.read();
            let config_state = state
                .configs
                .get(&self.config_path)
                .ok_or("Config not found")?;
            let service_config = config_state
                .config
                .services
                .get(service_name)
                .ok_or("Service not found")?
                .clone();
            (service_config, config_state.logs.clone())
        };

        // Run on_stop hook
        let working_dir = service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.clone());
        let env = build_service_env(&service_config, &self.config_dir)?;

        run_service_hook(
            &service_config.hooks,
            ServiceHookType::OnStop,
            service_name,
            &working_dir,
            &env,
            Some(&logs),
        )
        .await?;

        // Stop the service
        stop_service(&self.config_path, service_name, self.state.clone()).await?;

        Ok(())
    }

    /// Stop all services
    pub async fn stop_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        let service_names: Vec<String> = {
            let state = self.state.read();
            if let Some(config_state) = state.configs.get(&self.config_path) {
                config_state.services.keys().cloned().collect()
            } else {
                Vec::new()
            }
        };

        for name in service_names {
            let _ = self.stop_service(&name).await;
        }

        Ok(())
    }

    /// Get the current status of a service
    pub fn get_status(&self, service_name: &str) -> Option<ServiceStatus> {
        let state = self.state.read();
        state
            .configs
            .get(&self.config_path)?
            .services
            .get(service_name)
            .map(|s| s.status)
    }

    /// Check if a service has a health check configured
    pub fn has_healthcheck(&self, service_name: &str) -> bool {
        let state = self.state.read();
        if let Some(config_state) = state.configs.get(&self.config_path) {
            if let Some(service_config) = config_state.config.services.get(service_name) {
                return service_config.healthcheck.is_some();
            }
        }
        false
    }

    /// Get the logs buffer
    pub fn logs(&self) -> Option<SharedLogBuffer> {
        let state = self.state.read();
        state.configs.get(&self.config_path).map(|c| c.logs.clone())
    }

    /// Reload the config from disk
    pub fn reload_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut state = self.state.write();
        state.load_config(self.config_path.clone())?;
        Ok(())
    }
}

impl Drop for TestDaemonHarness {
    fn drop(&mut self) {
        // Cancel all health checks and watchers
        let mut state = self.state.write();

        // Cancel health checks for this config
        let health_keys: Vec<_> = state
            .health_checks
            .keys()
            .filter(|(p, _)| p == &self.config_path)
            .cloned()
            .collect();
        for key in health_keys {
            if let Some(handle) = state.health_checks.remove(&key) {
                handle.abort();
            }
        }

        // Cancel watchers for this config
        let watcher_keys: Vec<_> = state
            .watchers
            .keys()
            .filter(|(p, _)| p == &self.config_path)
            .cloned()
            .collect();
        for key in watcher_keys {
            if let Some(handle) = state.watchers.remove(&key) {
                handle.abort();
            }
        }
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
        assert_eq!(harness.get_status("test"), Some(ServiceStatus::Running));

        // Stop the service
        harness.stop_service("test").await.unwrap();
        assert_eq!(harness.get_status("test"), Some(ServiceStatus::Stopped));
    }
}
