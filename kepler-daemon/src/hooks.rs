use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info};

use crate::config::{resolve_log_store, GlobalHooks, HookCommand, LogConfig, ServiceHooks};
use crate::env::build_hook_env;
use crate::errors::{DaemonError, Result};
use crate::logs::LogWriterConfig;
use crate::process::{spawn_blocking, BlockingMode, CommandSpec};
use kepler_protocol::protocol::{ProgressEvent, ServicePhase};
use kepler_protocol::server::ProgressSender;

/// Emit a hook progress event if progress sender is available.
async fn emit_hook_event(
    progress: &Option<ProgressSender>,
    service: &str,
    phase: ServicePhase,
) {
    if let Some(p) = progress {
        p.send(ProgressEvent {
            service: service.to_string(),
            phase,
        }).await;
    }
}

/// Context for running a hook
pub struct HookRunContext<'a> {
    pub base_working_dir: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub log_config: Option<&'a LogWriterConfig>,
    pub service_name: &'a str,
    pub service_user: Option<&'a str>,
    pub service_groups: &'a [String],
    pub store_stdout: bool,
    pub store_stderr: bool,
}

/// Parameters for running a service hook
pub struct ServiceHookParams<'a> {
    pub working_dir: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub log_config: Option<&'a LogWriterConfig>,
    pub service_user: Option<&'a str>,
    pub service_groups: &'a [String],
    pub service_log_config: Option<&'a LogConfig>,
    pub global_log_config: Option<&'a LogConfig>,
}

impl<'a> ServiceHookParams<'a> {
    /// Create ServiceHookParams from service context.
    ///
    /// This builder method centralizes the common pattern of constructing
    /// ServiceHookParams from a ServiceConfig and related context.
    pub fn from_service_context(
        service_config: &'a crate::config::ServiceConfig,
        working_dir: &'a Path,
        env: &'a HashMap<String, String>,
        log_config: Option<&'a LogWriterConfig>,
        global_log_config: Option<&'a LogConfig>,
    ) -> Self {
        Self {
            working_dir,
            env,
            log_config,
            service_user: service_config.user.as_deref(),
            service_groups: &service_config.groups,
            service_log_config: service_config.logs.as_ref(),
            global_log_config,
        }
    }
}

/// Types of global hooks
#[derive(Debug, Clone, Copy)]
pub enum GlobalHookType {
    OnInit,
    PreStart,
    PostStart,
    PreStop,
    PostStop,
    PreRestart,
    PostRestart,
    PreCleanup,
}

impl GlobalHookType {
    pub fn as_str(&self) -> &'static str {
        match self {
            GlobalHookType::OnInit => "on_init",
            GlobalHookType::PreStart => "pre_start",
            GlobalHookType::PostStart => "post_start",
            GlobalHookType::PreStop => "pre_stop",
            GlobalHookType::PostStop => "post_stop",
            GlobalHookType::PreRestart => "pre_restart",
            GlobalHookType::PostRestart => "post_restart",
            GlobalHookType::PreCleanup => "pre_cleanup",
        }
    }
}

impl std::fmt::Display for GlobalHookType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl GlobalHookType {
    /// Get the hook slot for this type from a GlobalHooks struct.
    pub fn get(self, hooks: &GlobalHooks) -> &Option<HookCommand> {
        match self {
            Self::OnInit => &hooks.on_init,
            Self::PreStart => &hooks.pre_start,
            Self::PostStart => &hooks.post_start,
            Self::PreStop => &hooks.pre_stop,
            Self::PostStop => &hooks.post_stop,
            Self::PreRestart => &hooks.pre_restart,
            Self::PostRestart => &hooks.post_restart,
            Self::PreCleanup => &hooks.pre_cleanup,
        }
    }
}

/// Types of service hooks
#[derive(Debug, Clone, Copy)]
pub enum ServiceHookType {
    OnInit,
    PreStart,
    PostStart,
    PreStop,
    PostStop,
    PreRestart,
    PostRestart,
    PostExit,
    PreCleanup,
    PostHealthcheckSuccess,
    PostHealthcheckFail,
}

impl ServiceHookType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceHookType::OnInit => "on_init",
            ServiceHookType::PreStart => "pre_start",
            ServiceHookType::PostStart => "post_start",
            ServiceHookType::PreStop => "pre_stop",
            ServiceHookType::PostStop => "post_stop",
            ServiceHookType::PreRestart => "pre_restart",
            ServiceHookType::PostRestart => "post_restart",
            ServiceHookType::PostExit => "post_exit",
            ServiceHookType::PreCleanup => "pre_cleanup",
            ServiceHookType::PostHealthcheckSuccess => "post_healthcheck_success",
            ServiceHookType::PostHealthcheckFail => "post_healthcheck_fail",
        }
    }
}

impl std::fmt::Display for ServiceHookType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl ServiceHookType {
    /// Get the hook slot for this type from a ServiceHooks struct.
    pub fn get(self, hooks: &ServiceHooks) -> &Option<HookCommand> {
        match self {
            Self::OnInit => &hooks.on_init,
            Self::PreStart => &hooks.pre_start,
            Self::PostStart => &hooks.post_start,
            Self::PreStop => &hooks.pre_stop,
            Self::PostStop => &hooks.post_stop,
            Self::PreRestart => &hooks.pre_restart,
            Self::PostRestart => &hooks.post_restart,
            Self::PostExit => &hooks.post_exit,
            Self::PreCleanup => &hooks.pre_cleanup,
            Self::PostHealthcheckSuccess => &hooks.post_healthcheck_success,
            Self::PostHealthcheckFail => &hooks.post_healthcheck_fail,
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

    // Determine effective groups
    // Priority: hook groups > service groups
    let hook_groups = hook.groups();
    let effective_groups = if !hook_groups.is_empty() {
        hook_groups.to_vec()
    } else {
        ctx.service_groups.to_vec()
    };

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
        effective_groups,
    );

    // Spawn using blocking mode with logging
    let mode = BlockingMode::WithLogging {
        log_config: ctx.log_config.cloned(),
        log_service_name: ctx.service_name.to_string(),
        store_stdout: ctx.store_stdout,
        store_stderr: ctx.store_stderr,
    };

    let result = spawn_blocking(spec, mode).await?;
    if result.exit_code != Some(0) {
        return Err(DaemonError::HookFailed {
            hook_type: format!("{} {:?}", &program_and_args[0], &program_and_args[1..]),
            message: format!("Exit code: {:?}", result.exit_code),
        });
    }
    Ok(())
}

/// The prefix used for global hook logs
pub const GLOBAL_HOOK_PREFIX: &str = "global.";

/// Format the service name for global hook logs (e.g., "global.on_start")
pub fn global_hook_service_name(hook_type: GlobalHookType) -> String {
    format!("global.{}", hook_type.as_str())
}

/// Run a global hook if it exists.
///
/// When `progress` is `Some`, emits `HookStarted` before and
/// `HookCompleted`/`HookFailed` after execution. Skipped when the hook
/// is not defined.
///
/// Note: Global hooks always run as the daemon user (no service user to inherit).
pub async fn run_global_hook(
    hooks: &Option<GlobalHooks>,
    hook_type: GlobalHookType,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_config: Option<&LogWriterConfig>,
    global_log_config: Option<&LogConfig>,
    progress: &Option<ProgressSender>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    if let Some(hook) = hook_type.get(hooks) {
        info!("Running global {} hook", hook_type.as_str());
        let hook_name = hook_type.as_str().to_string();
        emit_hook_event(progress, "global", ServicePhase::HookStarted { hook: hook_name.clone() }).await;

        let service_name = global_hook_service_name(hook_type);
        // Resolve store settings from global config
        let (store_stdout, store_stderr) = resolve_log_store(None, global_log_config);
        // Global hooks run as daemon user (no service user to inherit)
        let ctx = HookRunContext {
            base_working_dir: working_dir,
            env,
            log_config,
            service_name: &service_name,
            service_user: None,
            service_groups: &[],
            store_stdout,
            store_stderr,
        };
        match run_hook(hook, &ctx).await {
            Ok(()) => {
                emit_hook_event(progress, "global", ServicePhase::HookCompleted { hook: hook_name }).await;
            }
            Err(e) => {
                emit_hook_event(progress, "global", ServicePhase::HookFailed { hook: hook_name, message: e.to_string() }).await;
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Format the log name for service hooks (e.g., "backend.on_start")
pub fn service_hook_log_name(service_name: &str, hook_type: ServiceHookType) -> String {
    format!("{}.{}", service_name, hook_type.as_str())
}

/// Run a service hook if it exists.
///
/// When `progress` is `Some`, emits `HookStarted` before and
/// `HookCompleted`/`HookFailed` after execution. Skipped when the hook
/// is not defined.
pub async fn run_service_hook(
    hooks: &Option<ServiceHooks>,
    hook_type: ServiceHookType,
    service_name: &str,
    params: &ServiceHookParams<'_>,
    progress: &Option<ProgressSender>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    if let Some(hook) = hook_type.get(hooks) {
        info!(
            "Running {} hook for service {}",
            hook_type.as_str(),
            service_name
        );
        let hook_name = hook_type.as_str().to_string();
        emit_hook_event(progress, service_name, ServicePhase::HookStarted { hook: hook_name.clone() }).await;

        let log_name = service_hook_log_name(service_name, hook_type);
        // Resolve store settings: service config > global config > default
        let (store_stdout, store_stderr) = resolve_log_store(params.service_log_config, params.global_log_config);
        let ctx = HookRunContext {
            base_working_dir: params.working_dir,
            env: params.env,
            log_config: params.log_config,
            service_name: &log_name,
            service_user: params.service_user,
            service_groups: params.service_groups,
            store_stdout,
            store_stderr,
        };
        match run_hook(hook, &ctx).await {
            Ok(()) => {
                emit_hook_event(progress, service_name, ServicePhase::HookCompleted { hook: hook_name }).await;
            }
            Err(e) => {
                error!(
                    "Hook {} failed for service {}: {}",
                    hook_type.as_str(),
                    service_name,
                    e
                );
                emit_hook_event(progress, service_name, ServicePhase::HookFailed { hook: hook_name, message: e.to_string() }).await;
                return Err(e);
            }
        }
    }

    Ok(())
}
