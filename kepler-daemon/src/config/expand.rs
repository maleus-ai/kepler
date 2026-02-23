//! Expression evaluation for configuration files
//!
//! This module handles `${{ expr }}$` inline Lua expression evaluation in
//! configuration values, and `!lua` tag evaluation, using a unified
//! `LuaEvaluator` + `EvalContext` system.
//!
//! `${{ expr }}$` expressions are evaluated as Lua code via `eval_inline_expr`,
//! which provides `env`, `service`/`hook`, `deps`, `global`, and stdlib access.
//! Unknown bare names resolve to nil (standard Lua behavior).
//!
//! Standalone `${{ expr }}$` (the entire string) preserves the Lua return type
//! (bool, number, table, etc.). Embedded `${{ expr }}$` in a larger string
//! coerces each result to a string.

use std::path::Path;

use crate::config::lua::lua_to_yaml;
use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

/// Resolve inherit_env for a service/hook/healthcheck.
/// Priority: local > service > global > default (true)
pub fn resolve_inherit_env(
    local: Option<bool>,
    service: Option<bool>,
    global: Option<bool>,
) -> bool {
    local.or(service).or(global).unwrap_or(true)
}

// ============================================================================
// ExprToken — tokenizer for `${{ expr }}$` expressions
// ============================================================================

/// A token from parsing a `${{ expr }}$` expression string.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprToken<'a> {
    /// Literal text outside any `${{ }}$` marker.
    String(&'a str),
    /// Lua expression content (trimmed text between `${{` and `}}$`).
    Expression(&'a str),
}

/// Parse a string into a sequence of literal and expression tokens.
///
/// Scans for `${{` opening markers and `}}$` closing markers.
/// The `}}$` closing delimiter is not valid Lua syntax, so there is
/// no ambiguity with nested braces in Lua table constructors.
///
/// An unclosed `${{` (no matching `}}$`) is emitted as literal text.
pub fn parse_expr_tokens(s: &str) -> Vec<ExprToken<'_>> {
    let mut tokens = Vec::new();
    let mut remaining = s;
    let mut offset = 0;

    while let Some(start) = remaining.find("${{") {
        // Emit literal text before the marker
        if start > 0 {
            tokens.push(ExprToken::String(&s[offset..offset + start]));
        }

        let after_open = &remaining[start + 3..];
        if let Some(end) = after_open.find("}}$") {
            let expr = after_open[..end].trim();
            tokens.push(ExprToken::Expression(expr));
            let skip = start + 3 + end + 3; // "${{" + content + "}}$"
            offset += skip;
            remaining = &s[offset..];
        } else {
            // No closing `}}$` — emit "${{" as literal and continue scanning
            tokens.push(ExprToken::String(&s[offset..offset + start + 3]));
            offset += start + 3;
            remaining = &s[offset..];
        }
    }

    // Emit trailing literal text
    if offset < s.len() {
        tokens.push(ExprToken::String(&s[offset..]));
    }

    tokens
}

/// Check if tokens represent a standalone expression (single `${{ expr }}$`
/// with only optional surrounding whitespace).
pub fn is_standalone_tokens(tokens: &[ExprToken<'_>]) -> bool {
    let mut found_expr = false;
    for token in tokens {
        match token {
            ExprToken::Expression(_) => {
                if found_expr {
                    return false; // multiple expressions
                }
                found_expr = true;
            }
            ExprToken::String(s) => {
                if !s.trim().is_empty() {
                    return false; // non-whitespace literal
                }
            }
        }
    }
    found_expr
}

/// Extract the expression from standalone tokens.
/// Panics if the tokens are not standalone.
pub fn extract_expr_from_standalone<'a>(tokens: &[ExprToken<'a>]) -> &'a str {
    for token in tokens {
        if let ExprToken::Expression(expr) = token {
            return expr;
        }
    }
    unreachable!("extract_expr_from_standalone called on non-standalone tokens")
}

// ============================================================================
// Legacy helpers — thin wrappers used by tests
// ============================================================================

/// Check if a string is a standalone `${{ expr }}$` (the entire string is one expression).
#[cfg(test)]
fn is_standalone_expression(s: &str) -> bool {
    is_standalone_tokens(&parse_expr_tokens(s))
}

// ============================================================================
// Expression evaluation
// ============================================================================

/// Shared implementation: tokenize, evaluate expressions, interpolate into a string.
fn evaluate_tokens_to_string(
    s: &str,
    evaluator: &LuaEvaluator,
    env_table: &mlua::Table,
    config_path: &Path,
    chunk_name: &str,
) -> Result<String> {
    let tokens = parse_expr_tokens(s);
    let mut result = String::with_capacity(s.len());

    for token in &tokens {
        match token {
            ExprToken::String(text) => result.push_str(text),
            ExprToken::Expression(expr) => {
                let lua_result = evaluator
                    .eval_inline_expr_with_env(expr, env_table, chunk_name)
                    .map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error evaluating ${{{{ {} }}}}$: {}", expr, e),
                    })?;
                result.push_str(&lua_value_to_string(&lua_result));
            }
        }
    }

    Ok(result)
}

/// Evaluate all `${{ ... }}$` expressions in a string, coercing results to strings.
///
/// Scans for `${{ ... }}$`, evaluates each expression via `evaluator.eval_inline_expr()`,
/// coerces the Lua result to string, and interpolates into the result.
pub fn evaluate_expression_string(
    s: &str,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_path: &Path,
    chunk_name: &str,
) -> Result<String> {
    let env_table = evaluator
        .prepare_env(ctx)
        .map_err(|e| DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: format!("Error building Lua environment: {}", e),
        })?;
    evaluate_tokens_to_string(s, evaluator, &env_table, config_path, chunk_name)
}

/// Evaluate all `${{ ... }}$` expressions in a string, reusing a pre-built env table.
///
/// Like `evaluate_expression_string` but avoids rebuilding the Lua environment
/// table. Used when a cached env table is already available.
pub fn evaluate_expression_string_with_env(
    s: &str,
    evaluator: &LuaEvaluator,
    env_table: &mlua::Table,
    config_path: &Path,
    chunk_name: &str,
) -> Result<String> {
    evaluate_tokens_to_string(s, evaluator, env_table, config_path, chunk_name)
}

/// Convert a Lua value to a string representation for interpolation.
pub(crate) fn lua_value_to_string(value: &mlua::Value) -> String {
    match value {
        mlua::Value::Nil => String::new(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        mlua::Value::String(s) => s.to_string_lossy().to_string(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Evaluate all `${{ }}$` expressions and `!lua` tags in a YAML value tree in-place.
///
/// Single pass replacing both the old `expand_value_tree` and `process_lua_tags_recursive`:
///
/// - `Value::String` exactly `${{ expr }}$` (standalone) → eval Lua → replace Value with typed result
/// - `Value::String` with embedded `${{ expr }}$` → eval each, interpolate as string
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

/// Like `evaluate_value_tree` but accepts an externalized `cached_env` to share
/// a single Lua env table across multiple calls.
pub fn evaluate_value_tree_with_env(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_path: &Path,
    field_path: &str,
    cached_env: &mut Option<mlua::Table>,
) -> Result<()> {
    evaluate_value_tree_inner(value, evaluator, ctx, config_path, field_path, false, cached_env)
}

fn evaluate_value_tree_inner(
    value: &mut serde_yaml::Value,
    evaluator: &LuaEvaluator,
    ctx: &EvalContext,
    config_path: &Path,
    field_path: &str,
    in_deferred_field: bool,
    cached_env: &mut Option<mlua::Table>,
) -> Result<()> {
    use serde_yaml::Value;

    match &*value {
        Value::String(s) if !in_deferred_field && s.contains("${{") => {
            let env_table = super::get_or_build_env(evaluator, ctx, config_path, cached_env)?;

            let tokens = parse_expr_tokens(s);
            if is_standalone_tokens(&tokens) {
                // Standalone: evaluate and preserve Lua type
                let expr = extract_expr_from_standalone(&tokens);
                let lua_result = evaluator
                    .eval_inline_expr_with_env(expr, &env_table, field_path)
                    .map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error evaluating ${{{{ {} }}}}$: {}", expr, e),
                    })?;
                *value = lua_to_yaml(lua_result, config_path)?;
            } else {
                // Embedded: interpolate as string using the cached env table
                let expanded =
                    evaluate_expression_string_with_env(s, evaluator, &env_table, config_path, field_path)?;
                *value = Value::String(expanded);
            }
        }
        Value::Tagged(tagged) if tagged.tag == "!lua" => {
            // Evaluate !lua tag — use cached env table if available
            let code = tagged.value.as_str().ok_or_else(|| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: "!lua value must be a string".to_string(),
            })?;

            let env_table = super::get_or_build_env(evaluator, ctx, config_path, cached_env)?;

            let lua_result: mlua::Value =
                evaluator
                    .eval_with_env(code, &env_table, field_path)
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
                    in_deferred_field,
                    cached_env,
                )?;
            }
        }
        Value::Mapping(_) => {
            if let Value::Mapping(map) = value {
                let keys: Vec<serde_yaml::Value> = map.keys().cloned().collect();
                for key in keys {
                    let is_deferred = key.as_str() == Some("depends_on")
                        || key.as_str() == Some("outputs");
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
                            is_deferred,
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
                        in_deferred_field,
                        cached_env,
                    )?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}


#[cfg(test)]
mod tests;
