//! Lua scripting support for config templating using Luau.
//!
//! This module provides the `LuaEvaluator` struct which manages a Lua state
//! and allows evaluation of `!lua` tagged values in configs.

use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::{DynamicExpr, expr::OwnedExprToken};
use crate::hardening::HardeningLevel;

/// Result of evaluating a hook `if:` condition.
#[derive(Debug, Clone)]
pub struct ConditionResult {
    pub value: bool,
    /// Whether `failure()` (no args) was called during evaluation.
    pub failure_checked: bool,
}

/// Pre-built Lua environment table with an updatable `env` sub-table.
///
/// Created by `LuaEvaluator::prepare_env_mutable()`. The env sub-table is
/// always frozen from Lua's perspective (Lua code cannot mutate it), but
/// `set_env()` can add entries by temporarily toggling readonly from Rust.
/// Call `freeze_env()` once env resolution is complete to permanently lock
/// the table — after that, `set_env()` will error.
pub struct PreparedEnv {
    /// The full env table to set as Lua chunk environment.
    pub table: Table,
    /// Handle to the env sub-table (always readonly from Lua's perspective).
    env_table: Table,
    /// Once true, `set_env()` will error.
    permanently_frozen: std::cell::Cell<bool>,
}

impl PreparedEnv {
    /// Add an env var to the live Lua env table.
    ///
    /// Temporarily unfreezes the table, writes the entry, and re-freezes.
    /// Errors if `freeze_env()` has been called.
    pub fn set_env(&self, key: &str, value: &str) -> mlua::Result<()> {
        if self.permanently_frozen.get() {
            return Err(mlua::Error::RuntimeError(
                "env table is permanently frozen".to_string(),
            ));
        }
        self.env_table.set_readonly(false);
        let result = self.env_table.raw_set(key, value);
        self.env_table.set_readonly(true);
        result
    }

    /// Permanently freeze the env sub-table. After this, `set_env()` will error.
    pub fn freeze_env(&self) {
        self.permanently_frozen.set(true);
    }
}

/// Information about a dependency service, exposed to Lua conditions.
#[derive(Debug, Clone, Default)]
pub struct DepInfo {
    pub status: String,
    pub exit_code: Option<i32>,
    pub initialized: bool,
    pub restart_count: u32,
    /// Dependency's computed environment variables
    pub env: HashMap<String, String>,
    /// Final combined outputs (process + resolved) for dependent service access
    pub outputs: HashMap<String, String>,
}

/// Service evaluation context — exposed as the `service` Lua table.
#[derive(Debug, Clone, Default)]
pub struct ServiceEvalContext {
    pub name: String,
    pub env_file: HashMap<String, String>,
    /// Fully resolved: kepler_env + env_file + environment
    pub env: HashMap<String, String>,
    pub initialized: Option<bool>,
    pub restart_count: Option<u32>,
    pub exit_code: Option<i32>,
    pub status: Option<String>,
    /// Hook outputs: `hook_name -> step_name -> { key -> value }`
    pub hooks: HashMap<String, HashMap<String, HashMap<String, String>>>,
}

/// Hook evaluation context — exposed as the `hook` Lua table.
#[derive(Debug, Clone, Default)]
pub struct HookEvalContext {
    pub name: String,
    pub env_file: HashMap<String, String>,
    /// Fully resolved: service env + hook env_file + hook environment
    pub env: HashMap<String, String>,
    pub had_failure: Option<bool>,
}

/// Owner identity context — who loaded the config.
#[derive(Debug, Clone, Default)]
pub struct OwnerEvalContext {
    pub uid: u32,
    pub gid: u32,
    pub user: Option<String>,
}

/// Context passed to each Lua evaluation.
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    pub service: Option<ServiceEvalContext>,
    pub hook: Option<HookEvalContext>,
    /// Dependency service states (keyed by dep service name)
    pub deps: HashMap<String, DepInfo>,
    /// Config owner identity (None for root-owned or legacy configs)
    pub owner: Option<OwnerEvalContext>,
    /// Hardening level for privilege checks in os.getgroups()
    pub hardening: HardeningLevel,
    /// Kepler-level environment variables (replaces raw_env / sys_env)
    pub kepler_env: HashMap<String, String>,
    /// When true, `kepler.env` access raises a Lua error (autostart: true without environment)
    pub kepler_env_denied: bool,
}

impl EvalContext {
    /// Get a reference to the "active" env — hook env if present, else service env.
    /// Falls back to an empty map if neither is present.
    pub fn active_env(&self) -> &HashMap<String, String> {
        static EMPTY: std::sync::LazyLock<HashMap<String, String>> = std::sync::LazyLock::new(HashMap::new);
        if let Some(ref hook) = self.hook {
            &hook.env
        } else if let Some(ref svc) = self.service {
            &svc.env
        } else {
            &EMPTY
        }
    }

    /// Get a mutable reference to the "active" env — hook env if present, else service env.
    /// Panics if neither is present.
    pub fn active_env_mut(&mut self) -> &mut HashMap<String, String> {
        if let Some(ref mut hook) = self.hook {
            &mut hook.env
        } else {
            &mut self.service.as_mut().unwrap().env
        }
    }

    /// Get a mutable reference to the "active" env_file — hook env_file if present, else service env_file.
    /// Panics if neither is present.
    pub fn active_env_file_mut(&mut self) -> &mut HashMap<String, String> {
        if let Some(ref mut hook) = self.hook {
            &mut hook.env_file
        } else {
            &mut self.service.as_mut().unwrap().env_file
        }
    }
}

/// Evaluator for Lua scripts in config files.
///
/// Manages a single Lua state that persists across all evaluations within
/// a config load. Functions defined in `lua:` blocks are available to all
/// `!lua` blocks.
pub struct LuaEvaluator {
    lua: Lua,
}

impl LuaEvaluator {
    /// Create a new Lua evaluator with a fresh Lua state.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();

        // Create the shared `global` table for cross-block state
        let global_table = lua.create_table()?;
        lua.globals().set("global", global_table)?;

        // Remove `require` from globals to prevent loading external modules
        lua.globals().set("require", Value::Nil)?;

        // Register json stdlib
        let json_table = lua.create_table()?;
        json_table.set(
            "parse",
            lua.create_function(|lua, s: String| {
                let v: serde_json::Value = serde_json::from_str(&s)
                    .map_err(|e| mlua::Error::RuntimeError(format!("json.parse: {}", e)))?;
                lua.to_value(&v)
            })?,
        )?;
        json_table.set(
            "stringify",
            lua.create_function(|lua, (val, pretty): (Value, Option<bool>)| {
                let v: serde_json::Value = lua.from_value(val)?;
                if pretty.unwrap_or(false) {
                    serde_json::to_string_pretty(&v)
                } else {
                    serde_json::to_string(&v)
                }
                .map_err(|e| mlua::Error::RuntimeError(format!("json.stringify: {}", e)))
            })?,
        )?;
        json_table.set_readonly(true);
        lua.globals().set("json", json_table)?;

        // Register yaml stdlib
        let yaml_table = lua.create_table()?;
        yaml_table.set(
            "parse",
            lua.create_function(|lua, s: String| {
                let v: serde_yaml::Value = serde_yaml::from_str(&s)
                    .map_err(|e| mlua::Error::RuntimeError(format!("yaml.parse: {}", e)))?;
                lua.to_value(&v)
            })?,
        )?;
        yaml_table.set(
            "stringify",
            lua.create_function(|lua, val: Value| {
                let v: serde_yaml::Value = lua.from_value(val)?;
                serde_yaml::to_string(&v)
                    .map_err(|e| mlua::Error::RuntimeError(format!("yaml.stringify: {}", e)))
            })?,
        )?;
        yaml_table.set_readonly(true);
        lua.globals().set("yaml", yaml_table)?;

        // Freeze standard library tables to prevent tampering
        for name in &["string", "math", "table", "coroutine", "bit32", "utf8", "buffer", "os"] {
            if let Ok(tbl) = lua.globals().get::<Table>(*name) {
                tbl.set_readonly(true);
            }
        }

        Ok(Self { lua })
    }

    /// Load the `lua:` block - runs in global scope to define functions.
    pub fn load_inline(&self, code: &str) -> LuaResult<()> {
        self.lua.load(code).set_name("lua").exec()
    }

    /// Evaluate a `!lua` block and return the result.
    ///
    /// The code runs with a custom environment containing:
    /// - `service.*`: Read-only table with service context (name, env, env_file, etc.)
    /// - `hook.*`: Read-only table with hook context (name, env, env_file, had_failure)
    /// - `kepler.*`: Read-only table with kepler-level context (env)
    /// - `hooks`: Shortcut for service.hooks
    /// - `global`: Shared mutable table for cross-block state
    /// - Access to all functions defined in `lua:` block and standard library
    pub fn eval<T: FromLua>(&self, code: &str, ctx: &EvalContext, chunk_name: &str) -> LuaResult<T> {
        let env_table = self.build_env_table(ctx)?;

        let chunk = self.lua.load(code);
        let func = chunk.set_name(chunk_name).set_environment(env_table).into_function()?;

        func.call(())
    }

    /// Evaluate a condition expression and return a `ConditionResult`.
    ///
    /// Uses an environment that additionally exposes status functions
    /// (`always()`, `success()`, `failure()`, `skipped()`) and a `deps` table.
    ///
    /// The result's `value` is `true` unless the Lua result is `nil` or `false`.
    /// The result's `failure_checked` is `true` if `failure()` (no args) was called.
    pub fn eval_condition(&self, code: &str, ctx: &EvalContext) -> LuaResult<ConditionResult> {
        let (env_table, failure_flag) = self.build_condition_env_table(ctx)?;

        let wrapped = format!("return {}", code);
        let chunk = self.lua.load(&wrapped);
        let func = chunk
            .set_name("if-condition")
            .set_environment(env_table)
            .into_function()?;

        let result: Value = func.call(())?;
        Ok(ConditionResult {
            value: !matches!(result, Value::Nil | Value::Boolean(false)),
            failure_checked: failure_flag.load(Ordering::Relaxed),
        })
    }

    /// Evaluate a condition from a `DynamicExpr` and return a `ConditionResult`.
    ///
    /// Like `eval_condition` but accepts a pre-parsed `DynamicExpr` instead of a raw string.
    /// Handles all three expression variants (Expression, Interpolated, Lua).
    pub fn eval_condition_expr(&self, expr: &DynamicExpr, ctx: &EvalContext) -> LuaResult<ConditionResult> {
        let (env_table, failure_flag) = self.build_condition_env_table(ctx)?;

        let code = match expr {
            DynamicExpr::Expression(s) => format!("return {}", s),
            DynamicExpr::Lua(s) => s.clone(),
            DynamicExpr::Interpolated(tokens) => {
                let parts: Vec<String> = tokens.iter().map(|t| match t {
                    OwnedExprToken::Literal(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
                    OwnedExprToken::Expression(s) => format!("tostring({})", s),
                }).collect();
                format!("return {}", parts.join(" .. "))
            }
        };

        let chunk = self.lua.load(&code);
        let func = chunk.set_name("if-condition").set_environment(env_table).into_function()?;
        let result: Value = func.call(())?;

        Ok(ConditionResult {
            value: !matches!(result, Value::Nil | Value::Boolean(false)),
            failure_checked: failure_flag.load(Ordering::Relaxed),
        })
    }

    /// Evaluate an inline `${{ expr }}$` expression and return the raw Lua value.
    ///
    /// Uses the same environment as `!lua` blocks: includes `service`, `hook`,
    /// `deps`, `kepler`, `global`, and a `__index` fallback to Lua globals.
    pub fn eval_inline_expr(&self, expr: &str, ctx: &EvalContext, chunk_name: &str) -> LuaResult<Value> {
        let env_table = self.build_inline_env_table(ctx)?;
        let wrapped = format!("return ({})", expr);
        let chunk = self.lua.load(&wrapped);
        let func = chunk.set_name(chunk_name).set_environment(env_table).into_function()?;
        func.call(())
    }

    /// Evaluate an inline `${{ expr }}$` reusing a pre-built environment table.
    ///
    /// Avoids rebuilding the env table for each expression within the same evaluation context.
    pub fn eval_inline_expr_with_env(&self, expr: &str, env_table: &Table, chunk_name: &str) -> LuaResult<Value> {
        let wrapped = format!("return ({})", expr);
        let chunk = self.lua.load(&wrapped);
        let func = chunk.set_name(chunk_name).set_environment(env_table.clone()).into_function()?;
        func.call(())
    }

    /// Build and return the environment table for inline `${{ }}$` evaluation.
    ///
    /// Use this to build the table once, then pass it to `eval_inline_expr_with_env`
    /// for each expression within the same context.
    pub fn prepare_env(&self, ctx: &EvalContext) -> LuaResult<Table> {
        self.build_env_table(ctx)
    }

    /// Build a mutable environment table for sequential env resolution.
    ///
    /// Same as `prepare_env` but leaves the active env (hook.env, service.env,
    /// or kepler.env when no service/hook context) unfrozen so new entries can
    /// be added via `PreparedEnv::set_env()`. Call `PreparedEnv::freeze_env()`
    /// after env resolution completes, then reuse `PreparedEnv::table` for all
    /// remaining field resolutions.
    pub fn prepare_env_mutable(&self, ctx: &EvalContext) -> LuaResult<PreparedEnv> {
        let (table, env_table) = self.build_full_env_table(ctx, false)?;
        Ok(PreparedEnv {
            table,
            env_table,
            permanently_frozen: std::cell::Cell::new(false),
        })
    }

    /// Evaluate a `!lua` block reusing a pre-built environment table.
    ///
    /// Like `eval()` but avoids rebuilding the env table.
    pub fn eval_with_env<T: FromLua>(&self, code: &str, env_table: &Table, chunk_name: &str) -> LuaResult<T> {
        let chunk = self.lua.load(code);
        let func = chunk.set_name(chunk_name).set_environment(env_table.clone()).into_function()?;
        func.call(())
    }

    /// Build the custom environment table for a `!lua` block.
    ///
    /// Includes `service`, `hook`, `deps`, `hooks` shortcut,
    /// `global`, and a `__index` fallback to Lua globals (stdlib + user functions).
    fn build_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        self.build_full_env_table(ctx, true).map(|(table, _)| table)
    }

    /// Build the full environment table with service/hook tables, deps, kepler, global, and metatable.
    ///
    /// When `freeze_env` is true, the active env sub-table is frozen (normal path).
    /// When false, it stays unfrozen for `PreparedEnv::set_env()`.
    /// Returns `(env_table, active_env_sub_table)`.
    fn build_full_env_table(&self, ctx: &EvalContext, freeze_env: bool) -> LuaResult<(Table, Table)> {
        let env_table = self.lua.create_table()?;
        let mut active_env_sub: Option<Table> = None;
        let mut hooks_table_for_shortcut: Option<Table> = None;

        // Build `service` table if present, otherwise stub with empty env
        if let Some(ref svc) = ctx.service {
            // When hook is present, service.env is always frozen (it's the base).
            // When no hook, use the freeze_env parameter.
            let svc_freeze = if ctx.hook.is_some() { true } else { freeze_env };
            let (svc_table, svc_env) = self.build_service_table(svc, svc_freeze)?;
            if let Ok(h) = svc_table.get::<Table>("hooks") {
                hooks_table_for_shortcut = Some(h);
            }
            env_table.set("service", svc_table)?;
            if ctx.hook.is_none() {
                active_env_sub = Some(svc_env);
            }
        } else {
            // Stub so service.env.X resolves to nil instead of erroring
            let stub = self.lua.create_table()?;
            stub.raw_set("env", self.create_frozen_env(&HashMap::new())?)?;
            stub.set_readonly(true);
            env_table.set("service", stub)?;
        }

        // Build `hook` table if present, otherwise stub with empty env
        if let Some(ref hook) = ctx.hook {
            let (hook_table, hook_env) = self.build_hook_table(hook, freeze_env)?;
            env_table.set("hook", hook_table)?;
            active_env_sub = Some(hook_env);
        } else {
            // Stub so hook.env.X resolves to nil instead of erroring
            let stub = self.lua.create_table()?;
            stub.raw_set("env", self.create_frozen_env(&HashMap::new())?)?;
            stub.set_readonly(true);
            env_table.set("hook", stub)?;
        }

        // `hooks` shortcut → from service.hooks
        if let Some(hooks_shortcut) = hooks_table_for_shortcut {
            env_table.set("hooks", hooks_shortcut)?;
        }

        // Add `deps` table (frozen, from EvalContext.deps)
        let deps_table = self.build_deps_table(ctx)?;
        env_table.set("deps", deps_table)?;

        // Build `owner` table if present
        if let Some(ref owner) = ctx.owner {
            let owner_table = self.lua.create_table()?;
            owner_table.raw_set("uid", owner.uid)?;
            owner_table.raw_set("gid", owner.gid)?;
            if let Some(ref user) = owner.user {
                owner_table.raw_set("user", user.as_str())?;
            }
            owner_table.set_readonly(true);
            env_table.set("owner", owner_table)?;
        }

        // Build per-context `os` table with `getgroups` + fallback to global os
        {
            let os_table = self.lua.create_table()?;

            let hardening = ctx.hardening;
            let owner_uid = ctx.owner.as_ref().map(|o| o.uid);
            // Cache resolved groups per user spec for the lifetime of this env context.
            // Avoids redundant syscalls when multiple !lua / ${{ }}$ blocks query the same user.
            let groups_cache: Arc<Mutex<HashMap<String, Vec<String>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let getgroups_fn = self.lua.create_function(move |lua, user_spec: Option<String>| {
                let Some(spec) = user_spec else {
                    return Err(mlua::Error::RuntimeError(
                        "os.getgroups: expected a username or uid argument".to_string(),
                    ));
                };

                // Check cache first (keyed by raw spec string)
                {
                    let cache = groups_cache.lock().unwrap();
                    if let Some(cached) = cache.get(&spec) {
                        let result = lua.create_table()?;
                        for (i, name) in cached.iter().enumerate() {
                            result.raw_set(i + 1, name.as_str())?;
                        }
                        result.set_readonly(true);
                        return Ok(mlua::Value::Table(result));
                    }
                }

                // Resolve user spec to uid/gid/username
                let resolved = crate::user::resolve_user(&spec)
                    .map_err(|e| mlua::Error::RuntimeError(format!("os.getgroups: {}", e)))?;

                // Hardening checks
                match hardening {
                    HardeningLevel::NoRoot | HardeningLevel::Strict if resolved.uid == 0 => {
                        return Err(mlua::Error::RuntimeError(
                            format!("os.getgroups: access denied for user '{}' (uid 0) — hardening level: {}", spec, hardening)
                        ));
                    }
                    HardeningLevel::Strict => {
                        if let Some(owner) = owner_uid
                            && resolved.uid != owner {
                                return Err(mlua::Error::RuntimeError(
                                    format!("os.getgroups: access denied for user '{}' (uid {}) — only config owner uid {} allowed (hardening level: strict)", spec, resolved.uid, owner)
                                ));
                            }
                    }
                    _ => {}
                }

                // Get the username for getgrouplist
                let username = match &resolved.username {
                    Some(name) => name.clone(),
                    None => {
                        // Numeric-only user with no passwd entry — cache and return empty
                        groups_cache.lock().unwrap().insert(spec, Vec::new());
                        let t = lua.create_table()?;
                        t.set_readonly(true);
                        return Ok(mlua::Value::Table(t));
                    }
                };

                let c_name = std::ffi::CString::new(username.as_str())
                    .map_err(|_| mlua::Error::RuntimeError("os.getgroups: invalid username".to_string()))?;

                let gids = kepler_unix::groups::getgrouplist(&c_name, resolved.gid)
                    .map_err(|e| mlua::Error::RuntimeError(format!("os.getgroups: getgrouplist failed: {}", e)))?;

                // Resolve each gid to group name
                let mut group_names = Vec::new();
                for gid in gids {
                    if let Ok(Some(group)) = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid)) {
                        group_names.push(group.name);
                    }
                }

                // Store in cache
                groups_cache.lock().unwrap().insert(spec, group_names.clone());

                let result = lua.create_table()?;
                for (i, name) in group_names.iter().enumerate() {
                    result.raw_set(i + 1, name.as_str())?;
                }
                result.set_readonly(true);
                Ok(mlua::Value::Table(result))
            })?;
            os_table.set("getgroups", getgroups_fn)?;

            // Fallback to global os table (os.clock, os.date, etc.)
            if let Ok(global_os) = self.lua.globals().get::<Table>("os") {
                let os_meta = self.lua.create_table()?;
                os_meta.set("__index", global_os)?;
                os_table.set_metatable(Some(os_meta));
            }
            os_table.set_readonly(true);
            env_table.set("os", os_table)?;
        }

        // Build `kepler` table — always created so kepler.env.X resolves to nil.
        // When kepler_env_denied is true (autostart: true without environment),
        // accessing kepler.env.X raises a Lua error explaining the issue.
        // When no service/hook context and freeze_env is false (mutable path), the
        // kepler.env sub-table stays unfrozen since set_env() will add entries later.
        let needs_mutable_kepler = active_env_sub.is_none() && !freeze_env;
        let kepler_env_table = {
            let kepler_table = self.lua.create_table()?;
            let kepler_env_sub = if ctx.kepler_env_denied {
                // Error-raising table: any access to kepler.env.X calls error()
                let t = self.lua.create_table()?;
                let meta = self.lua.create_table()?;
                let error_fn = self.lua.create_function(|_, (_tbl, key): (Table, mlua::Value)| -> LuaResult<mlua::Value> {
                    let key_str = match &key {
                        mlua::Value::String(s) => s.to_string_lossy().to_string(),
                        _ => "...".to_string(),
                    };
                    Err(mlua::Error::RuntimeError(format!(
                        "kepler.env.{key} is not available: kepler.autostart is enabled but kepler.autostart.environment is not set. \
                         To fix, add 'kepler.autostart.environment: [{key}, ...]' or set 'kepler.autostart: false' to use the full CLI environment.",
                        key = key_str
                    )))
                })?;
                meta.set("__index", error_fn)?;
                t.set_metatable(Some(meta));
                t.set_readonly(true);
                t
            } else if !needs_mutable_kepler {
                self.create_frozen_env(&ctx.kepler_env)?
            } else {
                // No service/hook: kepler.env is the mutable target
                let t = self.lua.create_table()?;
                for (k, v) in &ctx.kepler_env {
                    t.raw_set(k.as_str(), v.as_str())?;
                }
                t.set_readonly(true);
                t
            };
            kepler_table.raw_set("env", kepler_env_sub.clone())?;
            kepler_table.set_readonly(true);
            env_table.set("kepler", kepler_table)?;
            kepler_env_sub
        };

        // Determine mutable env sub-table for PreparedEnv
        let env_sub = active_env_sub.unwrap_or(kepler_env_table);

        // Add `global` (shared mutable table)
        let global: Table = self.lua.globals().get("global")?;
        env_table.set("global", global)?;

        // Set metatable with __index fallback to globals
        // This allows access to functions defined in lua: block and standard library
        let globals = self.lua.globals();
        let meta = self.lua.create_table()?;
        meta.set("__index", globals)?;
        env_table.set_metatable(Some(meta));

        Ok((env_table, env_sub))
    }

    /// Build the `service` Lua table from a `ServiceEvalContext`.
    ///
    /// Contains: name, env_file, env, initialized, restart_count, exit_code, status, hooks.
    /// When `freeze_env` is true, service.env is frozen; when false, it stays unfrozen
    /// for `PreparedEnv::set_env()`.
    /// Returns `(service_table, env_sub_table)`.
    fn build_service_table(&self, svc: &ServiceEvalContext, freeze_env: bool) -> LuaResult<(Table, Table)> {
        let table = self.lua.create_table()?;

        table.raw_set("name", svc.name.as_str())?;

        // env_file (read-only)
        let env_file = self.create_frozen_env(&svc.env_file)?;
        table.raw_set("env_file", env_file)?;

        // env (frozen or unfrozen depending on freeze_env)
        let env = if freeze_env {
            self.create_frozen_env(&svc.env)?
        } else {
            let t = self.lua.create_table()?;
            for (k, v) in &svc.env {
                t.raw_set(k.as_str(), v.as_str())?;
            }
            t.set_readonly(true);
            t
        };
        table.raw_set("env", env.clone())?;

        // Runtime fields
        if let Some(initialized) = svc.initialized {
            table.raw_set("initialized", initialized)?;
        }
        if let Some(restart_count) = svc.restart_count {
            table.raw_set("restart_count", restart_count)?;
        }
        if let Some(exit_code) = svc.exit_code {
            table.raw_set("exit_code", exit_code)?;
        }
        if let Some(ref status) = svc.status {
            table.raw_set("status", status.as_str())?;
        }

        // hooks (read-only, nested: hook_name → outputs → step_name → key → value)
        if !svc.hooks.is_empty() {
            let hooks_table = self.lua.create_table()?;
            for (hook_name, steps) in &svc.hooks {
                let hook_table = self.lua.create_table()?;
                let outputs_table = self.lua.create_table()?;
                for (step_name, outputs) in steps {
                    let step_table = self.create_frozen_env(outputs)?;
                    outputs_table.raw_set(step_name.as_str(), step_table)?;
                }
                outputs_table.set_readonly(true);
                hook_table.raw_set("outputs", outputs_table)?;
                hook_table.set_readonly(true);
                hooks_table.raw_set(hook_name.as_str(), hook_table)?;
            }
            hooks_table.set_readonly(true);
            table.raw_set("hooks", hooks_table)?;
        }

        table.set_readonly(true);
        Ok((table, env))
    }

    /// Build the `hook` Lua table from a `HookEvalContext`.
    ///
    /// Contains: name, env_file, env, had_failure.
    /// When `freeze_env` is true, hook.env is frozen; when false, it stays unfrozen
    /// for `PreparedEnv::set_env()`.
    /// Returns `(hook_table, env_sub_table)`.
    fn build_hook_table(&self, hook: &HookEvalContext, freeze_env: bool) -> LuaResult<(Table, Table)> {
        let table = self.lua.create_table()?;

        table.raw_set("name", hook.name.as_str())?;

        // env_file (read-only)
        let env_file = self.create_frozen_env(&hook.env_file)?;
        table.raw_set("env_file", env_file)?;

        // env (frozen or unfrozen depending on freeze_env)
        let env = if freeze_env {
            self.create_frozen_env(&hook.env)?
        } else {
            let t = self.lua.create_table()?;
            for (k, v) in &hook.env {
                t.raw_set(k.as_str(), v.as_str())?;
            }
            t.set_readonly(true);
            t
        };
        table.raw_set("env", env.clone())?;

        if let Some(had_failure) = hook.had_failure {
            table.raw_set("had_failure", had_failure)?;
        }

        table.set_readonly(true);
        Ok((table, env))
    }

    /// Build the `deps` table (frozen) from context dependencies.
    ///
    /// Each dep has: status, exit_code, initialized, restart_count, env (sub-table).
    fn build_deps_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        let deps_table = self.lua.create_table()?;
        for (name, dep) in &ctx.deps {
            let dep_table = self.lua.create_table()?;
            dep_table.raw_set("status", dep.status.as_str())?;
            match dep.exit_code {
                Some(code) => dep_table.raw_set("exit_code", code)?,
                None => dep_table.raw_set("exit_code", Value::Nil)?,
            }
            dep_table.raw_set("initialized", dep.initialized)?;
            dep_table.raw_set("restart_count", dep.restart_count)?;

            // Add dep.env sub-table (frozen)
            let dep_env = self.create_frozen_env(&dep.env)?;
            dep_table.raw_set("env", dep_env)?;

            // Add dep.outputs sub-table (frozen)
            let dep_outputs = self.create_frozen_env(&dep.outputs)?;
            dep_table.raw_set("outputs", dep_outputs)?;

            dep_table.set_readonly(true);
            deps_table.raw_set(name.as_str(), dep_table)?;
        }
        deps_table.set_readonly(true);
        Ok(deps_table)
    }

    /// Build environment table for `if` condition evaluation.
    ///
    /// Same as `build_env_table` but additionally exposes four status functions:
    /// - `always()` — always returns `true`, unconditionally runs the hook.
    /// - `success(name?)` — no arg: `true` if no previous hook in this chain failed;
    ///   with arg: `true` if the named dependency is in a successful state (not failed/killed/skipped,
    ///   exit code 0 or not yet exited).
    /// - `failure(name?)` — no arg: `true` if a previous hook in this chain failed;
    ///   with arg: `true` if the named dependency failed/killed or exited non-zero.
    /// - `skipped(name)` — requires a dependency name, returns `true` if that dependency
    ///   has status "skipped".
    fn build_condition_env_table(&self, ctx: &EvalContext) -> LuaResult<(Table, Arc<AtomicBool>)> {
        // Start with the base env table (service/hook, deps, kepler, global, __index→globals)
        let env_table = self.build_env_table(ctx)?;

        // Flag to track whether `failure()` (no args) was called
        let failure_checked_flag = Arc::new(AtomicBool::new(false));

        // --- Status functions ---
        let hook_had_failure = ctx.hook.as_ref().and_then(|h| h.had_failure);
        let deps = std::sync::Arc::new(ctx.deps.clone());

        // always() — always returns true
        let always_fn = self.lua.create_function(|_, ()| Ok(true))?;
        env_table.set("always", always_fn)?;

        // success(name?) — no args: !hook_had_failure; with arg: dep is in successful state
        let deps_for_success = deps.clone();
        let success_fn = self.lua.create_function(move |_, name: Option<String>| {
            match name {
                None => {
                    // No args: returns true if no previous hook failed
                    Ok(!hook_had_failure.unwrap_or(false))
                }
                Some(dep_name) => {
                    // With arg: check if dep is in a successful state
                    match deps_for_success.get(&dep_name) {
                        Some(dep) => {
                            let failed = matches!(
                                dep.status.as_str(),
                                "failed" | "killed" | "skipped"
                            ) || matches!(dep.exit_code, Some(code) if code != 0);
                            Ok(!failed)
                        }
                        None => Err(mlua::Error::RuntimeError(format!(
                            "unknown dependency: '{}'",
                            dep_name
                        ))),
                    }
                }
            }
        })?;
        env_table.set("success", success_fn)?;

        // failure(name?) — no args: hook_had_failure; with arg: dep is in failed state
        let deps_for_failure = deps.clone();
        let failure_checked_for_closure = failure_checked_flag.clone();
        let failure_fn = self.lua.create_function(move |_, name: Option<String>| {
            match name {
                None => {
                    // No args: returns true if a previous hook failed
                    failure_checked_for_closure.store(true, Ordering::Relaxed);
                    Ok(hook_had_failure.unwrap_or(false))
                }
                Some(dep_name) => {
                    // With arg: check if dep is failed/killed/exited with non-zero
                    match deps_for_failure.get(&dep_name) {
                        Some(dep) => {
                            let failed = matches!(
                                dep.status.as_str(),
                                "failed" | "killed"
                            ) || matches!(dep.exit_code, Some(code) if code != 0);
                            Ok(failed)
                        }
                        None => Err(mlua::Error::RuntimeError(format!(
                            "unknown dependency: '{}'",
                            dep_name
                        ))),
                    }
                }
            }
        })?;
        env_table.set("failure", failure_fn)?;

        // skipped(name) — requires string arg, returns true if dep is skipped
        let deps_for_skipped = deps;
        let skipped_fn = self.lua.create_function(move |_, name: String| {
            match deps_for_skipped.get(&name) {
                Some(dep) => Ok(dep.status == "skipped"),
                None => Err(mlua::Error::RuntimeError(format!(
                    "unknown dependency: '{}'",
                    name
                ))),
            }
        })?;
        env_table.set("skipped", skipped_fn)?;

        Ok((env_table, failure_checked_flag))
    }

    /// Build the environment table for inline `${{ expr }}$` evaluation.
    ///
    /// Intentionally delegates to `build_env_table` so that `${{ expr }}$` and
    /// `!lua` blocks share the same environment (service/hook, deps, env, global, stdlib).
    /// This indirection exists so the two contexts can diverge in the future
    /// without changing callers.
    fn build_inline_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        self.build_env_table(ctx)
    }

    /// Set an interrupt handler on the Lua VM for watchdog timeouts.
    pub fn set_interrupt<F>(&self, f: F)
    where
        F: Fn(&Lua) -> LuaResult<mlua::VmState> + Send + 'static,
    {
        self.lua.set_interrupt(f);
    }

    /// Remove the interrupt handler from the Lua VM.
    pub fn remove_interrupt(&self) {
        self.lua.remove_interrupt();
    }

    /// Create a read-only table of environment variables.
    ///
    /// Uses Luau's native `table.freeze` (set_readonly) which makes the table
    /// truly immutable — blocks writes, rawset, and any mutation — while
    /// `pairs()` still works naturally since data is in the table itself.
    fn create_frozen_env(&self, vars: &HashMap<String, String>) -> LuaResult<Table> {
        let table = self.lua.create_table()?;
        for (k, v) in vars {
            table.raw_set(k.as_str(), v.as_str())?;
        }
        table.set_readonly(true);
        Ok(table)
    }

}

/// Convert a Lua table (array or map) to a Vec<String> for environment variables.
///
/// Accepts two formats:
/// - Array: `{"FOO=bar", "BAZ=qux"}`
/// - Map: `{FOO="bar", BAZ="qux"}` -> converted to `["FOO=bar", "BAZ=qux"]`
pub fn lua_table_to_env_vec(value: Value) -> LuaResult<Vec<String>> {
    match value {
        Value::Table(table) => {
            let mut result = Vec::new();
            let mut is_array = true;
            let mut max_index = 0i32;

            // First pass: check if it's an array (sequential integer keys starting at 1)
            for pair in table.clone().pairs::<Value, Value>() {
                let (k, _) = pair?;
                match k {
                    Value::Integer(i) if i > 0 => {
                        max_index = max_index.max(i);
                    }
                    Value::Integer(_) => {
                        is_array = false;
                        break;
                    }
                    _ => {
                        is_array = false;
                        break;
                    }
                }
            }

            if is_array && max_index > 0 {
                // Array format: each element should be a "KEY=value" string
                for i in 1..=max_index {
                    let v: Value = table.get(i)?;
                    match v {
                        Value::String(s) => result.push(s.to_str()?.to_string()),
                        _ => {
                            return Err(mlua::Error::RuntimeError(format!(
                                "environment array element {} must be a string, got {}",
                                i,
                                v.type_name()
                            )))
                        }
                    }
                }
            } else {
                // Map format: convert {KEY="value"} to ["KEY=value", ...]
                for pair in table.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    match v {
                        Value::String(s) => {
                            result.push(format!("{}={}", k, s.to_str()?));
                        }
                        Value::Integer(i) => {
                            result.push(format!("{}={}", k, i));
                        }
                        Value::Number(n) => {
                            result.push(format!("{}={}", k, n));
                        }
                        Value::Boolean(b) => {
                            result.push(format!("{}={}", k, b));
                        }
                        Value::Nil => {
                            // Skip nil values
                        }
                        _ => {
                            return Err(mlua::Error::RuntimeError(format!(
                                "environment value for '{}' must be a string, number, or boolean, got {}",
                                k,
                                v.type_name()
                            )))
                        }
                    }
                }
            }

            Ok(result)
        }
        Value::Nil => Ok(Vec::new()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "environment must be a table, got {}",
            value.type_name()
        ))),
    }
}

/// Convert a Lua table to a HashMap<String, String> for environment variables.
///
/// Accepts the same two formats as `lua_table_to_env_vec`.
pub fn lua_table_to_env_map(value: Value) -> LuaResult<HashMap<String, String>> {
    match value {
        Value::Table(table) => {
            let mut result = HashMap::new();

            for pair in table.pairs::<Value, Value>() {
                let (k, v) = pair?;

                let key = match k {
                    Value::String(s) => s.to_str()?.to_string(),
                    Value::Integer(i) => {
                        // If integer key, the value should be "KEY=value" format
                        match v {
                            Value::String(s) => {
                                let entry = s.to_str()?;
                                if let Some((key, value)) = entry.split_once('=') {
                                    result.insert(key.to_string(), value.to_string());
                                }
                                continue;
                            }
                            _ => {
                                return Err(mlua::Error::RuntimeError(format!(
                                    "array element {} must be a 'KEY=value' string",
                                    i
                                )))
                            }
                        }
                    }
                    _ => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "environment key must be a string, got {}",
                            k.type_name()
                        )))
                    }
                };

                let value = match v {
                    Value::String(s) => s.to_str()?.to_string(),
                    Value::Integer(i) => i.to_string(),
                    Value::Number(n) => n.to_string(),
                    Value::Boolean(b) => b.to_string(),
                    Value::Nil => continue,
                    _ => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "environment value for '{}' must be a string, number, or boolean, got {}",
                            key,
                            v.type_name()
                        )))
                    }
                };

                result.insert(key, value);
            }

            Ok(result)
        }
        Value::Nil => Ok(HashMap::new()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "environment must be a table, got {}",
            value.type_name()
        ))),
    }
}

/// Convert a Lua value to a Vec<String> (for commands, healthcheck tests, etc.)
pub fn lua_value_to_string_vec(value: Value) -> LuaResult<Vec<String>> {
    match value {
        Value::Table(table) => {
            let mut result = Vec::new();
            let len = table.raw_len();

            for i in 1..=len {
                let v: Value = table.get(i)?;
                match v {
                    Value::String(s) => result.push(s.to_str()?.to_string()),
                    Value::Integer(i) => result.push(i.to_string()),
                    Value::Number(n) => result.push(n.to_string()),
                    _ => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "array element {} must be a string or number, got {}",
                            i,
                            v.type_name()
                        )))
                    }
                }
            }

            Ok(result)
        }
        Value::Nil => Ok(Vec::new()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected array, got {}",
            value.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests;
