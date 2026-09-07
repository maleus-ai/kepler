//! Process spawning functions for blocking and detached execution

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::CommandSpec;
use crate::errors::{DaemonError, Result};
use crate::logs::{LogStoreHandle, LogWriter};

/// Max consecutive read errors tolerated on a captured stream before giving up.
/// A read error does not close the pipe fd, so we retry rather than permanently
/// ending capture; the cap stops a persistent error from hot-spinning.
const MAX_CAPTURE_READ_RETRIES: u32 = 5;
/// Backoff between capture read retries (keeps a persistent error from spinning).
const CAPTURE_READ_RETRY_BACKOFF: Duration = Duration::from_millis(50);

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
    /// Wait for completion, capturing combined stdout+stderr
    CaptureOutput,
    /// Wait for completion with logging to tracing and disk (for hooks)
    WithLogging {
        log_store: Option<LogStoreHandle>,
        log_service_name: String,
        /// Hook name (set for lifecycle hooks, None for regular commands)
        hook: Option<String>,
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
    /// Combined stdout+stderr (only set with `BlockingMode::CaptureOutput`)
    pub combined_output: Option<String>,
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
    // A cgroup to join also requires the wrapper: joining has to happen before
    // exec, and the daemon's own Command deliberately avoids pre_exec so Rust
    // can use posix_spawnp (see kepler-exec's module docs).
    let needs_wrapper = spec.user.is_some()
        || spec.limits.is_some()
        || spec.no_new_privileges
        || spec.cgroup_path.is_some();

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
            let max = kepler_unix::groups::ngroups_max();
            let groups = if spec.groups.len() > max {
                warn!(
                    "Truncating supplementary groups from {} to {} (NGROUPS_MAX on this platform)",
                    spec.groups.len(), max
                );
                &spec.groups[..max]
            } else {
                &spec.groups
            };
            wrapper_args.push("--groups".to_string());
            wrapper_args.push(groups.join(","));
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

        // Let kepler-exec join the cgroup before exec'ing, so the service is
        // already inside it when it starts forking.
        if let Some(ref cgroup) = spec.cgroup_path {
            wrapper_args.push("--cgroup".to_string());
            wrapper_args.push(cgroup.to_string_lossy().to_string());
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
    kepler_unix::process_tree::configure_process_tree(&mut cmd);

    // Clear environment if requested (secure default)
    if spec.clear_env {
        cmd.env_clear();
    }
    cmd.envs(&spec.environment);

    Ok((cmd, program.clone()))
}

/// Strip a single trailing `\n` (and a preceding `\r`, if present) from a line
/// buffer read via `read_until(b'\n', ...)`, matching the newline trimming that
/// `AsyncBufReadExt::lines()` performs.
fn strip_trailing_newline(buf: &mut Vec<u8>) {
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
}

/// Spawn a task that reads and discards all output from a stream.
/// Prevents the child process from blocking on a full pipe buffer.
///
/// Reads raw bytes (not UTF-8 lines) so that non-UTF-8 output cannot abort the
/// drain early and leave the child blocked writing to a full pipe.
fn spawn_drain_task(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Some(stream) = stream {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break, // EOF or I/O error
                    Ok(_) => {}              // discard
                }
            }
        }
    })
}

/// Spawn a task that collects all output from a stream into a String.
///
/// Reads raw bytes and lossily decodes each line, so non-UTF-8 output is
/// preserved (as U+FFFD) instead of truncating the collected output.
fn spawn_collect_task(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
) -> JoinHandle<String> {
    tokio::spawn(async move {
        let mut out = String::new();
        if let Some(stream) = stream {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break, // EOF or I/O error
                    Ok(_) => {
                        strip_trailing_newline(&mut buf);
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&String::from_utf8_lossy(&buf));
                    }
                }
            }
        }
        out
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
    log_store: Option<LogStoreHandle>,
    service_name: String,
    level: &'static str,
    hook: Option<String>,
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
            let writer = if should_store {
                log_store.map(|store| {
                    if let Some(ref hook_name) = hook {
                        LogWriter::with_hook(&store, &service_name, hook_name, level)
                    } else {
                        LogWriter::new(&store, &service_name, level)
                    }
                })
            } else {
                None
            };

            // Read raw bytes (not UTF-8 lines): a single invalid-UTF-8 byte must
            // not return an Err that silently ends capture and freezes this
            // service's logs. Invalid bytes are lossily decoded to U+FFFD; a
            // genuine I/O error is logged before ending the loop.
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            // Consecutive read errors. A read error does NOT close the pipe fd,
            // so retry (bounded, with backoff) instead of permanently ending
            // capture — one transient hiccup (more likely under GVisor's syscall
            // emulation) must not silence a service's logs until it restarts.
            // Reset on any successful read.
            let mut read_errors: u32 = 0;
            loop {
                // NOTE: `buf` is cleared only after a line is successfully
                // extracted (below), NOT at the top of the loop. On a transient
                // read error mid-line, read_until leaves the partial bytes it
                // already read in `buf`; keeping them means the retry's
                // read_until appends the rest and we emit the whole line intact
                // instead of splitting/dropping it.
                let line = match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        read_errors = 0;
                        strip_trailing_newline(&mut buf);
                        let line = String::from_utf8_lossy(&buf).into_owned();
                        buf.clear();
                        line
                    }
                    Err(e) => {
                        read_errors += 1;
                        if read_errors > MAX_CAPTURE_READ_RETRIES {
                            warn!(
                                "[{}] ending log capture after {} consecutive read errors: {}",
                                service_name, read_errors, e
                            );
                            break;
                        }
                        warn!(
                            "[{}] log capture read error (retry {}/{}): {}",
                            service_name, read_errors, MAX_CAPTURE_READ_RETRIES, e
                        );
                        tokio::time::sleep(CAPTURE_READ_RETRY_BACKOFF).await;
                        // Do NOT clear `buf` — preserve the partial line for retry.
                        continue;
                    }
                };

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
                if let Some(ref w) = writer {
                    w.write(&line);
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
/// - `WithLogging`: Wait with logging to tracing and LogWriter
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
                combined_output: None,
            })
        }
        BlockingMode::CaptureOutput => {
            let stdout_handle = spawn_collect_task(child.stdout.take());
            let stderr_handle = spawn_collect_task(child.stderr.take());

            let status = child.wait().await.map_err(|e| DaemonError::ProcessSpawn {
                service: program.clone(),
                source: e,
            })?;

            let stdout_text = stdout_handle.await.unwrap_or_default();
            let stderr_text = stderr_handle.await.unwrap_or_default();

            let mut combined = String::new();
            if !stdout_text.is_empty() {
                combined.push_str(&stdout_text);
            }
            if !stderr_text.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr_text);
            }

            Ok(BlockingResult {
                exit_code: status.code(),
                captured_output: None,
                combined_output: if combined.is_empty() { None } else { Some(combined) },
            })
        }
        BlockingMode::WithLogging {
            log_store,
            log_service_name,
            hook,
            store_stdout,
            store_stderr,
            output_capture,
        } => {
            // Only capture output from stdout (not stderr)
            let stdout_handle = spawn_capture_task(
                child.stdout.take(),
                if store_stdout { log_store.clone() } else { None },
                log_service_name.clone(),
                "info",
                hook.clone(),
                store_stdout,
                true,
                output_capture,
            );
            let stderr_handle = spawn_capture_task(
                child.stderr.take(),
                if store_stderr { log_store } else { None },
                log_service_name,
                "error",
                hook,
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
                combined_output: None,
            })
        }
    }
}

/// Spawn a detached command, returning the Child and output tasks for monitoring.
pub async fn spawn_detached(
    spec: CommandSpec,
    log_store: LogStoreHandle,
    log_service_name: String,
    store_stdout: bool,
    store_stderr: bool,
    output_capture: Option<OutputCaptureConfig>,
) -> Result<DetachedResult> {
    let (mut cmd, program) = build_command(&spec)?;

    let mut child = cmd.spawn().map_err(|e| {
        DaemonError::ProcessSpawn {
            service: program.clone(),
            source: e,
        }
    })?;

    let pid = child.id();
    debug!("Command spawned with PID {:?}", pid);

    let stdout_task = child.stdout.take().map(|stdout| {
        spawn_capture_task(
            Some(stdout),
            Some(log_store.clone()),
            log_service_name.clone(),
            "info",
            None,
            store_stdout,
            false,
            output_capture,
        )
    });

    let stderr_task = child.stderr.take().map(|stderr| {
        spawn_capture_task(
            Some(stderr),
            Some(log_store),
            log_service_name,
            "error",
            None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tempfile::TempDir;
    use tokio::io::{AsyncRead, ReadBuf};

    use crate::config::StorageMode;
    use crate::logs::{DEFAULT_BATCH_SIZE, SqliteLogReader};

    /// An `AsyncRead` that replays a scripted sequence of results: each `Ok`
    /// delivers a chunk of bytes, each `Err` surfaces a (transient) read error
    /// exactly once, and the end of the script is EOF. Models a flaky pipe.
    struct ScriptedReader {
        steps: VecDeque<std::io::Result<&'static [u8]>>,
    }

    impl AsyncRead for ScriptedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.steps.pop_front() {
                Some(Ok(bytes)) => {
                    buf.put_slice(bytes);
                    Poll::Ready(Ok(()))
                }
                Some(Err(e)) => Poll::Ready(Err(e)),
                None => Poll::Ready(Ok(())), // EOF
            }
        }
    }

    /// A transient read error mid-stream must NOT permanently end capture: the
    /// line emitted after the error must still be stored (the pipe fd stays
    /// valid across a read error, so the loop retries instead of giving up).
    #[tokio::test]
    async fn capture_recovers_from_transient_read_error() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("logs").join("logs.db");

        let store = LogStoreHandle::spawn(
            db_path.clone(),
            Duration::from_millis(5),
            DEFAULT_BATCH_SIZE,
            StorageMode::Local,
            None,
            None,
            Duration::from_secs(3600),
        );

        let reader = ScriptedReader {
            steps: VecDeque::from(vec![
                Ok(b"before_error\n".as_slice()),
                Err(std::io::Error::new(std::io::ErrorKind::Other, "transient pipe error")),
                Ok(b"after_error\n".as_slice()),
            ]),
        };

        let handle = spawn_capture_task(
            Some(reader),
            Some(store.clone()),
            "svc".to_string(),
            "info",
            None,
            true,  // should_store
            false, // log_to_tracing
            None,  // output_capture
        );
        let _ = handle.await;
        store.wait_flush_sync();

        let log_reader = SqliteLogReader::new(db_path, StorageMode::Local);
        let lines: Vec<String> = log_reader
            .tail(100, &["svc".to_string()], false, None, None, None)
            .into_iter()
            .map(|e| e.line)
            .collect();

        assert!(
            lines.iter().any(|l| l == "before_error"),
            "line before the error should be captured; got {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "after_error"),
            "line AFTER a transient read error must still be captured \
             (capture must retry, not end); got {lines:?}"
        );
    }

    /// A transient read error that lands MID-LINE must not split or drop the
    /// line: the partial bytes already read are preserved and the retry
    /// completes the line, so it is stored whole.
    #[tokio::test]
    async fn capture_preserves_partial_line_across_mid_line_read_error() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("logs").join("logs.db");

        let store = LogStoreHandle::spawn(
            db_path.clone(),
            Duration::from_millis(5),
            DEFAULT_BATCH_SIZE,
            StorageMode::Local,
            None,
            None,
            Duration::from_secs(3600),
        );

        // "partial_" arrives, then a transient error mid-line, then "rest\n".
        let reader = ScriptedReader {
            steps: VecDeque::from(vec![
                Ok(b"partial_".as_slice()),
                Err(std::io::Error::new(std::io::ErrorKind::Other, "transient mid-line")),
                Ok(b"rest\n".as_slice()),
            ]),
        };

        let handle = spawn_capture_task(
            Some(reader),
            Some(store.clone()),
            "svc".to_string(),
            "info",
            None,
            true,
            false,
            None,
        );
        let _ = handle.await;
        store.wait_flush_sync();

        let log_reader = SqliteLogReader::new(db_path, StorageMode::Local);
        let lines: Vec<String> = log_reader
            .tail(100, &["svc".to_string()], false, None, None, None)
            .into_iter()
            .map(|e| e.line)
            .collect();

        assert!(
            lines.iter().any(|l| l == "partial_rest"),
            "a mid-line read error must not split the line; expected 'partial_rest', got {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "rest"),
            "the line must not be split into a truncated 'rest'; got {lines:?}"
        );
    }
}
