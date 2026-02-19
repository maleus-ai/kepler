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
    let vec = lua_table_to_env_vec(result).unwrap();

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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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

// --- eval_inline_expr tests ---

#[test]
fn test_inline_expr_returns_string() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar".to_string());
    let ctx = EvalContext { env, ..Default::default() };

    let result = eval.eval_inline_expr("env.FOO", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "bar");
}

#[test]
fn test_inline_expr_returns_number() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval_inline_expr("42", &ctx, "test").unwrap();
    assert!(matches!(result, Value::Integer(42)));
}

#[test]
fn test_inline_expr_returns_bool() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval_inline_expr("true", &ctx, "test").unwrap();
    assert!(matches!(result, Value::Boolean(true)));
}

#[test]
fn test_inline_expr_returns_table() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval_inline_expr("{1, 2, 3}", &ctx, "test").unwrap();
    assert!(matches!(result, Value::Table(_)));
}

#[test]
fn test_inline_expr_env_shortcut() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("MY_VAR".to_string(), "hello".to_string());
    let ctx = EvalContext { env, ..Default::default() };

    // env.MY_VAR should work as shortcut for ctx.env.MY_VAR
    let result = eval.eval_inline_expr("env.MY_VAR", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "hello");

    let result2 = eval.eval_inline_expr("ctx.env.MY_VAR", &ctx, "test").unwrap();
    assert_eq!(result2.as_str().unwrap().to_string(), "hello");
}

#[test]
fn test_inline_expr_deps_env() {
    let eval = LuaEvaluator::new().unwrap();
    let mut dep_env = HashMap::new();
    dep_env.insert("DB_URL".to_string(), "postgres://localhost".to_string());
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "healthy".to_string(),
        env: dep_env,
        ..Default::default()
    });
    let ctx = EvalContext { deps, ..Default::default() };

    let result = eval.eval_inline_expr("deps.db.env.DB_URL", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "postgres://localhost");
}

#[test]
fn test_inline_expr_bare_var_returns_nil() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("HOME".to_string(), "/home/user".to_string());
    let ctx = EvalContext { env, ..Default::default() };

    // Bare variable names resolve to nil (standard Lua behavior)
    let result = eval.eval_inline_expr("HOME", &ctx, "test").unwrap();
    assert!(result.is_nil(), "Bare variable should resolve to nil, got: {:?}", result);

    // Can use `or` to provide a default
    let result = eval.eval_inline_expr("HOME or \"fallback\"", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "fallback");
}

#[test]
fn test_inline_expr_or_default() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // env.MISSING is nil, so `or "default"` should return "default"
    let result = eval.eval_inline_expr("env.MISSING or \"default\"", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "default");
}

#[test]
fn test_inline_expr_stdlib_available() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval_inline_expr("string.upper('hello')", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "HELLO");
}

#[test]
fn test_inline_expr_lua_block_functions() {
    let eval = LuaEvaluator::new().unwrap();
    eval.load_inline("function greet(name) return 'hello ' .. name end").unwrap();

    let ctx = EvalContext::default();
    let result = eval.eval_inline_expr("greet('world')", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "hello world");
}

#[test]
fn test_lua_eval_has_deps_and_env_shortcut() {
    // Verify that !lua blocks also get deps and env shortcut
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar".to_string());
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "healthy".to_string(),
        ..Default::default()
    });
    let ctx = EvalContext { env, deps, ..Default::default() };

    let result: String = eval.eval(r#"return env.FOO"#, &ctx, "test").unwrap();
    assert_eq!(result, "bar");

    let result: String = eval.eval(r#"return deps.db.status"#, &ctx, "test").unwrap();
    assert_eq!(result, "healthy");
}

// --- Hook outputs context tests ---

#[test]
fn test_ctx_hooks_access() {
    let eval = LuaEvaluator::new().unwrap();
    let mut hooks = HashMap::new();
    let mut steps = HashMap::new();
    steps.insert("step1".to_string(), HashMap::from([
        ("token".to_string(), "abc".to_string()),
    ]));
    hooks.insert("pre_start".to_string(), steps);
    let ctx = EvalContext { hooks, ..Default::default() };

    let result: String = eval.eval(r#"return ctx.hooks.pre_start.outputs.step1.token"#, &ctx, "test").unwrap();
    assert_eq!(result, "abc");
}

#[test]
fn test_hooks_shortcut_access() {
    let eval = LuaEvaluator::new().unwrap();
    let mut hooks = HashMap::new();
    let mut steps = HashMap::new();
    steps.insert("step1".to_string(), HashMap::from([
        ("token".to_string(), "abc".to_string()),
    ]));
    hooks.insert("pre_start".to_string(), steps);
    let ctx = EvalContext { hooks, ..Default::default() };

    let result: String = eval.eval(r#"return hooks.pre_start.outputs.step1.token"#, &ctx, "test").unwrap();
    assert_eq!(result, "abc");
}

#[test]
fn test_deps_outputs_access() {
    let eval = LuaEvaluator::new().unwrap();
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "healthy".to_string(),
        outputs: HashMap::from([("port".to_string(), "8080".to_string())]),
        ..Default::default()
    });
    let ctx = EvalContext { deps, ..Default::default() };

    let result = eval.eval_inline_expr("deps.db.outputs.port", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "8080");
}

#[test]
fn test_deps_outputs_empty() {
    let eval = LuaEvaluator::new().unwrap();
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "healthy".to_string(),
        outputs: HashMap::new(),
        ..Default::default()
    });
    let ctx = EvalContext { deps, ..Default::default() };

    let result = eval.eval_inline_expr("deps.db.outputs.key", &ctx, "test").unwrap();
    assert!(result.is_nil(), "Missing output key should return nil, got: {:?}", result);
}

#[test]
fn test_ctx_hooks_frozen() {
    let eval = LuaEvaluator::new().unwrap();
    let mut hooks = HashMap::new();
    let mut steps = HashMap::new();
    steps.insert("step1".to_string(), HashMap::from([
        ("token".to_string(), "abc".to_string()),
    ]));
    hooks.insert("pre_start".to_string(), steps);
    let ctx = EvalContext { hooks, ..Default::default() };

    let result = eval.eval::<Value>(r#"ctx.hooks.pre_start.outputs.step1.new_key = "x""#, &ctx, "test");
    assert!(result.is_err(), "Writing to hooks table should fail");
}

#[test]
fn test_deps_outputs_frozen() {
    let eval = LuaEvaluator::new().unwrap();
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "healthy".to_string(),
        outputs: HashMap::from([("port".to_string(), "8080".to_string())]),
        ..Default::default()
    });
    let ctx = EvalContext { deps, ..Default::default() };

    let result = eval.eval::<Value>(r#"deps.db.outputs.port = "9090""#, &ctx, "test");
    assert!(result.is_err(), "Writing to deps outputs table should fail");
}

#[test]
fn test_inline_expr_hooks_access() {
    let eval = LuaEvaluator::new().unwrap();
    let mut hooks = HashMap::new();
    let mut steps = HashMap::new();
    steps.insert("step1".to_string(), HashMap::from([
        ("token".to_string(), "abc".to_string()),
    ]));
    hooks.insert("pre_start".to_string(), steps);
    let ctx = EvalContext { hooks, ..Default::default() };

    let result = eval.eval_inline_expr("hooks.pre_start.outputs.step1.token", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "abc");
}
