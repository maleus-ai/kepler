//! Environment variable expansion for configuration files
//!
//! This module handles shell-style variable expansion in configuration values,
//! supporting ${VAR}, ${VAR:-default}, ${VAR:+value}, and ~ expansion.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use super::{
    DependencyEntry, GlobalHooks, HookCommand, RestartConfig, ServiceConfig, ServiceHooks,
    SysEnvPolicy,
};

/// Expand a string using the given environment context and sys_env.
/// Priority: context (env_file vars) > sys_env (CLI environment)
pub fn expand_with_context(
    s: &str,
    context: &HashMap<String, String>,
    sys_env: &HashMap<String, String>,
) -> String {
    shellexpand::env_with_context(
        s,
        |var| -> std::result::Result<Option<Cow<'_, str>>, std::env::VarError> {
            // First check context (env_file), then fall back to sys_env (captured CLI environment)
            Ok(context
                .get(var)
                .map(|v| Cow::Borrowed(v.as_str()))
                .or_else(|| sys_env.get(var).map(|v| Cow::Borrowed(v.as_str()))))
        },
    )
    .map(|expanded| expanded.into_owned())
    .unwrap_or_else(|_| s.to_string())
}

/// Expand environment variables in a string using shell-style expansion.
/// Supports ${VAR}, ${VAR:-default}, ${VAR:+value}, ~ (tilde), etc.
/// Uses the provided context for variable lookup (context overrides sys_env).
pub fn expand_value(
    s: &str,
    context: &HashMap<String, String>,
    sys_env: &HashMap<String, String>,
) -> String {
    expand_with_context(s, context, sys_env)
}

/// Expand environment variables in global hooks
pub fn expand_global_hooks(
    hooks: &mut GlobalHooks,
    context: &HashMap<String, String>,
    sys_env: &HashMap<String, String>,
) {
    if let Some(ref mut hook) = hooks.on_init {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_start {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_start {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_stop {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_stop {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_restart {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_restart {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_cleanup {
        expand_hook_command(hook, context, sys_env);
    }
}

/// Expand environment variables in a hook command.
/// Expands: user, group, working_dir, env_file, environment.
/// NOTE: We intentionally do NOT expand run/command - shell variables should be
/// expanded by the shell at runtime using the process's environment.
pub fn expand_hook_command(
    hook: &mut HookCommand,
    context: &HashMap<String, String>,
    sys_env: &HashMap<String, String>,
) {
    match hook {
        HookCommand::Script {
            run: _, // Intentionally not expanded - shell expands at runtime
            user,
            group,
            working_dir,
            environment,
            env_file,
        } => {
            // Expand user/group
            if let Some(u) = user {
                *u = expand_value(u, context, sys_env);
            }
            if let Some(g) = group {
                *g = expand_value(g, context, sys_env);
            }

            // Expand paths
            if let Some(wd) = working_dir {
                *wd = PathBuf::from(expand_value(&wd.to_string_lossy(), context, sys_env));
            }
            if let Some(ef) = env_file {
                *ef = PathBuf::from(expand_value(&ef.to_string_lossy(), context, sys_env));
            }

            // Expand environment entries
            for entry in environment {
                *entry = expand_value(entry, context, sys_env);
            }
        }
        HookCommand::Command {
            command: _, // Intentionally not expanded - shell expands at runtime
            user,
            group,
            working_dir,
            environment,
            env_file,
        } => {
            // Expand user/group
            if let Some(u) = user {
                *u = expand_value(u, context, sys_env);
            }
            if let Some(g) = group {
                *g = expand_value(g, context, sys_env);
            }

            // Expand paths
            if let Some(wd) = working_dir {
                *wd = PathBuf::from(expand_value(&wd.to_string_lossy(), context, sys_env));
            }
            if let Some(ef) = env_file {
                *ef = PathBuf::from(expand_value(&ef.to_string_lossy(), context, sys_env));
            }

            // Expand environment entries
            for entry in environment {
                *entry = expand_value(entry, context, sys_env);
            }
        }
    }
}

/// Expand environment variables in a service config.
/// Expands ALL string fields using the provided context (env_file vars) and sys_env.
/// The env_file path should already be expanded before calling this.
///
/// Environment entries are processed in order, with each entry's key=value
/// being added to the context for subsequent entries. This allows entries
/// to reference variables defined earlier in the same array.
pub fn expand_service_config(
    service: &mut ServiceConfig,
    context: &HashMap<String, String>,
    sys_env: &HashMap<String, String>,
) {
    // Create a mutable copy of context for environment processing
    let mut expanded_context = context.clone();

    // NOTE: We intentionally do NOT expand service.command here.
    // Commands should have their $VAR references expanded by the shell at runtime,
    // using the process's environment. Values should be passed via the environment array.

    // Expand working_dir (already partially expanded, but re-expand with full context)
    if let Some(ref mut wd) = service.working_dir {
        *wd = PathBuf::from(expand_value(&wd.to_string_lossy(), &expanded_context, sys_env));
    }

    // NOTE: env_file path is already expanded before this function is called
    // (using sys_env only, since we need it to load the env_file first)

    // Expand user/group
    if let Some(ref mut u) = service.user {
        *u = expand_value(u, &expanded_context, sys_env);
    }
    if let Some(ref mut g) = service.group {
        *g = expand_value(g, &expanded_context, sys_env);
    }

    // Expand environment entries IN ORDER, adding each to context for subsequent entries
    // This allows entries like:
    //   - BASE_VAR=base
    //   - EXPANDED=${BASE_VAR}_suffix
    for entry in &mut service.environment {
        // Expand the value using current context
        let expanded_entry = expand_value(entry, &expanded_context, sys_env);
        *entry = expanded_entry.clone();

        // Add this entry to context for subsequent entries
        if let Some((key, value)) = expanded_entry.split_once('=') {
            expanded_context.insert(key.to_string(), value.to_string());
        }
    }

    // Expand depends_on entries (only simple string entries need expansion)
    for entry in &mut service.depends_on.0 {
        if let DependencyEntry::Simple(name) = entry {
            *name = expand_value(name, &expanded_context, sys_env);
        }
        // Extended entries have map keys which shouldn't be expanded
    }

    // NOTE: We intentionally do NOT expand healthcheck.test here.
    // Healthcheck commands should have their $VAR references expanded by the shell
    // at runtime, using the process's environment.

    // Expand resource limits
    if let Some(ref mut limits) = service.limits
        && let Some(ref mut mem) = limits.memory {
            *mem = expand_value(mem, &expanded_context, sys_env);
        }

    // Expand restart watch patterns
    if let RestartConfig::Extended { ref mut watch, .. } = service.restart {
        for pattern in watch {
            *pattern = expand_value(pattern, &expanded_context, sys_env);
        }
    }

    // Expand service hooks
    if let Some(ref mut hooks) = service.hooks {
        expand_service_hooks(hooks, &expanded_context, sys_env);
    }
}

/// Expand environment variables in service hooks
pub fn expand_service_hooks(
    hooks: &mut ServiceHooks,
    context: &HashMap<String, String>,
    sys_env: &HashMap<String, String>,
) {
    if let Some(ref mut hook) = hooks.on_init {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_start {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_start {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_stop {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_stop {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.pre_restart {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_restart {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_exit {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_healthcheck_success {
        expand_hook_command(hook, context, sys_env);
    }
    if let Some(ref mut hook) = hooks.post_healthcheck_fail {
        expand_hook_command(hook, context, sys_env);
    }
}

/// Resolve sys_env policy for a service.
/// Priority: service explicit setting > global setting > default (Clear)
///
/// A service's sys_env is considered "explicit" if it's not the default value.
/// This allows services to explicitly set `sys_env: clear` to override a global
/// `inherit` setting.
pub fn resolve_sys_env(
    service_sys_env: &SysEnvPolicy,
    global_sys_env: Option<&SysEnvPolicy>,
) -> SysEnvPolicy {
    // Service explicit setting > Global > Default(Clear)
    // Note: We can't distinguish between "not specified" and "specified as default"
    // in the current config model, so we check if service_sys_env != default
    if *service_sys_env != SysEnvPolicy::default() {
        service_sys_env.clone()
    } else {
        global_sys_env.cloned().unwrap_or_default()
    }
}
