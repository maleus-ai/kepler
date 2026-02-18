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
