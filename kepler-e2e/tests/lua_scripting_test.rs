//! E2E tests for Lua scripting in config generation
//!
//! Tests the !lua tag for dynamic command and value generation.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "lua_scripting_test";

/// Test that !lua tag generates command correctly
#[tokio::test]
async fn test_lua_command_generation() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_command_generation")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to produce logs
    harness
        .wait_for_service_status(&config_path, "lua-command-service", "running", Duration::from_secs(10))
        .await?;

    // The Lua-generated command should have produced this output
    harness.wait_for_log_content(&config_path, "LUA_GENERATED_COMMAND", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that Lua can access the env table
#[tokio::test]
async fn test_lua_environment_access() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_environment_access")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "lua-env-service", "running", Duration::from_secs(10))
        .await?;

    // Lua should have access to environment variables
    // The command is generated using env.HOME or similar
    let logs = harness.wait_for_log_content(&config_path, "ENV_ACCESS_WORKS", Duration::from_secs(5)).await?;
    logs.assert_success();

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that Lua has access to the service context variable
#[tokio::test]
async fn test_lua_service_context() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_service_context")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "lua-context-service", "running", Duration::from_secs(10))
        .await?;

    // The Lua script uses the service name in its output
    harness.wait_for_log_content(&config_path, "SERVICE_NAME=lua-context-service", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that lua: block defines global functions usable by !lua tags
#[tokio::test]
async fn test_lua_global_functions() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_lua_global_functions")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "lua-global-fn-service", "running", Duration::from_secs(10))
        .await?;

    // The Lua global function should have been used
    harness.wait_for_log_content(&config_path, "GLOBAL_FN_RESULT", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}
