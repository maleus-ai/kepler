//! E2E tests for hook inherit_env feature
//!
//! Tests that hook inherit_env controls whether hooks inherit the service's
//! computed environment.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "hook_inherit_env_test";

/// Test that inherit_env: false hides service env but keeps hook's own env
#[tokio::test]
async fn test_hook_inherit_env_false_hides_service_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.load_config(
        TEST_MODULE,
        "test_hook_inherit_env_false",
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Hook output should show SERVICE_VAR=MISSING and HOOK_VAR=from_hook
    let logs = harness
        .wait_for_log_content(&config_path, "HOOK_VAR=from_hook", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("SERVICE_VAR=MISSING"),
        "inherit_env: false should hide service vars. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("HOOK_VAR=from_hook"),
        "Hook's own env should still work. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that inherit_env: true inherits service env
#[tokio::test]
async fn test_hook_inherit_env_true_inherits_service_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.load_config(
        TEST_MODULE,
        "test_hook_inherit_env_true",
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Hook output should show SERVICE_VAR=from_service
    let logs = harness
        .wait_for_log_content(&config_path, "SERVICE_VAR=from_service", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("SERVICE_VAR=from_service"),
        "inherit_env: true should see service vars. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that inherit_env: false still allows Lua service.env access in hook environment expressions
#[tokio::test]
async fn test_hook_inherit_env_false_lua_service_env_accessible() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.load_config(
        TEST_MODULE,
        "test_hook_inherit_env_false_lua_service_env",
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Hook uses ${{ service.env.SERVICE_VAR }}$ in its own environment to set CAPTURED
    let logs = harness
        .wait_for_log_content(&config_path, "CAPTURED=from_service", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("CAPTURED=from_service"),
        "Lua service.env should be accessible even with inherit_env: false. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
