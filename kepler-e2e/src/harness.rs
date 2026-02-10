//! E2E test harness for Kepler
//!
//! Provides utilities for spawning and managing Kepler binaries in isolated
//! test environments.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::time::timeout;

/// Path to the config directory relative to the e2e crate root
const CONFIG_DIR: &str = "config";

/// Result type for E2E operations
pub type E2eResult<T> = Result<T, E2eError>;

/// Errors that can occur during E2E testing
#[derive(Error, Debug)]
pub enum E2eError {
    #[error("Failed to find kepler binary: {0}")]
    BinaryNotFound(String),

    #[error("Failed to spawn process: {0}")]
    SpawnFailed(#[from] std::io::Error),

    #[error("Timeout waiting for {0}")]
    Timeout(String),

    #[error("Command failed: {0}")]
    CommandFailed(String),

    #[error("Process error: {0}")]
    ProcessError(String),

    #[error("Config not found: {0}")]
    ConfigNotFound(PathBuf),
}

/// Output from a CLI command
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl CommandOutput {
    /// Check if the command succeeded (exit code 0)
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }

    /// Assert the command succeeded
    pub fn assert_success(&self) {
        assert!(
            self.success(),
            "Command failed with exit code {}.\nstdout: {}\nstderr: {}",
            self.exit_code, self.stdout, self.stderr
        );
    }

    /// Check if stdout contains a string
    pub fn stdout_contains(&self, s: &str) -> bool {
        self.stdout.contains(s)
    }

    /// Check if stderr contains a string
    pub fn stderr_contains(&self, s: &str) -> bool {
        self.stderr.contains(s)
    }
}

/// E2E test harness that manages isolated test environments
pub struct E2eHarness {
    /// Temporary directory for this test's state
    temp_dir: TempDir,
    /// Path to the kepler CLI binary
    kepler_bin: PathBuf,
    /// Path to the kepler-daemon binary
    daemon_bin: PathBuf,
    /// Running daemon process (if any)
    daemon_process: Option<Child>,
}

impl E2eHarness {
    /// Create a new E2E harness with isolated state directory
    pub async fn new() -> E2eResult<Self> {
        let temp_dir = TempDir::new()?;
        let kepler_bin = find_binary("kepler")?;
        let daemon_bin = find_binary("kepler-daemon")?;

        Ok(Self {
            temp_dir,
            kepler_bin,
            daemon_bin,
            daemon_process: None,
        })
    }

    /// Get the path to the isolated state directory
    pub fn state_dir(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Get a reference to the temp directory
    pub fn temp_dir(&self) -> &TempDir {
        &self.temp_dir
    }

    /// Get the socket path for the daemon
    pub fn socket_path(&self) -> PathBuf {
        self.temp_dir.path().join("kepler.sock")
    }

    /// Get the configs directory where per-config state is stored
    pub fn configs_dir(&self) -> PathBuf {
        self.temp_dir.path().join("configs")
    }

    /// Check if the state directory for a specific config exists
    /// Config state directories are named by hash and contain source_path.txt
    pub fn config_state_exists(&self, config_path: &Path) -> bool {
        let configs_dir = self.configs_dir();
        if !configs_dir.exists() {
            return false;
        }

        // Look for a directory containing source_path.txt that points to our config
        if let Ok(entries) = std::fs::read_dir(&configs_dir) {
            for entry in entries.flatten() {
                let source_path_file = entry.path().join("source_path.txt");
                if source_path_file.exists()
                    && let Ok(content) = std::fs::read_to_string(&source_path_file) {
                        let stored_path = PathBuf::from(content.trim());
                        if stored_path == config_path {
                            return true;
                        }
                    }
            }
        }
        false
    }

    /// Get the state directory for a specific config (if it exists)
    pub fn get_config_state_dir(&self, config_path: &Path) -> Option<PathBuf> {
        let configs_dir = self.configs_dir();
        if !configs_dir.exists() {
            return None;
        }

        if let Ok(entries) = std::fs::read_dir(&configs_dir) {
            for entry in entries.flatten() {
                let source_path_file = entry.path().join("source_path.txt");
                if source_path_file.exists()
                    && let Ok(content) = std::fs::read_to_string(&source_path_file) {
                        let stored_path = PathBuf::from(content.trim());
                        if stored_path == config_path {
                            return Some(entry.path());
                        }
                    }
            }
        }
        None
    }

    /// Check if logs exist for a specific config
    pub fn logs_exist(&self, config_path: &Path) -> bool {
        if let Some(state_dir) = self.get_config_state_dir(config_path) {
            let logs_dir = state_dir.join("logs");
            if logs_dir.exists() {
                // Check if there are any log files
                if let Ok(entries) = std::fs::read_dir(&logs_dir) {
                    return entries.flatten().next().is_some();
                }
            }
        }
        false
    }

    /// Start the daemon process
    pub async fn start_daemon(&mut self) -> E2eResult<()> {
        if self.daemon_process.is_some() {
            return Ok(()); // Already running
        }

        let child = Command::new(&self.daemon_bin)
            .env("KEPLER_DAEMON_PATH", self.temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        self.daemon_process = Some(child);

        // Wait for the socket to appear
        self.wait_for_socket(Duration::from_secs(10)).await?;

        Ok(())
    }

    /// Start the daemon process with a clean environment.
    /// Only essential variables (PATH, HOME, etc.) and KEPLER_DAEMON_PATH are set.
    /// This is useful for testing that CLI environment is used, not daemon's.
    pub async fn start_daemon_with_clean_env(&mut self, excluded_vars: &[&str]) -> E2eResult<()> {
        if self.daemon_process.is_some() {
            return Ok(()); // Already running
        }

        let mut cmd = Command::new(&self.daemon_bin);

        // Clear environment and only set essential variables
        cmd.env_clear();

        // Inherit essential system variables
        for key in ["PATH", "HOME", "USER", "SHELL", "TERM", "LANG", "LC_ALL"] {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }

        // Set the daemon path
        cmd.env("KEPLER_DAEMON_PATH", self.temp_dir.path());

        // Inherit other env vars EXCEPT the excluded ones
        for (key, value) in std::env::vars() {
            if !excluded_vars.contains(&key.as_str())
                && !["PATH", "HOME", "USER", "SHELL", "TERM", "LANG", "LC_ALL", "KEPLER_DAEMON_PATH"].contains(&key.as_str())
            {
                cmd.env(key, value);
            }
        }

        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        self.daemon_process = Some(child);

        // Wait for the socket to appear
        self.wait_for_socket(Duration::from_secs(10)).await?;

        Ok(())
    }

    /// Wait for the daemon socket to be available
    async fn wait_for_socket(&self, timeout_duration: Duration) -> E2eResult<()> {
        let socket = self.socket_path();
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            if socket.exists() {
                // Give the daemon a moment to start accepting connections
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(E2eError::Timeout("daemon socket".to_string()))
    }

    /// Stop the daemon process
    pub async fn stop_daemon(&mut self) -> E2eResult<()> {
        if let Some(mut child) = self.daemon_process.take() {
            // Try graceful shutdown first via CLI
            let _ = self.run_cli(&["daemon", "stop"]).await;

            // Wait for process to exit
            let exit_timeout = timeout(Duration::from_secs(5), child.wait()).await;

            if exit_timeout.is_err() {
                // Force kill if graceful shutdown failed
                let _ = child.kill().await;
            }
        }

        Ok(())
    }

    /// Run the kepler CLI with the given arguments
    pub async fn run_cli(&self, args: &[&str]) -> E2eResult<CommandOutput> {
        self.run_cli_with_timeout(args, Duration::from_secs(30)).await
    }

    /// Run the kepler CLI with custom environment variables.
    /// The provided env vars are set IN ADDITION to the harness's base env.
    pub async fn run_cli_with_env(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> E2eResult<CommandOutput> {
        self.run_cli_with_env_and_timeout(args, env, Duration::from_secs(30)).await
    }

    /// Run the kepler CLI with custom environment variables and timeout.
    pub async fn run_cli_with_env_and_timeout(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        timeout_duration: Duration,
    ) -> E2eResult<CommandOutput> {
        let mut cmd = Command::new(&self.kepler_bin);
        cmd.args(args)
            .env("KEPLER_DAEMON_PATH", self.temp_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn()?;

        // Take stdout/stderr handles so we can read them in spawned tasks
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        // Spawn tasks to drain stdout/stderr (these complete when the pipes close)
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut out) = stdout_pipe {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf).await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut err) = stderr_pipe {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
            }
            buf
        });

        // Wait for the process with timeout — child is NOT moved into the future
        let result = timeout(timeout_duration, child.wait()).await;

        match result {
            Ok(Ok(status)) => {
                let stdout = stdout_task.await.unwrap_or_default();
                let stderr = stderr_task.await.unwrap_or_default();
                Ok(CommandOutput {
                    stdout: String::from_utf8_lossy(&stdout).to_string(),
                    stderr: String::from_utf8_lossy(&stderr).to_string(),
                    exit_code: status.code().unwrap_or(-1),
                })
            }
            Ok(Err(e)) => Err(E2eError::SpawnFailed(e)),
            Err(_) => {
                // Timeout — kill the child process to prevent orphans
                let _ = child.kill().await;
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                Err(E2eError::Timeout(format!(
                    "CLI command: {:?}",
                    args.join(" ")
                )))
            }
        }
    }

    /// Start services with custom environment variables for the CLI.
    /// This tests that the CLI's environment is used, not the daemon's.
    pub async fn start_services_with_env(
        &self,
        config_path: &Path,
        env: &[(&str, &str)],
    ) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli_with_env(&["-f", config_str, "start", "-d"], env).await
    }

    /// Run the kepler CLI with the given arguments and a custom timeout
    pub async fn run_cli_with_timeout(
        &self,
        args: &[&str],
        timeout_duration: Duration,
    ) -> E2eResult<CommandOutput> {
        self.run_cli_with_env_and_timeout(args, &[], timeout_duration).await
    }

    /// Start services from a config file (detached mode for tests)
    pub async fn start_services(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "start", "-d"]).await
    }

    /// Start services from a config file in detached --wait mode (blocks until startup cluster is ready, then returns)
    pub async fn start_services_wait(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli_with_timeout(
            &["-f", config_str, "start", "-d", "--wait"],
            Duration::from_secs(30),
        ).await
    }

    /// Stop services from a config file
    pub async fn stop_services(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "stop"]).await
    }

    /// Stop services and clean up state directory
    pub async fn stop_services_clean(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "stop", "--clean"]).await
    }

    /// Restart all services from a config file
    pub async fn restart_services(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "restart", "-d", "--wait"]).await
    }

    /// Restart a specific service
    pub async fn restart_service(&self, config_path: &Path, service: &str) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "restart", "-d", "--wait", service]).await
    }

    /// Recreate all services (stop → re-bake config → start fresh)
    pub async fn recreate_services(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        // Stop first, then recreate (re-bake), then start
        self.run_cli(&["-f", config_str, "stop"]).await?;
        self.run_cli(&["-f", config_str, "recreate"]).await?;
        self.run_cli(&["-f", config_str, "start", "-d", "--wait"]).await
    }

    /// Recreate services with custom environment variables
    pub async fn recreate_services_with_env(
        &self,
        config_path: &Path,
        env: &[(&str, &str)],
    ) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        // Stop first, then recreate with env (re-bake), then start with env
        self.run_cli(&["-f", config_str, "stop"]).await?;
        self.run_cli_with_env(&["-f", config_str, "recreate"], env).await?;
        self.run_cli_with_env(&["-f", config_str, "start", "-d", "--wait"], env).await
    }

    /// Start a specific service (detached mode for tests)
    pub async fn start_service(&self, config_path: &Path, service: &str) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "start", "-d", service]).await
    }

    /// Stop services with a specific signal
    pub async fn stop_services_with_signal(&self, config_path: &Path, signal: &str) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "stop", &format!("--signal={}", signal)]).await
    }

    /// Stop a specific service with a specific signal
    pub async fn stop_service_with_signal(&self, config_path: &Path, service: &str, signal: &str) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "stop", service, &format!("--signal={}", signal)]).await
    }

    /// Stop a specific service
    pub async fn stop_service(&self, config_path: &Path, service: &str) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "stop", service]).await
    }

    /// Get daemon status
    pub async fn daemon_status(&self) -> E2eResult<CommandOutput> {
        self.run_cli(&["daemon", "status"]).await
    }

    /// Create a file in the temp directory (for file watcher tests)
    pub fn create_temp_file(&self, name: &str, content: &str) -> E2eResult<PathBuf> {
        let file_path = self.temp_dir.path().join(name);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file_path, content)?;
        Ok(file_path)
    }

    /// Create a file in the config directory (for env_file tests).
    /// This places the file alongside config files so env_file can use relative paths.
    pub fn create_config_file(&self, name: &str, content: &str) -> E2eResult<PathBuf> {
        let config_dir = self.temp_dir.path().join("test_configs");
        std::fs::create_dir_all(&config_dir)?;
        let file_path = config_dir.join(name);
        std::fs::write(&file_path, content)?;
        Ok(file_path)
    }

    /// Modify a file (for file watcher tests)
    pub fn modify_file(&self, path: &Path, content: &str) -> E2eResult<()> {
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Create a subdirectory in the temp directory
    pub fn create_temp_dir(&self, name: &str) -> E2eResult<PathBuf> {
        let dir_path = self.temp_dir.path().join(name);
        std::fs::create_dir_all(&dir_path)?;
        Ok(dir_path)
    }

    /// Get logs with timestamps (same as get_logs since timestamps are configured in service logs config)
    pub async fn get_logs_with_timestamps(
        &self,
        config_path: &Path,
        service: Option<&str>,
        lines: usize,
    ) -> E2eResult<CommandOutput> {
        // Note: timestamps come from the logs config (timestamp: true), not a CLI flag
        self.get_logs(config_path, service, lines).await
    }

    /// Extract PID from ps output for a service.
    /// PID is always the last column: "NAME  STATUS  PID"
    /// STATUS can be multi-word (e.g., "Up 5s", "Exited (0) 14s ago")
    pub fn extract_pid_from_ps(&self, ps_output: &str, service: &str) -> Option<u32> {
        for line in ps_output.lines() {
            if line.contains(service) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                // PID is the last token
                if let Some(last) = parts.last() {
                    return last.parse().ok();
                }
            }
        }
        None
    }

    /// Wait for a service to NOT be in a specific status (useful for waiting for stop)
    pub async fn wait_for_service_not_status(
        &self,
        config_path: &Path,
        service: &str,
        status_to_avoid: &str,
        timeout_duration: Duration,
    ) -> E2eResult<()> {
        let display_pattern = status_to_display_pattern(status_to_avoid);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            let output = self.ps(config_path).await?;
            if output.success() {
                let mut found_with_status = false;
                for line in output.stdout.lines() {
                    if line.contains(service) && line.contains(display_pattern) {
                        found_with_status = true;
                        break;
                    }
                }
                if !found_with_status {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Err(E2eError::Timeout(format!(
            "service {} to leave status {}",
            service, status_to_avoid
        )))
    }

    /// Wait for service to reach any of several possible statuses.
    ///
    /// Accepts logical status names ("running", "stopped", etc.) and maps them
    /// to the Docker-style display format used by `kepler ps`.
    pub async fn wait_for_service_status_any(
        &self,
        config_path: &Path,
        service: &str,
        expected_statuses: &[&str],
        timeout_duration: Duration,
    ) -> E2eResult<String> {
        let patterns: Vec<(&str, &str)> = expected_statuses
            .iter()
            .map(|s| (*s, status_to_display_pattern(s)))
            .collect();
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            let output = self.ps(config_path).await?;
            if output.success() {
                for line in output.stdout.lines() {
                    if line.contains(service) {
                        for (logical, display) in &patterns {
                            if line.contains(display) {
                                return Ok(logical.to_string());
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Err(E2eError::Timeout(format!(
            "service {} to reach any of: {:?}",
            service, expected_statuses
        )))
    }

    /// Check if socket exists
    pub fn socket_exists(&self) -> bool {
        self.socket_path().exists()
    }

    /// Get logs for services
    pub async fn get_logs(
        &self,
        config_path: &Path,
        service: Option<&str>,
        lines: usize,
    ) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;

        let lines_str = lines.to_string();
        let mut args = vec!["-f", config_str, "logs", "--tail", &lines_str];
        let service_owned;
        if let Some(s) = service {
            service_owned = s.to_string();
            args.push(&service_owned);
        }

        self.run_cli(&args).await
    }

    /// Get logs for services with --no-hook flag (filters out hook logs)
    pub async fn get_logs_no_hooks(
        &self,
        config_path: &Path,
        service: Option<&str>,
        lines: usize,
    ) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;

        let lines_str = lines.to_string();
        let mut args = vec!["-f", config_str, "logs", "--tail", &lines_str, "--no-hook"];
        let service_owned;
        if let Some(s) = service {
            service_owned = s.to_string();
            args.push(&service_owned);
        }

        self.run_cli(&args).await
    }

    /// Prune orphaned config state
    pub async fn prune(&self, force: bool) -> E2eResult<CommandOutput> {
        let mut args = vec!["prune"];
        if force {
            args.push("--force");
        }
        self.run_cli(&args).await
    }

    /// Prune dry run
    pub async fn prune_dry_run(&self) -> E2eResult<CommandOutput> {
        self.run_cli(&["prune", "--dry-run"]).await
    }

    /// Get process status
    pub async fn ps(&self, config_path: &Path) -> E2eResult<CommandOutput> {
        let config_str = config_path.to_str().ok_or_else(|| {
            E2eError::CommandFailed("Invalid config path".to_string())
        })?;
        self.run_cli(&["-f", config_str, "ps"]).await
    }

    /// Wait for a service to be in a specific status.
    ///
    /// Accepts logical status names ("running", "stopped", etc.) and maps them
    /// to the Docker-style display format used by `kepler ps`.
    pub async fn wait_for_service_status(
        &self,
        config_path: &Path,
        service: &str,
        expected_status: &str,
        timeout_duration: Duration,
    ) -> E2eResult<()> {
        let display_pattern = status_to_display_pattern(expected_status);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            let output = self.ps(config_path).await?;
            if output.success() {
                for line in output.stdout.lines() {
                    if line.contains(service) && line.contains(display_pattern) {
                        return Ok(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Err(E2eError::Timeout(format!(
            "service {} to reach status {}",
            service, expected_status
        )))
    }

    /// Wait for logs to appear for a config
    pub async fn wait_for_logs(
        &self,
        config_path: &Path,
        timeout_duration: Duration,
    ) -> E2eResult<()> {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            if self.logs_exist(config_path) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Err(E2eError::Timeout("logs to appear".to_string()))
    }

    /// Wait for logs to contain specific text
    pub async fn wait_for_log_content(
        &self,
        config_path: &Path,
        expected_content: &str,
        timeout_duration: Duration,
    ) -> E2eResult<CommandOutput> {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            let output = self.get_logs(config_path, None, 1000).await?;
            if output.stdout_contains(expected_content) {
                return Ok(output);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Err(E2eError::Timeout(format!(
            "logs to contain '{}'",
            expected_content
        )))
    }

    /// Wait for a file to contain specific text (useful for hook marker files)
    pub async fn wait_for_file_content(
        &self,
        file_path: &Path,
        expected_content: &str,
        timeout_duration: Duration,
    ) -> E2eResult<String> {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout_duration {
            if let Ok(content) = std::fs::read_to_string(file_path)
                && content.contains(expected_content) {
                    return Ok(content);
                }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Return the current content (if any) in the error for debugging
        let current_content = std::fs::read_to_string(file_path).unwrap_or_default();
        Err(E2eError::Timeout(format!(
            "file '{}' to contain '{}'. Current content: {}",
            file_path.display(),
            expected_content,
            current_content
        )))
    }

    /// Create a test config file in a temp directory and return its path
    pub fn create_test_config(&self, content: &str) -> E2eResult<PathBuf> {
        let config_dir = self.temp_dir.path().join("test_configs");
        std::fs::create_dir_all(&config_dir)?;

        let config_path = config_dir.join("test.kepler.yaml");
        std::fs::write(&config_path, content)?;

        Ok(config_path)
    }

    /// Create a test config file with a specific name
    pub fn create_named_config(&self, name: &str, content: &str) -> E2eResult<PathBuf> {
        let config_dir = self.temp_dir.path().join("test_configs");
        std::fs::create_dir_all(&config_dir)?;

        let config_path = config_dir.join(name);
        std::fs::write(&config_path, content)?;

        Ok(config_path)
    }

    /// Copy a supporting file from the source config directory to the test's config directory.
    /// This is useful for Lua modules that need to be alongside the config for `require()`.
    ///
    /// # Arguments
    /// * `test_module` - The test module name (e.g., "lua_advanced_test")
    /// * `file_name` - The file name to copy (e.g., "order_tracker.lua")
    pub fn copy_supporting_file(&self, test_module: &str, file_name: &str) -> E2eResult<PathBuf> {
        let source_file = get_config_dir().join(test_module).join(file_name);

        if !source_file.exists() {
            return Err(E2eError::ConfigNotFound(source_file));
        }

        let config_dir = self.temp_dir.path().join("test_configs");
        std::fs::create_dir_all(&config_dir)?;

        let dest_path = config_dir.join(file_name);
        std::fs::copy(&source_file, &dest_path)?;

        Ok(dest_path)
    }

    /// Load a config from the e2e/config directory and copy it to the test's temp directory.
    ///
    /// # Arguments
    /// * `test_module` - The test module name (e.g., "stop_clean_test")
    /// * `config_name` - The config file name without extension (e.g., "test_stop_clean_removes_state_directory")
    ///
    /// # Example
    /// ```ignore
    /// let config_path = harness.load_config("stop_clean_test", "test_stop_clean_removes_state_directory")?;
    /// ```
    pub fn load_config(&self, test_module: &str, config_name: &str) -> E2eResult<PathBuf> {
        let source_config = get_config_dir()
            .join(test_module)
            .join(format!("{}.kepler.yaml", config_name));

        if !source_config.exists() {
            return Err(E2eError::ConfigNotFound(source_config));
        }

        let content = std::fs::read_to_string(&source_config)?;
        self.create_named_config(&format!("{}.kepler.yaml", config_name), &content)
    }

    /// Load a config and replace placeholders with values.
    ///
    /// # Arguments
    /// * `test_module` - The test module name
    /// * `config_name` - The config file name without extension
    /// * `replacements` - List of (placeholder, value) tuples
    ///
    /// # Example
    /// ```ignore
    /// let config_path = harness.load_config_with_replacements(
    ///     "prune_start_test",
    ///     "test_multiple_prune_start_cycles",
    ///     &[("CYCLE_MARKER", "CYCLE_1_MARKER")]
    /// )?;
    /// ```
    pub fn load_config_with_replacements(
        &self,
        test_module: &str,
        config_name: &str,
        replacements: &[(&str, &str)],
    ) -> E2eResult<PathBuf> {
        let source_config = get_config_dir()
            .join(test_module)
            .join(format!("{}.kepler.yaml", config_name));

        if !source_config.exists() {
            return Err(E2eError::ConfigNotFound(source_config));
        }

        let mut content = std::fs::read_to_string(&source_config)?;
        for (placeholder, value) in replacements {
            content = content.replace(placeholder, value);
        }

        self.create_named_config(&format!("{}.kepler.yaml", config_name), &content)
    }

    /// Update an existing config file with new content (for tests that modify configs between runs)
    pub fn update_config(&self, config_path: &Path, content: &str) -> E2eResult<()> {
        std::fs::write(config_path, content)?;
        Ok(())
    }

    /// Load a config from the e2e/config directory, apply replacements, and write to a specific destination
    pub fn load_config_to(
        &self,
        test_module: &str,
        config_name: &str,
        dest_name: &str,
        replacements: &[(&str, &str)],
    ) -> E2eResult<PathBuf> {
        let source_config = get_config_dir()
            .join(test_module)
            .join(format!("{}.kepler.yaml", config_name));

        if !source_config.exists() {
            return Err(E2eError::ConfigNotFound(source_config));
        }

        let mut content = std::fs::read_to_string(&source_config)?;
        for (placeholder, value) in replacements {
            content = content.replace(placeholder, value);
        }

        self.create_named_config(dest_name, &content)
    }
}

/// Map a logical service status name to the display pattern used by `kepler ps`.
///
/// The CLI renders Docker-style status strings (e.g., "Up 5s", "Stopped 3s ago"),
/// so tests that check for "running" need to look for "Up " in the output.
fn status_to_display_pattern(status: &str) -> &str {
    match status {
        "running" => "Up ",
        "healthy" => "(healthy)",
        "unhealthy" => "(unhealthy)",
        "stopped" => "Stopped",
        "exited" => "Exited",
        "failed" => "Failed",
        "starting" => "Starting",
        "stopping" => "Stopping",
        // Fallback: use the status as-is (for forward compatibility)
        other => other,
    }
}

/// Get the path to the e2e config directory
fn get_config_dir() -> PathBuf {
    // During cargo test, CARGO_MANIFEST_DIR points to the e2e crate
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        return PathBuf::from(manifest_dir).join(CONFIG_DIR);
    }

    // Fallback: relative to current directory
    PathBuf::from("e2e").join(CONFIG_DIR)
}

impl Drop for E2eHarness {
    fn drop(&mut self) {
        // Try to stop the daemon if running
        if let Some(mut child) = self.daemon_process.take() {
            let _ = child.start_kill();
        }
    }
}

/// Find a binary in common locations
fn find_binary(name: &str) -> E2eResult<PathBuf> {
    // 1. Check alongside current executable (same build profile)
    // Test binaries are typically in target/debug/deps, so we need to go up
    if let Ok(exe) = std::env::current_exe() {
        // Check same directory as executable
        if let Some(dir) = exe.parent() {
            let path = dir.join(name);
            if path.exists() {
                return Ok(path.canonicalize()?);
            }

            // Check parent directory (for when running from deps/)
            if let Some(parent) = dir.parent() {
                let path = parent.join(name);
                if path.exists() {
                    return Ok(path.canonicalize()?);
                }
            }
        }
    }

    // 2. Check target/release relative to current directory
    let release_path = PathBuf::from(format!("target/release/{}", name));
    if release_path.exists() {
        return Ok(release_path.canonicalize()?);
    }

    // 3. Check target/debug relative to current directory
    let debug_path = PathBuf::from(format!("target/debug/{}", name));
    if debug_path.exists() {
        return Ok(debug_path.canonicalize()?);
    }

    // 4. Check from CARGO_MANIFEST_DIR (set during cargo test)
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let workspace_root = PathBuf::from(&manifest_dir)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(&manifest_dir));

        // Try release
        let release_path = workspace_root.join("target/release").join(name);
        if release_path.exists() {
            return Ok(release_path.canonicalize()?);
        }

        // Try debug
        let debug_path = workspace_root.join("target/debug").join(name);
        if debug_path.exists() {
            return Ok(debug_path.canonicalize()?);
        }
    }

    // 5. Check PATH
    if let Ok(path) = which::which(name) {
        return Ok(path);
    }

    Err(E2eError::BinaryNotFound(format!(
        "{} not found. Run 'cargo build' first.",
        name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_output() {
        let output = CommandOutput {
            stdout: "Hello World".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
        };

        assert!(output.success());
        assert!(output.stdout_contains("Hello"));
        assert!(!output.stdout_contains("Goodbye"));
    }
}
