use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use crate::config::HealthCheck;
use crate::env::build_service_env;
use crate::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use crate::state::ServiceStatus;
use crate::state_actor::StateHandle;

/// Spawn a health check monitoring task for a service
pub fn spawn_health_checker(
    config_path: PathBuf,
    service_name: String,
    health_config: HealthCheck,
    state: StateHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        health_check_loop(config_path, service_name, health_config, state).await;
    })
}

async fn health_check_loop(
    config_path: PathBuf,
    service_name: String,
    config: HealthCheck,
    state: StateHandle,
) {
    // Wait for start_period before beginning checks
    if !config.start_period.is_zero() {
        debug!(
            "Health check for {} waiting for start period: {:?}",
            service_name, config.start_period
        );
        sleep(config.start_period).await;
    }

    loop {
        // Check if service is still running
        if !state
            .is_service_running(config_path.clone(), service_name.clone())
            .await
        {
            debug!(
                "Health check for {} stopping - service not running",
                service_name
            );
            return;
        }

        // Run health check
        let passed = run_health_check(&config.test, config.timeout).await;

        // Update state based on result
        let update_result = state
            .update_health_check(
                config_path.clone(),
                service_name.clone(),
                passed,
                config.retries,
            )
            .await;

        match update_result {
            Ok(update) => {
                if passed {
                    debug!("Health check passed for {}", service_name);
                } else {
                    warn!(
                        "Health check failed for {} ({}/{})",
                        service_name, update.failures, config.retries
                    );

                    if update.failures >= config.retries
                        && update.previous_status != ServiceStatus::Unhealthy
                    {
                        info!(
                            "Service {} marked as unhealthy after {} failures",
                            service_name, update.failures
                        );
                    }
                }

                // Run hooks if status changed
                if update.previous_status != update.new_status {
                    run_status_change_hook(
                        &config_path,
                        &service_name,
                        update.previous_status,
                        update.new_status,
                        &state,
                    )
                    .await;
                }
            }
            Err(e) => {
                error!(
                    "Failed to update health check for {}: {}",
                    service_name, e
                );
            }
        }

        // Wait for next interval
        sleep(config.interval).await;
    }
}

/// Run hook when health status changes
async fn run_status_change_hook(
    config_path: &std::path::Path,
    service_name: &str,
    previous_status: ServiceStatus,
    new_status: ServiceStatus,
    state: &StateHandle,
) {
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
        // Get service context (bundles service_config, config_dir, logs, global_log_config)
        let ctx = match state
            .get_service_context(config_path.to_path_buf(), service_name.to_string())
            .await
        {
            Some(ctx) => ctx,
            None => return,
        };

        let working_dir = ctx
            .service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| ctx.config_dir.clone());

        // Build environment for hook
        let env = match build_service_env(&ctx.service_config, &ctx.config_dir) {
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

        let hook_params = ServiceHookParams::from_service_context(
            &ctx.service_config,
            &working_dir,
            &env,
            Some(&ctx.logs),
            ctx.global_log_config.as_ref(),
        );

        if let Err(e) = run_service_hook(
            &ctx.service_config.hooks,
            hook_type,
            service_name,
            &hook_params,
        )
        .await
        {
            warn!(
                "Hook {} failed for {}: {}",
                hook_type.as_str(),
                service_name,
                e
            );
        }
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
