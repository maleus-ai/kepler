use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{parse_memory_limit, resolve_log_store, LogRetention, ResourceLimits, ServiceConfig};
use crate::env::build_service_env;
use crate::errors::{DaemonError, Result};
use crate::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use crate::logs::{LogStream, SharedLogBuffer};
use crate::state::{ProcessHandle, ServiceStatus, SharedDaemonState};

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
}

impl CommandSpec {
    /// Create a new CommandSpec with all fields
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

    /// Build the CommandSpec (panics if working_dir not set)
    pub fn build(self) -> CommandSpec {
        CommandSpec {
            program_and_args: self.program_and_args,
            working_dir: self.working_dir.expect("working_dir is required"),
            environment: self.environment,
            user: self.user,
            group: self.group,
            limits: self.limits,
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
        }
    }
}

/// Apply resource limits using setrlimit (Unix only)
#[cfg(unix)]
fn apply_resource_limits(limits: &ResourceLimits) -> std::io::Result<()> {
    use libc::{rlimit, setrlimit, RLIMIT_AS, RLIMIT_CPU, RLIMIT_NOFILE};

    if let Some(ref mem_str) = limits.memory {
        if let Ok(bytes) = parse_memory_limit(mem_str) {
            let rlim = rlimit {
                rlim_cur: bytes,
                rlim_max: bytes,
            };
            // SAFETY: setrlimit is safe to call with valid arguments
            if unsafe { setrlimit(RLIMIT_AS, &rlim) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }

    if let Some(cpu_secs) = limits.cpu_time {
        let rlim = rlimit {
            rlim_cur: cpu_secs,
            rlim_max: cpu_secs,
        };
        // SAFETY: setrlimit is safe to call with valid arguments
        if unsafe { setrlimit(RLIMIT_CPU, &rlim) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    if let Some(max_fds) = limits.max_fds {
        let rlim = rlimit {
            rlim_cur: max_fds,
            rlim_max: max_fds,
        };
        // SAFETY: setrlimit is safe to call with valid arguments
        if unsafe { setrlimit(RLIMIT_NOFILE, &rlim) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Mode for spawning commands
#[derive(Debug)]
pub enum SpawnMode {
    /// Wait for completion and check exit code (simple use case)
    Synchronous,
    /// Wait for completion with logging to tracing and SharedLogBuffer (for hooks)
    SynchronousWithLogging {
        logs: Option<SharedLogBuffer>,
        log_service_name: String,
        /// Whether to store stdout output
        store_stdout: bool,
        /// Whether to store stderr output
        store_stderr: bool,
    },
    /// Return a handle for async monitoring (for services)
    Asynchronous {
        logs: SharedLogBuffer,
        log_service_name: String,
        /// Whether to store stdout output
        store_stdout: bool,
        /// Whether to store stderr output
        store_stderr: bool,
    },
}

/// Result of spawning a command
#[derive(Debug)]
pub enum SpawnResult {
    /// Command completed (for synchronous mode)
    Completed { exit_code: Option<i32> },
    /// Process handle returned (for asynchronous mode)
    Handle(ProcessHandle),
}

/// Unified command spawning function for both services and hooks
///
/// This function handles all the common logic for spawning processes:
/// - Command validation
/// - Working directory setup
/// - Environment configuration
/// - User/group privilege dropping (Unix only)
/// - Output capture (stdout/stderr)
///
/// The `mode` parameter determines behavior:
/// - `Synchronous`: Wait for completion and return exit code
/// - `Asynchronous`: Return a ProcessHandle for later monitoring
pub async fn spawn_command(spec: CommandSpec, mode: SpawnMode) -> Result<SpawnResult> {
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
        .stderr(Stdio::piped())
        .envs(&spec.environment);

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
        SpawnMode::Synchronous => {
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

            Ok(SpawnResult::Completed {
                exit_code: status.code(),
            })
        }
        SpawnMode::SynchronousWithLogging {
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

            Ok(SpawnResult::Completed {
                exit_code: status.code(),
            })
        }
        SpawnMode::Asynchronous {
            logs,
            log_service_name,
            store_stdout,
            store_stderr,
        } => {
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

            Ok(SpawnResult::Handle(ProcessHandle {
                child,
                stdout_task,
                stderr_task,
            }))
        }
    }
}

/// Message for process exit events
#[derive(Debug)]
pub struct ProcessExitEvent {
    pub config_path: PathBuf,
    pub service_name: String,
    pub exit_code: Option<i32>,
}

use crate::config::LogConfig;

/// Parameters for spawning a service
pub struct SpawnServiceParams<'a> {
    pub config_path: &'a Path,
    pub service_name: &'a str,
    pub service_config: &'a ServiceConfig,
    pub config_dir: &'a Path,
    pub logs: SharedLogBuffer,
    pub state: SharedDaemonState,
    pub exit_tx: mpsc::Sender<ProcessExitEvent>,
    pub global_log_config: Option<&'a LogConfig>,
}

/// Spawn a service process
pub async fn spawn_service(params: SpawnServiceParams<'_>) -> Result<ProcessHandle> {
    let SpawnServiceParams {
        config_path,
        service_name,
        service_config,
        config_dir,
        logs,
        state,
        exit_tx,
        global_log_config,
    } = params;
    let working_dir = service_config
        .working_dir
        .clone()
        .unwrap_or_else(|| config_dir.to_path_buf());

    // Build environment
    let env = build_service_env(service_config, config_dir)?;

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

    // Build CommandSpec with resource limits
    let spec = CommandSpec::with_limits(
        service_config.command.clone(),
        working_dir,
        env,
        service_config.user.clone(),
        service_config.group.clone(),
        service_config.limits.clone(),
    );

    // Resolve store settings
    let (store_stdout, store_stderr) = resolve_log_store(
        service_config.logs.as_ref(),
        global_log_config,
    );

    // Spawn using unified function
    let mode = SpawnMode::Asynchronous {
        logs,
        log_service_name: service_name.to_string(),
        store_stdout,
        store_stderr,
    };

    let handle = match spawn_command(spec, mode).await? {
        SpawnResult::Handle(handle) => handle,
        SpawnResult::Completed { .. } => unreachable!("Asynchronous mode should return Handle"),
    };

    debug!("Service {} spawned with PID {:?}", service_name, handle.child.id());

    // Spawn process monitor
    let config_path = config_path.to_path_buf();
    let service_name_clone = service_name.to_string();
    let state_clone = state.clone();

    tokio::spawn(async move {
        monitor_process(config_path, service_name_clone, state_clone, exit_tx).await;
    });

    Ok(handle)
}

/// Monitor a process and send exit event when it terminates
async fn monitor_process(
    config_path: PathBuf,
    service_name: String,
    state: SharedDaemonState,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
) {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let should_check = {
            let state = state.read();
            if let Some(config_state) = state.configs.get(&config_path) {
                if let Some(service_state) = config_state.services.get(&service_name) {
                    service_state.status.is_running()
                } else {
                    false
                }
            } else {
                false
            }
        };

        if !should_check {
            break;
        }

        // Check if process has exited
        let exit_status = {
            let mut state = state.write();
            let key = (config_path.clone(), service_name.clone());
            if let Some(handle) = state.processes.get_mut(&key) {
                match handle.child.try_wait() {
                    Ok(Some(status)) => Some(status.code()),
                    Ok(None) => None, // Still running
                    Err(e) => {
                        error!("Error checking process status: {}", e);
                        Some(None)
                    }
                }
            } else {
                break;
            }
        };

        if let Some(exit_code) = exit_status {
            info!(
                "Service {} exited with code {:?}",
                service_name, exit_code
            );

            let _ = exit_tx
                .send(ProcessExitEvent {
                    config_path,
                    service_name,
                    exit_code,
                })
                .await;
            break;
        }
    }
}

/// Stop a service process
pub async fn stop_service(
    config_path: &Path,
    service_name: &str,
    state: SharedDaemonState,
) -> Result<()> {
    info!("Stopping service {}", service_name);

    let key = (config_path.to_path_buf(), service_name.to_string());

    // Update status to stopping
    {
        let mut state = state.write();
        if let Some(config_state) = state.configs.get_mut(&config_path.to_path_buf()) {
            if let Some(service_state) = config_state.services.get_mut(service_name) {
                service_state.status = ServiceStatus::Stopping;
            }
        }
    }

    // Get the process handle
    let handle = {
        let mut state = state.write();
        state.processes.remove(&key)
    };

    if let Some(mut handle) = handle {
        // Try graceful shutdown first with SIGTERM
        #[cfg(unix)]
        {
            if let Some(pid) = handle.child.id() {
                debug!("Sending SIGTERM to process {}", pid);
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = handle.child.kill().await;
        }

        // Wait for process to exit with timeout
        let timeout = tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            handle.child.wait(),
        )
        .await;

        match timeout {
            Ok(Ok(status)) => {
                debug!("Service {} stopped with status {:?}", service_name, status);
            }
            Ok(Err(e)) => {
                warn!("Error waiting for service {}: {}", service_name, e);
            }
            Err(_) => {
                // Timeout - force kill
                warn!("Service {} did not stop gracefully, force killing", service_name);
                let _ = handle.child.kill().await;
            }
        }

        // Cancel output tasks
        if let Some(task) = handle.stdout_task {
            task.abort();
        }
        if let Some(task) = handle.stderr_task {
            task.abort();
        }
    }

    // Cancel health check
    {
        let mut state = state.write();
        if let Some(handle) = state.health_checks.remove(&key) {
            handle.abort();
        }
    }

    // Cancel watcher
    {
        let mut state = state.write();
        if let Some(handle) = state.watchers.remove(&key) {
            handle.abort();
        }
    }

    // Update status to stopped
    {
        let mut state = state.write();
        if let Some(config_state) = state.configs.get_mut(&config_path.to_path_buf()) {
            if let Some(service_state) = config_state.services.get_mut(service_name) {
                service_state.status = ServiceStatus::Stopped;
                service_state.pid = None;
            }
        }
    }

    Ok(())
}

/// Handle process exit based on restart policy
pub async fn handle_process_exit(
    config_path: PathBuf,
    service_name: String,
    exit_code: Option<i32>,
    state: SharedDaemonState,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
) {
    // Get service config and state info
    let (service_config, config_dir, logs, hooks, global_log_config) = {
        let state = state.read();
        let config_state = match state.configs.get(&config_path) {
            Some(cs) => cs,
            None => return,
        };

        let service_config = match config_state.config.services.get(&service_name) {
            Some(sc) => sc.clone(),
            None => return,
        };

        (
            service_config.clone(),
            config_state
                .config_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
            config_state.logs.clone(),
            service_config.hooks.clone(),
            config_state.config.logs.clone(),
        )
    };

    // Update state
    {
        let mut state = state.write();
        if let Some(config_state) = state.configs.get_mut(&config_path) {
            if let Some(service_state) = config_state.services.get_mut(&service_name) {
                service_state.exit_code = exit_code;
                service_state.pid = None;

                // Remove process handle
                state
                    .processes
                    .remove(&(config_path.clone(), service_name.clone()));
            }
        }
    }

    // Run on_exit hook
    let env = build_service_env(&service_config, &config_dir).unwrap_or_default();
    let working_dir = service_config
        .working_dir
        .clone()
        .unwrap_or_else(|| config_dir.clone());
    let hook_params = ServiceHookParams {
        working_dir: &working_dir,
        env: &env,
        logs: Some(&logs),
        service_user: service_config.user.as_deref(),
        service_group: service_config.group.as_deref(),
        service_log_config: service_config.logs.as_ref(),
        global_log_config: global_log_config.as_ref(),
    };
    let _ = run_service_hook(
        &hooks,
        ServiceHookType::OnExit,
        &service_name,
        &hook_params,
    )
    .await;

    // Clear logs based on on_exit retention policy
    {
        use crate::config::resolve_log_retention;

        let retention = resolve_log_retention(
            service_config.logs.as_ref(),
            global_log_config.as_ref(),
            |l| l.get_on_exit(),
            LogRetention::Retain, // New default for on_exit
        );
        let should_clear = retention == LogRetention::Clear;

        if should_clear {
            logs.clear_service(&service_name);
            logs.clear_service_prefix(&format!("[{}.", service_name));
        }
    }

    // Determine if we should restart
    let should_restart = service_config.restart.should_restart_on_exit(exit_code);

    if should_restart {
        // Update status to starting
        {
            let mut state = state.write();
            if let Some(config_state) = state.configs.get_mut(&config_path) {
                if let Some(service_state) = config_state.services.get_mut(&service_name) {
                    service_state.restart_count += 1;
                    service_state.status = ServiceStatus::Starting;
                }
            }
        }

        // Small delay before restart
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        info!("Restarting service {} (policy: {:?})", service_name, service_config.restart.policy());

        // Run on_restart hook
        let _ = run_service_hook(
            &hooks,
            ServiceHookType::OnRestart,
            &service_name,
            &hook_params,
        )
        .await;

        // Clear logs based on on_restart retention policy
        {
            use crate::config::resolve_log_retention;

            let retention = resolve_log_retention(
                service_config.logs.as_ref(),
                global_log_config.as_ref(),
                |l| l.get_on_restart(),
                LogRetention::Retain, // New default for on_restart
            );
            let should_clear = retention == LogRetention::Clear;

            if should_clear {
                logs.clear_service(&service_name);
                logs.clear_service_prefix(&format!("[{}.", service_name));
            }
        }

        // Spawn new process
        let spawn_params = SpawnServiceParams {
            config_path: &config_path,
            service_name: &service_name,
            service_config: &service_config,
            config_dir: &config_dir,
            logs,
            state: state.clone(),
            exit_tx,
            global_log_config: global_log_config.as_ref(),
        };
        match spawn_service(spawn_params).await {
            Ok(handle) => {
                let pid = handle.child.id();
                let mut state = state.write();

                // Store process handle
                state
                    .processes
                    .insert((config_path.clone(), service_name.clone()), handle);

                // Update state
                if let Some(config_state) = state.configs.get_mut(&config_path) {
                    if let Some(service_state) = config_state.services.get_mut(&service_name) {
                        service_state.status = ServiceStatus::Running;
                        service_state.pid = pid;
                        service_state.started_at = Some(Utc::now());
                    }
                }
            }
            Err(e) => {
                error!("Failed to restart service {}: {}", service_name, e);
                let mut state = state.write();
                if let Some(config_state) = state.configs.get_mut(&config_path) {
                    if let Some(service_state) = config_state.services.get_mut(&service_name) {
                        service_state.status = ServiceStatus::Failed;
                    }
                }
            }
        }
    } else {
        // Mark as stopped or failed
        let mut state = state.write();
        if let Some(config_state) = state.configs.get_mut(&config_path) {
            if let Some(service_state) = config_state.services.get_mut(&service_name) {
                service_state.status = if exit_code == Some(0) {
                    ServiceStatus::Stopped
                } else {
                    ServiceStatus::Failed
                };
            }
        }
    }
}
