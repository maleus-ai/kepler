//! Process spawning functions for blocking and detached execution

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::CommandSpec;
use crate::errors::{DaemonError, Result};
use crate::logs::{BufferedLogWriter, LogStream, LogWriterConfig};

/// Cached result of kepler-exec binary lookup.
/// `Some(path)` = found, `None` = not found (will fall back to fork).
static KEPLER_EXEC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Locate the `kepler-exec` binary near the current executable.
/// Checks as a sibling first, then in the parent directory (handles
/// `target/debug/deps/` layout during `cargo test`).
/// Validates ownership and permissions before trusting.
fn find_kepler_exec() -> Option<PathBuf> {
    KEPLER_EXEC_PATH
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;

            // Check as sibling (installed layout: both in the same directory)
            let sibling = exe.with_file_name("kepler-exec");
            if sibling.is_file() && verify_binary_permissions(&sibling) {
                debug!("Found kepler-exec at {:?}", sibling);
                return Some(sibling);
            }

            None
        })
        .clone()
}

/// Verify that a kepler-exec binary is safe to execute:
/// - Owned by root or the same user running the daemon
/// - Not world-writable
#[cfg(unix)]
fn verify_binary_permissions(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    // Use symlink_metadata to avoid following symlinks
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            warn!("Cannot stat kepler-exec at {:?}: {}", path, e);
            return false;
        }
    };

    // Reject symlinks — binary must be a regular file
    if meta.file_type().is_symlink() {
        warn!(
            "Rejecting kepler-exec at {:?}: is a symlink",
            path
        );
        return false;
    }

    let file_uid = meta.uid();
    let my_euid = nix::unistd::geteuid().as_raw();

    // Must be owned by root or by the daemon's effective user
    if file_uid != 0 && file_uid != my_euid {
        warn!(
            "Rejecting kepler-exec at {:?}: owned by uid {} (expected root or uid {})",
            path, file_uid, my_euid
        );
        return false;
    }

    let mode = meta.mode();

    // Must not be group-writable (mode & 0o020)
    if mode & 0o020 != 0 {
        warn!(
            "Rejecting kepler-exec at {:?}: group-writable (mode {:o})",
            path, mode
        );
        return false;
    }

    // Must not be world-writable (mode & 0o002)
    if mode & 0o002 != 0 {
        warn!(
            "Rejecting kepler-exec at {:?}: world-writable (mode {:o})",
            path, mode
        );
        return false;
    }

    true
}

#[cfg(not(unix))]
fn verify_binary_permissions(_path: &std::path::Path) -> bool {
    true
}


/// Configuration for capturing `::output::KEY=VALUE` marker lines from stdout.
#[derive(Debug, Clone)]
pub struct OutputCaptureConfig {
    pub max_size: usize,
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
        /// Optional output capture config for `::output::` marker lines
        output_capture: Option<OutputCaptureConfig>,
    },
}

/// Result of spawning a blocking command
#[derive(Debug)]
pub struct BlockingResult {
    pub exit_code: Option<i32>,
    /// Captured `KEY=VALUE` lines from `::output::` markers (if output capture was enabled)
    pub captured_output: Option<Vec<String>>,
}

/// Result of spawning a detached command
pub struct DetachedResult {
    pub child: Child,
    pub stdout_task: Option<JoinHandle<Option<Vec<String>>>>,
    pub stderr_task: Option<JoinHandle<Option<Vec<String>>>>,
}

/// Build a `Command` from a `CommandSpec`, applying all common configuration:
/// validation, working directory, environment, user/group dropping, and resource limits.
///
/// When uid/gid or resource limits are needed, delegates to `kepler-exec` wrapper
/// binary to avoid fork() overhead (keeps the Command on the posix_spawn fast path).
/// Returns an error if `kepler-exec` is needed but not found.
///
/// Returns the configured `Command` and the program name (for error context).
fn build_command(spec: &CommandSpec) -> Result<(Command, String)> {
    if spec.program_and_args.is_empty() {
        return Err(DaemonError::Config("Empty command".to_string()));
    }

    let program = &spec.program_and_args[0];
    let needs_wrapper = spec.user.is_some() || spec.limits.is_some() || spec.no_new_privileges;

    let mut cmd;
    #[cfg(unix)]
    if needs_wrapper {
        let exec_path = find_kepler_exec().ok_or_else(|| {
            DaemonError::Config(
                "kepler-exec binary not found; it must be installed alongside kepler-daemon \
                 to apply user/group/resource-limit settings"
                    .to_string(),
            )
        })?;
        let mut wrapper_args: Vec<String> = Vec::new();

        // Validate and pass user spec to kepler-exec
        if let Some(ref user) = spec.user {
            // Pre-validate: catch errors early with a clear message
            // (kepler-exec would also reject, but only as exit code 127)
            use crate::user::resolve_user;
            let resolved = resolve_user(user)?;
            debug!("Command will run as uid={}, gid={} (via kepler-exec)", resolved.uid, resolved.gid);
            wrapper_args.push("--user".to_string());
            wrapper_args.push(user.clone());
        }

        // Validate and pass explicit groups lockdown
        if !spec.groups.is_empty() {
            for g in &spec.groups {
                crate::user::resolve_group(g)?;
            }
            wrapper_args.push("--groups".to_string());
            wrapper_args.push(spec.groups.join(","));
        }

        // Resolve resource limits to numeric values
        if let Some(ref limits) = spec.limits {
            use crate::config::parse_memory_limit;

            if let Some(ref mem_str) = limits.memory
                && let Ok(bytes) = parse_memory_limit(mem_str)
            {
                wrapper_args.push("--rlimit-as".to_string());
                wrapper_args.push(bytes.to_string());
            }
            if let Some(cpu_secs) = limits.cpu_time {
                wrapper_args.push("--rlimit-cpu".to_string());
                wrapper_args.push(cpu_secs.to_string());
            }
            if let Some(max_fds) = limits.max_fds {
                wrapper_args.push("--rlimit-nofile".to_string());
                wrapper_args.push(max_fds.to_string());
            }
        }

        if spec.no_new_privileges {
            wrapper_args.push("--no-new-privileges".to_string());
        }

        wrapper_args.push("--".to_string());
        wrapper_args.extend(spec.program_and_args.iter().cloned());

        debug!("Spawning via kepler-exec: {:?} {:?}", exec_path, wrapper_args);

        cmd = Command::new(exec_path);
        cmd.args(&wrapper_args);
    } else {
        let args = &spec.program_and_args[1..];
        debug!("Spawning command: {} {:?}", program, args);

        cmd = Command::new(program);
        cmd.args(args);
    }

    #[cfg(not(unix))]
    {
        if needs_wrapper {
            return Err(DaemonError::Config(
                "User/group/resource-limit settings are only supported on Unix".to_string(),
            ));
        }
        let args = &spec.program_and_args[1..];
        debug!("Spawning command: {} {:?}", program, args);

        cmd = Command::new(program);
        cmd.args(args);
    }

    if !spec.working_dir.exists() {
        return Err(DaemonError::Config(format!(
            "Working directory '{}' does not exist",
            spec.working_dir.display()
        )));
    }

    cmd.current_dir(&spec.working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Create a new process group so we can kill all descendants
    #[cfg(unix)]
    cmd.process_group(0);

    // Clear environment if requested (secure default)
    if spec.clear_env {
        cmd.env_clear();
    }
    cmd.envs(&spec.environment);

    Ok((cmd, program.clone()))
}

/// Spawn a task that reads and discards all lines from a stream.
/// Prevents the child process from blocking on a full pipe buffer.
fn spawn_drain_task(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Some(stream) = stream {
            let reader = BufReader::new(stream);
            let mut lines = reader.lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        }
    })
}

/// Spawn a task that captures lines from a stream, optionally logging and writing to disk.
///
/// - `log_to_tracing`: if true, also emits `info!` for each line (used by hooks)
/// - `output_capture`: if Some, filters `::output::KEY=VALUE` lines and returns them
///
/// Returns `Some(Vec<String>)` of raw `KEY=VALUE` strings if output capture is enabled,
/// `None` otherwise.
fn spawn_capture_task(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    log_config: Option<LogWriterConfig>,
    service_name: String,
    log_stream: LogStream,
    should_store: bool,
    log_to_tracing: bool,
    output_capture: Option<OutputCaptureConfig>,
) -> JoinHandle<Option<Vec<String>>> {
    tokio::spawn(async move {
        let mut captured: Option<Vec<String>> = output_capture.as_ref().map(|_| Vec::new());
        let max_size = output_capture.as_ref().map(|c| c.max_size).unwrap_or(0);
        let mut captured_bytes: usize = 0;
        let mut capture_overflow = false;

        if let Some(stream) = stream {
            let mut writer = if should_store {
                log_config.map(|cfg| {
                    BufferedLogWriter::from_config(&cfg, &service_name, log_stream)
                })
            } else {
                None
            };

            let reader = BufReader::new(stream);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Check for ::output:: marker
                if let Some(ref mut cap) = captured
                    && let Some(kv) = line.strip_prefix("::output::") {
                        if kv.contains('=') {
                            if !capture_overflow {
                                let line_size = kv.len();
                                if captured_bytes + line_size <= max_size {
                                    cap.push(kv.to_string());
                                    captured_bytes += line_size;
                                } else {
                                    capture_overflow = true;
                                    warn!(
                                        "[{}] Output capture exceeded max size ({}), ignoring further markers",
                                        service_name, max_size
                                    );
                                }
                            }
                        } else {
                            warn!(
                                "[{}] Malformed output marker (missing '='): {}",
                                service_name, line
                            );
                        }
                        // Marker lines are NOT written to logs
                        continue;
                    }

                if log_to_tracing {
                    info!(target: "hook", "[{}] {}", service_name, line);
                }
                if let Some(ref mut w) = writer {
                    w.write(&line);
                    if lines.get_ref().buffer().is_empty() {
                        w.flush();
                    }
                }
            }
        }

        captured
    })
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
            let stdout_handle = spawn_drain_task(child.stdout.take());
            let stderr_handle = spawn_drain_task(child.stderr.take());

            let status = child.wait().await.map_err(|e| DaemonError::ProcessSpawn {
                service: program.clone(),
                source: e,
            })?;

            let _ = stdout_handle.await;
            let _ = stderr_handle.await;

            Ok(BlockingResult {
                exit_code: status.code(),
                captured_output: None,
            })
        }
        BlockingMode::WithLogging {
            log_config,
            log_service_name,
            store_stdout,
            store_stderr,
            output_capture,
        } => {
            // Only capture output from stdout (not stderr)
            let stdout_handle = spawn_capture_task(
                child.stdout.take(),
                if store_stdout { log_config.clone() } else { None },
                log_service_name.clone(),
                LogStream::Stdout,
                store_stdout,
                true,
                output_capture,
            );
            let stderr_handle = spawn_capture_task(
                child.stderr.take(),
                if store_stderr { log_config } else { None },
                log_service_name,
                LogStream::Stderr,
                store_stderr,
                true,
                None, // No output capture on stderr
            );

            let status = child.wait().await.map_err(|e| DaemonError::ProcessSpawn {
                service: program.clone(),
                source: e,
            })?;

            let captured_output = stdout_handle.await.ok().flatten();
            let _ = stderr_handle.await;

            Ok(BlockingResult {
                exit_code: status.code(),
                captured_output,
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
    output_capture: Option<OutputCaptureConfig>,
) -> Result<DetachedResult> {
    let (mut cmd, program) = build_command(&spec)?;

    let mut child = cmd.spawn().map_err(|e| DaemonError::ProcessSpawn {
        service: program.clone(),
        source: e,
    })?;

    let pid = child.id();
    debug!("Command spawned with PID {:?}", pid);

    let stdout_task = child.stdout.take().map(|stdout| {
        spawn_capture_task(
            Some(stdout),
            Some(log_config.clone()),
            log_service_name.clone(),
            LogStream::Stdout,
            store_stdout,
            false,
            output_capture,
        )
    });

    let stderr_task = child.stderr.take().map(|stderr| {
        spawn_capture_task(
            Some(stderr),
            Some(log_config),
            log_service_name,
            LogStream::Stderr,
            store_stderr,
            false,
            None, // No output capture on stderr
        )
    });

    Ok(DetachedResult {
        child,
        stdout_task,
        stderr_task,
    })
}
