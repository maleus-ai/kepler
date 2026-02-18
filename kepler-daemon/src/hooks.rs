use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info};

use crate::config::{resolve_log_store, GlobalHooks, HookCommand, HookList, LogConfig, ServiceHooks};
use crate::config_actor::ConfigActorHandle;
use crate::env::build_hook_env;
use crate::errors::{DaemonError, Result};
use crate::logs::LogWriterConfig;
use crate::lua_eval::{DepInfo, EvalContext};
use crate::process::{spawn_blocking, BlockingMode, CommandSpec, OutputCaptureConfig};
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
    /// Lua code from config's `lua:` block, for `${{ }}$` evaluation in hook env
    pub lua_code: Option<&'a str>,
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
    /// Dependency service states (keyed by dep service name)
    pub deps: HashMap<String, DepInfo>,
    /// Lua code from config's `lua:` block, for `${{ }}$` evaluation in hook env
    pub lua_code: Option<&'a str>,
    /// All hook outputs from prior phases: `hook_name -> step_name -> { key -> value }`
    pub all_hook_outputs: HashMap<String, HashMap<String, HashMap<String, String>>>,
    /// Maximum size in bytes for output capture per step
    pub output_max_size: usize,
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
            deps: HashMap::new(),
            lua_code: None,
            all_hook_outputs: HashMap::new(),
            output_max_size: 1024 * 1024, // 1MB default
        }
    }
}

/// Types of global hooks
#[derive(Debug, Clone, Copy)]
pub enum GlobalHookType {
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
    pub fn get(self, hooks: &GlobalHooks) -> &Option<HookList> {
        match self {
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
    pub fn get(self, hooks: &ServiceHooks) -> &Option<HookList> {
        match self {
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

/// Execute a hook command.
///
/// When `output_capture` is `Some`, `::output::KEY=VALUE` marker lines from stdout
/// are captured and returned. Otherwise returns `None`.
pub async fn run_hook(
    hook: &HookCommand,
    ctx: &HookRunContext<'_>,
    service: &str,
    hook_name: &str,
    output_capture: Option<OutputCaptureConfig>,
) -> Result<Option<Vec<String>>> {
    // Convert hook to program and args
    let program_and_args = match hook {
        HookCommand::Script { run, .. } => {
            // Run script through shell
            vec!["sh".to_string(), "-c".to_string(), run.clone()]
        }
        HookCommand::Command { command, .. } => {
            if command.is_empty() {
                return Ok(None);
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
    let hook_env = build_hook_env(hook, ctx.env, &effective_working_dir, ctx.lua_code)?;

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
        output_capture,
    };

    let result = spawn_blocking(spec, mode).await?;
    if result.exit_code != Some(0) {
        let message = match result.exit_code {
            Some(code) => format!("exit code {}", code),
            None => "killed by signal".to_string(),
        };
        return Err(DaemonError::HookFailed {
            service: service.to_string(),
            hook: hook_name.to_string(),
            message,
        });
    }
    Ok(result.captured_output)
}

/// Execute a hook command without output capture.
/// Convenience wrapper around `run_hook` that discards captured output.
pub async fn run_hook_simple(hook: &HookCommand, ctx: &HookRunContext<'_>, service: &str, hook_name: &str) -> Result<()> {
    run_hook(hook, ctx, service, hook_name, None).await?;
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
/// Hooks without an `if` condition are implicitly `success()` — they are
/// skipped when a previous hook in the list has failed. Hooks with
/// `if: "always()"` or `if: "failure()"` still run after a failure.
/// The first error is propagated after all eligible hooks have run.
///
/// Context for running a global hook.
pub struct GlobalHookParams<'a> {
    pub working_dir: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub log_config: Option<&'a LogWriterConfig>,
    pub global_log_config: Option<&'a LogConfig>,
    pub progress: &'a Option<ProgressSender>,
    pub handle: Option<&'a ConfigActorHandle>,
    /// Lua code from config's `lua:` block, for `${{ }}$` evaluation in hook env
    pub lua_code: Option<&'a str>,
}

/// Note: Global hooks always run as the daemon user (no service user to inherit).
pub async fn run_global_hook(
    hooks: &Option<GlobalHooks>,
    hook_type: GlobalHookType,
    params: &GlobalHookParams<'_>,
) -> Result<()> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(()),
    };

    let hook_list = match hook_type.get(hooks) {
        Some(list) if !list.0.is_empty() => list,
        _ => return Ok(()),
    };

    let mut had_failure = false;
    let mut first_error: Option<DaemonError> = None;

    for hook in &hook_list.0 {
        // If a previous hook failed and this hook has no `if` condition,
        // skip it (implicit success() semantics)
        if had_failure && hook.condition().is_none() {
            debug!("Global hook {} skipped: previous hook failed (implicit success())", hook_type.as_str());
            continue;
        }

        // Evaluate runtime `if` condition if present
        if let Some(condition) = hook.condition()
            && let Some(h) = params.handle {
                let initialized = h.is_config_initialized().await;
                let eval_ctx = EvalContext {
                    env: params.env.clone(),
                    initialized: Some(initialized),
                    hook_had_failure: Some(had_failure),
                    ..Default::default()
                };
                match h.eval_if_condition(condition, eval_ctx).await {
                    Ok(true) => {} // condition passed, run the hook
                    Ok(false) => {
                        debug!("Global hook {} skipped: if condition '{}' is falsy", hook_type.as_str(), condition);
                        continue;
                    }
                    Err(e) => {
                        error!("Global hook {} if-condition error: {}", hook_type.as_str(), e);
                        continue;
                    }
                }
            }

        // Warn if output is used on global hooks (not supported)
        if hook.output().is_some() {
            debug!("Global hook {} has 'output' field which is ignored for global hooks", hook_type.as_str());
        }

        info!("Running global {} hook", hook_type.as_str());
        let hook_name = hook_type.as_str().to_string();
        emit_hook_event(params.progress, "global", ServicePhase::HookStarted { hook: hook_name.clone() }).await;

        let service_name = global_hook_service_name(hook_type);
        // Resolve store settings from global config
        let (store_stdout, store_stderr) = resolve_log_store(None, params.global_log_config);
        // Global hooks run as daemon user (no service user to inherit)
        let ctx = HookRunContext {
            base_working_dir: params.working_dir,
            env: params.env,
            log_config: params.log_config,
            service_name: &service_name,
            service_user: None,
            service_groups: &[],
            store_stdout,
            store_stderr,
            lua_code: params.lua_code,
        };
        match run_hook(hook, &ctx, "global", hook_type.as_str(), None).await {
            Ok(_) => {
                emit_hook_event(params.progress, "global", ServicePhase::HookCompleted { hook: hook_name }).await;
            }
            Err(e) => {
                had_failure = true;
                emit_hook_event(params.progress, "global", ServicePhase::HookFailed { hook: hook_name, message: e.to_string() }).await;
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
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
///
/// Hooks without an `if` condition are implicitly `success()` — they are
/// skipped when a previous hook in the list has failed. Hooks with
/// `if: "always()"` or `if: "failure()"` still run after a failure.
/// The first error is propagated after all eligible hooks have run.
///
/// Returns a map of `step_name -> { key -> value }` for steps that had
/// `output: <step_name>` set and captured `::output::KEY=VALUE` lines.
pub async fn run_service_hook(
    hooks: &Option<ServiceHooks>,
    hook_type: ServiceHookType,
    service_name: &str,
    params: &ServiceHookParams<'_>,
    progress: &Option<ProgressSender>,
    handle: Option<&ConfigActorHandle>,
) -> Result<HashMap<String, HashMap<String, String>>> {
    let hooks = match hooks {
        Some(h) => h,
        None => return Ok(HashMap::new()),
    };

    let hook_list = match hook_type.get(hooks) {
        Some(list) if !list.0.is_empty() => list,
        _ => return Ok(HashMap::new()),
    };

    let mut had_failure = false;
    let mut first_error: Option<DaemonError> = None;
    let mut hook_outputs: HashMap<String, HashMap<String, String>> = HashMap::new();

    for hook in &hook_list.0 {
        // If a previous hook failed and this hook has no `if` condition,
        // skip it (implicit success() semantics)
        if had_failure && hook.condition().is_none() {
            debug!("Hook {} for {} skipped: previous hook failed (implicit success())", hook_type.as_str(), service_name);
            continue;
        }

        // Evaluate runtime `if` condition if present
        if let Some(condition) = hook.condition()
            && let Some(h) = handle {
                // Build runtime eval context from service state
                let state = h.get_service_state(service_name).await;
                let mut eval_ctx = EvalContext {
                    env: params.env.clone(),
                    service_name: Some(service_name.to_string()),
                    hook_name: Some(hook_type.as_str().to_string()),
                    initialized: state.as_ref().map(|s| s.initialized),
                    restart_count: state.as_ref().map(|s| s.restart_count),
                    exit_code: state.as_ref().and_then(|s| s.exit_code),
                    status: state.as_ref().map(|s| s.status.as_str().to_string()),
                    hook_had_failure: Some(had_failure),
                    deps: params.deps.clone(),
                    ..Default::default()
                };
                // Merge all_hook_outputs with current phase's hook_outputs into ctx.hooks
                let mut hooks_ctx = params.all_hook_outputs.clone();
                if !hook_outputs.is_empty() {
                    hooks_ctx.entry(hook_type.as_str().to_string())
                        .or_default()
                        .extend(hook_outputs.clone());
                }
                eval_ctx.hooks = hooks_ctx;

                match h.eval_if_condition(condition, eval_ctx).await {
                    Ok(true) => {} // condition passed, run the hook
                    Ok(false) => {
                        debug!("Hook {} for {} skipped: if condition '{}' is falsy", hook_type.as_str(), service_name, condition);
                        continue;
                    }
                    Err(e) => {
                        error!("Hook {} for {} if-condition error: {}", hook_type.as_str(), service_name, e);
                        continue;
                    }
                }
            }

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
            lua_code: params.lua_code,
        };

        // Determine output capture for this step
        let output_capture = hook.output().map(|_| OutputCaptureConfig {
            max_size: params.output_max_size,
        });

        match run_hook(hook, &ctx, service_name, hook_type.as_str(), output_capture).await {
            Ok(captured) => {
                // If this step had output capture and a step name, store the results
                if let Some(step_name) = hook.output() {
                    if let Some(lines) = captured {
                        let parsed = crate::outputs::parse_capture_lines(&lines);
                        if !parsed.is_empty() {
                            hook_outputs.insert(step_name.to_string(), parsed);
                        }
                    }
                }
                emit_hook_event(progress, service_name, ServicePhase::HookCompleted { hook: hook_name }).await;
            }
            Err(e) => {
                had_failure = true;
                error!(
                    "Hook {} failed for service {}: {}",
                    hook_type.as_str(),
                    service_name,
                    e
                );
                emit_hook_event(progress, service_name, ServicePhase::HookFailed { hook: hook_name, message: e.to_string() }).await;
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }
    Ok(hook_outputs)
}
