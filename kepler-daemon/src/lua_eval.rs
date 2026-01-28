//! Lua scripting support for config templating using Luau.
//!
//! This module provides the `LuaEvaluator` struct which manages a Lua state
//! and allows evaluation of `!lua` and `!lua_file` tagged values in configs.

use mlua::{FromLua, Lua, Result as LuaResult, Table, Value};
use std::collections::HashMap;
use std::path::Path;

/// Context passed to each Lua evaluation.
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// Environment variables available as `env` table in Lua.
    pub env: HashMap<String, String>,
    /// Current service name (available as `service` variable).
    pub service: Option<String>,
    /// Current hook name (available as `hook` variable).
    pub hook: Option<String>,
}

/// Evaluator for Lua scripts in config files.
///
/// Manages a single Lua state that persists across all evaluations within
/// a config load. Functions defined in `lua:` blocks and `lua_import` files
/// are available to all `!lua` blocks.
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

        Ok(Self { lua })
    }

    /// Load the `lua:` block - runs in global scope to define functions.
    pub fn load_inline(&self, code: &str) -> LuaResult<()> {
        self.lua.load(code).set_name("lua").exec()
    }

    /// Load a `lua_import` file - runs in global scope to define functions.
    pub fn load_file(&self, path: &Path) -> LuaResult<()> {
        let code = std::fs::read_to_string(path).map_err(|e| {
            mlua::Error::RuntimeError(format!("Failed to read {}: {}", path.display(), e))
        })?;
        self.lua
            .load(&code)
            .set_name(path.to_string_lossy())
            .exec()
    }

    /// Evaluate a `!lua` block and return the result.
    ///
    /// The code runs with a custom environment containing:
    /// - `env`: Read-only table of environment variables
    /// - `global`: Shared mutable table for cross-block state
    /// - `service`: Current service name (or nil)
    /// - `hook`: Current hook name (or nil)
    /// - Access to all functions defined in `lua:` and `lua_import`
    pub fn eval<T: FromLua>(&self, code: &str, ctx: &EvalContext) -> LuaResult<T> {
        let env_table = self.build_env_table(ctx)?;

        let chunk = self.lua.load(code);
        let func = chunk.set_name("!lua").set_environment(env_table).into_function()?;

        func.call(())
    }

    /// Evaluate a `!lua_file` and return the result.
    pub fn eval_file<T: FromLua>(&self, path: &Path, ctx: &EvalContext) -> LuaResult<T> {
        let code = std::fs::read_to_string(path).map_err(|e| {
            mlua::Error::RuntimeError(format!("Failed to read {}: {}", path.display(), e))
        })?;

        let env_table = self.build_env_table(ctx)?;

        let chunk = self.lua.load(&code).set_name(path.to_string_lossy());
        let func = chunk.set_environment(env_table).into_function()?;

        func.call(())
    }

    /// Build the custom environment table for a `!lua` block.
    fn build_env_table(&self, ctx: &EvalContext) -> LuaResult<Table> {
        let env_table = self.lua.create_table()?;

        // Add `env` (read-only table of environment variables)
        let env = self.create_frozen_env(&ctx.env)?;
        env_table.set("env", env)?;

        // Add `global` (shared mutable table)
        let global: Table = self.lua.globals().get("global")?;
        env_table.set("global", global)?;

        // Add context-specific variables
        if let Some(ref service) = ctx.service {
            env_table.set("service", service.as_str())?;
        }
        if let Some(ref hook) = ctx.hook {
            env_table.set("hook", hook.as_str())?;
        }

        // Set metatable with __index fallback to globals
        // This allows access to functions defined in lua: block and standard library
        let globals = self.lua.globals();
        let meta = self.lua.create_table()?;
        meta.set("__index", globals)?;
        env_table.set_metatable(Some(meta));

        Ok(env_table)
    }

    /// Create a read-only table of environment variables.
    fn create_frozen_env(&self, vars: &HashMap<String, String>) -> LuaResult<Table> {
        // Create a regular table with all env values
        // We'll use metatables to prevent writes, but allow direct iteration
        let env_table = self.lua.create_table()?;
        for (k, v) in vars {
            env_table.raw_set(k.as_str(), v.as_str())?;
        }

        // Create metatable that prevents modifications
        let meta = self.lua.create_table()?;
        meta.set(
            "__newindex",
            self.lua
                .create_function(|_, _: (Value, Value, Value)| -> LuaResult<()> {
                    Err(mlua::Error::RuntimeError("env is read-only".to_string()))
                })?,
        )?;

        // Prevent removing the metatable
        meta.set("__metatable", "env is read-only")?;

        env_table.set_metatable(Some(meta));
        Ok(env_table)
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

        let result: String = eval.eval(r#"return "hello""#, &ctx).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_lua_returns_array() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval.eval(r#"return {"a", "b", "c"}"#, &ctx).unwrap();
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

        let result: String = eval.eval(r#"return env.FOO"#, &ctx).unwrap();
        assert_eq!(result, "bar");
    }

    #[test]
    fn test_lua_env_readonly() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result = eval.eval::<Value>(r#"env.NEW = "value""#, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("read-only"));
    }

    #[test]
    fn test_lua_global_shared() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // First block sets global
        let _: Value = eval.eval(r#"global.port = 8080"#, &ctx).unwrap();

        // Second block reads it
        let port: i64 = eval.eval(r#"return global.port"#, &ctx).unwrap();
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_lua_service_context() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            env: HashMap::new(),
            service: Some("backend".to_string()),
            hook: None,
        };

        let result: String = eval.eval(r#"return service"#, &ctx).unwrap();
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
        let result: i64 = eval.eval(r#"return double(21)"#, &ctx).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_lua_env_map_format() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval.eval(r#"return {FOO="bar", BAZ="qux"}"#, &ctx).unwrap();
        let map = lua_table_to_env_map(result).unwrap();

        assert_eq!(map.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(map.get("BAZ"), Some(&"qux".to_string()));
    }

    #[test]
    fn test_lua_env_array_format() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval
            .eval(r#"return {"FOO=bar", "BAZ=qux"}"#, &ctx)
            .unwrap();
        let vec = lua_table_to_env_vec(&eval.lua, result).unwrap();

        assert!(vec.contains(&"FOO=bar".to_string()));
        assert!(vec.contains(&"BAZ=qux".to_string()));
    }

    #[test]
    fn test_lua_hook_context() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext {
            env: HashMap::new(),
            service: Some("api".to_string()),
            hook: Some("on_start".to_string()),
        };

        let result: String = eval.eval(r#"return hook"#, &ctx).unwrap();
        assert_eq!(result, "on_start");
    }

    #[test]
    fn test_lua_nil_for_missing_context() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // service should be nil when not set
        let result: Value = eval.eval(r#"return service"#, &ctx).unwrap();
        assert!(matches!(result, Value::Nil));
    }

    #[test]
    fn test_lua_standard_library_available() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();

        // String functions should work
        let result: String = eval
            .eval(r#"return string.upper("hello")"#, &ctx)
            .unwrap();
        assert_eq!(result, "HELLO");

        // Math functions should work
        let result: i64 = eval.eval(r#"return math.max(1, 5, 3)"#, &ctx).unwrap();
        assert_eq!(result, 5);
    }
}
