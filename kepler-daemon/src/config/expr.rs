//! Pre-parsed dynamic expression types for `ConfigValue::Dynamic`.
//!
//! At deserialization time, strings containing `${{ }}$` or `!lua` tags are
//! parsed into a structured `DynamicExpr` instead of being stored as raw
//! `serde_yaml::Value`. This avoids the tree-walking bug where a single
//! inner `${{ }}$` causes entire parent mappings/sequences to become Dynamic.

use std::path::Path;

use crate::config::lua::lua_to_yaml;
use crate::errors::{DaemonError, Result};
use crate::lua_eval::LuaEvaluator;

use super::expand::{
    ExprToken, extract_expr_from_standalone, is_standalone_tokens, lua_value_to_string,
    parse_expr_tokens,
};

/// Pre-parsed dynamic expression, stored at config load time.
#[derive(Debug, Clone)]
pub enum DynamicExpr {
    /// Standalone `${{ expr }}$` — single expression, preserves Lua return type.
    /// Can resolve to any type T (number, bool, string, table → struct, etc.)
    Expression(String),
    /// Interpolated string with mixed literals and expressions.
    /// Always resolves to String (concatenated).
    Interpolated(Vec<OwnedExprToken>),
    /// `!lua` block — evaluates Lua code, returns any type.
    Lua(String),
}

/// Owned version of ExprToken for storage in DynamicExpr.
#[derive(Debug, Clone)]
pub enum OwnedExprToken {
    Literal(String),
    Expression(String),
}

impl DynamicExpr {
    /// Classify a serde_yaml::Value as a DynamicExpr, if it contains dynamic content.
    ///
    /// Returns `Some(DynamicExpr)` for `!lua` tags and strings containing `${{ }}$`.
    /// Returns `None` for everything else (mappings, sequences, numbers, bools, plain strings).
    ///
    /// Mappings and sequences are always `None` — their inner fields are deserialized
    /// individually as `ConfigValue<T>` which handles dynamic content per-field.
    pub fn classify(value: &serde_yaml::Value) -> Option<DynamicExpr> {
        use serde_yaml::Value;
        match value {
            Value::Tagged(t) if t.tag == "!lua" => {
                t.value.as_str().map(|s| DynamicExpr::Lua(s.to_string()))
            }
            Value::String(s) if s.contains("${{") => {
                let tokens = parse_expr_tokens(s);
                if is_standalone_tokens(&tokens) {
                    let expr = extract_expr_from_standalone(&tokens);
                    Some(DynamicExpr::Expression(expr.to_string()))
                } else {
                    let owned = tokens
                        .into_iter()
                        .map(|t| match t {
                            ExprToken::String(s) => OwnedExprToken::Literal(s.to_string()),
                            ExprToken::Expression(s) => OwnedExprToken::Expression(s.to_string()),
                        })
                        .collect();
                    Some(DynamicExpr::Interpolated(owned))
                }
            }
            _ => None,
        }
    }

    /// Evaluate this expression using the given Lua evaluator and environment table.
    ///
    /// Returns a `serde_yaml::Value` which the caller deserializes into `T`.
    pub fn evaluate(
        &self,
        evaluator: &LuaEvaluator,
        env_table: &mlua::Table,
        config_path: &Path,
        field_path: &str,
    ) -> Result<serde_yaml::Value> {
        match self {
            DynamicExpr::Expression(expr) => {
                let lua_result = evaluator
                    .eval_inline_expr_with_env(expr, env_table, field_path)
                    .map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error evaluating ${{{{ {} }}}}$: {}", expr, e),
                    })?;
                lua_to_yaml(lua_result, config_path)
            }
            DynamicExpr::Interpolated(tokens) => {
                let mut result = String::new();
                for token in tokens {
                    match token {
                        OwnedExprToken::Literal(s) => result.push_str(s),
                        OwnedExprToken::Expression(expr) => {
                            let val = evaluator
                                .eval_inline_expr_with_env(expr, env_table, field_path)
                                .map_err(|e| DaemonError::LuaError {
                                    path: config_path.to_path_buf(),
                                    message: format!(
                                        "Error evaluating ${{{{ {} }}}}$: {}",
                                        expr, e
                                    ),
                                })?;
                            result.push_str(&lua_value_to_string(&val));
                        }
                    }
                }
                Ok(serde_yaml::Value::String(result))
            }
            DynamicExpr::Lua(code) => {
                let lua_result: mlua::Value =
                    evaluator
                        .eval_with_env(code, env_table, field_path)
                        .map_err(|e| DaemonError::LuaError {
                            path: config_path.to_path_buf(),
                            message: format!("Lua error: {}", e),
                        })?;
                lua_to_yaml(lua_result, config_path)
            }
        }
    }

    /// Reconstruct the YAML representation for serialization (round-tripping).
    pub fn to_yaml_value(&self) -> serde_yaml::Value {
        match self {
            DynamicExpr::Expression(expr) => {
                serde_yaml::Value::String(format!("${{{{ {} }}}}$", expr))
            }
            DynamicExpr::Interpolated(tokens) => {
                let mut s = String::new();
                for token in tokens {
                    match token {
                        OwnedExprToken::Literal(lit) => s.push_str(lit),
                        OwnedExprToken::Expression(expr) => {
                            s.push_str("${{ ");
                            s.push_str(expr);
                            s.push_str(" }}$");
                        }
                    }
                }
                serde_yaml::Value::String(s)
            }
            DynamicExpr::Lua(code) => {
                serde_yaml::Value::Tagged(Box::new(serde_yaml::value::TaggedValue {
                    tag: serde_yaml::value::Tag::new("!lua"),
                    value: serde_yaml::Value::String(code.clone()),
                }))
            }
        }
    }
}
