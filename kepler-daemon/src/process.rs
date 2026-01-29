use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, trace, warn};

use crate::config::{
    parse_memory_limit, resolve_log_store, resolve_sys_env, LogConfig, ResourceLimits, ServiceConfig, SysEnvPolicy,
};
use crate::config_actor::{ConfigActorHandle, TaskHandleType};
use crate::errors::{DaemonError, Result};
use crate::logs::{LogStream, SharedLogBuffer};
use crate::state::{ProcessHandle, ServiceStatus};

/// Unified command specification for both services and hooks
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Program and arguments (e.g., ["sh", "-c", "script"] or ["prog", "arg1"])
    pub program_and_args: Vec<String>,
    /// Working directory for the command
    pub working_dir: PathBuf,
    /// Environment variables
    pub environment: HashMap<String, String>,
    /// User to run the command as (Unix only)
    pub user: Option<String>,
    /// Group to run the command as (Unix only)
    pub group: Option<String>,
    /// Resource limits (Unix only)
    pub limits: Option<ResourceLimits>,
    /// Whether to clear the environment before applying environment vars
    /// If false, inherits the daemon's environment
    pub clear_env: bool,
}

impl CommandSpec {
    /// Create a new CommandSpec with all fields (clears env by default)
    pub fn new(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        group: Option<String>,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            group,
            limits: None,
            clear_env: true, // Secure default
        }
    }

    /// Create a new CommandSpec with resource limits
    pub fn with_limits(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        group: Option<String>,
        limits: Option<ResourceLimits>,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            group,
            limits,
            clear_env: true, // Secure default
        }
    }

    /// Create a new CommandSpec with all options including clear_env
    pub fn with_all_options(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        group: Option<String>,
        limits: Option<ResourceLimits>,
        clear_env: bool,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            group,
            limits,
            clear_env,
        }
    }

    /// Create a builder for CommandSpec
    pub fn builder(program_and_args: Vec<String>) -> CommandSpecBuilder {
        CommandSpecBuilder::new(program_and_args)
    }
}

/// Builder for CommandSpec
#[derive(Debug)]
pub struct CommandSpecBuilder {
    program_and_args: Vec<String>,
    working_dir: Option<PathBuf>,
    environment: HashMap<String, String>,
    user: Option<String>,
    group: Option<String>,
    limits: Option<ResourceLimits>,
    clear_env: bool,
}

impl CommandSpecBuilder {
    /// Create a new builder with required program and args
    pub fn new(program_and_args: Vec<String>) -> Self {
        Self {
            program_and_args,
            working_dir: None,
            environment: HashMap::new(),
            user: None,
            group: None,
            limits: None,
            clear_env: true, // Secure default
        }
    }

    /// Set the working directory
    pub fn working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    /// Set the environment variables
    pub fn environment(mut self, env: HashMap<String, String>) -> Self {
        self.environment = env;
        self
    }

    /// Set the user
    pub fn user(mut self, user: Option<String>) -> Self {
        self.user = user;
        self
    }

    /// Set the group
    pub fn group(mut self, group: Option<String>) -> Self {
        self.group = group;
        self
    }

    /// Set resource limits
    pub fn limits(mut self, limits: Option<ResourceLimits>) -> Self {
        self.limits = limits;
        self
    }

    /// Set whether to clear the environment (default: true)
    pub fn clear_env(mut self, clear: bool) -> Self {
        self.clear_env = clear;
        self
    }

    /// Build the CommandSpec (panics if working_dir not set)
    pub fn build(self) -> CommandSpec {
        CommandSpec {
            program_and_args: self.program_and_args,
            working_dir: self.working_dir.expect("working_dir is required"),
            environment: self.environment,
            user: self.user,
            group: self.group,
            limits: self.limits,
            clear_env: self.clear_env,
        }
    }

    /// Build the CommandSpec with a default working directory
    pub fn build_with_default_dir(self, default_dir: PathBuf) -> CommandSpec {
        CommandSpec {
            program_and_args: self.program_and_args,
            working_dir: self.working_dir.unwrap_or(default_dir),
            environment: self.environment,
            user: self.user,
            group: self.group,
            limits: self.limits,
            clear_env: self.clear_env,
        }
    }
}

/// Apply resource limits using setrlimit (Unix only)
#[cfg(unix)]
fn apply_resource_limits(limits: &ResourceLimits) -> std::io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};

    if let Some(ref mem_str) = limits.memory {
        if let Ok(bytes) = parse_memory_limit(mem_str) {
            setrlimit(Resource::RLIMIT_AS, bytes, bytes).map_err(std::io::Error::other)?;
        }
    }

    if let Some(cpu_secs) = limits.cpu_time {
        setrlimit(Resource::RLIMIT_CPU, cpu_secs, cpu_secs).map_err(std::io::Error::other)?;
    }

    if let Some(max_fds) = limits.max_fds {
        setrlimit(Resource::RLIMIT_NOFILE, max_fds, max_fds).map_err(std::io::Error::other)?;
    }

    Ok(())
}

/// Mode for blocking command execution
#[derive(Debug)]
pub enum BlockingMode {
    /// Wait for completion silently
    Silent,
    /// Wait for completion with logging to tracing and SharedLogBuffer (for hooks)
    WithLogging {
        logs: Option<SharedLogBuffer>,
        log_service_name: String,
        /// Whether to store stdout output
        store_stdout: bool,
        /// Whether to store stderr output
        store_stderr: bool,
    },
}

/// Result of spawning a blocking command
#[derive(Debug)]
pub struct BlockingResult {
    pub exit_code: Option<i32>,
}

/// Result of spawning a detached command
pub struct DetachedResult {
    pub child: Child,
    pub stdout_task: Option<JoinHandle<()>>,
    pub stderr_task: Option<JoinHandle<()>>,
}

/// Spawn a command and wait for completion
///
/// This function handles all the common logic for spawning processes:
/// - Command validation
/// - Working directory setup
/// - Environment configuration
/// - User/group privilege dropping (Unix only)
/// - Output capture (stdout/stderr)
///
/// The `mode` parameter determines behavior:
/// - `Silent`: Wait for completion and return exit code
/// - `WithLogging`: Wait with logging to tracing and SharedLogBuffer
pub async fn spawn_blocking(spec: CommandSpec, mode: BlockingMode) -> Result<BlockingResult> {
    // Validate command
    if spec.program_and_args.is_empty() {
        return Err(DaemonError::Config("Empty command".to_string()));
    }

    let program = &spec.program_and_args[0];
    let args = &spec.program_and_args[1..];

    debug!("Spawning command: {} {:?}", program, args);

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&spec.working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Clear environment if requested (secure default)
    if spec.clear_env {
        cmd.env_clear();
    }
    cmd.envs(&spec.environment);

    // Apply user/group if configured (Unix only)
    #[cfg(unix)]
    if let Some(ref user) = spec.user {
        use crate::user::resolve_user;

        let (uid, gid) = resolve_user(user, spec.group.as_deref())?;
        cmd.uid(uid);
        cmd.gid(gid);
        debug!("Command will run as uid={}, gid={}", uid, gid);
    }

    // Apply resource limits via pre_exec (Unix only)
    #[cfg(unix)]
    if let Some(ref limits) = spec.limits {
        let limits = limits.clone();
        // SAFETY: pre_exec runs in a forked child process before exec.
        // apply_resource_limits only calls setrlimit which is async-signal-safe.
        unsafe {
            cmd.pre_exec(move || apply_resource_limits(&limits));
        }
    }

    let mut child = cmd.spawn().map_err(|e| DaemonError::ProcessSpawn {
        service: program.clone(),
        source: e,
    })?;

    let pid = child.id();
    debug!("Command spawned with PID {:?}", pid);

    match mode {
        BlockingMode::Silent => {
            // Capture stdout
            let stdout = child.stdout.take();
            let stdout_handle = tokio::spawn(async move {
                let mut output = Vec::new();
                if let Some(stdout) = stdout {
                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        output.push(line);
                    }
                }
                output
            });

            // Capture stderr
            let stderr = child.stderr.take();
            let stderr_handle = tokio::spawn(async move {
                let mut output = Vec::new();
                if let Some(stderr) = stderr {
                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        output.push(line);
                    }
                }
                output
            });

            // Wait for process to complete
            let status = child.wait().await.map_err(|e| DaemonError::ProcessSpawn {
                service: program.clone(),
                source: e,
            })?;

            // Wait for output capture to complete
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;

            Ok(BlockingResult {
                exit_code: status.code(),
            })
        }
        BlockingMode::WithLogging {
            logs,
            log_service_name,
            store_stdout,
            store_stderr,
        } => {
            // Capture stdout with conditional logging
            let stdout = child.stdout.take();
            let logs_for_stdout = if store_stdout { logs.clone() } else { None };
            let service_name_stdout = log_service_name.clone();
            let should_store_stdout = store_stdout;
            let stdout_handle = tokio::spawn(async move {
                if let Some(stdout) = stdout {
                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if should_store_stdout {
                            // Log to daemon's log output
                            info!(target: "hook", "[{}] {}", service_name_stdout, line);
                            // Also capture in log buffer
                            if let Some(ref logs) = logs_for_stdout {
                                logs.push(service_name_stdout.clone(), line, LogStream::Stdout);
                            }
                        }
                    }
                }
            });

            // Capture stderr with conditional logging
            let stderr = child.stderr.take();
            let logs_for_stderr = if store_stderr { logs.clone() } else { None };
            let service_name_stderr = log_service_name.clone();
            let should_store_stderr = store_stderr;
            let stderr_handle = tokio::spawn(async move {
                if let Some(stderr) = stderr {
                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if should_store_stderr {
                            // Log to daemon's log output
                            info!(target: "hook", "[{}] {}", service_name_stderr, line);
                            // Also capture in log buffer
                            if let Some(ref logs) = logs_for_stderr {
                                logs.push(service_name_stderr.clone(), line, LogStream::Stderr);
                            }
                        }
                    }
                }
            });

            // Wait for process to complete
            let status = child.wait().await.map_err(|e| DaemonError::ProcessSpawn {
                service: program.clone(),
                source: e,
            })?;

            // Wait for output capture to complete
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;

            Ok(BlockingResult {
                exit_code: status.code(),
            })
        }
    }
}

/// Spawn a detached command, returning the Child and output tasks for monitoring
pub async fn spawn_detached(
    spec: CommandSpec,
    logs: SharedLogBuffer,
    log_service_name: String,
    store_stdout: bool,
    store_stderr: bool,
) -> Result<DetachedResult> {
    // Validate command
    if spec.program_and_args.is_empty() {
        return Err(DaemonError::Config("Empty command".to_string()));
    }

    let program = &spec.program_and_args[0];
    let args = &spec.program_and_args[1..];

    debug!("Spawning command: {} {:?}", program, args);

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&spec.working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Clear environment if requested (secure default)
    if spec.clear_env {
        cmd.env_clear();
    }
    cmd.envs(&spec.environment);

    // Apply user/group if configured (Unix only)
    #[cfg(unix)]
    if let Some(ref user) = spec.user {
        use crate::user::resolve_user;

        let (uid, gid) = resolve_user(user, spec.group.as_deref())?;
        cmd.uid(uid);
        cmd.gid(gid);
        debug!("Command will run as uid={}, gid={}", uid, gid);
    }

    // Apply resource limits via pre_exec (Unix only)
    #[cfg(unix)]
    if let Some(ref limits) = spec.limits {
        let limits = limits.clone();
        // SAFETY: pre_exec runs in a forked child process before exec.
        // apply_resource_limits only calls setrlimit which is async-signal-safe.
        unsafe {
            cmd.pre_exec(move || apply_resource_limits(&limits));
        }
    }

    let mut child = cmd.spawn().map_err(|e| DaemonError::ProcessSpawn {
        service: program.clone(),
        source: e,
    })?;

    let pid = child.id();
    debug!("Command spawned with PID {:?}", pid);

    // Capture stdout
    let stdout = child.stdout.take();
    let stdout_task = if let Some(stdout) = stdout {
        let logs_clone = logs.clone();
        let service_name_clone = log_service_name.clone();
        let should_store = store_stdout;
        Some(tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if should_store {
                    logs_clone.push(service_name_clone.clone(), line, LogStream::Stdout);
                }
            }
        }))
    } else {
        None
    };

    // Capture stderr
    let stderr = child.stderr.take();
    let stderr_task = if let Some(stderr) = stderr {
        let logs_clone = logs.clone();
        let service_name_clone = log_service_name.clone();
        let should_store = store_stderr;
        Some(tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if should_store {
                    logs_clone.push(service_name_clone.clone(), line, LogStream::Stderr);
                }
            }
        }))
    } else {
        None
    };

    Ok(DetachedResult {
        child,
        stdout_task,
        stderr_task,
    })
}

/// Message for process exit events
#[derive(Debug)]
pub struct ProcessExitEvent {
    pub config_path: PathBuf,
    pub service_name: String,
    pub exit_code: Option<i32>,
}

/// Parameters for spawning a service
pub struct SpawnServiceParams<'a> {
    pub service_name: &'a str,
    pub service_config: &'a ServiceConfig,
    pub config_dir: &'a Path,
    pub logs: SharedLogBuffer,
    pub handle: ConfigActorHandle,
    pub exit_tx: mpsc::Sender<ProcessExitEvent>,
    pub global_log_config: Option<&'a LogConfig>,
}

/// Spawn a service process with signal-based monitoring
pub async fn spawn_service(params: SpawnServiceParams<'_>) -> Result<ProcessHandle> {
    let SpawnServiceParams {
        service_name,
        service_config,
        config_dir,
        logs,
        handle,
        exit_tx,
        global_log_config,
    } = params;

    let working_dir = service_config
        .working_dir
        .clone()
        .unwrap_or_else(|| config_dir.to_path_buf());

    // Get environment from the service context (already pre-computed)
    let env = handle
        .get_service_context(service_name)
        .await
        .map(|ctx| ctx.env)
        .unwrap_or_default();

    // Validate command
    if service_config.command.is_empty() {
        return Err(DaemonError::Config(format!(
            "Service {} has empty command",
            service_name
        )));
    }

    info!(
        "Starting service {}: {} {:?}",
        service_name,
        &service_config.command[0],
        &service_config.command[1..]
    );

    // Resolve sys_env policy: service setting > global setting > default (Clear)
    let global_sys_env = handle.get_global_sys_env().await;
    let resolved_sys_env = resolve_sys_env(&service_config.sys_env, global_sys_env.as_ref());
    let clear_env = resolved_sys_env == SysEnvPolicy::Clear;

    let spec = CommandSpec::with_all_options(
        service_config.command.clone(),
        working_dir,
        env,
        service_config.user.clone(),
        service_config.group.clone(),
        service_config.limits.clone(),
        clear_env,
    );

    // Resolve store settings
    let (store_stdout, store_stderr) =
        resolve_log_store(service_config.logs.as_ref(), global_log_config);

    // Spawn the command detached for monitoring
    let result = spawn_detached(
        spec,
        logs,
        service_name.to_string(),
        store_stdout,
        store_stderr,
    )
    .await?;

    let pid = result.child.id();
    debug!(
        "Service {} spawned with PID {:?}",
        service_name, pid
    );

    // Store the PID in state immediately after spawning
    let _ = handle
        .set_service_pid(service_name, pid, Some(Utc::now()))
        .await;

    // Create shutdown channel for graceful stop
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    // Spawn process monitor with the Child (signal-based monitoring)
    let config_path = handle.config_path().clone();
    let service_name_clone = service_name.to_string();
    let handle_clone = handle.clone();

    tokio::spawn(async move {
        monitor_process(
            config_path,
            service_name_clone,
            result.child,
            shutdown_rx,
            handle_clone,
            exit_tx,
        )
        .await;
    });

    // Create ProcessHandle with output tasks and shutdown channel
    let process_handle = ProcessHandle {
        shutdown_tx: Some(shutdown_tx),
        stdout_task: result.stdout_task,
        stderr_task: result.stderr_task,
    };

    Ok(process_handle)
}

/// Monitor a process using signal-based waiting (child.wait())
/// This replaces the previous polling-based approach
async fn monitor_process(
    config_path: PathBuf,
    service_name: String,
    mut child: Child,
    shutdown_rx: oneshot::Receiver<()>,
    _handle: ConfigActorHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
) {
    // Use tokio::select! to wait for either process exit or shutdown signal
    tokio::select! {
        // Wait for process to exit naturally
        result = child.wait() => {
            let exit_code = result.ok().and_then(|s| s.code());
            info!(
                "Service {} exited with code {:?}",
                service_name, exit_code
            );

            // Send exit event with overflow handling
            let event = ProcessExitEvent {
                config_path,
                service_name: service_name.clone(),
                exit_code,
            };

            // Try to send immediately, with fallback to blocking send
            match exit_tx.try_send(event) {
                Ok(_) => {}
                Err(mpsc::error::TrySendError::Full(event)) => {
                    // Channel is full - log warning and try blocking send with timeout
                    warn!(
                        "Exit event channel near capacity for service {}, applying backpressure",
                        service_name
                    );
                    // Use send with timeout to avoid permanent blocking
                    let send_result = tokio::time::timeout(
                        tokio::time::Duration::from_secs(5),
                        exit_tx.send(event),
                    ).await;

                    if send_result.is_err() {
                        warn!(
                            "Failed to send exit event for service {} - channel timeout",
                            service_name
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!("Exit event channel closed for service {}", service_name);
                }
            }
        }
        // Wait for shutdown signal
        _ = shutdown_rx => {
            debug!("Shutdown signal received for {}", service_name);

            // Send SIGTERM for graceful shutdown
            #[cfg(unix)]
            {
                if let Some(pid) = child.id() {
                    debug!("Sending SIGTERM to process {}", pid);
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
                }
            }

            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }

            // Wait for process to exit with timeout
            let timeout_result = tokio::time::timeout(
                tokio::time::Duration::from_secs(10),
                child.wait(),
            )
            .await;

            match timeout_result {
                Ok(Ok(status)) => {
                    debug!("Service {} stopped with status {:?}", service_name, status);
                }
                Ok(Err(e)) => {
                    warn!("Error waiting for service {}: {}", service_name, e);
                }
                Err(_) => {
                    // Timeout - force kill
                    warn!("Service {} did not stop gracefully, force killing", service_name);
                    let _ = child.kill().await;
                }
            }

            // Note: We don't send an exit event here because stop_service handles the state update
        }
    }
}

/// Stop a service process
pub async fn stop_service(
    service_name: &str,
    handle: ConfigActorHandle,
) -> Result<()> {
    info!("Stopping service {}", service_name);

    // Update status to stopping
    let _ = handle
        .set_service_status(service_name, ServiceStatus::Stopping)
        .await;

    // Get the process handle
    let process_handle = handle.remove_process_handle(service_name).await;

    if let Some(process_handle) = process_handle {
        // Send shutdown signal to monitor task
        if let Some(shutdown_tx) = process_handle.shutdown_tx {
            let _ = shutdown_tx.send(());
        }

        // Cancel output tasks
        if let Some(task) = process_handle.stdout_task {
            task.abort();
        }
        if let Some(task) = process_handle.stderr_task {
            task.abort();
        }

        // Give the monitor some time to handle shutdown
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Cancel health check
    handle
        .cancel_task_handle(service_name, TaskHandleType::HealthCheck)
        .await;

    // Cancel watcher
    handle
        .cancel_task_handle(service_name, TaskHandleType::FileWatcher)
        .await;

    // Update status to stopped
    let _ = handle
        .set_service_status(service_name, ServiceStatus::Stopped)
        .await;

    let _ = handle.set_service_pid(service_name, None, None).await;

    Ok(())
}

// ============================================================================
// Process validation for reconnection
// ============================================================================

/// Validate that a process with the given PID is still running.
///
/// This is used during daemon restart to verify that a previously-recorded
/// process is still alive before attempting to reconnect to it.
///
/// # Arguments
/// * `pid` - The process ID to check
/// * `expected_start_time` - Optional expected start time (Unix timestamp) for
///   additional validation against PID reuse
///
/// # Returns
/// * `true` if the process exists (and optionally matches the start time)
/// * `false` if the process doesn't exist or has been reused
pub fn validate_running_process(pid: u32, expected_start_time: Option<i64>) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        // Check if process exists by sending signal 0 (doesn't actually send anything)
        let process_exists = kill(Pid::from_raw(pid as i32), None).is_ok();

        if !process_exists {
            trace!("Process {} does not exist", pid);
            return false;
        }

        // If we have an expected start time, validate against /proc/{pid}/stat
        if let Some(expected_ts) = expected_start_time {
            if let Some(actual_ts) = get_process_start_time(pid) {
                // Allow some tolerance (5 seconds) for timestamp differences
                let diff = (expected_ts - actual_ts).abs();
                if diff > 5 {
                    trace!(
                        "Process {} start time mismatch: expected {}, got {} (diff {})",
                        pid, expected_ts, actual_ts, diff
                    );
                    return false;
                }
            }
        }

        true
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms, we can't easily check process existence
        // Just assume the process is gone if we can't verify
        let _ = (pid, expected_start_time);
        false
    }
}

/// Get the start time of a process from /proc/{pid}/stat.
///
/// Returns the start time as a Unix timestamp, or None if it can't be determined.
#[cfg(unix)]
fn get_process_start_time(pid: u32) -> Option<i64> {
    use std::fs;

    // Read /proc/{pid}/stat
    let stat_path = format!("/proc/{}/stat", pid);
    let stat_content = fs::read_to_string(&stat_path).ok()?;

    // The stat file format has the start time as field 22 (1-indexed)
    // Format: pid (comm) state ppid pgrp session tty_nr ... starttime ...
    // We need to handle the case where comm might contain spaces or parentheses
    let _start_paren = stat_content.find('(')?;
    let end_paren = stat_content.rfind(')')?;
    let fields_after_comm = &stat_content[end_paren + 2..];
    let fields: Vec<&str> = fields_after_comm.split_whitespace().collect();

    // Field 20 (0-indexed after comm) is starttime in clock ticks since boot
    let starttime_ticks: u64 = fields.get(19)?.parse().ok()?;

    // Get system boot time
    let boot_time = get_boot_time()?;

    // Get clock ticks per second (typically 100 on Linux)
    let ticks_per_sec = get_clock_ticks_per_sec();

    // Calculate absolute start time
    let start_time = boot_time + (starttime_ticks / ticks_per_sec) as i64;

    Some(start_time)
}

/// Get the system boot time from /proc/stat.
#[cfg(unix)]
fn get_boot_time() -> Option<i64> {
    use std::fs;

    let stat_content = fs::read_to_string("/proc/stat").ok()?;
    for line in stat_content.lines() {
        if line.starts_with("btime ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            return parts.get(1)?.parse().ok();
        }
    }
    None
}

/// Get the number of clock ticks per second.
#[cfg(unix)]
fn get_clock_ticks_per_sec() -> u64 {
    // sysconf(_SC_CLK_TCK) typically returns 100 on Linux
    // We could use libc::sysconf but 100 is the common default
    100
}
