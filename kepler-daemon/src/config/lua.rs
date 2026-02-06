//! Lua script processing for configuration files
//!
//! This module handles !lua and !lua_file tags in YAML configuration,
//! evaluating Lua code and converting results back to YAML values.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

use super::load_env_file;

/// Process Lua scripts in the config value tree.
///
/// This function:
/// 1. Extracts and loads the `lua:` block
/// 2. Walks the value tree to find and evaluate `!lua` and `!lua_file` tags
/// 3. Replaces tagged values with their Lua evaluation results
pub fn process_lua_scripts(
    value: &mut serde_yaml::Value,
    config_dir: &Path,
    config_path: &Path,
    sys_env: &HashMap<String, String>,
) -> Result<()> {
    use serde_yaml::Value;

    // Extract lua: from the root mapping
    let lua_code = if let Value::Mapping(map) = &*value {
        map.get(Value::String("lua".to_string()))
            .and_then(|v| v.as_str())
            .map(String::from)
    } else {
        None
    };

    // Create Lua evaluator and load the code
    let evaluator = LuaEvaluator::new(config_dir).map_err(|e| DaemonError::LuaError {
        path: config_path.to_path_buf(),
        message: e.to_string(),
    })?;

    // Load lua: block
    if let Some(ref code) = lua_code {
        evaluator.load_inline(code).map_err(|e| DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: format!("Error in lua: block: {}", e),
        })?;
    }

    // Now process the services
    if let Value::Mapping(map) = value {
        if let Some(Value::Mapping(services)) = map.get_mut(Value::String("services".to_string())) {
            // Collect service names first to avoid borrowing issues
            let service_names: Vec<String> = services
                .keys()
                .filter_map(|k| k.as_str().map(String::from))
                .collect();

            for service_name in service_names {
                if let Some(service_value) = services.get_mut(Value::String(service_name.clone())) {
                    process_service_lua(
                        service_value,
                        &service_name,
                        &evaluator,
                        config_dir,
                        config_path,
                        sys_env,
                    )?;
                }
            }
        }

        // Process kepler namespace (global hooks and logs)
        if let Some(Value::Mapping(kepler_map)) = map.get_mut(Value::String("kepler".to_string())) {
            // Process kepler.logs (with sys_env only, before services)
            if let Some(logs_value) = kepler_map.get_mut(Value::String("logs".to_string())) {
                let ctx = EvalContext {
                    sys_env: sys_env.clone(),
                    env_file: HashMap::new(),
                    env: sys_env.clone(),
                    service_name: None,
                    hook_name: None,
                };
                process_lua_tags_recursive(logs_value, &evaluator, &ctx, config_dir, config_path)?;
            }

            // Process kepler.hooks
            if let Some(hooks_value) = kepler_map.get_mut(Value::String("hooks".to_string())) {
                process_global_hooks_lua(hooks_value, &evaluator, sys_env, config_dir, config_path)?;
            }
        }
    }

    Ok(())
}

/// Process Lua tags within a service configuration.
///
/// Follows the order specified in the design:
/// 1. Evaluate env_file with ctx.sys_env only
/// 2. Load the resulting env_file into ctx.env_file
/// 3. Evaluate environment with ctx.sys_env + ctx.env_file available
/// 4. Evaluate all other fields with full ctx.env
fn process_service_lua(
    service_value: &mut serde_yaml::Value,
    service_name: &str,
    evaluator: &LuaEvaluator,
    config_dir: &Path,
    config_path: &Path,
    sys_env: &HashMap<String, String>,
) -> Result<()> {
    use serde_yaml::Value;

    let service_map = match service_value {
        Value::Mapping(map) => map,
        _ => return Ok(()),
    };

    // Step 2: Evaluate env_file if it's a Lua tag (only sys_env available)
    if let Some(env_file_value) = service_map.get_mut(Value::String("env_file".to_string())) {
        let ctx = EvalContext {
            sys_env: sys_env.clone(),
            env_file: HashMap::new(),
            env: sys_env.clone(), // At this point, only sys_env is available
            service_name: Some(service_name.to_string()),
            hook_name: None,
        };
        process_single_lua_tag(env_file_value, evaluator, &ctx, config_dir, config_path)?;
    }

    // Step 3: Load env_file content if specified (into ctx.env_file)
    let env_file_vars = if let Some(Value::String(env_file_path)) =
        service_map.get(Value::String("env_file".to_string()))
    {
        let path = if PathBuf::from(env_file_path).is_relative() {
            config_dir.join(env_file_path)
        } else {
            PathBuf::from(env_file_path)
        };
        load_env_file(&path)
    } else {
        HashMap::new()
    };

    // Build intermediate env (sys_env + env_file) for environment evaluation
    let mut env_for_environment = sys_env.clone();
    for (k, v) in &env_file_vars {
        env_for_environment.insert(k.clone(), v.clone());
    }

    // Step 4: Evaluate environment if it's a Lua tag (sys_env + env_file available)
    if let Some(environment_value) = service_map.get_mut(Value::String("environment".to_string())) {
        let ctx = EvalContext {
            sys_env: sys_env.clone(),
            env_file: env_file_vars.clone(),
            env: env_for_environment.clone(),
            service_name: Some(service_name.to_string()),
            hook_name: None,
        };
        process_single_lua_tag(environment_value, evaluator, &ctx, config_dir, config_path)?;
    }

    // Step 5: Merge environment array into full env (sys_env + env_file + environment)
    let mut full_env = env_for_environment;
    if let Some(Value::Sequence(environment)) =
        service_map.get(Value::String("environment".to_string()))
    {
        for entry in environment {
            if let Value::String(s) = entry
                && let Some((key, value)) = s.split_once('=') {
                    full_env.insert(key.to_string(), value.to_string());
                }
        }
    }

    // Step 6: Evaluate all other Lua tags with full context
    let ctx = EvalContext {
        sys_env: sys_env.clone(),
        env_file: env_file_vars,
        env: full_env,
        service_name: Some(service_name.to_string()),
        hook_name: None,
    };

    // Process all fields except env_file and environment (already done)
    for (key, field_value) in service_map.iter_mut() {
        let key_str = key.as_str().unwrap_or("");
        if key_str != "env_file" && key_str != "environment" {
            // Special handling for hooks - pass hook_name context
            if key_str == "hooks" {
                process_service_hooks_lua(
                    field_value,
                    service_name,
                    evaluator,
                    &ctx,
                    config_dir,
                    config_path,
                )?;
            } else {
                process_lua_tags_recursive(field_value, evaluator, &ctx, config_dir, config_path)?;
            }
        }
    }

    Ok(())
}

/// Process Lua tags in service hooks with proper hook_name context.
fn process_service_hooks_lua(
    hooks_value: &mut serde_yaml::Value,
    service_name: &str,
    evaluator: &LuaEvaluator,
    base_ctx: &EvalContext,
    config_dir: &Path,
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
            let ctx = EvalContext {
                sys_env: base_ctx.sys_env.clone(),
                env_file: base_ctx.env_file.clone(),
                env: base_ctx.env.clone(),
                service_name: Some(service_name.to_string()),
                hook_name: Some(hook_name),
            };
            process_lua_tags_recursive(hook_value, evaluator, &ctx, config_dir, config_path)?;
        }
    }

    Ok(())
}

/// Process Lua tags in global hooks with proper hook_name context.
fn process_global_hooks_lua(
    hooks_value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    sys_env: &HashMap<String, String>,
    config_dir: &Path,
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
            let ctx = EvalContext {
                sys_env: sys_env.clone(),
                env_file: HashMap::new(),
                env: sys_env.clone(),
                service_name: None, // Global hooks have no service
                hook_name: Some(hook_name),
            };
            process_lua_tags_recursive(hook_value, evaluator, &ctx, config_dir, config_path)?;
        }
    }

    Ok(())
}

/// Process a single value that might be a Lua tag.
fn process_single_lua_tag(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_dir: &Path,
    config_path: &Path,
) -> Result<()> {
    use serde_yaml::Value;

    if let Value::Tagged(tagged) = &*value {
        let tag = tagged.tag.to_string();
        if tag == "!lua" || tag == "!lua_file" {
            let result =
                evaluate_lua_tag(&tag, &tagged.value, evaluator, ctx, config_dir, config_path)?;
            *value = result;
        }
    }

    Ok(())
}

/// Recursively process Lua tags in a value tree.
fn process_lua_tags_recursive(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_dir: &Path,
    config_path: &Path,
) -> Result<()> {
    use serde_yaml::Value;

    match &*value {
        Value::Tagged(tagged) => {
            let tag = tagged.tag.to_string();
            if tag == "!lua" || tag == "!lua_file" {
                let result = evaluate_lua_tag(
                    &tag,
                    &tagged.value,
                    evaluator,
                    ctx,
                    config_dir,
                    config_path,
                )?;
                *value = result;
            }
        }
        Value::Mapping(_) => {
            // We need to get mutable access after matching
            if let Value::Mapping(map) = value {
                let keys: Vec<serde_yaml::Value> = map.keys().cloned().collect();
                for key in keys {
                    if let Some(v) = map.get_mut(&key) {
                        process_lua_tags_recursive(v, evaluator, ctx, config_dir, config_path)?;
                    }
                }
            }
        }
        Value::Sequence(_) => {
            if let Value::Sequence(seq) = value {
                for item in seq.iter_mut() {
                    process_lua_tags_recursive(item, evaluator, ctx, config_dir, config_path)?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

/// Evaluate a Lua tag and return the resulting YAML value.
fn evaluate_lua_tag(
    tag: &str,
    code_value: &serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_dir: &Path,
    config_path: &Path,
) -> Result<serde_yaml::Value> {
    let code = if tag == "!lua_file" {
        // For !lua_file, the value is a path to a Lua file
        let file_path = code_value.as_str().ok_or_else(|| DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: "!lua_file value must be a string path".to_string(),
        })?;

        let full_path = if PathBuf::from(file_path).is_relative() {
            config_dir.join(file_path)
        } else {
            PathBuf::from(file_path)
        };

        std::fs::read_to_string(&full_path).map_err(|e| DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: format!("Failed to read {}: {}", full_path.display(), e),
        })?
    } else {
        // For !lua, the value is inline Lua code
        code_value
            .as_str()
            .ok_or_else(|| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: "!lua value must be a string".to_string(),
            })?
            .to_string()
    };

    // Evaluate the Lua code
    let result: mlua::Value = evaluator.eval(&code, ctx).map_err(|e| DaemonError::LuaError {
        path: config_path.to_path_buf(),
        message: format!("Lua error: {}", e),
    })?;

    // Convert Lua result to YAML value
    lua_to_yaml(result, config_path)
}

/// Convert a Lua value to a YAML value.
fn lua_to_yaml(lua_value: mlua::Value, config_path: &Path) -> Result<serde_yaml::Value> {
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
