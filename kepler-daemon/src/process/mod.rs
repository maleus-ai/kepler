//! Process management for Kepler daemon
//!
//! This module provides:
//! - `CommandSpec` - Unified command specification for services and hooks
//! - `spawn_blocking` - Spawn a process and wait for completion
//! - `spawn_detached` - Spawn a process for background monitoring
//! - `spawn_service` - Spawn a managed service with full lifecycle
//! - `stop_service` - Stop a running service
//! - `validate_running_process` - Validate process existence for reconnection

mod command;
mod spawn;
mod validation;

pub use command::CommandSpec;
pub use spawn::{spawn_blocking, spawn_detached, BlockingMode, BlockingResult, DetachedResult, OutputCaptureConfig};
pub use validation::{kill_process_by_pid, validate_running_process};

use chrono::Utc;
use std::path::{Path, PathBuf};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config::{resolve_log_store, resolve_sys_env, LogConfig, ServiceConfig, SysEnvPolicy};
use crate::config_actor::{ConfigActorHandle, TaskHandleType};
use crate::errors::{DaemonError, Result};
use crate::logs::LogWriterConfig;
use crate::state::{ProcessHandle, ServiceStatus};

/// Message for process exit events
#[derive(Debug)]
pub struct ProcessExitEvent {
    pub config_path: PathBuf,
    pub service_name: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

/// Parameters for spawning a service
pub struct SpawnServiceParams<'a> {
    pub service_name: &'a str,
    pub service_config: &'a ServiceConfig,
    pub config_dir: &'a Path,
    pub log_config: LogWriterConfig,
    pub handle: ConfigActorHandle,
    pub exit_tx: mpsc::Sender<ProcessExitEvent>,
    pub global_log_config: Option<&'a LogConfig>,
    /// Optional output capture for `::output::KEY=VALUE` markers
    pub output_capture: Option<OutputCaptureConfig>,
}

/// Spawn a service process with signal-based monitoring
pub async fn spawn_service(params: SpawnServiceParams<'_>) -> Result<ProcessHandle> {
    let SpawnServiceParams {
        service_name,
        service_config,
        config_dir,
        log_config,
        handle,
        exit_tx,
        global_log_config,
        output_capture,
    } = params;

    let working_dir = service_config
        .working_dir
        .as_ref()
        .map(|wd| config_dir.join(wd))
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
    let resolved_sys_env = resolve_sys_env(service_config.sys_env.as_ref(), global_sys_env.as_ref());
    let clear_env = resolved_sys_env == SysEnvPolicy::Clear;

    let spec = CommandSpec::with_all_options(
        service_config.command.clone(),
        working_dir,
        env,
        service_config.user.clone(),
        service_config.groups.clone(),
        service_config.limits.clone(),
        clear_env,
    );

    // Resolve store settings
    let (store_stdout, store_stderr) =
        resolve_log_store(service_config.logs.as_ref(), global_log_config);

    // Spawn the command detached for monitoring
    let result = spawn_detached(
        spec,
        log_config,
        service_name.to_string(),
        store_stdout,
        store_stderr,
        output_capture,
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

    // Create shutdown channel for graceful stop (carries signal number)
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<i32>();

    // Spawn process monitor with the Child (signal-based monitoring)
    let config_path = handle.config_path().to_path_buf();
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
    shutdown_rx: oneshot::Receiver<i32>,
    _handle: ConfigActorHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
) {
    // Use tokio::select! to wait for either process exit or shutdown signal
    tokio::select! {
        // Wait for process to exit naturally
        result = child.wait() => {
            let status = result.ok();
            let exit_code = status.as_ref().and_then(|s| s.code());
            #[cfg(unix)]
            let signal = if exit_code.is_none() {
                status.as_ref().and_then(|s| {
                    use std::os::unix::process::ExitStatusExt;
                    s.signal()
                })
            } else {
                None
            };
            #[cfg(not(unix))]
            let signal = None;
            info!(
                "Service {} exited with code {:?} signal {:?}",
                service_name, exit_code, signal
            );

            // Send exit event with overflow handling
            let event = ProcessExitEvent {
                config_path,
                service_name: service_name.clone(),
                exit_code,
                signal,
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
        // Wait for shutdown signal (carries signal number)
        signal_result = shutdown_rx => {
            let signal_num = signal_result.unwrap_or(15); // Default SIGTERM
            debug!("Shutdown signal received for {} (signal {})", service_name, signal_num);

            #[cfg(unix)]
            {
                if let Some(pid) = child.id() {
                    debug!("Sending signal {} to process group {}", signal_num, pid);
                    use nix::sys::signal::{killpg, Signal};
                    use nix::unistd::Pid;
                    if let Ok(sig) = Signal::try_from(signal_num) {
                        let _ = killpg(Pid::from_raw(pid as i32), sig);
                    } else {
                        warn!("Invalid signal number {}, falling back to SIGTERM", signal_num);
                        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
                    }
                }
            }

            #[cfg(not(unix))]
            {
                let _ = signal_num; // suppress unused warning
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
                    // Timeout - force kill the entire process group
                    warn!("Service {} did not stop gracefully, force killing", service_name);
                    #[cfg(unix)]
                    if let Some(pid) = child.id() {
                        use nix::sys::signal::{killpg, Signal};
                        use nix::unistd::Pid;
                        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
                    }
                    let _ = child.kill().await; // reap zombie
                }
            }

            // Note: We don't send an exit event here because stop_service handles the state update
        }
    }
}

/// Parse a signal name or number string into a signal number
pub fn parse_signal_name(name: &str) -> Option<i32> {
    match name.to_uppercase().trim_start_matches("SIG") {
        "TERM" => Some(15),
        "KILL" => Some(9),
        "INT" => Some(2),
        "HUP" => Some(1),
        "QUIT" => Some(3),
        "USR1" => Some(10),
        "USR2" => Some(12),
        _ => name.parse::<i32>().ok(),
    }
}

/// Stop a service process
pub async fn stop_service(
    service_name: &str,
    handle: ConfigActorHandle,
    signal: Option<i32>,
) -> Result<()> {
    info!("Stopping service {}", service_name);

    // Update status to stopping
    let _ = handle
        .set_service_status(service_name, ServiceStatus::Stopping)
        .await;

    // Get the process handle
    let process_handle = handle.remove_process_handle(service_name).await;

    if let Some(process_handle) = process_handle {
        // Send shutdown signal to monitor task (with signal number)
        if let Some(shutdown_tx) = process_handle.shutdown_tx {
            let _ = shutdown_tx.send(signal.unwrap_or(15));
        }

        // Wait for output tasks to finish flushing remaining pipe data to log files,
        // then abort if they don't complete in time.
        if let Some(mut task) = process_handle.stdout_task {
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                    warn!("stdout capture task for {} did not finish in time, aborting", service_name);
                    task.abort();
                }
            }
        }
        if let Some(mut task) = process_handle.stderr_task {
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                    warn!("stderr capture task for {} did not finish in time, aborting", service_name);
                    task.abort();
                }
            }
        }
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

#[cfg(test)]
mod tests;
