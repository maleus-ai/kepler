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
        service: Some(ServiceEvalContext { env, ..Default::default() }),
        ..Default::default()
    };

    let result: String = eval.eval(r#"return service.env.FOO"#, &ctx, "test").unwrap();
    assert_eq!(result, "bar");
}

#[test]
fn test_lua_env_readonly() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext { service: Some(ServiceEvalContext::default()), ..Default::default() };

    let result = eval.eval::<Value>(r#"service.env.NEW = "value""#, &ctx, "test");
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
        service: Some(ServiceEvalContext { name: "backend".to_string(), ..Default::default() }),
        ..Default::default()
    };

    let result: String = eval.eval(r#"return service.name"#, &ctx, "test").unwrap();
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
        service: Some(ServiceEvalContext { name: "api".to_string(), ..Default::default() }),
        hook: Some(HookEvalContext { name: "on_start".to_string(), ..Default::default() }),
        ..Default::default()
    };

    let result: String = eval.eval(r#"return hook.name"#, &ctx, "test").unwrap();
    assert_eq!(result, "on_start");
}

#[test]
fn test_lua_nil_for_missing_context() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // service/hook/kepler are always tables (stubs when context is absent)
    // so that service.env.X / hook.env.X / kepler.env.X resolve to nil instead of erroring
    let result: Value = eval.eval(r#"return service"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Table(_)));

    // env sub-tables exist but accessing missing keys returns nil
    let result: Value = eval
        .eval(r#"return service.env.NONEXISTENT"#, &ctx, "test")
        .unwrap();
    assert!(matches!(result, Value::Nil));

    let result: Value = eval
        .eval(r#"return hook.env.NONEXISTENT"#, &ctx, "test")
        .unwrap();
    assert!(matches!(result, Value::Nil));

    let result: Value = eval
        .eval(r#"return kepler.env.NONEXISTENT"#, &ctx, "test")
        .unwrap();
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
        hook: Some(HookEvalContext { had_failure: Some(true), ..Default::default() }),
        ..Default::default()
    };
    let result = eval.eval_condition("always()", &ctx).unwrap();
    assert!(result.value);
    assert!(!result.failure_checked);
}

#[test]
fn test_success_no_args_no_failure() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        hook: Some(HookEvalContext { had_failure: Some(false), ..Default::default() }),
        ..Default::default()
    };
    assert!(eval.eval_condition("success()", &ctx).unwrap().value);
}

#[test]
fn test_success_no_args_after_failure() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        hook: Some(HookEvalContext { had_failure: Some(true), ..Default::default() }),
        ..Default::default()
    };
    assert!(!eval.eval_condition("success()", &ctx).unwrap().value);
}

#[test]
fn test_failure_no_args() {
    let eval = LuaEvaluator::new().unwrap();

    // No failure
    let ctx = EvalContext {
        hook: Some(HookEvalContext { had_failure: Some(false), ..Default::default() }),
        ..Default::default()
    };
    let result = eval.eval_condition("failure()", &ctx).unwrap();
    assert!(!result.value);
    assert!(result.failure_checked);

    // After failure
    let ctx = EvalContext {
        hook: Some(HookEvalContext { had_failure: Some(true), ..Default::default() }),
        ..Default::default()
    };
    let result = eval.eval_condition("failure()", &ctx).unwrap();
    assert!(result.value);
    assert!(result.failure_checked);
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
    assert!(eval.eval_condition("success('db')", &ctx).unwrap().value);
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
    assert!(!eval.eval_condition("success('db')", &ctx).unwrap().value);
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
    let result = eval.eval_condition("failure('db')", &ctx).unwrap();
    assert!(result.value);
    // failure('db') is failure-with-arg, should NOT set failure_checked
    assert!(!result.failure_checked);
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
    assert!(!eval.eval_condition("failure('db')", &ctx).unwrap().value);
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
    assert!(eval.eval_condition("failure('db')", &ctx).unwrap().value);
    assert!(!eval.eval_condition("success('db')", &ctx).unwrap().value);
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
    assert!(eval.eval_condition("skipped('db')", &ctx).unwrap().value);
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
    assert!(!eval.eval_condition("skipped('db')", &ctx).unwrap().value);
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

    assert!(eval.eval_condition("deps.db.status == 'healthy'", &ctx).unwrap().value);
    assert!(eval.eval_condition("deps.db.exit_code == 0", &ctx).unwrap().value);
    assert!(eval.eval_condition("deps.db.initialized == true", &ctx).unwrap().value);
    assert!(eval.eval_condition("deps.db.restart_count == 3", &ctx).unwrap().value);
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
    assert!(eval.eval_condition("deps.db.exit_code == nil", &ctx).unwrap().value);
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
    assert!(eval.eval_condition("require == nil", &ctx).unwrap().value);
}

#[test]
fn test_condition_sandbox_allows_stdlib() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    assert!(eval.eval_condition("string.upper('hello') == 'HELLO'", &ctx).unwrap().value);
    assert!(eval.eval_condition("math.max(1, 5, 3) == 5", &ctx).unwrap().value);
    assert!(eval.eval_condition("type(42) == 'number'", &ctx).unwrap().value);
    assert!(eval.eval_condition("tonumber('42') == 42", &ctx).unwrap().value);
    assert!(eval.eval_condition("tostring(42) == '42'", &ctx).unwrap().value);
}

#[test]
fn test_condition_sandbox_lua_block_functions() {
    let eval = LuaEvaluator::new().unwrap();

    // Define a function in the lua: block
    eval.load_inline("function my_check() return true end").unwrap();

    let ctx = EvalContext::default();
    assert!(eval.eval_condition("my_check()", &ctx).unwrap().value);
}

#[test]
fn test_success_no_args_default_when_not_in_hook() {
    let eval = LuaEvaluator::new().unwrap();
    // hook_had_failure = None means not in a hook list context
    let ctx = EvalContext::default();
    // Should default to true (no failure)
    assert!(eval.eval_condition("success()", &ctx).unwrap().value);
}

#[test]
fn test_failure_no_args_default_when_not_in_hook() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();
    // Should default to false (no failure)
    let result = eval.eval_condition("failure()", &ctx).unwrap();
    assert!(!result.value);
    assert!(result.failure_checked);
}

#[test]
fn test_failure_with_arg_does_not_set_failure_checked() {
    let eval = LuaEvaluator::new().unwrap();
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "failed".to_string(),
        exit_code: Some(1),
        initialized: false,
        restart_count: 0,
        ..Default::default()
    });
    let ctx = EvalContext {
        hook: Some(HookEvalContext { had_failure: Some(true), ..Default::default() }),
        deps,
        ..Default::default()
    };

    // failure('db') — with arg — should NOT set failure_checked
    let result = eval.eval_condition("failure('db')", &ctx).unwrap();
    assert!(result.value);
    assert!(!result.failure_checked, "failure('db') should NOT set failure_checked");

    // failure('db') or always() — neither calls failure() no-args
    let result = eval.eval_condition("failure('db') or always()", &ctx).unwrap();
    assert!(result.value);
    assert!(!result.failure_checked, "failure('db') or always() should NOT set failure_checked");

    // failure() — no args — SHOULD set failure_checked
    let result = eval.eval_condition("failure()", &ctx).unwrap();
    assert!(result.value);
    assert!(result.failure_checked, "failure() should set failure_checked");
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
    let ctx = EvalContext { service: Some(ServiceEvalContext { env, ..Default::default() }), ..Default::default() };

    let result = eval.eval_inline_expr("service.env.FOO", &ctx, "test").unwrap();
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
fn test_inline_expr_service_env() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("MY_VAR".to_string(), "hello".to_string());
    let ctx = EvalContext { service: Some(ServiceEvalContext { env, ..Default::default() }), ..Default::default() };

    // service.env.MY_VAR should work
    let result = eval.eval_inline_expr("service.env.MY_VAR", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "hello");
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
    let ctx = EvalContext { service: Some(ServiceEvalContext { env, ..Default::default() }), ..Default::default() };

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
    let ctx = EvalContext { service: Some(ServiceEvalContext::default()), ..Default::default() };

    // service.env.MISSING is nil (not set), so `or "default"` should return "default"
    let result = eval.eval_inline_expr("service.env.MISSING or \"default\"", &ctx, "test").unwrap();
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
fn test_lua_eval_has_deps_and_service_env() {
    // Verify that !lua blocks get deps and service.env
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar".to_string());
    let mut deps = HashMap::new();
    deps.insert("db".to_string(), DepInfo {
        status: "healthy".to_string(),
        ..Default::default()
    });
    let ctx = EvalContext { service: Some(ServiceEvalContext { env, ..Default::default() }), deps, ..Default::default() };

    let result: String = eval.eval(r#"return service.env.FOO"#, &ctx, "test").unwrap();
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
    let ctx = EvalContext { service: Some(ServiceEvalContext { hooks, ..Default::default() }), ..Default::default() };

    let result: String = eval.eval(r#"return service.hooks.pre_start.outputs.step1.token"#, &ctx, "test").unwrap();
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
    let ctx = EvalContext { service: Some(ServiceEvalContext { hooks, ..Default::default() }), ..Default::default() };

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
    let ctx = EvalContext { service: Some(ServiceEvalContext { hooks, ..Default::default() }), ..Default::default() };

    let result = eval.eval::<Value>(r#"service.hooks.pre_start.outputs.step1.new_key = "x""#, &ctx, "test");
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
    let ctx = EvalContext { service: Some(ServiceEvalContext { hooks, ..Default::default() }), ..Default::default() };

    let result = eval.eval_inline_expr("hooks.pre_start.outputs.step1.token", &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "abc");
}

// --- PreparedEnv tests ---

#[test]
fn test_prepared_env_set_env_visible_in_lua() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("INITIAL".to_string(), "yes".to_string());
    let ctx = EvalContext { service: Some(ServiceEvalContext { env, ..Default::default() }), ..Default::default() };

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();

    // Initial env var is visible
    let result = eval.eval_inline_expr_with_env("service.env.INITIAL", &prepared.table, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "yes");

    // Lua cannot write to env (it's frozen from Lua's perspective)
    let result = eval.eval_with_env::<Value>(r#"service.env.HACK = "bad""#, &prepared.table, "test");
    assert!(result.is_err(), "Lua should not be able to write to service.env before freeze_env");

    // Add a new env var via set_env (Rust-side toggle)
    prepared.set_env("ADDED", "new_value").unwrap();

    // New env var is visible in subsequent evaluations via service.env
    let result = eval.eval_inline_expr_with_env("service.env.ADDED", &prepared.table, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "new_value");
}

#[test]
fn test_prepared_env_freeze_prevents_writes() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();

    // Can write before freezing
    prepared.set_env("KEY", "value").unwrap();

    // Freeze the env sub-table
    prepared.freeze_env();

    // Cannot write after freezing
    let result = prepared.set_env("ANOTHER", "val");
    assert!(result.is_err(), "set_env should fail after freeze_env");
}

#[test]
fn test_prepared_env_lua_cannot_mutate_frozen_env() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext { service: Some(ServiceEvalContext::default()), ..Default::default() };

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();
    prepared.freeze_env();

    // Lua code should not be able to write to service.env
    let result = eval.eval_with_env::<Value>(r#"service.env.NEW = "value""#, &prepared.table, "test");
    assert!(result.is_err(), "Lua should not be able to write to frozen service.env");
}

#[test]
fn test_eval_with_env_reuses_table() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("PORT".to_string(), "3000".to_string());
    let ctx = EvalContext { service: Some(ServiceEvalContext { env, ..Default::default() }), ..Default::default() };

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();
    prepared.freeze_env();

    // eval_with_env should work with the prepared table
    let result: String = eval.eval_with_env(r#"return service.env.PORT"#, &prepared.table, "test").unwrap();
    assert_eq!(result, "3000");

    // Global state should be shared across calls
    let _: Value = eval.eval_with_env(r#"global.counter = 1"#, &prepared.table, "test").unwrap();
    let counter: i64 = eval.eval_with_env(r#"return global.counter"#, &prepared.table, "test").unwrap();
    assert_eq!(counter, 1);
}

#[test]
fn test_prepared_env_shallow_freeze() {
    // Verify that freezing service table (shallow) does NOT freeze the nested env table
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext { service: Some(ServiceEvalContext::default()), ..Default::default() };

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();

    // ctx is frozen but env sub-table should still be writable from Rust
    prepared.set_env("KEY", "val").unwrap();

    // Lua should not be able to reassign service.env (service is frozen)
    let result = eval.eval_with_env::<Value>(r#"service.env = {}"#, &prepared.table, "test");
    assert!(result.is_err(), "Lua should not be able to reassign service.env on frozen service");

    // But the env sub-table entries should be accessible
    let result = eval.eval_inline_expr_with_env("service.env.KEY", &prepared.table, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "val");
}

// --- json/yaml stdlib tests ---

#[test]
fn test_json_parse_object() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: Value = eval.eval(r#"local t = json.parse('{"a":1,"b":"hello"}'); return t.a"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Integer(1)));
}

#[test]
fn test_json_parse_array() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: Value = eval.eval(r#"local t = json.parse('[1,2,3]'); return t[1]"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Integer(1)));
}

#[test]
fn test_json_parse_error() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval::<Value>(r#"return json.parse('invalid')"#, &ctx, "test");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("json.parse"), "Error should mention json.parse: {}", err);
}

#[test]
fn test_json_stringify() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: String = eval.eval(r#"return json.stringify({a = 1})"#, &ctx, "test").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[test]
fn test_json_stringify_pretty() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: String = eval.eval(r#"return json.stringify({a = 1}, true)"#, &ctx, "test").unwrap();
    assert!(result.contains('\n'), "Pretty JSON should contain newlines: {}", result);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[test]
fn test_json_roundtrip() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: String = eval.eval(
        r#"
        local original = '{"name":"test","count":42,"active":true}'
        local parsed = json.parse(original)
        return json.stringify(parsed)
        "#,
        &ctx,
        "test",
    ).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["name"], "test");
    assert_eq!(parsed["count"], 42);
    assert_eq!(parsed["active"], true);
}

#[test]
fn test_yaml_parse() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: Value = eval.eval(r#"local t = yaml.parse('a: 1\nb: hello'); return t.b"#, &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "hello");
}

#[test]
fn test_yaml_parse_error() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval::<Value>(r#"return yaml.parse('a: [invalid')"#, &ctx, "test");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("yaml.parse"), "Error should mention yaml.parse: {}", err);
}

#[test]
fn test_yaml_stringify() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: String = eval.eval(r#"return yaml.stringify({a = 1})"#, &ctx, "test").unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
    assert_eq!(parsed["a"], serde_yaml::Value::Number(serde_yaml::Number::from(1)));
}

#[test]
fn test_yaml_roundtrip() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: String = eval.eval(
        r#"
        local original = 'name: test\ncount: 42'
        local parsed = yaml.parse(original)
        return yaml.stringify(parsed)
        "#,
        &ctx,
        "test",
    ).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
    assert_eq!(parsed["name"], serde_yaml::Value::String("test".to_string()));
    assert_eq!(parsed["count"], serde_yaml::Value::Number(serde_yaml::Number::from(42)));
}

#[test]
fn test_json_yaml_available_in_inline() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval_inline_expr(r#"json.stringify({x = 1})"#, &ctx, "test").unwrap();
    let s = result.as_str().unwrap().to_string();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed["x"], 1);

    let result = eval.eval_inline_expr(r#"yaml.stringify({x = 1})"#, &ctx, "test").unwrap();
    assert!(result.as_str().is_some(), "yaml.stringify should return a string");
}

// --- json/yaml table frozen tests ---

#[test]
fn test_json_table_frozen() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval::<Value>(r#"json.parse = function() end"#, &ctx, "test");
    assert!(result.is_err(), "Writing to json table should fail");
}

#[test]
fn test_yaml_table_frozen() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval::<Value>(r#"yaml.parse = function() end"#, &ctx, "test");
    assert!(result.is_err(), "Writing to yaml table should fail");
}

#[test]
fn test_os_table_frozen() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Per-env os table should be frozen
    let result = eval.eval::<Value>(r#"os.getgroups = function() end"#, &ctx, "test");
    assert!(result.is_err(), "Writing to per-env os table should fail");

    // Global os functions inherited via metatable should also be safe
    let result = eval.eval::<Value>(r#"os.clock = function() return 0 end"#, &ctx, "test");
    assert!(result.is_err(), "Writing os.clock on frozen per-env os table should fail");
}

#[test]
fn test_global_stdlib_tables_cannot_be_overridden() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Check if stdlib tables can be overridden from a sandboxed env
    for name in &["string", "math", "table", "coroutine", "bit32", "utf8", "buffer"] {
        let code = format!("{}.fake = function() end", name);
        let result = eval.eval::<Value>(&code, &ctx, "test");
        if result.is_ok() {
            panic!("Global stdlib table '{}' is NOT frozen — Lua code can mutate it", name);
        }
    }
}

#[test]
fn test_global_os_table_frozen() {
    let eval = LuaEvaluator::new().unwrap();

    // The lua: block runs in global scope — verify global os table is frozen
    let result = eval.load_inline(r#"os.clock = function() return 0 end"#);
    assert!(result.is_err(), "Writing to global os table should fail");
}

// --- Owner context tests ---

#[test]
fn test_owner_uid_gid_user_accessible() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        owner: Some(super::OwnerEvalContext {
            uid: 1000,
            gid: 1000,
            user: Some("alice".to_string()),
        }),
        ..Default::default()
    };

    let uid: i64 = eval.eval(r#"return owner.uid"#, &ctx, "test").unwrap();
    assert_eq!(uid, 1000);

    let gid: i64 = eval.eval(r#"return owner.gid"#, &ctx, "test").unwrap();
    assert_eq!(gid, 1000);

    let user: String = eval.eval(r#"return owner.user"#, &ctx, "test").unwrap();
    assert_eq!(user, "alice");
}

#[test]
fn test_owner_nil_when_not_set() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result: Value = eval.eval(r#"return owner"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Nil), "owner should be nil when not set");
}

#[test]
fn test_owner_user_nil_for_numeric_uid() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        owner: Some(super::OwnerEvalContext {
            uid: 9999,
            gid: 9999,
            user: None,
        }),
        ..Default::default()
    };

    let result: Value = eval.eval(r#"return owner.user"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Nil), "owner.user should be nil for numeric-only UIDs");

    // uid and gid should still work
    let uid: i64 = eval.eval(r#"return owner.uid"#, &ctx, "test").unwrap();
    assert_eq!(uid, 9999);
}

#[test]
fn test_owner_table_readonly() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        owner: Some(super::OwnerEvalContext {
            uid: 1000,
            gid: 1000,
            user: Some("alice".to_string()),
        }),
        ..Default::default()
    };

    let result = eval.eval::<Value>(r#"owner.uid = 0"#, &ctx, "test");
    assert!(result.is_err(), "Writing to owner table should fail");
}

// --- os.getgroups() tests ---

#[test]
fn test_os_getgroups_no_arg_errors() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let result = eval.eval::<Value>(r#"return os.getgroups()"#, &ctx, "test");
    assert!(result.is_err(), "os.getgroups() with no argument should error");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("expected a username or uid"), "Error should mention missing argument: {}", err);
}

#[test]
fn test_os_getgroups_root_returns_groups_hardening_none() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        hardening: super::HardeningLevel::None,
        ..Default::default()
    };

    // With hardening=none, querying root should succeed
    let result: Value = eval.eval(r#"return os.getgroups("root")"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Table(_)), "os.getgroups('root') should return a table, got: {:?}", result);
}

#[test]
fn test_os_getgroups_root_errors_hardening_no_root() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        hardening: super::HardeningLevel::NoRoot,
        ..Default::default()
    };

    let result = eval.eval::<Value>(r#"return os.getgroups("root")"#, &ctx, "test");
    assert!(result.is_err(), "os.getgroups('root') should error with no-root hardening");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("access denied"), "Error should mention access denied: {}", err);
}

#[test]
fn test_os_getgroups_root_errors_hardening_strict() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        hardening: super::HardeningLevel::Strict,
        owner: Some(super::OwnerEvalContext {
            uid: 1000,
            gid: 1000,
            user: Some("testuser".to_string()),
        }),
        ..Default::default()
    };

    let result = eval.eval::<Value>(r#"return os.getgroups("root")"#, &ctx, "test");
    assert!(result.is_err(), "os.getgroups('root') should error with strict hardening");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("access denied"), "Error should mention access denied: {}", err);
}

#[test]
fn test_os_getgroups_other_user_errors_hardening_strict() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        hardening: super::HardeningLevel::Strict,
        owner: Some(super::OwnerEvalContext {
            uid: 1000,
            gid: 1000,
            user: Some("testuser".to_string()),
        }),
        ..Default::default()
    };

    // Querying a different user should fail under strict hardening
    // Use uid "0" to trigger uid mismatch (root uid) — this also hits the no-root check
    let result = eval.eval::<Value>(r#"return os.getgroups("0")"#, &ctx, "test");
    assert!(result.is_err(), "os.getgroups('0') should error with strict hardening when owner uid is 1000");
}

#[test]
fn test_os_clock_still_works_metatable_fallback() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // os.clock should still be accessible through the metatable fallback
    let result: Value = eval.eval(r#"return os.clock()"#, &ctx, "test").unwrap();
    assert!(matches!(result, Value::Number(_)), "os.clock() should return a number, got: {:?}", result);
}

#[test]
fn test_os_date_still_works_metatable_fallback() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // os.date should still be accessible
    let result: Value = eval.eval(r#"return type(os.date)"#, &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "function");
}

#[test]
fn test_os_getgroups_available_in_inline_expr() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // os.getgroups should be accessible from inline expressions too
    let result = eval.eval_inline_expr(r#"type(os.getgroups)"#, &ctx, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "function");
}

#[test]
fn test_os_getgroups_available_in_condition() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // os.getgroups should be accessible from condition expressions
    let result = eval.eval_condition("type(os.getgroups) == 'function'", &ctx).unwrap();
    assert!(result.value, "os.getgroups should be available in condition context");
}

#[test]
fn test_owner_available_in_condition() {
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        owner: Some(super::OwnerEvalContext {
            uid: 1000,
            gid: 1000,
            user: Some("alice".to_string()),
        }),
        ..Default::default()
    };

    let result = eval.eval_condition("owner.uid == 1000", &ctx).unwrap();
    assert!(result.value, "owner.uid should be accessible in condition context");

    let result = eval.eval_condition("owner.user == 'alice'", &ctx).unwrap();
    assert!(result.value, "owner.user should be accessible in condition context");
}

#[test]
fn test_kepler_env_denied_raises_error() {
    let eval = LuaEvaluator::new().unwrap();
    let mut kepler_env = HashMap::new();
    kepler_env.insert("MY_VAR".to_string(), "should_not_be_visible".to_string());
    let ctx = EvalContext {
        kepler_env,
        kepler_env_denied: true,
        ..Default::default()
    };

    let err = eval.eval::<Value>(r#"return kepler.env.MY_VAR"#, &ctx, "test")
        .expect_err("kepler.env access should error when denied");
    let msg = err.to_string();
    assert!(msg.contains("kepler.env.MY_VAR is not available"), "error should name the key: {msg}");
    assert!(msg.contains("kepler.autostart"), "error should reference kepler.autostart: {msg}");
    assert!(msg.contains("kepler.autostart.environment"), "error should reference kepler.autostart.environment: {msg}");
}

#[test]
fn test_kepler_env_denied_false_allows_access() {
    let eval = LuaEvaluator::new().unwrap();
    let mut kepler_env = HashMap::new();
    kepler_env.insert("MY_VAR".to_string(), "hello".to_string());
    let ctx = EvalContext {
        kepler_env,
        kepler_env_denied: false,
        ..Default::default()
    };

    let result: String = eval.eval(r#"return kepler.env.MY_VAR"#, &ctx, "test").unwrap();
    assert_eq!(result, "hello");
}
