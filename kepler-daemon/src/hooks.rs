use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info};

use crate::config::{resolve_log_store, GlobalHooks, HookCommand, HookList, LogConfig, ResolvableCommand, ServiceHooks};
use crate::config_actor::ConfigActorHandle;
use crate::errors::{DaemonError, Result};
use crate::logs::LogWriterConfig;
use crate::lua_eval::{DepInfo, EvalContext, LuaEvaluator};
use crate::process::{spawn_blocking, BlockingMode, OutputCaptureConfig};
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
    /// All hook outputs from prior phases: `hook_name -> step_name -> { key -> value }`
    pub all_hook_outputs: HashMap<String, HashMap<String, HashMap<String, String>>>,
    /// Maximum size in bytes for output capture per step
    pub output_max_size: usize,
    /// Shared Lua evaluator for resolving ConfigValue fields in hooks
    pub evaluator: Option<&'a LuaEvaluator>,
    /// Config file path for error reporting and Lua evaluation
    pub config_path: Option<&'a Path>,
    /// Config directory for relative path resolution
    pub config_dir: Option<&'a Path>,
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

/// Execute a single hook step using ResolvableCommand.
///
/// Resolves all ConfigValue fields (environment, working_dir, user, groups, limits)
/// through the Lua evaluator, then spawns the process.
async fn run_hook_step(
    hook: &HookCommand,
    params: &ServiceHookParams<'_>,
    service_name: &str,
    hook_name: &str,
    eval_ctx: &mut EvalContext,
    output_capture: Option<OutputCaptureConfig>,
) -> Result<Option<Vec<String>>> {
    // Determine effective working_dir base for relative paths
    let config_dir = params.config_dir.unwrap_or(params.working_dir);
    let config_path = params.config_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("<hook>"));

    let field_prefix = format!("{}.hooks.{}", service_name, hook_name);

    // If we have a shared evaluator, use ResolvableCommand for full resolution
    if let Some(evaluator) = params.evaluator {
        let resolvable = ResolvableCommand::from_hook(hook);
        let mut spec = resolvable.resolve(
            evaluator,
            eval_ctx,
            &config_path,
            config_dir,
            &field_prefix,
            true, // hooks always clear env
        )?;

        // If hook has no explicit working_dir, inherit service working_dir
        if hook.common().working_dir.as_static().map(|v| v.is_none()).unwrap_or(false) {
            spec.working_dir = params.working_dir.to_path_buf();
        }

        // Apply user/group inheritance: hook user > service user > daemon user
        if spec.user.is_none() {
            spec.user = params.service_user.map(|s| s.to_string());
        }
        // "daemon" means run as daemon user (no change)
        if spec.user.as_deref() == Some("daemon") {
            spec.user = None;
        }
        if spec.groups.is_empty() {
            spec.groups = params.service_groups.to_vec();
        }

        debug!(
            "Running hook: {} {:?}",
            &spec.program_and_args[0],
            &spec.program_and_args[1..]
        );

        // Spawn using blocking mode with logging
        let log_name = format!("{}.{}", service_name, hook_name);
        let (store_stdout, store_stderr) = resolve_log_store(params.service_log_config, params.global_log_config);
        let mode = BlockingMode::WithLogging {
            log_config: params.log_config.cloned(),
            log_service_name: log_name,
            store_stdout,
            store_stderr,
            output_capture,
        };

        let result = spawn_blocking(spec, mode).await?;
        if result.exit_code != Some(0) {
            let message = match result.exit_code {
                Some(code) => format!("exit code {}", code),
                None => "killed by signal".to_string(),
            };
            return Err(DaemonError::HookFailed {
                service: service_name.to_string(),
                hook: hook_name.to_string(),
                message,
            });
        }
        Ok(result.captured_output)
    } else {
        // Fallback: no evaluator available, use static values only (legacy path for global hooks)
        run_hook_static(hook, params, service_name, hook_name, output_capture).await
    }
}

/// Fallback hook execution using only static ConfigValue fields.
/// Used for global hooks which don't have a shared evaluator.
async fn run_hook_static(
    hook: &HookCommand,
    params: &ServiceHookParams<'_>,
    service_name: &str,
    hook_name: &str,
    output_capture: Option<OutputCaptureConfig>,
) -> Result<Option<Vec<String>>> {
    // Convert hook to program and args (static only)
    let program_and_args = match hook {
        HookCommand::Script { run, .. } => {
            let run_str = run.as_static()
                .ok_or_else(|| DaemonError::Config(
                    format!("Hook {}.{}: dynamic 'run' requires a Lua evaluator", service_name, hook_name)
                ))?;
            vec!["sh".to_string(), "-c".to_string(), run_str.clone()]
        }
        HookCommand::Command { command, .. } => {
            let items = command.as_static()
                .ok_or_else(|| DaemonError::Config(
                    format!("Hook {}.{}: dynamic 'command' requires a Lua evaluator", service_name, hook_name)
                ))?;
            let cmd: Vec<String> = items.iter()
                .map(|v| v.as_static().cloned().ok_or_else(|| DaemonError::Config(
                    format!("Hook {}.{}: dynamic 'command' requires a Lua evaluator", service_name, hook_name)
                )))
                .collect::<Result<Vec<String>>>()?;
            if cmd.is_empty() {
                return Ok(None);
            }
            cmd
        }
    };

    // Determine effective working directory
    let effective_working_dir = match hook.working_dir() {
        Some(dir) => {
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                params.working_dir.join(dir)
            }
        }
        None => params.working_dir.to_path_buf(),
    };

    // Build environment from base env + hook's static env entries
    let mut env = params.env.clone();

    // Load from hook's env_file if specified
    if let Some(env_file_path) = hook.env_file() {
        let resolved_path = if env_file_path.is_relative() {
            effective_working_dir.join(env_file_path)
        } else {
            env_file_path.to_path_buf()
        };
        if resolved_path.exists() {
            let hook_env_file_vars = crate::env::load_env_file(&resolved_path)?;
            env.extend(hook_env_file_vars);
        }
    }

    // Add hook's environment entries
    for entry in hook.environment() {
        if let Some((key, value)) = entry.split_once('=') {
            env.insert(key.to_string(), value.to_string());
        }
    }

    // Determine effective user
    let effective_user = match hook.user() {
        Some("daemon") => None,
        Some(user) => Some(user.to_string()),
        None => params.service_user.map(|s| s.to_string()),
    };

    // Determine effective groups
    let hook_groups = hook.groups();
    let effective_groups = if !hook_groups.is_empty() {
        hook_groups
    } else {
        params.service_groups.to_vec()
    };

    debug!(
        "Running hook: {} {:?}",
        &program_and_args[0],
        &program_and_args[1..]
    );

    let spec = crate::process::CommandSpec::new(
        program_and_args,
        effective_working_dir,
        env,
        effective_user,
        effective_groups,
    );

    let log_name = format!("{}.{}", service_name, hook_name);
    let (store_stdout, store_stderr) = resolve_log_store(params.service_log_config, params.global_log_config);
    let mode = BlockingMode::WithLogging {
        log_config: params.log_config.cloned(),
        log_service_name: log_name,
        store_stdout,
        store_stderr,
        output_capture,
    };

    let result = spawn_blocking(spec, mode).await?;
    if result.exit_code != Some(0) {
        let message = match result.exit_code {
            Some(code) => format!("exit code {}", code),
            None => "killed by signal".to_string(),
        };
        return Err(DaemonError::HookFailed {
            service: service_name.to_string(),
            hook: hook_name.to_string(),
            message,
        });
    }
    Ok(result.captured_output)
}

/// The prefix used for global hook logs
pub const GLOBAL_HOOK_PREFIX: &str = "global.";

/// Format the service name for global hook logs (e.g., "global.on_start")
pub fn global_hook_service_name(hook_type: GlobalHookType) -> String {
    format!("global.{}", hook_type.as_str())
}

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
        // Global hooks run as daemon user (no service user to inherit)
        let hook_params = ServiceHookParams {
            working_dir: params.working_dir,
            env: params.env,
            log_config: params.log_config,
            service_user: None,
            service_groups: &[],
            service_log_config: None,
            global_log_config: params.global_log_config,
            deps: HashMap::new(),
            all_hook_outputs: HashMap::new(),
            output_max_size: crate::config::DEFAULT_OUTPUT_MAX_SIZE,
            evaluator: None, // Global hooks use static fallback
            config_path: None,
            config_dir: None,
        };
        let mut eval_ctx = EvalContext {
            env: params.env.clone(),
            ..Default::default()
        };
        match run_hook_step(hook, &hook_params, &service_name, hook_type.as_str(), &mut eval_ctx, None).await {
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
/// Hooks without an `if` condition are implicitly `success()` — they are
/// skipped when a previous hook in the list has failed. Hooks with
/// `if: "always()"` or `if: "failure()"` still run after a failure.
/// The first error is propagated after all eligible hooks have run.
///
/// Each hook step is resolved individually via `ResolvableCommand`, enabling
/// per-step `${{ }}$` evaluation including hook step output accumulation.
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

        // Build EvalContext once per hook step — reused for both condition and execution.
        let state = if let Some(h) = handle {
            h.get_service_state(service_name).await
        } else {
            None
        };
        // Merge all_hook_outputs with current phase's hook_outputs into ctx.hooks.
        // Avoid cloning all_hook_outputs when it's empty.
        let hooks_ctx = if params.all_hook_outputs.is_empty() && hook_outputs.is_empty() {
            HashMap::new()
        } else {
            let mut ctx = params.all_hook_outputs.clone();
            if !hook_outputs.is_empty() {
                ctx.entry(hook_type.as_str().to_string())
                    .or_default()
                    .extend(hook_outputs.clone());
            }
            ctx
        };
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
            hooks: hooks_ctx,
            ..Default::default()
        };

        // Evaluate runtime `if` condition if present
        if let Some(condition) = hook.condition()
            && let Some(h) = handle {
                match h.eval_if_condition(condition, eval_ctx.clone()).await {
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

        // Determine output capture for this step
        let output_capture = hook.output().map(|_| OutputCaptureConfig {
            max_size: params.output_max_size,
        });

        match run_hook_step(hook, params, service_name, hook_type.as_str(), &mut eval_ctx, output_capture).await {
            Ok(captured) => {
                // If this step had output capture and a step name, store the results
                if let Some(step_name) = hook.output()
                    && let Some(lines) = captured {
                        let parsed = crate::outputs::parse_capture_lines(&lines);
                        if !parsed.is_empty() {
                            hook_outputs.insert(step_name.to_string(), parsed);
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
