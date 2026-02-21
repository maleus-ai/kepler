use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use crate::config::HealthCheck;
use crate::config_actor::ConfigActorHandle;
use crate::events::{HealthStatus, ServiceEvent};
use crate::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use crate::state::ServiceStatus;

/// Spawn a health check monitoring task for a service
pub fn spawn_health_checker(
    service_name: String,
    health_config: HealthCheck,
    handle: ConfigActorHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        health_check_loop(service_name, health_config, handle).await;
    })
}

async fn health_check_loop(
    service_name: String,
    config: HealthCheck,
    handle: ConfigActorHandle,
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
        if !handle.is_service_running(&service_name).await {
            debug!(
                "Health check for {} stopping - service not running",
                service_name
            );
            return;
        }

        // Get service context for env and working_dir
        let ctx = match handle.get_service_context(&service_name).await {
            Some(ctx) => ctx,
            None => {
                debug!(
                    "Health check for {} stopping - service context not available",
                    service_name
                );
                return;
            }
        };

        // Run health check with service environment
        let test_cmd = config.test.as_static().cloned().unwrap_or_default();
        let passed = run_health_check(&test_cmd, config.timeout, &ctx.env, &ctx.working_dir).await;

        // Update state based on result
        let update_result = handle
            .update_health_check(&service_name, passed, config.retries)
            .await;

        match update_result {
            Ok(update) => {
                // Emit Healthcheck event
                let health_status = if passed {
                    HealthStatus::Success
                } else {
                    HealthStatus::Failure {
                        consecutive_failures: update.failures,
                    }
                };
                handle
                    .emit_event(
                        &service_name,
                        ServiceEvent::Healthcheck {
                            status: health_status,
                        },
                    )
                    .await;

                if passed {
                    if update.previous_status == ServiceStatus::Unhealthy {
                        info!("Health check passed for {} — service recovering", service_name);
                    } else {
                        debug!("Health check passed for {}", service_name);
                    }
                } else if update.new_status == ServiceStatus::Unhealthy {
                    if update.previous_status != ServiceStatus::Unhealthy {
                        // Just transitioned to unhealthy
                        warn!(
                            "Service {} is unhealthy after {} consecutive failures",
                            service_name, update.failures
                        );
                    } else {
                        // Already unhealthy — don't spam the counter
                        debug!(
                            "Health check still failing for {} (unhealthy)",
                            service_name
                        );
                    }
                } else {
                    // Still counting toward threshold
                    warn!(
                        "Health check failed for {} ({}/{})",
                        service_name, update.failures, config.retries
                    );
                }

                // Run hooks and emit state transition events if status changed.
                // Hooks run first so that post_healthcheck_fail completes before
                // the Unhealthy event triggers an on-unhealthy restart.
                if update.previous_status != update.new_status {
                    run_status_change_hook(
                        &service_name,
                        update.previous_status,
                        update.new_status,
                        &handle,
                    )
                    .await;

                    // Emit Healthy or Unhealthy event after hook completes
                    match update.new_status {
                        ServiceStatus::Healthy => {
                            handle.emit_event(&service_name, ServiceEvent::Healthy).await;
                        }
                        ServiceStatus::Unhealthy => {
                            handle
                                .emit_event(&service_name, ServiceEvent::Unhealthy)
                                .await;
                        }
                        _ => {}
                    }
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
    service_name: &str,
    previous_status: ServiceStatus,
    new_status: ServiceStatus,
    handle: &ConfigActorHandle,
) {
    let hook_type = match new_status {
        ServiceStatus::Healthy
            if previous_status == ServiceStatus::Running
                || previous_status == ServiceStatus::Unhealthy =>
        {
            Some(ServiceHookType::PostHealthcheckSuccess)
        }
        ServiceStatus::Unhealthy
            if previous_status == ServiceStatus::Running
                || previous_status == ServiceStatus::Healthy =>
        {
            Some(ServiceHookType::PostHealthcheckFail)
        }
        _ => None,
    };

    if let Some(hook_type) = hook_type {
        // Get service context (single round-trip)
        let ctx = match handle.get_service_context(service_name).await {
            Some(ctx) => ctx,
            None => return,
        };

        // Use resolved config (available after service has started)
        let resolved = match ctx.resolved_config.as_ref() {
            Some(rc) => rc,
            None => {
                warn!("No resolved config for '{}', skipping {:?} hook", service_name, hook_type);
                return;
            }
        };

        // Create evaluator for hook step resolution
        let config = handle.get_config().await;
        let evaluator = config.as_ref()
            .and_then(|c| c.create_lua_evaluator().ok());

        // Get raw_env (sys_env from daemon/CLI) for hook context
        let raw_env = handle.get_sys_env().await;

        let hook_params = ServiceHookParams {
            working_dir: &ctx.working_dir,
            env: &ctx.env,
            raw_env: &raw_env,
            env_file_vars: &ctx.env_file_vars,
            log_config: Some(&ctx.log_config),
            service_user: resolved.user.as_deref(),
            service_groups: &resolved.groups,
            service_log_config: resolved.logs.as_ref(),
            global_log_config: ctx.global_log_config.as_ref(),
            deps: HashMap::new(),
            all_hook_outputs: HashMap::new(),
            output_max_size: crate::config::DEFAULT_OUTPUT_MAX_SIZE,
            evaluator: evaluator.as_ref(),
            config_path: Some(handle.config_path()),
            config_dir: Some(&ctx.config_dir),
        };

        // Hooks are always available from resolved config (inner fields may still
        // be ConfigValue::Dynamic, resolved per-step in run_hook_step).
        let hooks = resolved.hooks.clone();

        if let Err(e) = run_service_hook(
            &hooks,
            hook_type,
            service_name,
            &hook_params,
            &None,
            Some(handle),
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

/// Execute a single health check command with the service's environment
async fn run_health_check(
    test: &[String],
    check_timeout: Duration,
    env: &HashMap<String, String>,
    working_dir: &Path,
) -> bool {
    if test.is_empty() {
        return true;
    }

    // Execute command directly - first element is program, rest are args
    let program = &test[0];
    let args = &test[1..];

    let result = timeout(check_timeout, async {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .envs(env)
            .current_dir(working_dir)
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
