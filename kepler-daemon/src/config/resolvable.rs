//! ResolvableCommand — shared abstraction for resolving process fields.
//!
//! Both services and hooks construct a `ResolvableCommand`, call `resolve()`,
//! and get back a `CommandSpec` ready for spawning.

use std::path::{Path, PathBuf};

use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};
use crate::process::CommandSpec;

use super::hooks::HookCommand;
use super::resources::ResourceLimits;
use super::{ConfigValue, EnvironmentEntries, RawServiceConfig};

/// Unresolved process specification referencing ConfigValue<T> fields.
///
/// Constructed from `RawServiceConfig` or `HookCommand`, resolved to concrete
/// values via `resolve()` which returns a `CommandSpec`.
///
/// Most fields are borrowed from the source config. Only `run` and `command`
/// are owned because hook Script variants need type transformation
/// (`ConfigValue<String>` → `ConfigValue<Option<String>>`).
pub struct ResolvableCommand<'a> {
    run: ConfigValue<Option<String>>,
    command: ConfigValue<Vec<ConfigValue<String>>>,
    user: &'a ConfigValue<Option<String>>,
    groups: &'a ConfigValue<Vec<ConfigValue<String>>>,
    working_dir: &'a ConfigValue<Option<PathBuf>>,
    environment: &'a ConfigValue<EnvironmentEntries>,
    env_file: &'a ConfigValue<Option<PathBuf>>,
    limits: &'a ConfigValue<Option<ResourceLimits>>,
}

impl<'a> ResolvableCommand<'a> {
    /// Create from a RawServiceConfig (borrows fields, clones only run and command).
    pub fn from_service(raw: &'a RawServiceConfig) -> Self {
        Self {
            run: raw.run.clone(),
            command: raw.command.clone(),
            user: &raw.user,
            groups: &raw.groups,
            working_dir: &raw.working_dir,
            environment: &raw.environment,
            env_file: &raw.env_file,
            limits: &raw.limits,
        }
    }

    /// Create from a HookCommand (borrows common fields, owns run/command).
    pub fn from_hook(hook: &'a HookCommand) -> Self {
        let common = hook.common();
        match hook {
            HookCommand::Script { run, .. } => Self {
                run: run.map_static(|s| Some(s.clone())),
                command: ConfigValue::Static(Vec::new()),
                user: &common.user,
                groups: &common.groups,
                working_dir: &common.working_dir,
                environment: &common.environment,
                env_file: &common.env_file,
                limits: &common.limits,
            },
            HookCommand::Command { command, .. } => Self {
                run: ConfigValue::Static(None),
                command: command.clone(),
                user: &common.user,
                groups: &common.groups,
                working_dir: &common.working_dir,
                environment: &common.environment,
                env_file: &common.env_file,
                limits: &common.limits,
            },
        }
    }

    /// Resolve all ConfigValue fields and return a CommandSpec.
    ///
    /// Resolution pipeline:
    /// 1. Resolve `env_file` → load env file variables into active env
    /// 2. Build `PreparedEnv`, resolve environment sequentially
    /// 3. Freeze env table
    /// 4. Resolve remaining fields (command/run, working_dir, user, groups, limits)
    /// 5. Absolutize working_dir relative to `config_dir`
    /// 6. Build process env from active env
    /// 7. Return `CommandSpec`
    ///
    /// The `clear_env` parameter controls whether the spawned process inherits
    /// the daemon's environment. Services use sys_env policy; hooks always clear.
    pub fn resolve(
        &self,
        evaluator: &LuaEvaluator,
        ctx: &mut EvalContext,
        config_path: &Path,
        config_dir: &Path,
        field_prefix: &str,
        clear_env: bool,
    ) -> Result<CommandSpec> {
        // Step 1: Resolve env_file and load its variables
        let env_file: Option<PathBuf> = match self.env_file {
            ConfigValue::Static(v) => v.clone(),
            ConfigValue::Dynamic(expr) => {
                let env_table = evaluator.prepare_env(ctx).map_err(|e| DaemonError::LuaError {
                    path: config_path.to_path_buf(),
                    message: format!("Error building Lua environment: {}", e),
                })?;
                let yaml = expr.evaluate(evaluator, &env_table, config_path,
                    &format!("{}.env_file", field_prefix))?;
                serde_yaml::from_value(yaml).map_err(|e| {
                    DaemonError::Config(format!(
                        "Failed to resolve '{}.env_file': {}", field_prefix, e
                    ))
                })?
            }
        };

        if let Some(ref ef_path) = env_file {
            let resolved_path = if ef_path.is_relative() {
                config_dir.join(ef_path)
            } else {
                ef_path.clone()
            };
            if resolved_path.exists() {
                match crate::env::load_env_file(&resolved_path) {
                    Ok(vars) => {
                        for (k, v) in &vars {
                            ctx.active_env_file_mut().insert(k.clone(), v.clone());
                            ctx.active_env_mut().insert(k.clone(), v.clone());
                        }
                    }
                    Err(e) => {
                        return Err(DaemonError::Config(format!(
                            "Failed to load env_file '{}' for '{}': {}",
                            resolved_path.display(), field_prefix, e
                        )));
                    }
                }
            }
        }

        // Step 2: Build PreparedEnv and resolve environment sequentially
        let prepared = evaluator.prepare_env_mutable(ctx).map_err(|e| {
            DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: format!("Error building Lua environment: {}", e),
            }
        })?;

        // Get the inner Vec<ConfigValue<String>> — either directly or by evaluating
        // a top-level !lua/${{ }}$ and deserializing the result.
        let env_entries: Vec<ConfigValue<String>> = match self.environment {
            ConfigValue::Static(entries) => entries.0.clone(),
            ConfigValue::Dynamic(expr) => {
                let yaml = expr.evaluate(
                    evaluator, &prepared.table, config_path,
                    &format!("{}.environment", field_prefix),
                )?;
                let env: EnvironmentEntries = serde_yaml::from_value(yaml).map_err(|e| {
                    DaemonError::Config(format!("{}.environment: {}", field_prefix, e))
                })?;
                env.0
            }
        };

        // Resolve environment entries sequentially (each entry can see previous entries)
        for (i, entry) in env_entries.iter().enumerate() {
            let resolved: String = match entry {
                ConfigValue::Static(s) => s.clone(),
                ConfigValue::Dynamic(expr) => {
                    let yaml = expr.evaluate(
                        evaluator, &prepared.table, config_path,
                        &format!("{}.environment[{}]", field_prefix, i),
                    )?;
                    serde_yaml::from_value(yaml).map_err(|e| {
                        DaemonError::Config(format!("{}.environment[{}]: {}", field_prefix, i, e))
                    })?
                }
            };
            // Add to both active env and the live Lua env table
            if let Some((k, v)) = resolved.split_once('=') {
                ctx.active_env_mut().insert(k.to_string(), v.to_string());
                prepared.set_env(k, v).map_err(|e| DaemonError::LuaError {
                    path: config_path.to_path_buf(),
                    message: format!("Error updating Lua env table: {}", e),
                })?;
            }
        }

        // Step 3: Freeze env sub-table
        prepared.freeze_env();

        // Step 4: Resolve remaining fields using the shared env table
        let mut shared_env: Option<mlua::Table> = Some(prepared.table);

        let has_run = match &self.run {
            ConfigValue::Static(v) => v.is_some(),
            ConfigValue::Dynamic(_) => true,
        };

        let program_and_args = if has_run {
            let run: Option<String> = self.run.resolve_with_env(
                evaluator, ctx, config_path,
                &format!("{}.run", field_prefix), &mut shared_env,
            )?;
            match run {
                Some(script) => {
                    let shell = super::resolve_shell(&ctx.service.as_ref().map(|s| &s.raw_env).cloned().unwrap_or_default());
                    vec![shell, "-c".to_string(), script]
                },
                None => Vec::new(),
            }
        } else {
            super::resolve_nested_vec(&self.command, evaluator, ctx, config_path,
                &format!("{}.command", field_prefix), &mut shared_env)?
        };

        let working_dir_raw: Option<PathBuf> = self.working_dir.resolve_with_env(
            evaluator, ctx, config_path,
            &format!("{}.working_dir", field_prefix), &mut shared_env,
        )?;

        let user: Option<String> = self.user.resolve_with_env(
            evaluator, ctx, config_path,
            &format!("{}.user", field_prefix), &mut shared_env,
        )?;

        let groups: Vec<String> = super::resolve_nested_vec(
            self.groups, evaluator, ctx, config_path,
            &format!("{}.groups", field_prefix), &mut shared_env,
        )?;

        let limits: Option<ResourceLimits> = self.limits.resolve_with_env(
            evaluator, ctx, config_path,
            &format!("{}.limits", field_prefix), &mut shared_env,
        )?;

        // Step 5: Absolutize working_dir relative to config_dir
        let working_dir = working_dir_raw
            .map(|wd| if wd.is_relative() { config_dir.join(wd) } else { wd })
            .unwrap_or_else(|| config_dir.to_path_buf());

        // Step 6: Build process env from active env
        let process_env = ctx.active_env().clone();

        // Step 7: Return CommandSpec
        Ok(CommandSpec::with_all_options(
            program_and_args,
            working_dir,
            process_env,
            user,
            groups,
            limits,
            clear_env,
        ))
    }
}
