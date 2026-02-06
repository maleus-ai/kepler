//! Process spawning functions for blocking and detached execution

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{debug, info};

use super::CommandSpec;
use crate::config::ResourceLimits;
use crate::errors::{DaemonError, Result};
use crate::logs::{BufferedLogWriter, LogStream, LogWriterConfig};

/// Apply resource limits using setrlimit (Unix only)
#[cfg(unix)]
pub(super) fn apply_resource_limits(limits: &ResourceLimits) -> std::io::Result<()> {
    use crate::config::parse_memory_limit;
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
    /// Wait for completion with logging to tracing and disk (for hooks)
    WithLogging {
        log_config: Option<LogWriterConfig>,
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

/// Build a `Command` from a `CommandSpec`, applying all common configuration:
/// validation, working directory, environment, user/group dropping, and resource limits.
///
/// Returns the configured `Command` and the program name (for error context).
fn build_command(spec: &CommandSpec) -> Result<(Command, String)> {
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

    Ok((cmd, program.clone()))
}

/// Spawn a command and wait for completion
///
/// The `mode` parameter determines behavior:
/// - `Silent`: Wait for completion and return exit code
/// - `WithLogging`: Wait with logging to tracing and BufferedLogWriter
pub async fn spawn_blocking(spec: CommandSpec, mode: BlockingMode) -> Result<BlockingResult> {
    let (mut cmd, program) = build_command(&spec)?;

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
            log_config,
            log_service_name,
            store_stdout,
            store_stderr,
        } => {
            // Capture stdout with conditional logging
            let stdout = child.stdout.take();
            let config_for_stdout = if store_stdout { log_config.clone() } else { None };
            let service_name_stdout = log_service_name.clone();
            let should_store_stdout = store_stdout;
            let stdout_handle = tokio::spawn(async move {
                if let Some(stdout) = stdout {
                    // Create per-task BufferedLogWriter - no shared state
                    let mut writer = config_for_stdout.map(|cfg| {
                        BufferedLogWriter::from_config(&cfg, &service_name_stdout, LogStream::Stdout)
                    });

                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if should_store_stdout {
                            // Log to daemon's log output
                            info!(target: "hook", "[{}] {}", service_name_stdout, line);
                            // Write to log file
                            if let Some(ref mut w) = writer {
                                w.write(&line);
                            }
                        }
                    }
                    // writer.flush() called by Drop
                }
            });

            // Capture stderr with conditional logging
            let stderr = child.stderr.take();
            let config_for_stderr = if store_stderr { log_config.clone() } else { None };
            let service_name_stderr = log_service_name.clone();
            let should_store_stderr = store_stderr;
            let stderr_handle = tokio::spawn(async move {
                if let Some(stderr) = stderr {
                    // Create per-task BufferedLogWriter - no shared state
                    let mut writer = config_for_stderr.map(|cfg| {
                        BufferedLogWriter::from_config(&cfg, &service_name_stderr, LogStream::Stderr)
                    });

                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if should_store_stderr {
                            // Log to daemon's log output
                            info!(target: "hook", "[{}] {}", service_name_stderr, line);
                            // Write to log file
                            if let Some(ref mut w) = writer {
                                w.write(&line);
                            }
                        }
                    }
                    // writer.flush() called by Drop
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
    log_config: LogWriterConfig,
    log_service_name: String,
    store_stdout: bool,
    store_stderr: bool,
) -> Result<DetachedResult> {
    let (mut cmd, program) = build_command(&spec)?;

    let mut child = cmd.spawn().map_err(|e| DaemonError::ProcessSpawn {
        service: program.clone(),
        source: e,
    })?;

    let pid = child.id();
    debug!("Command spawned with PID {:?}", pid);

    // Capture stdout - each task gets its own BufferedLogWriter (no shared state)
    let stdout = child.stdout.take();
    let stdout_task = if let Some(stdout) = stdout {
        let config_clone = log_config.clone();
        let service_name_clone = log_service_name.clone();
        let should_store = store_stdout;
        Some(tokio::spawn(async move {
            // Create per-task BufferedLogWriter - no locks, no contention
            let mut writer = if should_store {
                Some(BufferedLogWriter::from_config(&config_clone, &service_name_clone, LogStream::Stdout))
            } else {
                None
            };

            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ref mut w) = writer {
                    w.write(&line);
                }
            }
            // writer.flush() called by Drop
        }))
    } else {
        None
    };

    // Capture stderr - each task gets its own BufferedLogWriter (no shared state)
    let stderr = child.stderr.take();
    let stderr_task = if let Some(stderr) = stderr {
        let config_clone = log_config.clone();
        let service_name_clone = log_service_name.clone();
        let should_store = store_stderr;
        Some(tokio::spawn(async move {
            // Create per-task BufferedLogWriter - no locks, no contention
            let mut writer = if should_store {
                Some(BufferedLogWriter::from_config(&config_clone, &service_name_clone, LogStream::Stderr))
            } else {
                None
            };

            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ref mut w) = writer {
                    w.write(&line);
                }
            }
            // writer.flush() called by Drop
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
