use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{LogRetention, RestartPolicy, ServiceConfig};
use crate::env::build_service_env;
use crate::errors::{DaemonError, Result};
use crate::hooks::{run_service_hook, ServiceHookType};
use crate::logs::{LogStream, SharedLogBuffer};
use crate::state::{ProcessHandle, ServiceStatus, SharedDaemonState};

/// Message for process exit events
#[derive(Debug)]
pub struct ProcessExitEvent {
    pub config_path: PathBuf,
    pub service_name: String,
    pub exit_code: Option<i32>,
}

/// Spawn a service process
pub async fn spawn_service(
    config_path: &Path,
    service_name: &str,
    service_config: &ServiceConfig,
    config_dir: &Path,
    logs: SharedLogBuffer,
    state: SharedDaemonState,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
) -> Result<ProcessHandle> {
    let working_dir = service_config
        .working_dir
        .clone()
        .unwrap_or_else(|| config_dir.to_path_buf());

    // Build environment
    let env = build_service_env(service_config, config_dir)?;

    // Prepare command
    if service_config.command.is_empty() {
        return Err(DaemonError::Config(format!(
            "Service {} has empty command",
            service_name
        )));
    }

    let program = &service_config.command[0];
    let args = &service_config.command[1..];

    info!("Starting service {}: {} {:?}", service_name, program, args);

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(&env);

    let mut child = cmd.spawn().map_err(|e| DaemonError::ProcessSpawn {
        service: service_name.to_string(),
        source: e,
    })?;

    let pid = child.id();
    debug!("Service {} spawned with PID {:?}", service_name, pid);

    // Capture stdout
    let stdout = child.stdout.take();
    let stdout_task = if let Some(stdout) = stdout {
        let logs_clone = logs.clone();
        let service_name_clone = service_name.to_string();
        Some(tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                logs_clone.push(service_name_clone.clone(), line, LogStream::Stdout);
            }
        }))
    } else {
        None
    };

    // Capture stderr
    let stderr = child.stderr.take();
    let stderr_task = if let Some(stderr) = stderr {
        let logs_clone = logs.clone();
        let service_name_clone = service_name.to_string();
        Some(tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                logs_clone.push(service_name_clone.clone(), line, LogStream::Stderr);
            }
        }))
    } else {
        None
    };

    // Spawn process monitor
    let config_path = config_path.to_path_buf();
    let service_name_clone = service_name.to_string();
    let state_clone = state.clone();

    tokio::spawn(async move {
        monitor_process(config_path, service_name_clone, state_clone, exit_tx).await;
    });

    Ok(ProcessHandle {
        child,
        stdout_task,
        stderr_task,
    })
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
    let (restart_policy, service_config, config_dir, logs, hooks, global_log_config) = {
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
            service_config.restart.clone(),
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
    let _ = run_service_hook(
        &hooks,
        ServiceHookType::OnExit,
        &service_name,
        &working_dir,
        &env,
        Some(&logs),
    )
    .await;

    // Clear logs based on on_exit retention policy
    {
        let service_retention = service_config.logs.as_ref().map(|l| &l.on_exit);
        let global_retention = global_log_config.as_ref().map(|l| &l.on_exit);

        let should_clear = match service_retention.or(global_retention) {
            Some(LogRetention::Retain) => false,
            _ => true, // Default: clear
        };

        if should_clear {
            logs.clear_service(&service_name);
            logs.clear_service_prefix(&format!("[{}.", service_name));
        }
    }

    // Determine if we should restart
    let should_restart = match restart_policy {
        RestartPolicy::No => false,
        RestartPolicy::Always => true,
        RestartPolicy::OnFailure => exit_code != Some(0),
    };

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

        info!("Restarting service {} (policy: {:?})", service_name, restart_policy);

        // Run on_restart hook
        let _ = run_service_hook(
            &hooks,
            ServiceHookType::OnRestart,
            &service_name,
            &working_dir,
            &env,
            Some(&logs),
        )
        .await;

        // Clear logs based on on_restart retention policy
        let service_retention = service_config.logs.as_ref().map(|l| &l.on_restart);
        let global_retention = global_log_config.as_ref().map(|l| &l.on_restart);

        let should_clear = match service_retention.or(global_retention) {
            Some(LogRetention::Retain) => false,
            _ => true, // Default: clear
        };

        if should_clear {
            logs.clear_service(&service_name);
            logs.clear_service_prefix(&format!("[{}.", service_name));
        }

        // Spawn new process
        match spawn_service(
            &config_path,
            &service_name,
            &service_config,
            &config_dir,
            logs,
            state.clone(),
            exit_tx,
        )
        .await
        {
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
