//! Lua scripting support for config templating using Luau.
//!
//! This module provides the `LuaEvaluator` struct which manages a Lua state
//! and allows evaluation of `!lua` and `!lua_file` tagged values in configs.
//!
//! External Lua files can be imported using `require()` - the config directory
//! is added to the Lua package path automatically.

use mlua::{FromLua, Lua, Result as LuaResult, Table, Value};
use std::collections::HashMap;
use std::path::Path;

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
}

/// Evaluator for Lua scripts in config files.
///
/// Manages a single Lua state that persists across all evaluations within
/// a config load. Functions defined in `lua:` blocks are available to all
/// `!lua` blocks. External files can be imported using `require()`.
pub struct LuaEvaluator {
    lua: Lua,
}

impl LuaEvaluator {
    /// Create a new Lua evaluator with a fresh Lua state.
    ///
    /// The config directory is added to the Lua package path to enable
    /// `require()` to load external Lua files relative to the config.
    pub fn new(config_dir: &Path) -> LuaResult<Self> {
        let lua = Lua::new();

        // Create the shared `global` table for cross-block state
        let global_table = lua.create_table()?;
        lua.globals().set("global", global_table)?;

        // Add config directory to package.path for require()
        let package: Table = lua.globals().get("package")?;
        let existing_path: String = package.get("path").unwrap_or_default();
        let config_path = config_dir.to_string_lossy();
        let new_path = format!(
            "{}/?.lua;{}/?/init.lua;{}",
            config_path, config_path, existing_path
        );
        package.set("path", new_path)?;

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
    /// - `require()` can load files relative to the config directory
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
    use std::path::PathBuf;

    fn test_config_dir() -> PathBuf {
        PathBuf::from("/tmp")
    }

    #[test]
    fn test_lua_returns_string() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext::default();

        let result: String = eval.eval(r#"return "hello""#, &ctx).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_lua_returns_array() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval.eval(r#"return {"a", "b", "c"}"#, &ctx).unwrap();
        let vec = lua_value_to_string_vec(result).unwrap();
        assert_eq!(vec, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_lua_env_access() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let ctx = EvalContext {
            env,
            ..Default::default()
        };

        let result: String = eval.eval(r#"return ctx.env.FOO"#, &ctx).unwrap();
        assert_eq!(result, "bar");
    }

    #[test]
    fn test_lua_env_readonly() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext::default();

        let result = eval.eval::<Value>(r#"ctx.env.NEW = "value""#, &ctx);
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
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext::default();

        // First block sets global
        let _: Value = eval.eval(r#"global.port = 8080"#, &ctx).unwrap();

        // Second block reads it
        let port: i64 = eval.eval(r#"return global.port"#, &ctx).unwrap();
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_lua_service_context() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext {
            service_name: Some("backend".to_string()),
            ..Default::default()
        };

        let result: String = eval.eval(r#"return ctx.service_name"#, &ctx).unwrap();
        assert_eq!(result, "backend");
    }

    #[test]
    fn test_lua_functions_available() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();

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
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext::default();

        let result: Value = eval.eval(r#"return {FOO="bar", BAZ="qux"}"#, &ctx).unwrap();
        let map = lua_table_to_env_map(result).unwrap();

        assert_eq!(map.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(map.get("BAZ"), Some(&"qux".to_string()));
    }

    #[test]
    fn test_lua_env_array_format() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
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
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext {
            service_name: Some("api".to_string()),
            hook_name: Some("on_start".to_string()),
            ..Default::default()
        };

        let result: String = eval.eval(r#"return ctx.hook_name"#, &ctx).unwrap();
        assert_eq!(result, "on_start");
    }

    #[test]
    fn test_lua_nil_for_missing_context() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
        let ctx = EvalContext::default();

        // service_name should be nil when not set
        let result: Value = eval.eval(r#"return ctx.service_name"#, &ctx).unwrap();
        assert!(matches!(result, Value::Nil));
    }

    #[test]
    fn test_lua_standard_library_available() {
        let eval = LuaEvaluator::new(&test_config_dir()).unwrap();
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
