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

// --- PreparedEnv tests ---

#[test]
fn test_prepared_env_set_env_visible_in_lua() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("INITIAL".to_string(), "yes".to_string());
    let ctx = EvalContext { env, ..Default::default() };

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();

    // Initial env var is visible
    let result = eval.eval_inline_expr_with_env("env.INITIAL", &prepared.table, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "yes");

    // Lua cannot write to env (it's frozen from Lua's perspective)
    let result = eval.eval_with_env::<Value>(r#"env.HACK = "bad""#, &prepared.table, "test");
    assert!(result.is_err(), "Lua should not be able to write to env before freeze_env");

    // Add a new env var via set_env (Rust-side toggle)
    prepared.set_env("ADDED", "new_value").unwrap();

    // New env var is visible in subsequent evaluations
    let result = eval.eval_inline_expr_with_env("env.ADDED", &prepared.table, "test").unwrap();
    assert_eq!(result.as_str().unwrap().to_string(), "new_value");

    // Also visible via ctx.env
    let result = eval.eval_inline_expr_with_env("ctx.env.ADDED", &prepared.table, "test").unwrap();
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
    let ctx = EvalContext::default();

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();
    prepared.freeze_env();

    // Lua code should not be able to write to env
    let result = eval.eval_with_env::<Value>(r#"env.NEW = "value""#, &prepared.table, "test");
    assert!(result.is_err(), "Lua should not be able to write to frozen env");
}

#[test]
fn test_eval_with_env_reuses_table() {
    let eval = LuaEvaluator::new().unwrap();
    let mut env = HashMap::new();
    env.insert("PORT".to_string(), "3000".to_string());
    let ctx = EvalContext { env, ..Default::default() };

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();
    prepared.freeze_env();

    // eval_with_env should work with the prepared table
    let result: String = eval.eval_with_env(r#"return env.PORT"#, &prepared.table, "test").unwrap();
    assert_eq!(result, "3000");

    // Global state should be shared across calls
    let _: Value = eval.eval_with_env(r#"global.counter = 1"#, &prepared.table, "test").unwrap();
    let counter: i64 = eval.eval_with_env(r#"return global.counter"#, &prepared.table, "test").unwrap();
    assert_eq!(counter, 1);
}

#[test]
fn test_prepared_env_shallow_freeze() {
    // Verify that freezing ctx_table (shallow) does NOT freeze the nested env table
    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    let prepared = eval.prepare_env_mutable(&ctx).unwrap();

    // ctx is frozen but env sub-table should still be writable from Rust
    prepared.set_env("KEY", "val").unwrap();

    // Lua should not be able to reassign ctx.env (ctx is frozen)
    let result = eval.eval_with_env::<Value>(r#"ctx.env = {}"#, &prepared.table, "test");
    assert!(result.is_err(), "Lua should not be able to reassign ctx.env on frozen ctx");

    // But the env sub-table entries should be accessible
    let result = eval.eval_inline_expr_with_env("ctx.env.KEY", &prepared.table, "test").unwrap();
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
