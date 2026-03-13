use super::*;

fn make_context(action: &'static str) -> AuthorizerContext {
    let mut params = HashMap::new();
    params.insert("services".into(), ParamValue::StringList(vec!["web".into()]));
    AuthorizerContext {
        action,
        uid: 1000,
        gid: 1000,
        username: Some("testuser".into()),
        groups: vec![1000],
        is_token: false,
        params,
    }
}

// ---------------------------------------------------------------------------
// VM sandbox tests
// ---------------------------------------------------------------------------

#[test]
fn sandbox_no_require() {
    let lua = create_acl_lua_vm().unwrap();
    let result: LuaResult<LuaValue> = lua.load("return require").eval();
    match result {
        Ok(LuaValue::Nil) => {}
        other => panic!("expected nil, got {:?}", other),
    }
}

#[test]
fn sandbox_no_io() {
    let lua = create_acl_lua_vm().unwrap();
    let result: LuaResult<LuaValue> = lua.load("return io").eval();
    match result {
        Ok(LuaValue::Nil) => {}
        other => panic!("expected nil, got {:?}", other),
    }
}

#[test]
fn sandbox_no_debug() {
    let lua = create_acl_lua_vm().unwrap();
    let result: LuaResult<LuaValue> = lua.load("return debug").eval();
    match result {
        Ok(LuaValue::Nil) => {}
        other => panic!("expected nil, got {:?}", other),
    }
}

#[test]
fn sandbox_no_load() {
    let lua = create_acl_lua_vm().unwrap();
    let result: LuaResult<LuaValue> = lua.load("return load").eval();
    match result {
        Ok(LuaValue::Nil) => {}
        other => panic!("expected nil, got {:?}", other),
    }
}

#[test]
fn sandbox_os_clock_available() {
    let lua = create_acl_lua_vm().unwrap();
    let result: f64 = lua.load("return os.clock()").eval().unwrap();
    assert!(result >= 0.0);
}

#[test]
fn sandbox_os_time_available() {
    let lua = create_acl_lua_vm().unwrap();
    let result: i64 = lua.load("return os.time()").eval().unwrap();
    assert!(result > 0);
}

#[test]
fn sandbox_os_execute_unavailable() {
    let lua = create_acl_lua_vm().unwrap();
    let result: LuaResult<LuaValue> = lua.load("return os.execute").eval();
    match result {
        Ok(LuaValue::Nil) => {}
        other => panic!("expected nil, got {:?}", other),
    }
}

#[test]
fn sandbox_json_available() {
    let lua = create_acl_lua_vm().unwrap();
    let result: String = lua
        .load(r#"return json.stringify({a = 1})"#)
        .eval()
        .unwrap();
    assert!(result.contains("\"a\""));
}

#[test]
fn sandbox_frozen_stdlib() {
    let lua = create_acl_lua_vm().unwrap();
    let result = lua.load("string.custom = true").exec();
    assert!(result.is_err(), "should not be able to modify frozen string table");
}

// ---------------------------------------------------------------------------
// Authorizer compilation tests
// ---------------------------------------------------------------------------

#[test]
fn compile_simple_authorizer() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return true").unwrap();
    let func: LuaFunction = lua.registry_value(&key).unwrap();
    let result: bool = func.call(()).unwrap();
    assert!(result);
}

#[test]
fn compile_authorizer_with_args() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return request.action == 'start'").unwrap();
    let func: LuaFunction = lua.registry_value(&key).unwrap();

    let ctx = make_context("start");
    let req_table = build_request_table(&lua, &ctx).unwrap();
    let caller_table = build_caller_table(&lua, &ctx).unwrap();

    let result: bool = func.call((req_table, caller_table)).unwrap();
    assert!(result);
}

#[test]
fn compile_authorizer_denies_wrong_action() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return request.action == 'stop'").unwrap();
    let func: LuaFunction = lua.registry_value(&key).unwrap();

    let ctx = make_context("start");
    let req_table = build_request_table(&lua, &ctx).unwrap();
    let caller_table = build_caller_table(&lua, &ctx).unwrap();

    let result: bool = func.call((req_table, caller_table)).unwrap();
    assert!(!result);
}

#[test]
fn compile_authorizer_checks_caller() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return caller.username == 'testuser'").unwrap();
    let func: LuaFunction = lua.registry_value(&key).unwrap();

    let ctx = make_context("start");
    let req_table = build_request_table(&lua, &ctx).unwrap();
    let caller_table = build_caller_table(&lua, &ctx).unwrap();

    let result: bool = func.call((req_table, caller_table)).unwrap();
    assert!(result);
}

#[test]
fn compile_syntax_error() {
    let lua = create_acl_lua_vm().unwrap();
    let result = compile_authorizer(&lua, "return !!!invalid");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Authorizer evaluation tests
// ---------------------------------------------------------------------------

#[test]
fn evaluate_empty_authorizers() {
    let lua = create_acl_lua_vm().unwrap();
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[], &ctx);
    assert!(result.is_ok());
}

#[test]
fn evaluate_single_allow() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return true").unwrap();
    let handle = RegistryKeyHandle(Arc::new(key));
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[handle], &ctx);
    assert!(result.is_ok());
}

#[test]
fn evaluate_single_deny() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return false").unwrap();
    let handle = RegistryKeyHandle(Arc::new(key));
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[handle], &ctx);
    assert!(result.is_err());
}

#[test]
fn evaluate_nil_is_deny() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, "return nil").unwrap();
    let handle = RegistryKeyHandle(Arc::new(key));
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[handle], &ctx);
    assert!(result.is_err());
}

#[test]
fn evaluate_error_is_deny() {
    let lua = create_acl_lua_vm().unwrap();
    let key = compile_authorizer(&lua, r#"error("nope")"#).unwrap();
    let handle = RegistryKeyHandle(Arc::new(key));
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[handle], &ctx);
    assert!(result.is_err());
}

#[test]
fn evaluate_all_must_allow() {
    let lua = create_acl_lua_vm().unwrap();
    let k1 = RegistryKeyHandle(Arc::new(compile_authorizer(&lua, "return true").unwrap()));
    let k2 = RegistryKeyHandle(Arc::new(compile_authorizer(&lua, "return false").unwrap()));
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[k1, k2], &ctx);
    assert!(result.is_err());
}

#[test]
fn evaluate_all_allow_passes() {
    let lua = create_acl_lua_vm().unwrap();
    let k1 = RegistryKeyHandle(Arc::new(compile_authorizer(&lua, "return true").unwrap()));
    let k2 = RegistryKeyHandle(Arc::new(compile_authorizer(&lua, "return true").unwrap()));
    let ctx = make_context("start");
    let result = evaluate_authorizers(&lua, &[k1, k2], &ctx);
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Context building tests
// ---------------------------------------------------------------------------

#[test]
fn request_table_has_action() {
    let lua = create_acl_lua_vm().unwrap();
    let ctx = make_context("start");
    let tbl = build_request_table(&lua, &ctx).unwrap();
    let action: String = tbl.get("action").unwrap();
    assert_eq!(action, "start");
}

#[test]
fn request_table_has_services_in_params() {
    let lua = create_acl_lua_vm().unwrap();
    let ctx = make_context("start");
    let tbl = build_request_table(&lua, &ctx).unwrap();
    let params: LuaTable = tbl.get("params").unwrap();
    let services: LuaTable = params.get("services").unwrap();
    let first: String = services.get(1).unwrap();
    assert_eq!(first, "web");
}

#[test]
fn caller_table_has_uid() {
    let lua = create_acl_lua_vm().unwrap();
    let ctx = make_context("start");
    let tbl = build_caller_table(&lua, &ctx).unwrap();
    let uid: u32 = tbl.get("uid").unwrap();
    assert_eq!(uid, 1000);
}

#[test]
fn caller_table_has_username() {
    let lua = create_acl_lua_vm().unwrap();
    let ctx = make_context("start");
    let tbl = build_caller_table(&lua, &ctx).unwrap();
    let name: String = tbl.get("username").unwrap();
    assert_eq!(name, "testuser");
}

#[test]
fn caller_table_token_flag() {
    let lua = create_acl_lua_vm().unwrap();
    let mut ctx = make_context("start");
    ctx.is_token = true;
    let tbl = build_caller_table(&lua, &ctx).unwrap();
    let is_token: bool = tbl.get("token").unwrap();
    assert!(is_token);
}

// ---------------------------------------------------------------------------
// Shared acl.lua block tests
// ---------------------------------------------------------------------------

#[test]
fn shared_lua_block_defines_function() {
    let lua = create_acl_lua_vm().unwrap();
    lua.load("function is_admin(uid) return uid == 0 end")
        .set_name("acl.lua")
        .exec()
        .unwrap();
    lua.globals().set_readonly(true);

    let key = compile_authorizer(&lua, "return is_admin(caller.uid)").unwrap();
    let func: LuaFunction = lua.registry_value(&key).unwrap();

    let ctx = make_context("start");
    let req_table = build_request_table(&lua, &ctx).unwrap();
    let caller_table = build_caller_table(&lua, &ctx).unwrap();

    // uid=1000, not admin
    let result: bool = func.call((req_table, caller_table)).unwrap();
    assert!(!result);
}

// ---------------------------------------------------------------------------
// build_authorizer_context tests
// ---------------------------------------------------------------------------

#[test]
fn context_from_start_request() {
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec!["web".into()],
        sys_env: None,
        no_deps: true,
        override_envs: Some(HashMap::from([("KEY".into(), "val".into())])),
        hardening: Some("strict".into()),

        follow: false,
    };
    let ctx = build_authorizer_context(&req, 1000, 1000, Some("user".into()), vec![1000], false).unwrap();
    assert_eq!(ctx.action, "start");
    assert!(matches!(ctx.params.get("services"), Some(ParamValue::StringList(s)) if s == &vec!["web".to_string()]));
    assert!(matches!(ctx.params.get("no_deps"), Some(ParamValue::Bool(true))));
    assert!(matches!(ctx.params.get("hardening"), Some(ParamValue::String(s)) if s == "strict"));
    assert!(matches!(ctx.params.get("override_envs"), Some(ParamValue::StringMap(_))));
}

#[test]
fn context_from_stop_request() {
    let req = Request::Stop {
        config_path: "/test".into(),
        services: vec!["web".into()],
        clean: true,
        signal: Some("SIGKILL".into()),
    };
    let ctx = build_authorizer_context(&req, 1000, 1000, None, vec![1000], true).unwrap();
    assert_eq!(ctx.action, "stop");
    assert!(matches!(ctx.params.get("services"), Some(ParamValue::StringList(s)) if s == &vec!["web".to_string()]));
    assert!(ctx.is_token);
    assert!(matches!(ctx.params.get("clean"), Some(ParamValue::Bool(true))));
    assert!(matches!(ctx.params.get("signal"), Some(ParamValue::String(s)) if s == "SIGKILL"));
}

#[test]
fn context_from_stop_request_multiple_services() {
    let req = Request::Stop {
        config_path: "/test".into(),
        services: vec!["web".into(), "worker".into(), "scheduler".into()],
        clean: false,
        signal: None,
    };
    let ctx = build_authorizer_context(&req, 1000, 1000, None, vec![1000], false).unwrap();
    assert_eq!(ctx.action, "stop");
    assert!(matches!(
        ctx.params.get("services"),
        Some(ParamValue::StringList(s)) if s == &vec!["web".to_string(), "worker".to_string(), "scheduler".to_string()]
    ));
    assert!(matches!(ctx.params.get("clean"), Some(ParamValue::Bool(false))));
    assert!(!ctx.params.contains_key("signal"));
}

#[test]
fn context_from_stop_request_empty_services() {
    let req = Request::Stop {
        config_path: "/test".into(),
        services: vec![],
        clean: false,
        signal: None,
    };
    let ctx = build_authorizer_context(&req, 1000, 1000, None, vec![1000], false).unwrap();
    assert_eq!(ctx.action, "stop");
    assert!(matches!(
        ctx.params.get("services"),
        Some(ParamValue::StringList(s)) if s.is_empty()
    ));
}

#[test]
fn context_none_for_ping() {
    let req = Request::Ping;
    assert!(build_authorizer_context(&req, 0, 0, None, vec![0], false).is_none());
}

#[test]
fn context_none_for_user_rights() {
    let req = Request::UserRights { config_path: "/test".into() };
    assert!(build_authorizer_context(&req, 0, 0, None, vec![0], false).is_none());
}

#[test]
fn context_none_for_global_status() {
    let req = Request::Status { config_path: None };
    assert!(build_authorizer_context(&req, 0, 0, None, vec![0], false).is_none());
}

#[test]
fn context_some_for_per_config_status() {
    let req = Request::Status { config_path: Some("/test".into()) };
    let ctx = build_authorizer_context(&req, 1000, 1000, None, vec![1000], false).unwrap();
    assert_eq!(ctx.action, "status");
}

// ---------------------------------------------------------------------------
// Async worker integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_handle_allows() {
    let (handle, compiled) = AclLuaHandle::new(
        None,
        &[(1, "return true")],
    ).unwrap();

    let key = compiled[&1].clone();
    let ctx = make_context("start");
    let result = handle.check(vec![key], ctx).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn worker_handle_denies() {
    let (handle, compiled) = AclLuaHandle::new(
        None,
        &[(1, "return false")],
    ).unwrap();

    let key = compiled[&1].clone();
    let ctx = make_context("start");
    let result = handle.check(vec![key], ctx).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn worker_with_shared_lua() {
    let (handle, compiled) = AclLuaHandle::new(
        Some("function allowed_action(a) return a == 'start' or a == 'status' end"),
        &[(1, "return allowed_action(request.action)")],
    ).unwrap();

    let key = compiled[&1].clone();

    // start → allowed
    let ctx = make_context("start");
    assert!(handle.check(vec![key.clone()], ctx).await.is_ok());

    // stop → denied
    let ctx = make_context("stop");
    assert!(handle.check(vec![key], ctx).await.is_err());
}

#[tokio::test]
async fn worker_empty_keys_allows() {
    let (handle, _) = AclLuaHandle::new(None, &[]).unwrap();
    let ctx = make_context("start");
    let result = handle.check(vec![], ctx).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn worker_rebuild_replaces_vm() {
    let (handle, compiled) = AclLuaHandle::new(
        None,
        &[(1, "return true")],
    ).unwrap();

    // Initially allows
    let key = compiled[&1].clone();
    let ctx = make_context("start");
    assert!(handle.check(vec![key], ctx).await.is_ok());

    // Rebuild with denying authorizer
    let new_compiled = handle.rebuild(
        None,
        &[(1, "return false")],
    ).await.unwrap();

    let key = new_compiled[&1].clone();
    let ctx = make_context("start");
    assert!(handle.check(vec![key], ctx).await.is_err());
}
