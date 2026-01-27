use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use crate::config::HealthCheck;
use crate::env::build_service_env;
use crate::hooks::{run_service_hook, ServiceHookType};
use crate::state::{ServiceStatus, SharedDaemonState};

/// Spawn a health check monitoring task for a service
pub fn spawn_health_checker(
    config_path: PathBuf,
    service_name: String,
    health_config: HealthCheck,
    state: SharedDaemonState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        health_check_loop(config_path, service_name, health_config, state).await;
    })
}

async fn health_check_loop(
    config_path: PathBuf,
    service_name: String,
    config: HealthCheck,
    state: SharedDaemonState,
) {
    // Wait for start_period before beginning checks
    if !config.start_period.is_zero() {
        debug!(
            "Health check for {} waiting for start period: {:?}",
            service_name, config.start_period
        );
        sleep(config.start_period).await;
    }

    let mut consecutive_failures: u32 = 0;

    loop {
        // Check if service is still running
        {
            let state = state.read();
            if let Some(config_state) = state.configs.get(&config_path) {
                if let Some(service_state) = config_state.services.get(&service_name) {
                    if !service_state.status.is_running() {
                        debug!(
                            "Health check for {} stopping - service not running",
                            service_name
                        );
                        return;
                    }
                } else {
                    return;
                }
            } else {
                return;
            }
        }

        // Run health check
        let passed = run_health_check(&config.test, config.timeout).await;

        // Update state based on result and detect transitions for hooks
        let hook_info = {
            let mut state = state.write();
            if let Some(config_state) = state.configs.get_mut(&config_path) {
                if let Some(service_state) = config_state.services.get_mut(&service_name) {
                    let previous_status = service_state.status;

                    if passed {
                        consecutive_failures = 0;
                        service_state.health_check_failures = 0;
                        if service_state.status == ServiceStatus::Running
                            || service_state.status == ServiceStatus::Unhealthy
                        {
                            service_state.status = ServiceStatus::Healthy;
                            debug!("Health check passed for {}", service_name);
                        }
                    } else {
                        consecutive_failures += 1;
                        service_state.health_check_failures = consecutive_failures;
                        warn!(
                            "Health check failed for {} ({}/{})",
                            service_name, consecutive_failures, config.retries
                        );

                        if consecutive_failures >= config.retries {
                            if service_state.status != ServiceStatus::Unhealthy {
                                info!(
                                    "Service {} marked as unhealthy after {} failures",
                                    service_name, consecutive_failures
                                );
                                service_state.status = ServiceStatus::Unhealthy;
                            }
                        }
                    }

                    let new_status = service_state.status;

                    // Detect transition and gather hook info
                    if previous_status != new_status {
                        if let Some(service_config) = config_state.config.services.get(&service_name)
                        {
                            let config_dir = config_path
                                .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| PathBuf::from("."));
                            let working_dir = service_config
                                .working_dir
                                .clone()
                                .unwrap_or_else(|| config_dir.clone());

                            Some((
                                previous_status,
                                new_status,
                                service_config.hooks.clone(),
                                working_dir,
                                config_state.logs.clone(),
                                config_dir,
                                service_config.clone(),
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Call hooks outside the lock
        if let Some((previous_status, new_status, hooks, working_dir, logs, config_dir, service_config)) =
            hook_info
        {
            let hook_type = match new_status {
                ServiceStatus::Healthy
                    if previous_status == ServiceStatus::Running
                        || previous_status == ServiceStatus::Unhealthy =>
                {
                    Some(ServiceHookType::OnHealthcheckSuccess)
                }
                ServiceStatus::Unhealthy
                    if previous_status == ServiceStatus::Running
                        || previous_status == ServiceStatus::Healthy =>
                {
                    Some(ServiceHookType::OnHealthcheckFail)
                }
                _ => None,
            };

            if let Some(hook_type) = hook_type {
                // Build environment for hook
                let env = match build_service_env(&service_config, &config_dir) {
                    Ok(e) => e,
                    Err(e) => {
                        error!(
                            "Failed to build environment for {} hook: {}",
                            hook_type.as_str(),
                            e
                        );
                        std::collections::HashMap::new()
                    }
                };

                if let Err(e) = run_service_hook(
                    &hooks,
                    hook_type,
                    &service_name,
                    &working_dir,
                    &env,
                    Some(&logs),
                )
                .await
                {
                    error!(
                        "Failed to run {} hook for {}: {}",
                        hook_type.as_str(),
                        service_name,
                        e
                    );
                }
            }
        }

        // Wait for next interval
        sleep(config.interval).await;
    }
}

/// Execute a single health check command
async fn run_health_check(test: &[String], check_timeout: Duration) -> bool {
    if test.is_empty() {
        return true;
    }

    // Parse the test command
    // Format: ["CMD-SHELL", "command"] or ["CMD", "cmd", "arg1", "arg2"]
    let (program, args) = if test[0] == "CMD-SHELL" {
        if test.len() < 2 {
            return true;
        }
        ("sh".to_string(), vec!["-c".to_string(), test[1].clone()])
    } else if test[0] == "CMD" {
        if test.len() < 2 {
            return true;
        }
        (test[1].clone(), test[2..].to_vec())
    } else {
        // Assume direct command
        (test[0].clone(), test[1..].to_vec())
    };

    let result = timeout(check_timeout, async {
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match cmd.status().await {
            Ok(status) => status.success(),
            Err(e) => {
                debug!("Health check command failed to execute: {}", e);
                false
            }
        }
    })
    .await;

    match result {
        Ok(passed) => passed,
        Err(_) => {
            debug!("Health check timed out");
            false
        }
    }
}
