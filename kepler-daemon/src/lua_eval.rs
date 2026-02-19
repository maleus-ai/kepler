//! Lua scripting support for config templating using Luau.
//!
//! This module provides the `LuaEvaluator` struct which manages a Lua state
//! and allows evaluation of `!lua` tagged values in configs.

use mlua::{FromLua, Lua, Result as LuaResult, Table, Value};
use std::collections::HashMap;

/// Information about a dependency service, exposed to Lua conditions.
#[derive(Debug, Clone, Default)]
pub struct DepInfo {
    pub status: String,
    pub exit_code: Option<i32>,
    pub initialized: bool,
    pub restart_count: u32,
    /// Dependency's computed environment variables
    pub env: HashMap<String, String>,
}

/// Context passed to each Lua evaluation.
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// System environment variables (captured from the CLI)
    pub sys_env: HashMap<String, String>,
    /// Variables loaded from env_file (empty if no env_file)
    pub env_file: HashMap<String, String>,
    /// Full accumulated environment (sys_env + env_file + environment)
    pub env: HashMap<String, String>,
    /// Current service name (None if global context)
    pub service_name: Option<String>,
    /// Current hook name (None if not in a hook)
    pub hook_name: Option<String>,
    /// Whether the service has been initialized (on_init run)
    pub initialized: Option<bool>,
    /// Number of times the service has been restarted
    pub restart_count: Option<u32>,
    /// Last exit code of the service process
    pub exit_code: Option<i32>,
    /// Current service status as a string (e.g., "running", "stopped")
    pub status: Option<String>,
    /// Whether any previous hook in the current list failed (None if not in a hook list)
    pub hook_had_failure: Option<bool>,
    /// Dependency service states (keyed by dep service name)
    pub deps: HashMap<String, DepInfo>,
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

        Ok(Self { lua })
    }

    /// Load the `lua:` block - runs in global scope to define functions.
    pub fn load_inline(&self, code: &str) -> LuaResult<()> {
        self.lua.load(code).set_name("lua").exec()
    }

    /// Evaluate a `!lua` block and return the result.
    ///
    /// The code runs with a custom environment containing:
    /// - `ctx.env`: Read-only table of environment variables
    /// - `ctx.sys_env`: Read-only table of system environment variables
    /// - `ctx.env_file`: Read-only table of env_file variables
    /// - `ctx.service_name`: Current service name (or nil)
    /// - `ctx.hook_name`: Current hook name (or nil)
    /// - `global`: Shared mutable table for cross-block state
    /// - Access to all functions defined in `lua:` block and standard library
    pub fn eval<T: FromLua>(&self, code: &str, ctx: &EvalContext, chunk_name: &str) -> LuaResult<T> {
        let env_table = self.build_env_table(ctx)?;

        let chunk = self.lua.load(code);
        let func = chunk.set_name(chunk_name).set_environment(env_table).into_function()?;

        func.call(())
    }

    /// Evaluate a condition expression and return whether it's truthy.
    ///
    /// Uses an environment that additionally exposes status functions
    /// (`always()`, `success()`, `failure()`, `skipped()`) and a `deps` table.
    ///
    /// Returns `true` unless the result is `nil` or `false`.
    pub fn eval_condition(&self, code: &str, ctx: &EvalContext) -> LuaResult<bool> {
        let env_table = self.build_condition_env_table(ctx)?;

        let wrapped = format!("return {}", code);
        let chunk = self.lua.load(&wrapped);
        let func = chunk
            .set_name("if-condition")
            .set_environment(env_table)
            .into_function()?;

        let result: Value = func.call(())?;
        Ok(!matches!(result, Value::Nil | Value::Boolean(false)))
    }

    /// Evaluate an inline `${{ expr }}` expression and return the raw Lua value.
    ///
    /// Uses the same environment as `!lua` blocks: includes `ctx`, `deps`, `env`
    /// shortcut, `global`, and a `__index` fallback to Lua globals.
    pub fn eval_inline_expr(&self, expr: &str, ctx: &EvalContext, chunk_name: &str) -> LuaResult<Value> {
        let env_table = self.build_inline_env_table(ctx)?;
        let wrapped = format!("return ({})", expr);
        let chunk = self.lua.load(&wrapped);
        let func = chunk.set_name(chunk_name).set_environment(env_table).into_function()?;
        func.call(())
    }

    /// Evaluate an inline `${{ expr }}` reusing a pre-built environment table.
    ///
    /// Avoids rebuilding the env table for each expression within the same evaluation context.
    pub fn eval_inline_expr_with_env(&self, expr: &str, env_table: &Table, chunk_name: &str) -> LuaResult<Value> {
        let wrapped = format!("return ({})", expr);
        let chunk = self.lua.load(&wrapped);
        let func = chunk.set_name(chunk_name).set_environment(env_table.clone()).into_function()?;
        func.call(())
    }

    /// Build and return the environment table for inline `${{ }}` evaluation.
    ///
    /// Use this to build the table once, then pass it to `eval_inline_expr_with_env`
    /// for each expression within the same context.
    pub fn prepare_env(&self, ctx: &EvalContext) -> LuaResult<Table> {
        self.build_env_table(ctx)
    }

    /// Build the custom environment table for a `!lua` block.
    ///
    /// Includes `ctx`, `deps`, `env` (shortcut for `ctx.env`), `global`,
    /// and a `__index` fallback to Lua globals (stdlib + user functions).
    fn build_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        let env_table = self.lua.create_table()?;

        // Create the `ctx` table with granular access
        let ctx_table = self.build_ctx_table(ctx)?;
        env_table.set("ctx", ctx_table.clone())?;

        // Add `env` shortcut (alias for ctx.env)
        let env_shortcut: Table = ctx_table.get("env")?;
        env_table.set("env", env_shortcut)?;

        // Add `deps` table (frozen, from ctx.deps)
        let deps_table = self.build_deps_table(ctx)?;
        env_table.set("deps", deps_table)?;

        // Add `global` (shared mutable table)
        let global: Table = self.lua.globals().get("global")?;
        env_table.set("global", global)?;

        // Set metatable with __index fallback to globals
        // This allows access to functions defined in lua: block and standard library
        let globals = self.lua.globals();
        let meta = self.lua.create_table()?;
        meta.set("__index", globals)?;
        env_table.set_metatable(Some(meta));

        Ok(env_table)
    }

    /// Build the `ctx` table used by all evaluation environments.
    fn build_ctx_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        let ctx_table = self.lua.create_table()?;

        // Add ctx.sys_env (read-only)
        let sys_env = self.create_frozen_env(&ctx.sys_env)?;
        ctx_table.raw_set("sys_env", sys_env)?;

        // Add ctx.env_file (read-only)
        let env_file = self.create_frozen_env(&ctx.env_file)?;
        ctx_table.raw_set("env_file", env_file)?;

        // Add ctx.env (read-only full accumulated environment)
        let env = self.create_frozen_env(&ctx.env)?;
        ctx_table.raw_set("env", env)?;

        // Add ctx.service_name (nil if global)
        if let Some(ref service_name) = ctx.service_name {
            ctx_table.raw_set("service_name", service_name.as_str())?;
        }

        // Add ctx.hook_name (nil if not in hook)
        if let Some(ref hook_name) = ctx.hook_name {
            ctx_table.raw_set("hook_name", hook_name.as_str())?;
        }

        // Runtime fields (populated for if-condition evaluation)
        if let Some(initialized) = ctx.initialized {
            ctx_table.raw_set("initialized", initialized)?;
        }
        if let Some(restart_count) = ctx.restart_count {
            ctx_table.raw_set("restart_count", restart_count)?;
        }
        if let Some(exit_code) = ctx.exit_code {
            ctx_table.raw_set("exit_code", exit_code)?;
        }
        if let Some(ref status) = ctx.status {
            ctx_table.raw_set("status", status.as_str())?;
        }

        // Freeze ctx table using Luau's native table.freeze
        ctx_table.set_readonly(true);

        Ok(ctx_table)
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
    fn build_condition_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        // Start with the base env table (ctx, deps, env shortcut, global, __index→globals)
        let env_table = self.build_env_table(ctx)?;

        // --- Status functions ---
        let hook_had_failure = ctx.hook_had_failure;
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
        let failure_fn = self.lua.create_function(move |_, name: Option<String>| {
            match name {
                None => {
                    // No args: returns true if a previous hook failed
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

        Ok(env_table)
    }

    /// Build the environment table for inline `${{ expr }}` evaluation.
    ///
    /// Intentionally delegates to `build_env_table` so that `${{ expr }}` and
    /// `!lua` blocks share the same environment (ctx, deps, env, global, stdlib).
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
