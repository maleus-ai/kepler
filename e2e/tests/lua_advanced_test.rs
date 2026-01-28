//! E2E tests for advanced Lua scripting features
//!
//! Tests execution order, global table sharing, context availability in hooks,
//! and !lua usage in various config fields.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "lua_advanced_test";

/// Test that multiple !lua blocks can share data through the global table
#[tokio::test]
async fn test_lua_global_table_sharing() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_global_table_sharing")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "global-sharing-service", "running", Duration::from_secs(10))
        .await?;

    // The service should output SHARED_VALUE which was set in lua: block
    // and accessed in a !lua block
    let logs = harness
        .wait_for_log_content(&config_path, "SHARED_VALUE=shared_data", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("SHARED_VALUE=shared_data"),
        "Global table should share data between lua: and !lua blocks. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that env_file !lua runs before environment !lua
#[tokio::test]
async fn test_lua_execution_order_env_file_first() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create a simple env file
    let env_file = harness.create_temp_file("order.env", "FROM_FILE=file_value")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_execution_order",
        &[("__ENV_FILE_PATH__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "order-service", "running", Duration::from_secs(10))
        .await?;

    // The ORDER env var should show: lua_block,env_file,environment
    let logs = harness
        .wait_for_log_content(&config_path, "ORDER=", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("ORDER=lua_block,env_file,environment"),
        "Execution order should be lua: -> env_file -> environment. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that env_file content is available in subsequent !lua blocks
#[tokio::test]
async fn test_lua_env_file_injects_vars() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with a variable
    let env_file = harness.create_temp_file("inject.env", "INJECTED_VAR=injected_value")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_env_file_injects_vars",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "inject-service", "running", Duration::from_secs(10))
        .await?;

    // The !lua in environment should have access to INJECTED_VAR from env_file
    let logs = harness
        .wait_for_log_content(&config_path, "FROM_LUA=injected_value", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("FROM_LUA=injected_value"),
        "env_file vars should be available in environment !lua. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that lua: block runs before lua_import:
#[tokio::test]
async fn test_lua_block_before_import() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Copy the lua import file and get its absolute path
    let lua_file = harness.copy_supporting_file(TEST_MODULE, "order_tracker.lua")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_block_before_import",
        &[("__LUA_FILE__", lua_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "import-order-service", "running", Duration::from_secs(10))
        .await?;

    // Should show lua_block comes before lua_import
    let logs = harness
        .wait_for_log_content(&config_path, "LOAD_ORDER=", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("LOAD_ORDER=lua_block,lua_import"),
        "lua: should run before lua_import:. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that !lua blocks in hooks have access to service name
#[tokio::test]
async fn test_lua_service_context_in_hooks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("hook_context_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_service_context_in_hooks",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "hook-context-service", "running", Duration::from_secs(10))
        .await?;

    // Give hook time to run
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Check marker file - should contain the service name from !lua in hook
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("SERVICE=hook-context-service"),
        "Hook !lua should have access to service name. Marker: {}",
        marker_content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test !lua in working_dir field
#[tokio::test]
async fn test_lua_working_dir() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let work_dir = harness.create_temp_dir("lua_workdir")?;

    // Create env file with WORK_DIR
    let env_content = format!("WORK_DIR={}", work_dir.to_string_lossy());
    let env_file = harness.create_temp_file("lua_wd.env", &env_content)?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_working_dir",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "lua-wd-service", "running", Duration::from_secs(10))
        .await?;

    // Service runs pwd, should output the lua-generated working_dir
    let logs = harness
        .wait_for_log_content(&config_path, &work_dir.to_string_lossy(), Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains(&work_dir.to_string_lossy()),
        "!lua in working_dir should work. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test !lua in depends_on field
#[tokio::test]
async fn test_lua_depends_on() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_depends_on")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for both services
    harness
        .wait_for_service_status(&config_path, "dep-target", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "dep-source", "running", Duration::from_secs(10))
        .await?;

    // Check logs - dep-target should start before dep-source
    let logs = harness
        .wait_for_log_content(&config_path, "DEP_SOURCE_START", Duration::from_secs(5))
        .await?;

    let pos_target = logs.stdout.find("DEP_TARGET_START");
    let pos_source = logs.stdout.find("DEP_SOURCE_START");

    assert!(
        pos_target < pos_source,
        "Lua-generated depends_on should work. Target pos: {:?}, Source pos: {:?}",
        pos_target, pos_source
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test !lua in restart.watch field
#[tokio::test]
async fn test_lua_restart_watch() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let watch_dir = harness.create_temp_dir("lua_watch")?;
    let watch_file = watch_dir.join("test.txt");
    std::fs::write(&watch_file, "initial")?;

    // Create env file with WATCH_DIR
    let env_content = format!("WATCH_DIR={}", watch_dir.to_string_lossy());
    let env_file = harness.create_temp_file("lua_watch.env", &env_content)?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_restart_watch",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "lua-watch-service", "running", Duration::from_secs(10))
        .await?;

    harness
        .wait_for_log_content(&config_path, "LUA_WATCH_START", Duration::from_secs(5))
        .await?;

    // Modify watched file
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.modify_file(&watch_file, "modified")?;

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("LUA_WATCH_START").count();

    assert!(
        start_count >= 2,
        "Lua-generated watch pattern should trigger restart. Count: {}",
        start_count
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test multiple services have isolated Lua context (service variable differs)
#[tokio::test]
async fn test_lua_multiple_services_isolated_context() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_multiple_services")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "multi-svc-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "multi-svc-b", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "SVC_NAME=multi-svc-b", Duration::from_secs(5))
        .await?;

    // Each service should see its own name in the service variable
    assert!(
        logs.stdout_contains("SVC_NAME=multi-svc-a"),
        "Service A should see its own name. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("SVC_NAME=multi-svc-b"),
        "Service B should see its own name. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that ctx.env table is read-only in !lua blocks
#[tokio::test]
async fn test_lua_env_readonly() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_env_readonly")?;

    harness.start_daemon().await?;

    // This should fail to start because the Lua code tries to modify ctx.env
    let output = harness.start_services(&config_path).await?;

    // The service should fail to load due to Lua error
    assert!(
        !output.success() || output.stderr_contains("read-only") || output.stderr_contains("error"),
        "Modifying ctx.env should fail. exit: {}, stderr: {}",
        output.exit_code, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that ctx table is read-only in !lua blocks
#[tokio::test]
async fn test_lua_ctx_readonly() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_ctx_readonly")?;

    harness.start_daemon().await?;

    // This should fail to start because the Lua code tries to modify ctx
    let output = harness.start_services(&config_path).await?;

    // The service should fail to load due to Lua error
    assert!(
        !output.success() || output.stderr_contains("read-only") || output.stderr_contains("error"),
        "Modifying ctx should fail. exit: {}, stderr: {}",
        output.exit_code, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test granular ctx.sys_env, ctx.env_file, and ctx.env separation
#[tokio::test]
async fn test_lua_ctx_granular_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with a variable
    let env_file = harness.create_temp_file("granular.env", "FILE_VAR=from_file")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_ctx_granular_env",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-layers", "running", Duration::from_secs(10))
        .await?;

    // Check the output verifies all env layer checks passed
    let logs = harness
        .wait_for_log_content(&config_path, "ALL_PASS=", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("ALL_PASS=true"),
        "All granular env checks should pass. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test execution order of all Lua-evaluated fields
#[tokio::test]
async fn test_lua_execution_order_all_fields() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create a simple env file
    let env_file = harness.create_temp_file("order_all.env", "DUMMY=1")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_execution_order_all_fields",
        &[("__ENV_FILE_PATH__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service - it has a healthcheck so it may transition quickly to "healthy"
    harness
        .wait_for_service_status_any(&config_path, "order-test", &["running", "healthy"], Duration::from_secs(10))
        .await?;

    // Check the execution order
    let logs = harness
        .wait_for_log_content(&config_path, "FINAL_ORDER=", Duration::from_secs(5))
        .await?;

    // env_file and environment should be evaluated before other fields
    assert!(
        logs.stdout_contains("ORDER=env_file,environment"),
        "env_file and environment should be evaluated before command captures ORDER. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that lua: block is loaded before lua_import (regardless of YAML order)
#[tokio::test]
async fn test_lua_import_after_block() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Copy the lua import file
    let lua_file = harness.copy_supporting_file(TEST_MODULE, "import_order_tracker.lua")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_import_before_block",
        &[("__LUA_FILE__", lua_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "import-first-service", "running", Duration::from_secs(10))
        .await?;

    // lua: block should run first, then lua_import
    let logs = harness
        .wait_for_log_content(&config_path, "LOAD_ORDER=", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("LOAD_ORDER=lua_block,lua_import"),
        "lua: block should execute before lua_import. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test complex integration with multiple services, hooks, and dependencies
#[tokio::test]
async fn test_lua_complex_integration() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file for api service
    let api_env_file = harness.create_temp_file("api.env", "DATABASE_URL=postgres://localhost/test\nWATCH_DIR=/src")?;
    let marker_file = harness.create_temp_file("complex_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_lua_complex_integration",
        &[
            ("__API_ENV_FILE__", api_env_file.to_str().unwrap()),
            ("__MARKER_FILE__", marker_file.to_str().unwrap()),
        ],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for both services
    harness
        .wait_for_service_status(&config_path, "api", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Give hooks time to run
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check api service output
    let logs = harness.get_logs(&config_path, None, 100).await?;

    // API should have port 8000 (first call to next_port)
    assert!(
        logs.stdout_contains("Starting api on port 8000"),
        "API should have port 8000. stdout: {}",
        logs.stdout
    );

    // API should have the database URL from env_file
    assert!(
        logs.stdout_contains("DB=postgres://localhost/test"),
        "API should have DATABASE_URL from env_file. stdout: {}",
        logs.stdout
    );

    // Worker should have port 8001 (second call to next_port)
    assert!(
        logs.stdout_contains("Worker worker on port 8001"),
        "Worker should have port 8001. stdout: {}",
        logs.stdout
    );

    // Worker should see api in registered services (api registers first due to depends_on ordering)
    assert!(
        logs.stdout_contains("registered=api") || logs.stdout_contains("registered=api,worker"),
        "Worker should see registered services. stdout: {}",
        logs.stdout
    );

    // Check marker file for hook execution
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("Hook on_start for api"),
        "API on_start hook should have run with correct context. Marker: {}",
        marker_content
    );

    harness.stop_daemon().await?;
    Ok(())
}
