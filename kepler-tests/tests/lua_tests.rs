//! Integration tests for Lua scripting support in config files
//!
//! Tests the !lua and !lua_file YAML tags for dynamic config generation.

use kepler_daemon::config::KeplerConfig;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

/// Helper to write a config file and load it with test process's environment
fn load_config_from_string(yaml: &str, dir: &Path) -> Result<KeplerConfig, String> {
    let config_path = dir.join("kepler.yaml");
    std::fs::write(&config_path, yaml).map_err(|e| e.to_string())?;
    // Capture test process's environment for Lua scripts to access
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    KeplerConfig::load(&config_path, &sys_env).map_err(|e| e.to_string())
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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

    assert_eq!(service.command, vec!["echo", "hello", "world"]);
}

/// Test: !lua accesses ctx.env table
#[test]
fn test_lua_env_access() {
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
    let service = config.services.get("test").unwrap();

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

    let result = load_config_from_string(yaml, temp_dir.path());
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("read-only"));
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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("myservice").unwrap();

    assert!(service.environment.contains(&"SERVICE_NAME=myservice".to_string()));
}

/// Test: !lua_file loads and executes external Lua file
#[test]
fn test_lua_file_tag() {
    let temp_dir = TempDir::new().unwrap();

    // Create an external Lua file
    let lua_file_path = temp_dir.path().join("get_command.lua");
    std::fs::write(&lua_file_path, r#"return {"sh", "-c", "echo hello"}"#).unwrap();

    let yaml = format!(
        r#"
services:
  test:
    command: !lua_file {}
"#,
        lua_file_path.display()
    );

    let config = load_config_from_string(&yaml, temp_dir.path()).unwrap();
    let service = config.services.get("test").unwrap();

    assert_eq!(service.command, vec!["sh", "-c", "echo hello"]);
}

/// Test: require() loads external Lua files
#[test]
fn test_lua_require() {
    let temp_dir = TempDir::new().unwrap();

    // Create an external Lua module
    let lua_file_path = temp_dir.path().join("helpers.lua");
    std::fs::write(
        &lua_file_path,
        r#"
local M = {}
function M.get_default_port()
  return "3000"
end
return M
"#,
    )
    .unwrap();

    let yaml = r#"
lua: |
  local helpers = require("helpers")
  global.port = helpers.get_default_port()

services:
  test:
    command: ["echo", "hello"]
    environment: !lua |
      return {"PORT=" .. global.port}
"#;

    let config = load_config_from_string(yaml, temp_dir.path()).unwrap();
    let service = config.services.get("test").unwrap();

    assert!(service.environment.contains(&"PORT=3000".to_string()));
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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_ENV");
    }

    assert!(service.environment.contains(&"LOG_LEVEL=warn".to_string()));
}

/// Test: healthcheck test array from Lua
#[test]
fn test_lua_healthcheck() {
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
    let service = config.services.get("test").unwrap();

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_HEALTH_PORT");
    }

    let healthcheck = service.healthcheck.as_ref().unwrap();
    assert_eq!(
        healthcheck.test,
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
    let service = config.services.get("test").unwrap();

    assert!(service.environment.contains(&"UPPER=HELLO".to_string()));
    assert!(service.environment.contains(&"MAX=5".to_string()));
}

/// Test: env_file evaluation order (Lua sees system vars via ctx.sys_env)
#[test]
fn test_lua_env_file_evaluation_order() {
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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

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

    let result = load_config_from_string(yaml, temp_dir.path());
    assert!(result.is_err());
    // The error should mention it's a Lua error
    let err = result.unwrap_err();
    assert!(err.contains("Lua") || err.contains("lua"), "Error: {}", err);
}

/// Test: error handling for missing !lua_file
#[test]
fn test_lua_file_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let yaml = r#"
services:
  test:
    command: !lua_file /nonexistent/file.lua
"#;

    let result = load_config_from_string(yaml, temp_dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("Failed to read") || err.contains("not found") || err.contains("No such file"),
        "Error: {}",
        err
    );
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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

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
    let service = config.services.get("test").unwrap();

    assert_eq!(service.command, vec!["echo", "hello"]);
    assert!(service.environment.contains(&"FOO=bar".to_string()));
    assert!(service.environment.contains(&"BAZ=qux".to_string()));
}
