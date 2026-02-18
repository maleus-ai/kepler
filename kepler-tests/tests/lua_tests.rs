//! Integration tests for Lua scripting support in config files
//!
//! Tests the !lua and !lua_file YAML tags for dynamic config generation.

use kepler_daemon::config::KeplerConfig;
use kepler_daemon::lua_eval::EvalContext;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

/// Lock to serialize tests that modify process environment variables.
/// Without this, parallel tests calling set_var/remove_var race with
/// std::env::vars() in load_config_from_string, causing flaky failures.
/// Uses parking_lot::Mutex which does not poison on panic.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Helper to write a config file and load it with test process's environment
fn load_config_from_string(yaml: &str, dir: &Path) -> Result<KeplerConfig, String> {
    let config_path = dir.join("kepler.yaml");
    std::fs::write(&config_path, yaml).map_err(|e| e.to_string())?;
    // Capture test process's environment for Lua scripts to access
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    KeplerConfig::load(&config_path, &sys_env).map_err(|e| e.to_string())
}

/// Helper to resolve a service config with Lua evaluation and ${{ env.VAR }}$ expansion.
/// Uses the current process environment as sys_env.
fn resolve_svc(config: &KeplerConfig, name: &str, config_path: &Path) -> kepler_daemon::config::ServiceConfig {
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    resolve_svc_with_env(config, name, config_path, &sys_env)
}

/// Helper to resolve a service config with an explicit sys_env.
fn resolve_svc_with_env(
    config: &KeplerConfig,
    name: &str,
    config_path: &Path,
    sys_env: &HashMap<String, String>,
) -> kepler_daemon::config::ServiceConfig {
    let mut ctx = EvalContext {
        sys_env: sys_env.clone(),
        env: sys_env.clone(),
        ..Default::default()
    };
    let evaluator = config.create_lua_evaluator().unwrap();
    config.resolve_service(name, &mut ctx, &evaluator, config_path, None).unwrap()
}

/// Helper to attempt resolving a service config, returning an error string on failure.
fn try_resolve_svc(config: &KeplerConfig, name: &str, config_path: &Path) -> Result<kepler_daemon::config::ServiceConfig, String> {
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    let mut ctx = EvalContext {
        sys_env: sys_env.clone(),
        env: sys_env,
        ..Default::default()
    };
    let evaluator = config.create_lua_evaluator().map_err(|e| e.to_string())?;
    config.resolve_service(name, &mut ctx, &evaluator, config_path, None).map_err(|e| e.to_string())
}

/// Basic test: !lua tag returns a simple string
#[test]
fn test_lua_returns_string() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    working_dir: !lua |
      return "/tmp/test"
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(
        service.working_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        Some("/tmp/test".to_string())
    );
}

/// Test: !lua tag returns an array
#[test]
fn test_lua_returns_array() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: !lua |
      return {"echo", "hello", "world"}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.command, vec!["echo", "hello", "world"]);
}

/// Test: !lua accesses ctx.env table
#[test]
fn test_lua_env_access() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    // Set an env var that the Lua script will read
    unsafe {
        std::env::set_var("KEPLER_LUA_TEST_VAR", "test_value");
    }

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      return {"MY_VAR=" .. (ctx.env.KEPLER_LUA_TEST_VAR or "default")}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_LUA_TEST_VAR");
    }

    assert!(service.environment.contains(&"MY_VAR=test_value".to_string()));
}

/// Test: ctx.env table is read-only
#[test]
fn test_lua_env_readonly() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    working_dir: !lua |
      ctx.env.NEW_VAR = "should fail"
      return "/tmp"
"#;

    // With lazy evaluation, load succeeds (raw !lua tag is stored)
    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    // The error happens when resolving the service
    let result = try_resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("read-only") || err.contains("readonly"),
        "Expected 'read-only' or 'readonly' in error: {err}"
    );
}

/// Test: lua: block defines functions available to !lua tags
#[test]
fn test_lua_function_definition() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
lua: |
  function get_port()
    return "8080"
  end

services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      return {"PORT=" .. get_port()}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert!(service.environment.contains(&"PORT=8080".to_string()));
}

/// Test: global table for sharing state between !lua blocks
#[test]
fn test_lua_global_shared_state() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
lua: |
  global.shared_value = "from_lua_block"

services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      return {"SHARED=" .. global.shared_value}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert!(service.environment.contains(&"SHARED=from_lua_block".to_string()));
}

/// Test: ctx.service_name variable is available
#[test]
fn test_lua_service_context() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  myservice:
    command: ["echo", "hello"]
    environment: !lua |
      return {"SERVICE_NAME=" .. ctx.service_name}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "myservice", &temp_dir.path().join("kepler.yaml"));

    assert!(service.environment.contains(&"SERVICE_NAME=myservice".to_string()));
}


/// Test: environment array format from Lua
#[test]
fn test_lua_env_array_format() {
    let temp_dir = TempDir::new().unwrap();

    // Environment must be returned as an array of "KEY=value" strings
    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      return {"FOO=bar", "BAZ=qux"}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert!(
        service.environment.iter().any(|e| e == "FOO=bar"),
        "Expected FOO=bar in {:?}",
        service.environment
    );
    assert!(
        service.environment.iter().any(|e| e == "BAZ=qux"),
        "Expected BAZ=qux in {:?}",
        service.environment
    );
}

/// Test: conditional config based on environment
#[test]
fn test_lua_conditional_config() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    unsafe {
        std::env::set_var("KEPLER_ENV", "production");
    }

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      if ctx.env.KEPLER_ENV == "production" then
        return {"LOG_LEVEL=warn"}
      else
        return {"LOG_LEVEL=debug"}
      end
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_ENV");
    }

    assert!(service.environment.contains(&"LOG_LEVEL=warn".to_string()));
}

/// Test: healthcheck test array from Lua
#[test]
fn test_lua_healthcheck() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    unsafe {
        std::env::set_var("KEPLER_HEALTH_PORT", "8080");
    }

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    healthcheck:
      test: !lua |
        local port = ctx.env.KEPLER_HEALTH_PORT or "80"
        return {"sh", "-c", "curl -f http://localhost:" .. port .. "/health"}
      interval: 10s
      timeout: 5s
      retries: 3
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_HEALTH_PORT");
    }

    let healthcheck = service.healthcheck.as_ref().unwrap();
    assert_eq!(
        *healthcheck.test.as_static().unwrap(),
        vec!["sh", "-c", "curl -f http://localhost:8080/health"]
    );
}

/// Test: standard library functions are available
#[test]
fn test_lua_standard_library() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      local upper = string.upper("hello")
      local max = math.max(1, 5, 3)
      return {"UPPER=" .. upper, "MAX=" .. tostring(max)}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert!(service.environment.contains(&"UPPER=HELLO".to_string()));
    assert!(service.environment.contains(&"MAX=5".to_string()));
}

/// Test: env_file evaluation order (Lua sees system vars via ctx.sys_env)
#[test]
fn test_lua_env_file_evaluation_order() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    unsafe {
        std::env::set_var("KEPLER_BASE_PATH", "/opt/app");
    }

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    env_file: !lua |
      return ctx.sys_env.KEPLER_BASE_PATH .. "/.env"
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_BASE_PATH");
    }

    assert_eq!(
        service.env_file.as_ref().map(|p| p.to_string_lossy().to_string()),
        Some("/opt/app/.env".to_string())
    );
}

/// Test: environment transformation using inline Lua
/// Note: The Lua code must return an array directly without using table.insert
/// in a helper function, as Luau tables created with table.insert may not
/// preserve the array-like structure when passed between functions.
#[test]
fn test_lua_env_transform() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    // Set some test env vars
    unsafe {
        std::env::set_var("KEPLER_BACKEND_FOO", "1");
        std::env::set_var("KEPLER_BACKEND_BAR", "2");
        std::env::set_var("KEPLER_FRONTEND_BAZ", "3");
    }

    // Inline transformation in the !lua block
    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    sys_env: clear
    environment: !lua |
      local result = {}
      local prefix = "KEPLER_BACKEND_"
      local new_prefix = "APP_"
      for key, value in pairs(ctx.env) do
        if string.sub(key, 1, #prefix) == prefix then
          local new_key = new_prefix .. string.sub(key, #prefix + 1)
          table.insert(result, new_key .. "=" .. value)
        end
      end
      return result
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_BACKEND_FOO");
        std::env::remove_var("KEPLER_BACKEND_BAR");
        std::env::remove_var("KEPLER_FRONTEND_BAZ");
    }

    assert!(
        service.environment.iter().any(|e| e == "APP_FOO=1"),
        "Expected APP_FOO=1 in {:?}",
        service.environment
    );
    assert!(
        service.environment.iter().any(|e| e == "APP_BAR=2"),
        "Expected APP_BAR=2 in {:?}",
        service.environment
    );
    // FRONTEND should not be included
    assert!(
        !service.environment.iter().any(|e| e.contains("BAZ")),
        "Should not contain BAZ in {:?}",
        service.environment
    );
}

/// Test: error handling for invalid Lua code
#[test]
fn test_lua_syntax_error() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: !lua |
      return {"echo" "hello"}  -- missing comma
"#;

    // With lazy evaluation, load succeeds (raw !lua tag is stored)
    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    // The error happens when resolving the service
    let result = try_resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));
    assert!(result.is_err());
    // The error should mention it's a Lua error
    let err = result.unwrap_err();
    assert!(err.contains("Lua") || err.contains("lua"), "Error: {}", err);
}

/// Test: nil values are handled correctly
#[test]
fn test_lua_nil_handling() {
    let temp_dir = TempDir::new().unwrap();

    // When env var is not set, ctx.env.NONEXISTENT should be nil
    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      local val = ctx.env.KEPLER_NONEXISTENT_VAR
      if val == nil then
        return {"STATUS=not_found"}
      else
        return {"STATUS=found"}
      end
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert!(service.environment.contains(&"STATUS=not_found".to_string()));
}

/// Test: boolean values in Lua
#[test]
fn test_lua_boolean_to_string() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      return {"ENABLED=" .. tostring(true), "DISABLED=" .. tostring(false)}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert!(service.environment.contains(&"ENABLED=true".to_string()));
    assert!(service.environment.contains(&"DISABLED=false".to_string()));
}

/// Test: command arrays must return strings (numbers need tostring())
#[test]
fn test_lua_command_with_numbers() {
    let temp_dir = TempDir::new().unwrap();

    // Numbers in command arrays must be converted to strings using tostring()
    let yaml = r#"
services:
  test:
    command: !lua |
      local seconds = 10
      return {"sleep", tostring(seconds)}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.command, vec!["sleep", "10"]);
}

/// Test: backward compatibility - configs without Lua work as before
#[test]
fn test_backward_compatibility() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    environment:
      - FOO=bar
      - BAZ=qux
    restart: always
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.command, vec!["echo", "hello"]);
    assert!(service.environment.contains(&"FOO=bar".to_string()));
    assert!(service.environment.contains(&"BAZ=qux".to_string()));
}

/// Test: !lua tag works for global logs configuration (kepler.logs)
#[test]
fn test_lua_global_logs_config() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    // Set env vars for Lua scripts to use
    unsafe {
        std::env::set_var("KEPLER_LOG_MAX_SIZE", "50M");
        std::env::set_var("KEPLER_LOG_BUFFER", "16384");
    }

    let yaml = r#"
kepler:
  logs:
    max_size: !lua |
      return ctx.env.KEPLER_LOG_MAX_SIZE or "10M"
    buffer_size: !lua |
      return tonumber(ctx.env.KEPLER_LOG_BUFFER) or 0

services:
  test:
    command: ["echo", "hello"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_LOG_MAX_SIZE");
        std::env::remove_var("KEPLER_LOG_BUFFER");
    }

    let logs = config.global_logs().expect("kepler.logs should exist");
    assert_eq!(*logs.max_size.as_static().unwrap(), Some("50M".to_string()));
    assert_eq!(*logs.buffer_size.as_static().unwrap(), Some(16384));
}

/// Test: !lua tag works for service-level logs configuration
#[test]
fn test_lua_service_logs_config() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    // Set env vars for Lua scripts to use
    unsafe {
        std::env::set_var("KEPLER_SERVICE_MAX_SIZE", "100M");
    }

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
    logs:
      max_size: !lua |
        return ctx.env.KEPLER_SERVICE_MAX_SIZE or "20M"
      buffer_size: !lua |
        return 0  -- sync writes for this service
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();

    // Resolve while env var is still set (lazy evaluation happens at resolve time)
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    // Cleanup after resolve since Lua runs at resolve time
    unsafe {
        std::env::remove_var("KEPLER_SERVICE_MAX_SIZE");
    }

    let logs = service.logs.as_ref().expect("service.logs should exist");
    assert_eq!(*logs.max_size.as_static().unwrap(), Some("100M".to_string()));
    assert_eq!(*logs.buffer_size.as_static().unwrap(), Some(0));
}

/// Test: depends_on simple list with static service names
#[test]
fn test_lua_depends_on_simple_list() {
    use kepler_daemon::config::DependencyCondition;
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  database:
    command: ["echo", "db"]
  cache:
    command: ["echo", "cache"]
  backend:
    command: ["echo", "backend"]
    depends_on: ["database", "cache"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "backend", &temp_dir.path().join("kepler.yaml"));

    let deps = service.depends_on.names();
    assert!(deps.contains(&"database".to_string()));
    assert!(deps.contains(&"cache".to_string()));

    // Should default to service_started condition
    let db_config = service.depends_on.get("database").unwrap();
    assert_eq!(db_config.condition, DependencyCondition::ServiceStarted);
}

/// Test: depends_on extended form with static service names and conditions
#[test]
fn test_lua_depends_on_extended_form() {
    use kepler_daemon::config::DependencyCondition;
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  database:
    command: ["echo", "db"]
  backend:
    command: ["echo", "backend"]
    depends_on:
      database:
        condition: service_healthy
        timeout: "30s"
        restart: true
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "backend", &temp_dir.path().join("kepler.yaml"));

    let db_config = service.depends_on.get("database").unwrap();
    assert_eq!(db_config.condition, DependencyCondition::ServiceHealthy);
    assert_eq!(db_config.timeout, Some(std::time::Duration::from_secs(30)));
    assert!(db_config.restart);
}

/// Test: depends_on with service_completed_successfully condition
#[test]
fn test_lua_depends_on_completed_successfully() {
    use kepler_daemon::config::DependencyCondition;
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  init:
    command: ["echo", "init"]
  app:
    command: ["echo", "app"]
    depends_on:
      init:
        condition: service_completed_successfully
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "app", &temp_dir.path().join("kepler.yaml"));

    let init_config = service.depends_on.get("init").unwrap();
    assert_eq!(init_config.condition, DependencyCondition::ServiceCompletedSuccessfully);
}

/// Test: !lua tag works for individual fields within depends_on
#[test]
fn test_lua_depends_on_individual_fields() {
    let _guard = ENV_LOCK.lock();
    use kepler_daemon::config::DependencyCondition;
    let temp_dir = TempDir::new().unwrap();

    // Set env var to control the condition
    unsafe {
        std::env::set_var("KEPLER_DEP_CONDITION", "service_healthy");
        std::env::set_var("KEPLER_DEP_TIMEOUT", "60s");
    }

    let yaml = r#"
services:
  database:
    command: ["echo", "db"]
  backend:
    command: ["echo", "backend"]
    depends_on:
      database:
        condition: !lua |
          return ctx.env.KEPLER_DEP_CONDITION or "service_started"
        timeout: !lua |
          return ctx.env.KEPLER_DEP_TIMEOUT or "30s"
        restart: !lua |
          return true
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "backend", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_DEP_CONDITION");
        std::env::remove_var("KEPLER_DEP_TIMEOUT");
    }

    let db_config = service.depends_on.get("database").unwrap();

    assert_eq!(db_config.condition, DependencyCondition::ServiceHealthy);
    assert_eq!(db_config.timeout, Some(std::time::Duration::from_secs(60)));
    assert!(db_config.restart);
}

/// Test: depends_on with static service names and per-field !lua for conditional config
#[test]
fn test_lua_depends_on_conditional() {
    let _guard = ENV_LOCK.lock();
    let temp_dir = TempDir::new().unwrap();

    // Set env var to control condition
    unsafe {
        std::env::set_var("KEPLER_USE_CACHE", "true");
    }

    let yaml = r#"
services:
  database:
    command: ["echo", "db"]
  cache:
    command: ["echo", "cache"]
  backend:
    command: ["echo", "backend"]
    depends_on:
      database:
        condition: service_healthy
      cache:
        condition: service_started
        restart: !lua |
          return ctx.env.KEPLER_USE_CACHE == "true"
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "backend", &temp_dir.path().join("kepler.yaml"));

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_USE_CACHE");
    }

    let deps = service.depends_on.names();

    assert!(deps.contains(&"database".to_string()));
    assert!(deps.contains(&"cache".to_string()));

    // Cache should have restart: true (from !lua evaluation)
    assert!(service.depends_on.should_restart_on_dependency("cache"));
    // Database should not have restart (defaults to false)
    assert!(!service.depends_on.should_restart_on_dependency("database"));
}

/// Test: Static depends_on with new dependency conditions
#[test]
fn test_lua_depends_on_new_conditions() {
    use kepler_daemon::config::DependencyCondition;
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  worker:
    command: ["echo", "worker"]
  monitor:
    command: ["echo", "monitor"]
    depends_on:
      worker:
        condition: service_unhealthy
  handler:
    command: ["echo", "handler"]
    depends_on:
      worker:
        condition: service_failed
  cleanup:
    command: ["echo", "cleanup"]
    depends_on:
      worker:
        condition: service_stopped
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let monitor = resolve_svc(&config, "monitor", &config_path);
    let dep = monitor.depends_on.get("worker").unwrap();
    assert_eq!(dep.condition, DependencyCondition::ServiceUnhealthy);

    let handler = resolve_svc(&config, "handler", &config_path);
    let dep = handler.depends_on.get("worker").unwrap();
    assert_eq!(dep.condition, DependencyCondition::ServiceFailed);

    let cleanup = resolve_svc(&config, "cleanup", &config_path);
    let dep = cleanup.depends_on.get("worker").unwrap();
    assert_eq!(dep.condition, DependencyCondition::ServiceStopped);
}

/// Test: Static depends_on with exit_code mixed types (ranges and single values)
#[test]
fn test_lua_depends_on_exit_code_mixed() {
    use kepler_daemon::config::DependencyCondition;
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  worker:
    command: ["echo", "worker"]
  handler:
    command: ["echo", "handler"]
    depends_on:
      worker:
        condition: service_failed
        exit_code: ["1:10", 42]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let handler = resolve_svc(&config, "handler", &temp_dir.path().join("kepler.yaml"));
    let dep = handler.depends_on.get("worker").unwrap();
    assert_eq!(dep.condition, DependencyCondition::ServiceFailed);
    assert!(!dep.exit_code.is_empty());
    assert!(dep.exit_code.matches(1));
    assert!(dep.exit_code.matches(5));
    assert!(dep.exit_code.matches(10));
    assert!(dep.exit_code.matches(42));
    assert!(!dep.exit_code.matches(15));
    assert!(!dep.exit_code.matches(0));
}

/// Test: Lua individual field templating for exit_code
#[test]
fn test_lua_depends_on_exit_code_field() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  worker:
    command: ["echo", "worker"]
  handler:
    command: ["echo", "handler"]
    depends_on:
      worker:
        condition: service_stopped
        exit_code: !lua |
          return {"1:10"}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let handler = resolve_svc(&config, "handler", &temp_dir.path().join("kepler.yaml"));
    let dep = handler.depends_on.get("worker").unwrap();
    assert!(dep.exit_code.matches(1));
    assert!(dep.exit_code.matches(10));
    assert!(!dep.exit_code.matches(11));
}

// ============================================================================
// Service-level `if:` condition tests (boolean ConfigValue)
// ============================================================================

/// Test: `if: true` resolves to Some(true)
#[test]
fn test_service_if_static_true() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    if: true
    command: ["echo", "hello"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.condition, Some(true));
}

/// Test: `if: false` resolves to Some(false)
#[test]
fn test_service_if_static_false() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    if: false
    command: ["echo", "hello"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.condition, Some(false));
}

/// Test: `if: ${{ 1 == 1 }}$` (dynamic expression) resolves to Some(true)
#[test]
fn test_service_if_dynamic_expression() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    if: ${{ 1 == 1 }}$
    command: ["echo", "hello"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    assert!(config.services.get("test").unwrap().condition.is_dynamic());
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.condition, Some(true));
}

/// Test: `if: !lua | return true` resolves to Some(true)
#[test]
fn test_service_if_lua_block() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    if: !lua |
      return true
    command: ["echo", "hello"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    assert!(config.services.get("test").unwrap().condition.is_dynamic());
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.condition, Some(true));
}

/// Test: no `if:` field resolves to None
#[test]
fn test_service_if_absent() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: ["echo", "hello"]
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = resolve_svc(&config, "test", &temp_dir.path().join("kepler.yaml"));

    assert_eq!(service.condition, None);
}
