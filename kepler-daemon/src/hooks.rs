use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info};

use crate::config::{resolve_log_store, GlobalHooks, HookCommand, LogConfig, ServiceHooks};
use crate::env::build_hook_env;
use crate::errors::{DaemonError, Result};
use crate::logs::SharedLogBuffer;
use crate::process::{spawn_command_sync, CommandSpec, SpawnMode};

/// Context for running a hook
pub struct HookRunContext<'a> {
    pub base_working_dir: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub logs: Option<&'a SharedLogBuffer>,
    pub service_name: &'a str,
    pub service_user: Option<&'a str>,
    pub service_group: Option<&'a str>,
    pub store_stdout: bool,
    pub store_stderr: bool,
}

/// Parameters for running a service hook
pub struct ServiceHookParams<'a> {
    pub working_dir: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub logs: Option<&'a SharedLogBuffer>,
    pub service_user: Option<&'a str>,
    pub service_group: Option<&'a str>,
    pub service_log_config: Option<&'a LogConfig>,
    pub global_log_config: Option<&'a LogConfig>,
}

/// Types of global hooks
#[derive(Debug, Clone, Copy)]
pub enum GlobalHookType {
    OnInit,
    OnStart,
    OnStop,
    OnCleanup,
}

impl GlobalHookType {
    pub fn as_str(&self) -> &'static str {
        match self {
            GlobalHookType::OnInit => "on_init",
            GlobalHookType::OnStart => "on_start",
            GlobalHookType::OnStop => "on_stop",
            GlobalHookType::OnCleanup => "on_cleanup",
        }
    }
}

/// Types of service hooks
#[derive(Debug, Clone, Copy)]
pub enum ServiceHookType {
    OnInit,
    OnStart,
    OnStop,
    OnRestart,
    OnExit,
    OnHealthcheckSuccess,
    OnHealthcheckFail,
}

impl ServiceHookType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceHookType::OnInit => "on_init",
            ServiceHookType::OnStart => "on_start",
            ServiceHookType::OnStop => "on_stop",
            ServiceHookType::OnRestart => "on_restart",
            ServiceHookType::OnExit => "on_exit",
            ServiceHookType::OnHealthcheckSuccess => "on_healthcheck_success",
            ServiceHookType::OnHealthcheckFail => "on_healthcheck_fail",
        }
    }
}

/// Execute a hook command
pub async fn run_hook(hook: &HookCommand, ctx: &HookRunContext<'_>) -> Result<()> {
    // Convert hook to program and args
    let program_and_args = match hook {
        HookCommand::Script { run, .. } => {
            // Run script through shell
            vec!["sh".to_string(), "-c".to_string(), run.clone()]
        }
        HookCommand::Command { command, .. } => {
            if command.is_empty() {
                return Ok(());
            }
            command.clone()
        }
    };

    // Determine effective working directory
    // Priority: hook working_dir > base_working_dir (from service)
    let effective_working_dir = match hook.working_dir() {
        Some(dir) => {
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                ctx.base_working_dir.join(dir)
            }
        }
        None => ctx.base_working_dir.to_path_buf(),
    };

    // Build hook-specific environment (merges with base env)
    let hook_env = build_hook_env(hook, ctx.env, &effective_working_dir)?;

    // Determine effective user
    // Priority: hook user > service user > daemon user
    let effective_user = match hook.user() {
        Some("daemon") => None, // Run as daemon user (no change)
        Some(user) => Some(user.to_string()), // Hook specifies explicit user
        None => ctx.service_user.map(|s| s.to_string()), // Inherit from service
    };

    // Determine effective group
    // Priority: hook group > service group
    let effective_group = hook
        .group()
        .map(|s| s.to_string())
        .or_else(|| ctx.service_group.map(|s| s.to_string()));

    debug!(
        "Running hook: {} {:?}",
        &program_and_args[0],
        &program_and_args[1..]
    );

    // Build CommandSpec
    let spec = CommandSpec::new(
        program_and_args.clone(),
        effective_working_dir,
        hook_env,
        effective_user,
        effective_group,
    );

    // Spawn using unified function with logging
    let mode = SpawnMode::SynchronousWithLogging {
        logs: ctx.logs.cloned(),
        log_service_name: ctx.service_name.to_string(),
        store_stdout: ctx.store_stdout,
        store_stderr: ctx.store_stderr,
    };

    let result = spawn_command_sync(spec, mode).await?;
    if result.exit_code != Some(0) {
        return Err(DaemonError::HookFailed {
            hook_type: format!("{} {:?}", &program_and_args[0], &program_and_args[1..]),
            message: format!("Exit code: {:?}", result.exit_code),
        });
    }
    Ok(())
}

/// The prefix used for global hook logs
pub const GLOBAL_HOOK_PREFIX: &str = "[global]";

/// Format the service name for global hook logs (e.g., "[global.on_start]")
pub fn global_hook_service_name(hook_type: GlobalHookType) -> String {
    format!("[global.{}]", hook_type.as_str())
}

/// Run a global hook if it exists
///
/// Note: Global hooks always run as the daemon user (no service user to inherit).
pub async fn run_global_hook(
    hooks: &Option<GlobalHooks>,
    hook_type: GlobalHookType,
    working_dir: &Path,
    env: &HashMap<String, String>,
    logs: Option<&SharedLogBuffer>,
    global_log_config: Option<&LogConfig>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    let hook = match hook_type {
        GlobalHookType::OnInit => &hooks.on_init,
        GlobalHookType::OnStart => &hooks.on_start,
        GlobalHookType::OnStop => &hooks.on_stop,
        GlobalHookType::OnCleanup => &hooks.on_cleanup,
    };

    if let Some(hook) = hook {
        info!("Running global {} hook", hook_type.as_str());
        let service_name = global_hook_service_name(hook_type);
        // Resolve store settings from global config
        let (store_stdout, store_stderr) = resolve_log_store(None, global_log_config);
        // Global hooks run as daemon user (no service user to inherit)
        let ctx = HookRunContext {
            base_working_dir: working_dir,
            env,
            logs,
            service_name: &service_name,
            service_user: None,
            service_group: None,
            store_stdout,
            store_stderr,
        };
        run_hook(hook, &ctx).await?;
    }

    Ok(())
}

/// Format the log name for service hooks (e.g., "[backend.on_start]")
pub fn service_hook_log_name(service_name: &str, hook_type: ServiceHookType) -> String {
    format!("[{}.{}]", service_name, hook_type.as_str())
}

/// Run a service hook if it exists
pub async fn run_service_hook(
    hooks: &Option<ServiceHooks>,
    hook_type: ServiceHookType,
    service_name: &str,
    params: &ServiceHookParams<'_>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    let hook = match hook_type {
        ServiceHookType::OnInit => &hooks.on_init,
        ServiceHookType::OnStart => &hooks.on_start,
        ServiceHookType::OnStop => &hooks.on_stop,
        ServiceHookType::OnRestart => &hooks.on_restart,
        ServiceHookType::OnExit => &hooks.on_exit,
        ServiceHookType::OnHealthcheckSuccess => &hooks.on_healthcheck_success,
        ServiceHookType::OnHealthcheckFail => &hooks.on_healthcheck_fail,
    };

    if let Some(hook) = hook {
        info!(
            "Running {} hook for service {}",
            hook_type.as_str(),
            service_name
        );
        let log_name = service_hook_log_name(service_name, hook_type);
        // Resolve store settings: service config > global config > default
        let (store_stdout, store_stderr) = resolve_log_store(params.service_log_config, params.global_log_config);
        let ctx = HookRunContext {
            base_working_dir: params.working_dir,
            env: params.env,
            logs: params.logs,
            service_name: &log_name,
            service_user: params.service_user,
            service_group: params.service_group,
            store_stdout,
            store_stderr,
        };
        match run_hook(hook, &ctx).await {
            Ok(()) => {}
            Err(e) => {
                error!(
                    "Hook {} failed for service {}: {}",
                    hook_type.as_str(),
                    service_name,
                    e
                );
                return Err(e);
            }
        }
    }

    Ok(())
}
