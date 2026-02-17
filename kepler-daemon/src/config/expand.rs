//! Expression evaluation for configuration files
//!
//! This module handles `${{ expr }}` inline Lua expression evaluation in
//! configuration values, and `!lua` tag evaluation, using a unified
//! `LuaEvaluator` + `EvalContext` system.
//!
//! `${{ expr }}` expressions are evaluated as Lua code via `eval_inline_expr`,
//! which provides `env`, `ctx`, `deps`, `global`, and stdlib access.
//! Unknown bare names resolve to nil (standard Lua behavior).
//!
//! Standalone `${{ expr }}` (the entire string) preserves the Lua return type
//! (bool, number, table, etc.). Embedded `${{ expr }}` in a larger string
//! coerces each result to a string.

use std::path::Path;

use super::SysEnvPolicy;
use crate::config::lua::lua_to_yaml;
use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

/// Resolve sys_env policy for a service.
/// Priority: service explicit setting > global setting > default (Inherit)
///
/// Uses `Option<SysEnvPolicy>` so we can properly distinguish between
/// "not specified" (`None`) and an explicit value (`Some(...)`).
pub fn resolve_sys_env(
    service_sys_env: Option<&SysEnvPolicy>,
    global_sys_env: Option<&SysEnvPolicy>,
) -> SysEnvPolicy {
    // Service explicit setting > Global > Default(Inherit)
    service_sys_env
        .or(global_sys_env)
        .cloned()
        .unwrap_or(SysEnvPolicy::Inherit)
}

/// Check if a string is a standalone `${{ expr }}` (the entire string is one expression).
fn is_standalone_expression(s: &str) -> bool {
    let trimmed = s.trim();
    if !trimmed.starts_with("${{") {
        return false;
    }
    // Find the closing `}}`
    let after_open = &trimmed[3..];
    if let Some(end) = after_open.find("}}") {
        // Check that nothing follows after the closing `}}`
        let remainder = &after_open[end + 2..];
        remainder.trim().is_empty()
    } else {
        false
    }
}

/// Extract the expression content from a standalone `${{ expr }}`.
fn extract_standalone_expr(s: &str) -> &str {
    let trimmed = s.trim();
    let after_open = &trimmed[3..];
    let end = after_open.find("}}").unwrap();
    after_open[..end].trim()
}

/// Evaluate all `${{ ... }}` expressions in a string, coercing results to strings.
///
/// Scans for `${{ ... }}`, evaluates each expression via `evaluator.eval_inline_expr()`,
/// coerces the Lua result to string, and interpolates into the result.
pub fn evaluate_expression_string(
    s: &str,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_path: &Path,
    chunk_name: &str,
) -> Result<String> {
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    // Build env table once and reuse for all expressions in this string
    let env_table = evaluator
        .prepare_env(ctx)
        .map_err(|e| DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: format!("Error building Lua environment: {}", e),
        })?;

    while let Some(start) = remaining.find("${{") {
        // Append everything before the marker
        result.push_str(&remaining[..start]);

        let after_open = &remaining[start + 3..];
        if let Some(end) = after_open.find("}}") {
            let expr = after_open[..end].trim();
            let lua_result = evaluator
                .eval_inline_expr_with_env(expr, &env_table, chunk_name)
                .map_err(|e| DaemonError::LuaError {
                    path: config_path.to_path_buf(),
                    message: format!("Error evaluating ${{{{ {} }}}}: {}", expr, e),
                })?;
            let string_value = lua_value_to_string(&lua_result);
            result.push_str(&string_value);
            remaining = &after_open[end + 2..];
        } else {
            // No closing `}}` — treat as literal
            result.push_str("${{");
            remaining = after_open;
        }
    }

    // Append anything left after the last match
    result.push_str(remaining);
    Ok(result)
}

/// Convert a Lua value to a string representation for interpolation.
fn lua_value_to_string(value: &mlua::Value) -> String {
    match value {
        mlua::Value::Nil => String::new(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        mlua::Value::String(s) => s.to_string_lossy().to_string(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Evaluate all `${{ }}` expressions and `!lua` tags in a YAML value tree in-place.
///
/// Single pass replacing both the old `expand_value_tree` and `process_lua_tags_recursive`:
///
/// - `Value::String` exactly `${{ expr }}` (standalone) → eval Lua → replace Value with typed result
/// - `Value::String` with embedded `${{ expr }}` → eval each, interpolate as string
/// - `Value::Tagged(!lua)` → eval Lua code → replace Value (same as old behavior)
/// - `Value::Mapping/Sequence` → recurse, skip `depends_on`
pub fn evaluate_value_tree(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_path: &Path,
    field_path: &str,
) -> Result<()> {
    let mut cached_env: Option<mlua::Table> = None;
    evaluate_value_tree_inner(value, evaluator, ctx, config_path, field_path, false, &mut cached_env)
}

fn evaluate_value_tree_inner(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_path: &Path,
    field_path: &str,
    in_depends_on: bool,
    cached_env: &mut Option<mlua::Table>,
) -> Result<()> {
    use serde_yaml::Value;

    match &*value {
        Value::String(s) if !in_depends_on && s.contains("${{") => {
            // Lazily build env table on first dynamic expression
            let env_table = match cached_env {
                Some(t) => t.clone(),
                None => {
                    let t = evaluator.prepare_env(ctx).map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error building Lua environment: {}", e),
                    })?;
                    *cached_env = Some(t.clone());
                    t
                }
            };

            if is_standalone_expression(s) {
                // Standalone: evaluate and preserve Lua type
                let expr = extract_standalone_expr(s);
                let lua_result = evaluator
                    .eval_inline_expr_with_env(expr, &env_table, field_path)
                    .map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error evaluating ${{{{ {} }}}}: {}", expr, e),
                    })?;
                *value = lua_to_yaml(lua_result, config_path)?;
            } else {
                // Embedded: interpolate as string (reuses env_table internally)
                let expanded =
                    evaluate_expression_string(s, evaluator, ctx, config_path, field_path)?;
                *value = Value::String(expanded);
            }
        }
        Value::Tagged(tagged) if tagged.tag == "!lua" => {
            // Evaluate !lua tag
            let code = tagged.value.as_str().ok_or_else(|| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: "!lua value must be a string".to_string(),
            })?;
            let lua_result: mlua::Value =
                evaluator
                    .eval(code, ctx, field_path)
                    .map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Lua error: {}", e),
                    })?;
            *value = lua_to_yaml(lua_result, config_path)?;
        }
        Value::Tagged(_) => {
            // Non-!lua tag: recurse into the tagged value
            if let Value::Tagged(tagged) = value {
                evaluate_value_tree_inner(
                    &mut tagged.value,
                    evaluator,
                    ctx,
                    config_path,
                    field_path,
                    in_depends_on,
                    cached_env,
                )?;
            }
        }
        Value::Mapping(_) => {
            if let Value::Mapping(map) = value {
                let keys: Vec<serde_yaml::Value> = map.keys().cloned().collect();
                for key in keys {
                    let is_depends_on = key.as_str() == Some("depends_on");
                    let child_path = if let Some(key_str) = key.as_str() {
                        if field_path.is_empty() {
                            key_str.to_string()
                        } else {
                            format!("{}.{}", field_path, key_str)
                        }
                    } else {
                        field_path.to_string()
                    };
                    if let Some(v) = map.get_mut(&key) {
                        evaluate_value_tree_inner(
                            v,
                            evaluator,
                            ctx,
                            config_path,
                            &child_path,
                            is_depends_on,
                            cached_env,
                        )?;
                    }
                }
            }
        }
        Value::Sequence(_) => {
            if let Value::Sequence(seq) = value {
                for item in seq.iter_mut() {
                    evaluate_value_tree_inner(
                        item,
                        evaluator,
                        ctx,
                        config_path,
                        field_path,
                        in_depends_on,
                        cached_env,
                    )?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

/// Evaluate `${{ }}` in environment entries sequentially, building up context as we go.
///
/// Each entry's key=value is added to `ctx.env` after evaluation,
/// so later entries can reference earlier ones.
pub fn evaluate_environment_sequential(
    env_value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &mut EvalContext,
    config_path: &Path,
    service_name: &str,
) -> Result<()> {
    use serde_yaml::Value;

    if let Value::Sequence(seq) = env_value {
        for item in seq.iter_mut() {
            if let Value::String(s) = item {
                if s.contains("${{") {
                    let chunk_name = format!("{}.environment", service_name);
                    *s = evaluate_expression_string(s, evaluator, ctx, config_path, &chunk_name)?;
                }
                // Add to context for subsequent entries
                if let Some((key, value)) = s.split_once('=') {
                    ctx.env.insert(key.to_string(), value.to_string());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_evaluator_and_ctx() -> (LuaEvaluator, EvalContext) {
        let eval = LuaEvaluator::new().unwrap();
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/home/user".to_string());
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("DB_HOST".to_string(), "localhost".to_string());
        env.insert("APP_PORT".to_string(), "8080".to_string());
        let ctx = EvalContext {
            env,
            ..Default::default()
        };
        (eval, ctx)
    }

    fn test_path() -> PathBuf {
        PathBuf::from("/test/config.yaml")
    }

    #[test]
    fn test_basic_lookup() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        assert_eq!(
            evaluate_expression_string("${{ env.HOME }}", &eval, &ctx, &path, "test").unwrap(),
            "/home/user"
        );
        assert_eq!(
            evaluate_expression_string("${{ env.DB_HOST }}", &eval, &ctx, &path, "test").unwrap(),
            "localhost"
        );
        assert_eq!(
            evaluate_expression_string("${{ env.APP_PORT }}", &eval, &ctx, &path, "test").unwrap(),
            "8080"
        );
    }

    #[test]
    fn test_missing_var_nil_to_empty() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        assert_eq!(
            evaluate_expression_string("${{ env.NONEXISTENT }}", &eval, &ctx, &path, "test")
                .unwrap(),
            ""
        );
        assert_eq!(
            evaluate_expression_string(
                "prefix_${{ env.NONEXISTENT }}_suffix",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            "prefix__suffix"
        );
    }

    #[test]
    fn test_or_default_syntax() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        // Set variable — should use set value
        assert_eq!(
            evaluate_expression_string(
                "${{ env.HOME or '/default' }}",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            "/home/user"
        );
        // Unset variable — should use default
        assert_eq!(
            evaluate_expression_string(
                "${{ env.MISSING or 'fallback' }}",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            "fallback"
        );
    }

    #[test]
    fn test_deps_status() {
        let eval = LuaEvaluator::new().unwrap();
        let mut deps = HashMap::new();
        deps.insert(
            "db".to_string(),
            crate::lua_eval::DepInfo {
                status: "healthy".to_string(),
                exit_code: None,
                initialized: true,
                restart_count: 2,
                ..Default::default()
            },
        );
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        let path = test_path();

        assert_eq!(
            evaluate_expression_string("${{ deps.db.status }}", &eval, &ctx, &path, "test")
                .unwrap(),
            "healthy"
        );
        assert_eq!(
            evaluate_expression_string("${{ deps.db.exit_code }}", &eval, &ctx, &path, "test")
                .unwrap(),
            ""
        );
        assert_eq!(
            evaluate_expression_string("${{ deps.db.initialized }}", &eval, &ctx, &path, "test")
                .unwrap(),
            "true"
        );
        assert_eq!(
            evaluate_expression_string(
                "${{ deps.db.restart_count }}",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            "2"
        );
    }

    #[test]
    fn test_deps_env_var() {
        let eval = LuaEvaluator::new().unwrap();
        let mut dep_env = HashMap::new();
        dep_env.insert("MY_VAR".to_string(), "hello".to_string());
        let mut deps = HashMap::new();
        deps.insert(
            "setup".to_string(),
            crate::lua_eval::DepInfo {
                env: dep_env,
                ..Default::default()
            },
        );
        let ctx = EvalContext {
            deps,
            ..Default::default()
        };
        let path = test_path();

        assert_eq!(
            evaluate_expression_string(
                "${{ deps.setup.env.MY_VAR }}",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            "hello"
        );
        assert_eq!(
            evaluate_expression_string(
                "${{ deps.setup.env.MISSING }}",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn test_single_brace_syntax_treated_as_literal() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        // ${VAR} is not a valid expression — only ${{ }} is
        assert_eq!(
            evaluate_expression_string("${HOME}", &eval, &ctx, &path, "test").unwrap(),
            "${HOME}"
        );
    }

    #[test]
    fn test_multiple_expressions() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        assert_eq!(
            evaluate_expression_string(
                "${{ env.HOME }}/app:${{ env.APP_PORT }}",
                &eval,
                &ctx,
                &path,
                "test"
            )
            .unwrap(),
            "/home/user/app:8080"
        );
    }

    #[test]
    fn test_no_closing_braces() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        // Unclosed ${{ should be treated as literal
        assert_eq!(
            evaluate_expression_string("${{env.HOME", &eval, &ctx, &path, "test").unwrap(),
            "${{env.HOME"
        );
    }

    #[test]
    fn test_evaluate_value_tree_basic() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        let mut value = serde_yaml::Value::String("${{ env.HOME }}/app".to_string());
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();
        assert_eq!(value.as_str().unwrap(), "/home/user/app");
    }

    #[test]
    fn test_evaluate_value_tree_skips_depends_on() {
        let (eval, ctx) = make_evaluator_and_ctx();
        let path = test_path();
        let yaml = r#"
depends_on:
  - db
  - cache
command: ["${{ env.HOME }}/app"]
"#;
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();

        // depends_on should NOT be expanded (literal)
        let deps = value["depends_on"].as_sequence().unwrap();
        assert_eq!(deps[0].as_str().unwrap(), "db");

        // command should be expanded
        let cmd = value["command"].as_sequence().unwrap();
        assert_eq!(cmd[0].as_str().unwrap(), "/home/user/app");
    }

    #[test]
    fn test_sequential_environment_evaluation() {
        let eval = LuaEvaluator::new().unwrap();
        let mut env = HashMap::new();
        env.insert("BASE".to_string(), "hello".to_string());
        let mut ctx = EvalContext {
            env,
            ..Default::default()
        };
        let path = test_path();

        let yaml = r#"
- FIRST=${{ env.BASE }}_world
- SECOND=${{ env.FIRST }}_again
"#;
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        evaluate_environment_sequential(&mut value, &eval, &mut ctx, &path, "test").unwrap();

        let seq = value.as_sequence().unwrap();
        assert_eq!(seq[0].as_str().unwrap(), "FIRST=hello_world");
        assert_eq!(seq[1].as_str().unwrap(), "SECOND=hello_world_again");
    }

    #[test]
    fn test_standalone_expression_type_preservation() {
        let eval = LuaEvaluator::new().unwrap();
        let ctx = EvalContext::default();
        let path = test_path();

        // Standalone bool
        let mut value = serde_yaml::Value::String("${{ true }}".to_string());
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();
        assert_eq!(value, serde_yaml::Value::Bool(true));

        // Standalone number
        let mut value = serde_yaml::Value::String("${{ 42 }}".to_string());
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();
        assert_eq!(
            value,
            serde_yaml::Value::Number(serde_yaml::Number::from(42))
        );

        // Standalone table → sequence
        let mut value = serde_yaml::Value::String("${{ {1, 2, 3} }}".to_string());
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();
        assert!(value.is_sequence());
    }

    #[test]
    fn test_lua_tag_in_tree() {
        let eval = LuaEvaluator::new().unwrap();
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "8080".to_string());
        let ctx = EvalContext {
            env,
            ..Default::default()
        };
        let path = test_path();

        let yaml = r#"
command: !lua 'return {"echo", ctx.env.PORT}'
"#;
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();

        let cmd = value["command"].as_sequence().unwrap();
        assert_eq!(cmd[0].as_str().unwrap(), "echo");
        assert_eq!(cmd[1].as_str().unwrap(), "8080");
    }

    #[test]
    fn test_bare_var_in_inline_resolves_to_nil() {
        let eval = LuaEvaluator::new().unwrap();
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/home/user".to_string());
        let ctx = EvalContext {
            env,
            ..Default::default()
        };
        let path = test_path();

        // Bare variable names resolve to nil (empty string in embedded context)
        let mut value = serde_yaml::Value::String("prefix_${{ HOME }}_suffix".to_string());
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();
        assert_eq!(value.as_str().unwrap(), "prefix__suffix");

        // Standalone bare var resolves to nil
        let mut value = serde_yaml::Value::String("${{ HOME }}".to_string());
        evaluate_value_tree(&mut value, &eval, &ctx, &path, "test").unwrap();
        assert!(value.is_null(), "Standalone bare var should resolve to nil");
    }

    #[test]
    fn test_is_standalone_expression() {
        assert!(is_standalone_expression("${{ env.HOME }}"));
        assert!(is_standalone_expression("  ${{ 42 }}  "));
        assert!(!is_standalone_expression("prefix ${{ env.HOME }}"));
        assert!(!is_standalone_expression("${{ env.HOME }} suffix"));
        assert!(!is_standalone_expression("${{ a }} ${{ b }}"));
        assert!(!is_standalone_expression("no expression"));
    }
}
