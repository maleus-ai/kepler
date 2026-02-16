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

    /// Build the custom environment table for a `!lua` block.
    fn build_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        let env_table = self.lua.create_table()?;

        // Create the `ctx` table with granular access
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

        env_table.set("ctx", ctx_table)?;

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

    /// Build environment table for `if` condition evaluation.
    ///
    /// Same as `build_env_table` but additionally exposes:
    /// - `deps` table (frozen, from `ctx.deps`)
    /// - Status functions: `always()`, `success()`, `failure()`, `skipped()`
    fn build_condition_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        let env_table = self.lua.create_table()?;

        // --- ctx table (same as build_env_table) ---
        let ctx_table = self.lua.create_table()?;
        let sys_env = self.create_frozen_env(&ctx.sys_env)?;
        ctx_table.raw_set("sys_env", sys_env)?;
        let env_file = self.create_frozen_env(&ctx.env_file)?;
        ctx_table.raw_set("env_file", env_file)?;
        let env = self.create_frozen_env(&ctx.env)?;
        ctx_table.raw_set("env", env)?;
        if let Some(ref service_name) = ctx.service_name {
            ctx_table.raw_set("service_name", service_name.as_str())?;
        }
        if let Some(ref hook_name) = ctx.hook_name {
            ctx_table.raw_set("hook_name", hook_name.as_str())?;
        }
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
        ctx_table.set_readonly(true);
        env_table.set("ctx", ctx_table)?;

        // --- global table ---
        let global: Table = self.lua.globals().get("global")?;
        env_table.set("global", global)?;

        // --- deps table (frozen) ---
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
            dep_table.set_readonly(true);
            deps_table.raw_set(name.as_str(), dep_table)?;
        }
        deps_table.set_readonly(true);
        env_table.set("deps", deps_table)?;

        // --- Status functions ---
        let hook_had_failure = ctx.hook_had_failure;
        let deps_clone = ctx.deps.clone();

        // always() — always returns true
        let always_fn = self.lua.create_function(|_, ()| Ok(true))?;
        env_table.set("always", always_fn)?;

        // success(name?) — no args: !hook_had_failure; with arg: dep is in successful state
        let deps_for_success = deps_clone.clone();
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
        let deps_for_failure = deps_clone.clone();
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
        let deps_for_skipped = deps_clone;
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

        // Set metatable with __index fallback to globals
        let globals = self.lua.globals();
        let meta = self.lua.create_table()?;
        meta.set("__index", globals)?;
        env_table.set_metatable(Some(meta));

        Ok(env_table)
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
pub fn lua_table_to_env_vec(_lua: &Lua, value: Value) -> LuaResult<Vec<String>> {
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
mod tests {
    use super::*;

    #[test]
    fn test_lua_returns_string() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: String = eval.eval(r#"return "hello""#, &ctx, "test").unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_lua_returns_array() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval.eval(r#"return {"a", "b", "c"}"#, &ctx, "test").unwrap();
        let vec = lua_value_to_string_vec(result).unwrap();
        assert_eq!(vec, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_lua_env_access() {
        let eval = LuaEvaluator::new().unwrap();
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let ctx = EvalContext {
            env,
            ..Default::default()
        };

        let result: String = eval.eval(r#"return ctx.env.FOO"#, &ctx, "test").unwrap();
        assert_eq!(result, "bar");
    }

    #[test]
    fn test_lua_env_readonly() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result = eval.eval::<Value>(r#"ctx.env.NEW = "value""#, &ctx, "test");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("readonly") || err.contains("read-only"),
            "Error should mention readonly: {}",
            err
        );
    }

    #[test]
    fn test_lua_global_shared() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // First block sets global
        let _: Value = eval.eval(r#"global.port = 8080"#, &ctx, "test").unwrap();

        // Second block reads it
        let port: i64 = eval.eval(r#"return global.port"#, &ctx, "test").unwrap();
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_lua_service_context() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            service_name: Some("backend".to_string()),
            ..Default::default()
        };

        let result: String = eval.eval(r#"return ctx.service_name"#, &ctx, "test").unwrap();
        assert_eq!(result, "backend");
    }

    #[test]
    fn test_lua_functions_available() {
        let eval = LuaEvaluator::new().unwrap();

        // Load function in global scope
        eval.load_inline(
            r#"
            function double(x)
                return x * 2
            end
        "#,
        )
        .unwrap();

        // Use it from a !lua block
        let ctx = EvalContext::default();
        let result: i64 = eval.eval(r#"return double(21)"#, &ctx, "test").unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_lua_env_map_format() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval.eval(r#"return {FOO="bar", BAZ="qux"}"#, &ctx, "test").unwrap();
        let map = lua_table_to_env_map(result).unwrap();

        assert_eq!(map.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(map.get("BAZ"), Some(&"qux".to_string()));
    }

    #[test]
    fn test_lua_env_array_format() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval
            .eval(r#"return {"FOO=bar", "BAZ=qux"}"#, &ctx, "test")
            .unwrap();
        let vec = lua_table_to_env_vec(&eval.lua, result).unwrap();

        assert!(vec.contains(&"FOO=bar".to_string()));
        assert!(vec.contains(&"BAZ=qux".to_string()));
    }

    #[test]
    fn test_lua_hook_context() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            service_name: Some("api".to_string()),
            hook_name: Some("on_start".to_string()),
            ..Default::default()
        };

        let result: String = eval.eval(r#"return ctx.hook_name"#, &ctx, "test").unwrap();
        assert_eq!(result, "on_start");
    }

    #[test]
    fn test_lua_nil_for_missing_context() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // service_name should be nil when not set
        let result: Value = eval.eval(r#"return ctx.service_name"#, &ctx, "test").unwrap();
        assert!(matches!(result, Value::Nil));
    }

    #[test]
    fn test_lua_standard_library_available() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // String functions should work
        let result: String = eval
            .eval(r#"return string.upper("hello")"#, &ctx, "test")
            .unwrap();
        assert_eq!(result, "HELLO");

        // Math functions should work
        let result: i64 = eval.eval(r#"return math.max(1, 5, 3)"#, &ctx, "test").unwrap();
        assert_eq!(result, 5);
    }

    // --- Status function tests (eval_condition uses sandboxed env) ---

    #[test]
    fn test_always_returns_true() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            hook_had_failure: Some(true),
            ..Default::default()
        };
        assert!(eval.eval_condition("always()", &ctx).unwrap());
    }

    #[test]
    fn test_success_no_args_no_failure() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            hook_had_failure: Some(false),
            ..Default::default()
        };
        assert!(eval.eval_condition("success()", &ctx).unwrap());
    }

    #[test]
    fn test_success_no_args_after_failure() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            hook_had_failure: Some(true),
            ..Default::default()
        };
        assert!(!eval.eval_condition("success()", &ctx).unwrap());
    }

    #[test]
    fn test_failure_no_args() {
        let eval = LuaEvaluator::new().unwrap();

        // No failure
        let ctx = EvalContext {
            hook_had_failure: Some(false),
            ..Default::default()
        };
        assert!(!eval.eval_condition("failure()", &ctx).unwrap());

        // After failure
        let ctx = EvalContext {
            hook_had_failure: Some(true),
            ..Default::default()
        };
        assert!(eval.eval_condition("failure()", &ctx).unwrap());
    }

    #[test]
    fn test_success_with_dep_name() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "healthy".to_string(),
            exit_code: None,
            initialized: true,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(eval.eval_condition("success('db')", &ctx).unwrap());
    }

    #[test]
    fn test_success_with_dep_name_failed() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "failed".to_string(),
            exit_code: None,
            initialized: false,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(!eval.eval_condition("success('db')", &ctx).unwrap());
    }

    #[test]
    fn test_failure_with_dep_name() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "failed".to_string(),
            exit_code: None,
            initialized: false,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(eval.eval_condition("failure('db')", &ctx).unwrap());
    }

    #[test]
    fn test_failure_with_dep_name_healthy() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "healthy".to_string(),
            exit_code: None,
            initialized: true,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(!eval.eval_condition("failure('db')", &ctx).unwrap());
    }

    #[test]
    fn test_failure_with_nonzero_exit_code() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "exited".to_string(),
            exit_code: Some(1),
            initialized: true,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(eval.eval_condition("failure('db')", &ctx).unwrap());
        assert!(!eval.eval_condition("success('db')", &ctx).unwrap());
    }

    #[test]
    fn test_skipped_with_dep_name() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "skipped".to_string(),
            exit_code: None,
            initialized: false,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(eval.eval_condition("skipped('db')", &ctx).unwrap());
    }

    #[test]
    fn test_skipped_with_dep_not_skipped() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "running".to_string(),
            exit_code: None,
            initialized: true,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(!eval.eval_condition("skipped('db')", &ctx).unwrap());
    }

    #[test]
    fn test_deps_table_access() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "healthy".to_string(),
            exit_code: Some(0),
            initialized: true,
            restart_count: 3,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };

        assert!(eval.eval_condition("deps.db.status == 'healthy'", &ctx).unwrap());
        assert!(eval.eval_condition("deps.db.exit_code == 0", &ctx).unwrap());
        assert!(eval.eval_condition("deps.db.initialized == true", &ctx).unwrap());
        assert!(eval.eval_condition("deps.db.restart_count == 3", &ctx).unwrap());
    }

    #[test]
    fn test_deps_table_exit_code_nil() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "running".to_string(),
            exit_code: None,
            initialized: true,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        assert!(eval.eval_condition("deps.db.exit_code == nil", &ctx).unwrap());
    }

    #[test]
    fn test_deps_table_frozen() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert("db".to_string(), DepInfo {
            status: "healthy".to_string(),
            exit_code: None,
            initialized: true,
            restart_count: 0,
        });
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };

        // Attempting to modify deps table should error
        let result = eval.eval_condition("(function() deps.db.status = 'bad'; return true end)()", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_require_blocked() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // require should be nil everywhere
        let result: String = eval.eval(r#"return type(require)"#, &ctx, "test").unwrap();
        assert_eq!(result, "nil");
        assert!(eval.eval_condition("require == nil", &ctx).unwrap());
    }

    #[test]
    fn test_condition_sandbox_allows_stdlib() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        assert!(eval.eval_condition("string.upper('hello') == 'HELLO'", &ctx).unwrap());
        assert!(eval.eval_condition("math.max(1, 5, 3) == 5", &ctx).unwrap());
        assert!(eval.eval_condition("type(42) == 'number'", &ctx).unwrap());
        assert!(eval.eval_condition("tonumber('42') == 42", &ctx).unwrap());
        assert!(eval.eval_condition("tostring(42) == '42'", &ctx).unwrap());
    }

    #[test]
    fn test_condition_sandbox_lua_block_functions() {
        let eval = LuaEvaluator::new().unwrap();

        // Define a function in the lua: block
        eval.load_inline("function my_check() return true end").unwrap();

        let ctx = EvalContext::default();
        assert!(eval.eval_condition("my_check()", &ctx).unwrap());
    }

    #[test]
    fn test_success_no_args_default_when_not_in_hook() {
        let eval = LuaEvaluator::new().unwrap();
        // hook_had_failure = None means not in a hook list context
        let ctx = EvalContext::default();
        // Should default to true (no failure)
        assert!(eval.eval_condition("success()", &ctx).unwrap());
    }

    #[test]
    fn test_failure_no_args_default_when_not_in_hook() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();
        // Should default to false (no failure)
        assert!(!eval.eval_condition("failure()", &ctx).unwrap());
    }

    #[test]
    fn test_unknown_dep_errors() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        assert!(eval.eval_condition("success('nonexistent')", &ctx).is_err());
        assert!(eval.eval_condition("failure('nonexistent')", &ctx).is_err());
        assert!(eval.eval_condition("skipped('nonexistent')", &ctx).is_err());
    }
}
