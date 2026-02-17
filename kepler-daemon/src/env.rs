use std::collections::HashMap;
use std::path::Path;

use crate::config::{HookCommand, evaluate_expression_string};
use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

/// Load environment variables from a .env file
pub fn load_env_file(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Err(DaemonError::EnvFileNotFound(path.to_path_buf()));
    }

    let mut env = HashMap::new();

    // Use dotenvy to parse the file
    for item in dotenvy::from_path_iter(path).map_err(|e| DaemonError::EnvFileParse {
        path: path.to_path_buf(),
        source: e,
    })? {
        match item {
            Ok((key, value)) => {
                env.insert(key, value);
            }
            Err(e) => {
                return Err(DaemonError::EnvFileParse {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        }
    }

    Ok(env)
}

/// Insert KEY=VALUE entries from a string slice into a HashMap.
pub fn insert_env_entries(env: &mut HashMap<String, String>, entries: &[String]) {
    for entry in entries {
        if let Some((key, value)) = entry.split_once('=') {
            env.insert(key.to_string(), value.to_string());
        }
    }
}

/// Build environment for a hook, merging hook-specific env with base service env.
///
/// Hook environment entries are evaluated via `${{ }}` Lua expressions against
/// the accumulated env (base service env + hook env_file).
///
/// Priority (highest to lowest):
/// 1. Hook's environment variables (evaluated against base + env_file)
/// 2. Hook's env_file variables
/// 3. Base service environment (already includes service env and system env)
pub fn build_hook_env(
    hook: &HookCommand,
    base_env: &HashMap<String, String>,
    working_dir: &Path,
    lua_code: Option<&str>,
) -> Result<HashMap<String, String>> {
    let mut env = base_env.clone();

    // Load from hook's env_file if specified (path already expanded)
    if let Some(env_file_path) = hook.env_file() {
        let resolved_path = if env_file_path.is_relative() {
            working_dir.join(env_file_path)
        } else {
            env_file_path.to_path_buf()
        };

        if resolved_path.exists() {
            let hook_env_file_vars = load_env_file(&resolved_path)?;
            env.extend(hook_env_file_vars);
        }
    }

    // Evaluate ${{ }} in hook's environment entries against accumulated env
    let has_expressions = hook.environment().iter().any(|e| e.contains("${{"));

    if has_expressions {
        let evaluator = LuaEvaluator::new().map_err(|e| DaemonError::Internal(
            format!("Failed to create Lua evaluator for hook: {}", e),
        ))?;
        if let Some(code) = lua_code {
            evaluator.load_inline(code).map_err(|e| DaemonError::Internal(
                format!("Failed to load Lua code for hook: {}", e),
            ))?;
        }
        let eval_ctx = EvalContext {
            env: env.clone(),
            ..Default::default()
        };
        let config_path = std::path::PathBuf::from("<hook>");
        for entry in hook.environment() {
            let expanded = if entry.contains("${{") {
                evaluate_expression_string(entry, &evaluator, &eval_ctx, &config_path, "hook.environment")?
            } else {
                entry.to_string()
            };
            if let Some((key, value)) = expanded.split_once('=') {
                env.insert(key.to_string(), value.to_string());
            }
        }
    } else {
        for entry in hook.environment() {
            if let Some((key, value)) = entry.split_once('=') {
                env.insert(key.to_string(), value.to_string());
            }
        }
    }

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_env_entries() {
        let mut env = HashMap::new();
        let entries = vec![
            "FOO=bar".to_string(),
            "BAZ=qux".to_string(),
            "INVALID".to_string(), // no '=' — should be skipped
        ];
        insert_env_entries(&mut env, &entries);
        assert_eq!(env.get("FOO").unwrap(), "bar");
        assert_eq!(env.get("BAZ").unwrap(), "qux");
        assert!(!env.contains_key("INVALID"));
    }
}
