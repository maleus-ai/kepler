//! Lua script processing for configuration files
//!
//! This module handles `!lua` tags in YAML configuration,
//! evaluating Lua code and converting results back to YAML values.

use std::collections::HashMap;
use std::path::Path;

use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

/// Process Lua scripts in the config value tree using the provided evaluator.
///
/// This function walks the kepler namespace to find and evaluate `!lua` tags.
/// The evaluator should already have the `lua:` block loaded.
///
/// Service-level `!lua` processing is deferred to `resolve_service()` at service start time.
pub fn process_lua_scripts(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    config_path: &Path,
    sys_env: &HashMap<String, String>,
) -> Result<()> {
    use serde_yaml::Value;

    // Process kepler namespace only (global hooks and logs).
    // Service-level !lua processing is deferred to resolve_service() at service start time.
    if let Value::Mapping(map) = value
        && let Some(Value::Mapping(kepler_map)) = map.get_mut(Value::String("kepler".to_string())) {
            // Process kepler.logs (with sys_env only, before services)
            if let Some(logs_value) = kepler_map.get_mut(Value::String("logs".to_string())) {
                let ctx = EvalContext {
                    sys_env: sys_env.clone(),
                    env_file: HashMap::new(),
                    env: sys_env.clone(),
                    service_name: None,
                    hook_name: None,
                    ..Default::default()
                };
                super::expand::evaluate_value_tree(logs_value, evaluator, &ctx, config_path, "kepler.logs")?;
            }

            // Process kepler.hooks
            if let Some(hooks_value) = kepler_map.get_mut(Value::String("hooks".to_string())) {
                process_global_hooks_lua(hooks_value, evaluator, sys_env, config_path)?;
            }
        }

    Ok(())
}

/// Process Lua tags and `${{ }}$` expressions in global hooks with proper hook_name context.
fn process_global_hooks_lua(
    hooks_value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    sys_env: &HashMap<String, String>,
    config_path: &Path,
) -> Result<()> {
    use serde_yaml::Value;

    let hooks_map = match hooks_value {
        Value::Mapping(map) => map,
        _ => return Ok(()),
    };

    // Process each hook type with its name in the context
    let hook_names: Vec<String> = hooks_map
        .keys()
        .filter_map(|k| k.as_str().map(String::from))
        .collect();

    for hook_name in hook_names {
        if let Some(hook_value) = hooks_map.get_mut(Value::String(hook_name.clone())) {
            let field_path = format!("kepler.hooks.{}", hook_name);
            let ctx = EvalContext {
                sys_env: sys_env.clone(),
                env_file: HashMap::new(),
                env: sys_env.clone(),
                service_name: None, // Global hooks have no service
                hook_name: Some(hook_name),
                ..Default::default()
            };
            super::expand::evaluate_value_tree(hook_value, evaluator, &ctx, config_path, &field_path)?;
        }
    }

    Ok(())
}

/// Convert a Lua value to a YAML value.
pub(crate) fn lua_to_yaml(lua_value: mlua::Value, config_path: &Path) -> Result<serde_yaml::Value> {
    use serde_yaml::Value;

    match lua_value {
        mlua::Value::Nil => Ok(Value::Null),
        mlua::Value::Boolean(b) => Ok(Value::Bool(b)),
        mlua::Value::Integer(i) => Ok(Value::Number(serde_yaml::Number::from(i))),
        mlua::Value::Number(n) => {
            // Convert float to YAML number
            Ok(Value::Number(serde_yaml::Number::from(n)))
        }
        mlua::Value::String(s) => {
            let s = s.to_str().map_err(|e| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: format!("Invalid UTF-8 string: {}", e),
            })?;
            Ok(Value::String(s.to_string()))
        }
        mlua::Value::Table(table) => {
            // Determine if it's an array or a map
            let mut is_array = true;
            let mut max_index = 0i32;
            let mut count = 0;

            for pair in table.clone().pairs::<mlua::Value, mlua::Value>() {
                let (k, _) = pair.map_err(|e| DaemonError::LuaError {
                    path: config_path.to_path_buf(),
                    message: format!("Error iterating table: {}", e),
                })?;
                count += 1;
                match k {
                    mlua::Value::Integer(i) if i > 0 => {
                        max_index = max_index.max(i);
                    }
                    _ => {
                        is_array = false;
                    }
                }
            }

            if count == 0 || (is_array && max_index as usize == count) {
                // It's an array (or empty table treated as empty array)
                let mut seq = Vec::new();
                for i in 1..=max_index {
                    let v: mlua::Value = table.get(i).map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error getting array element {}: {}", i, e),
                    })?;
                    seq.push(lua_to_yaml(v, config_path)?);
                }
                Ok(Value::Sequence(seq))
            } else {
                // It's a map
                let mut map = serde_yaml::Mapping::new();
                for pair in table.pairs::<mlua::Value, mlua::Value>() {
                    let (k, v) = pair.map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error iterating table: {}", e),
                    })?;

                    let key = match k {
                        mlua::Value::String(s) => {
                            let s = s.to_str().map_err(|e| DaemonError::LuaError {
                                path: config_path.to_path_buf(),
                                message: format!("Invalid UTF-8 key: {}", e),
                            })?;
                            Value::String(s.to_string())
                        }
                        mlua::Value::Integer(i) => Value::Number(serde_yaml::Number::from(i)),
                        _ => {
                            return Err(DaemonError::LuaError {
                                path: config_path.to_path_buf(),
                                message: format!(
                                    "Table key must be string or integer, got {:?}",
                                    k.type_name()
                                ),
                            });
                        }
                    };

                    map.insert(key, lua_to_yaml(v, config_path)?);
                }
                Ok(Value::Mapping(map))
            }
        }
        other => Err(DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: format!("Cannot convert Lua {} to YAML", other.type_name()),
        }),
    }
}
