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
use std::path::PathBuf;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::config_actor::{ConfigActorHandle, TaskHandleType};
use crate::containment::ContainmentManager;
use crate::errors::{DaemonError, Result};
use crate::logs::LogWriterConfig;
use crate::state::{ProcessHandle, ServiceStatus, ShutdownRequest};

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
    pub spec: CommandSpec,
    pub log_config: LogWriterConfig,
    pub handle: ConfigActorHandle,
    pub exit_tx: mpsc::Sender<ProcessExitEvent>,
    pub store_stdout: bool,
    pub store_stderr: bool,
    /// Optional output capture for `::output::KEY=VALUE` markers
    pub output_capture: Option<OutputCaptureConfig>,
    pub containment: ContainmentManager,
    pub config_hash: String,
}

/// Spawn a service process with signal-based monitoring
pub async fn spawn_service(params: SpawnServiceParams<'_>) -> Result<ProcessHandle> {
    let SpawnServiceParams {
        service_name,
        spec,
        log_config,
        handle,
        exit_tx,
        store_stdout,
        store_stderr,
        output_capture,
        containment,
        config_hash,
    } = params;

    // Validate command
    if spec.program_and_args.is_empty() {
        return Err(DaemonError::Config(format!(
            "Service {} has empty command",
            service_name
        )));
    }

    info!(
        "Starting service {}: {} {:?}",
        service_name,
        &spec.program_and_args[0],
        &spec.program_and_args[1..]
    );

    // Prepare cgroup before spawning (creates cgroup directory if using cgroup v2)
    containment.prepare_spawn(&config_hash, service_name);

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

    // Register PID in cgroup (if using cgroup v2)
    if let Some(pid) = pid {
        containment.register_pid(&config_hash, service_name, pid);
    }

    // Store the PID in state immediately after spawning
    let _ = handle
        .set_service_pid(service_name, pid, Some(Utc::now()))
        .await;

    // Create shutdown channel for graceful stop (round-trip: request → reply)
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<ShutdownRequest>();

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
            containment,
            config_hash,
        )
        .await;
    });

    // Create ProcessHandle with output tasks and shutdown channel
    let process_handle = ProcessHandle {
        shutdown_tx: Some(shutdown_tx),
        stdout_task: result.stdout_task,
        stderr_task: result.stderr_task,
    };

    if let Some(fd_count) = crate::fd_count::count_open_fds() {
        debug!("FD count after spawning service {}: {}", service_name, fd_count);
    }

    Ok(process_handle)
}

/// Monitor a process using signal-based waiting (child.wait())
/// This replaces the previous polling-based approach
#[allow(clippy::too_many_arguments)]
async fn monitor_process(
    config_path: PathBuf,
    service_name: String,
    mut child: Child,
    shutdown_rx: oneshot::Receiver<ShutdownRequest>,
    handle: ConfigActorHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    containment: ContainmentManager,
    config_hash: String,
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
        // Wait for shutdown request (round-trip: request → reply)
        request_result = shutdown_rx => {
            let (signal_num, grace_period, reply_tx) = match request_result {
                Ok(req) => (req.signal, req.grace_period, Some(req.reply)),
                Err(_) => (15, std::time::Duration::ZERO, None), // Sender dropped — default SIGTERM, no grace, no reply
            };
            debug!("Shutdown signal received for {} (signal {}, grace_period {:?})", service_name, signal_num, grace_period);

            let (exit_code, signal) = if grace_period.is_zero() {
                // No grace period — force kill immediately
                debug!("Grace period is 0 for {}, force killing immediately", service_name);
                if let Some(pid) = child.id() {
                    containment.force_kill_service(&config_hash, &service_name, pid).await;
                }
                let _ = child.kill().await; // reap zombie
                (None, Some(9)) // killed by SIGKILL
            } else {
                // Send graceful signal and wait for grace period
                if let Some(pid) = child.id() {
                    debug!("Sending signal {} to process group {}", signal_num, pid);
                    containment.signal_service(pid, signal_num);
                } else {
                    let _ = child.start_kill();
                }

                // Wait for process to exit within grace period
                let timeout_result = tokio::time::timeout(
                    grace_period,
                    child.wait(),
                )
                .await;

                match timeout_result {
                    Ok(Ok(status)) => {
                        debug!("Service {} stopped with status {:?}", service_name, status);
                        let code = status.code();
                        #[cfg(unix)]
                        let sig = if code.is_none() {
                            use std::os::unix::process::ExitStatusExt;
                            status.signal()
                        } else {
                            None
                        };
                        #[cfg(not(unix))]
                        let sig = None;
                        (code, sig)
                    }
                    Ok(Err(e)) => {
                        warn!("Error waiting for service {}: {}", service_name, e);
                        (None, None)
                    }
                    Err(_) => {
                        // Timeout — force kill via cgroup or killpg SIGKILL
                        warn!("Service {} did not stop within grace period ({:?}), force killing", service_name, grace_period);
                        if let Some(pid) = child.id() {
                            containment.force_kill_service(&config_hash, &service_name, pid).await;
                        }
                        let _ = child.kill().await; // reap zombie
                        (None, Some(9)) // killed by SIGKILL
                    }
                }
            };

            // Record exit info so it's available even on explicit stop
            let _ = handle.record_process_exit(&service_name, exit_code, signal).await;

            // Signal stop_service that the process is dead
            if let Some(tx) = reply_tx {
                let _ = tx.send(());
            }
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
    skip_status_update: bool,
    grace_period: std::time::Duration,
) -> Result<()> {
    info!("Stopping service {}", service_name);

    // Update status to stopping (skipped during restart to preserve Restarting status)
    if !skip_status_update {
        let _ = handle
            .set_service_status(service_name, ServiceStatus::Stopping)
            .await;
    }

    // Get the process handle
    let process_handle = handle.remove_process_handle(service_name).await;

    if let Some(process_handle) = process_handle {
        // Send shutdown request to monitor task and wait for it to confirm the process is dead.
        // This ensures the process has exited before we drain output tasks.
        if let Some(shutdown_tx) = process_handle.shutdown_tx {
            let (reply_tx, reply_rx) = oneshot::channel();
            let request = ShutdownRequest {
                signal: signal.unwrap_or(15),
                grace_period,
                reply: reply_tx,
            };
            let _ = shutdown_tx.send(request);
            // Wait for monitor_process to confirm process death (grace_period + 15s safety margin)
            let safety_timeout = grace_period + std::time::Duration::from_secs(15);
            let _ = tokio::time::timeout(
                safety_timeout,
                reply_rx,
            ).await;
        }

        // Drain output tasks — process is already dead so pipes are closed.
        // Use a short timeout (2s) since data should flush quickly.
        let drain_timeout = std::time::Duration::from_secs(2);
        let stdout_fut = async {
            if let Some(mut task) = process_handle.stdout_task {
                tokio::select! {
                    _ = &mut task => {}
                    _ = tokio::time::sleep(drain_timeout) => {
                        warn!("stdout capture task for {} did not finish in time, aborting", service_name);
                        task.abort();
                    }
                }
            }
        };
        let stderr_fut = async {
            if let Some(mut task) = process_handle.stderr_task {
                tokio::select! {
                    _ = &mut task => {}
                    _ = tokio::time::sleep(drain_timeout) => {
                        warn!("stderr capture task for {} did not finish in time, aborting", service_name);
                        task.abort();
                    }
                }
            }
        };
        tokio::join!(stdout_fut, stderr_fut);
    }

    // Cancel health check
    handle
        .cancel_task_handle(service_name, TaskHandleType::HealthCheck)
        .await;

    // Cancel watcher
    handle
        .cancel_task_handle(service_name, TaskHandleType::FileWatcher)
        .await;

    // Update status to stopped (skipped during restart to preserve Restarting status)
    if !skip_status_update {
        let _ = handle
            .set_service_status(service_name, ServiceStatus::Stopped)
            .await;
    }

    let _ = handle.set_service_pid(service_name, None, None).await;

    if let Some(fd_count) = crate::fd_count::count_open_fds() {
        debug!("FD count after stopping service {}: {}", service_name, fd_count);
    }

    Ok(())
}

#[cfg(test)]
mod tests;
