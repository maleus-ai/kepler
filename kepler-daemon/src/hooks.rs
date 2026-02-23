use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info};

use crate::config::{resolve_log_store, ConfigValue, HookCommand, HookList, LogConfig, ResolvableCommand, ServiceHooks};
use crate::config_actor::ConfigActorHandle;
use crate::errors::{DaemonError, Result};
use crate::hardening::HardeningLevel;
use crate::logs::{BufferedLogWriter, LogStream, LogWriterConfig};
use crate::lua_eval::{DepInfo, EvalContext, HookEvalContext, LuaEvaluator, OwnerEvalContext, ServiceEvalContext};
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
    /// Kepler-level environment variables
    pub kepler_env: &'a HashMap<String, String>,
    /// Service's env_file vars
    pub env_file_vars: &'a HashMap<String, String>,
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
    /// Hardening level for privilege escalation checks
    pub hardening: HardeningLevel,
    /// Config owner UID for privilege escalation checks
    pub owner_uid: Option<u32>,
    /// Config owner GID for Lua owner context
    pub owner_gid: Option<u32>,
    /// Config owner username for Lua owner context
    pub owner_user: Option<&'a str>,
    /// Kepler group GID for group stripping
    pub kepler_gid: Option<u32>,
    /// When true, `kepler.env` access raises a Lua error (autostart: true without environment)
    pub kepler_env_denied: bool,
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
    step_index: usize,
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

        // Apply user/group inheritance: hook user > service user (includes config owner fallback)
        if spec.user.is_none() {
            spec.user = params.service_user.map(|s| s.to_string());
        }

        // Check privilege escalation before daemon→None conversion
        #[cfg(unix)]
        {
            let context = format!("hook '{}.{}'", service_name, hook_name);
            crate::auth::check_privilege_escalation(
                params.hardening,
                spec.user.as_deref(),
                params.owner_uid,
                &context,
            ).map_err(|e| DaemonError::Config(e))?;
        }

        if spec.groups.is_empty() {
            spec.groups = params.service_groups.to_vec();
        }

        // Strip kepler group from supplementary groups when hardening is enabled
        #[cfg(unix)]
        if params.hardening >= HardeningLevel::NoRoot
            && spec.groups.is_empty()
            && spec.user.is_some()
        {
            if let Some(kepler_gid) = params.kepler_gid {
                let user_spec = spec.user.as_deref().unwrap();
                match crate::user::compute_groups_excluding(user_spec, kepler_gid) {
                    Ok(g) => spec.groups = g,
                    Err(e) => {
                        debug!("Failed to compute groups for hook {}.{}: {}", service_name, hook_name, e);
                    }
                }
            }
        }

        debug!(
            "Running hook: {} {:?}",
            &spec.program_and_args[0],
            &spec.program_and_args[1..]
        );

        // Spawn using blocking mode with logging
        let log_name = format!("{}.{}.{}", service_name, hook_name, step_index);
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
        Err(DaemonError::Config(
            format!("Hook {}.{}: no Lua evaluator available for hook resolution", service_name, hook_name)
        ))
    }
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
    let mut failure_checked = false;

    for (step_idx, hook) in hook_list.0.iter().enumerate() {
        // Build EvalContext once per hook step — reused for both condition and execution.
        let state = if let Some(h) = handle {
            h.get_service_state(service_name).await
        } else {
            None
        };
        // Merge all_hook_outputs with current phase's hook_outputs into service.hooks.
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
            service: Some(ServiceEvalContext {
                name: service_name.to_string(),
                env_file: params.env_file_vars.clone(),
                env: params.env.clone(),
                initialized: state.as_ref().map(|s| s.initialized),
                restart_count: state.as_ref().map(|s| s.restart_count),
                exit_code: state.as_ref().and_then(|s| s.exit_code),
                status: state.as_ref().map(|s| s.status.as_str().to_string()),
                hooks: hooks_ctx,
            }),
            hook: Some(HookEvalContext {
                name: hook_type.as_str().to_string(),
                env_file: HashMap::new(),
                env: if hook.common().inherit_env == Some(false) {
                    HashMap::new()
                } else {
                    params.env.clone()
                },
                had_failure: Some(had_failure),
            }),
            deps: params.deps.clone(),
            owner: params.owner_uid.map(|uid| OwnerEvalContext {
                uid,
                gid: params.owner_gid.unwrap_or(uid),
                user: params.owner_user.map(|s| s.to_string()),
            }),
            hardening: params.hardening,
            kepler_env: params.kepler_env.clone(),
            kepler_env_denied: params.kepler_env_denied,
        };

        // Evaluate the `if` condition (ConfigValue-based)
        match hook.condition_value() {
            ConfigValue::Static(None) => {
                // No condition — implicit success(): skip if previous hook failed
                if had_failure {
                    debug!("Hook {} for {} skipped: previous hook failed (implicit success())", hook_type.as_str(), service_name);
                    continue;
                }
            }
            ConfigValue::Static(Some(false)) => {
                debug!("Hook {} for {} skipped: if condition is static false", hook_type.as_str(), service_name);
                continue;
            }
            ConfigValue::Static(Some(true)) => {
                // Always run, failure_checked stays false
            }
            ConfigValue::Dynamic(expr) => {
                if let Some(h) = handle {
                    match h.eval_if_condition(expr, eval_ctx.clone()).await {
                        Ok(result) if result.value => {
                            failure_checked = result.failure_checked;
                        }
                        Ok(_) => {
                            debug!("Hook {} for {} skipped: if condition is falsy", hook_type.as_str(), service_name);
                            continue;
                        }
                        Err(e) => {
                            error!("Hook {} for {} if-condition error: {}", hook_type.as_str(), service_name, e);
                            continue;
                        }
                    }
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

        match run_hook_step(hook, params, service_name, hook_type.as_str(), step_idx + 1, &mut eval_ctx, output_capture).await {
            Ok(captured) => {
                // If this step succeeded after a previous failure and `failure()` was
                // called in the `if:` condition, the failure is considered "handled" —
                // clear the error and resume normal execution.  Only `failure()` being
                // called is an explicit acknowledgment; `always()` means "run no matter
                // what" (cleanup), not "I'm handling this failure".
                if had_failure && failure_checked {
                    first_error = None;
                    had_failure = false;
                    failure_checked = false;
                }
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
                // Write error to service stderr log so it's visible via `kepler logs`
                if let Some(log_config) = params.log_config {
                    let mut writer = BufferedLogWriter::from_config(log_config, service_name, LogStream::Stderr);
                    writer.write(&format!("Hook {} failed: {}", hook_type.as_str(), e));
                }
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
