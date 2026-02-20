//! E2E tests for sys_env architecture
//!
//! These tests verify that:
//! - Environment variables come from CLI, not daemon
//! - sys_env: inherit works correctly with CLI env
//! - sys_env: clear properly excludes system env

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "sys_env_test";

/// Test that sys_env comes from the CLI process, not the daemon process.
///
/// This test verifies the core sys_env architecture:
/// 1. Start daemon WITHOUT a specific test variable in its environment
/// 2. Run CLI WITH that variable set
/// 3. Verify the service sees the CLI's variable (proving it came from CLI, not daemon)
#[tokio::test]
async fn test_cli_env_used_not_daemon() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // The test variable that should come from CLI, not daemon
    const TEST_VAR: &str = "CLI_TEST_VAR";
    const TEST_VALUE: &str = "from_cli_not_daemon";

    // Start daemon with the test variable EXCLUDED from its environment
    // This ensures the daemon doesn't have CLI_TEST_VAR
    harness.start_daemon_with_clean_env(&[TEST_VAR]).await?;

    let config_path = harness.load_config(TEST_MODULE, "test_cli_env_used_not_daemon")?;

    // Start services WITH the test variable set in the CLI's environment
    let output = harness
        .start_services_with_env(&config_path, &[(TEST_VAR, TEST_VALUE)])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-checker", "running", Duration::from_secs(10))
        .await?;

    // Check logs - the service should see the CLI's environment variable
    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, TEST_VALUE),
            Duration::from_secs(5),
        )
        .await?;

    assert!(
        logs.stdout_contains(&format!("{}={}", TEST_VAR, TEST_VALUE)),
        "Service should see CLI's env var, not daemon's. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that sys_env: clear properly excludes CLI environment variables
#[tokio::test]
async fn test_sys_env_clear_excludes_cli_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    const TEST_VAR: &str = "CLEAR_TEST_VAR";
    const TEST_VALUE: &str = "should_not_appear";

    harness.start_daemon().await?;

    // Create a config that uses sys_env: clear
    let config_content = r#"
kepler:
  logs:
    buffer_size: 0
services:
  clear-checker:
    command: ["sh", "-c", "echo CLEAR_TEST_VAR=${CLEAR_TEST_VAR:-EMPTY} && sleep 30"]
    sys_env: clear
    restart: no
"#;
    let config_path = harness.create_test_config(config_content)?;

    // Start services with the test variable set
    let output = harness
        .start_services_with_env(&config_path, &[(TEST_VAR, TEST_VALUE)])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "clear-checker", "running", Duration::from_secs(10))
        .await?;

    // Check logs - with sys_env: clear, the CLI env should NOT be visible
    let logs = harness
        .wait_for_log_content(&config_path, "CLEAR_TEST_VAR=", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("CLEAR_TEST_VAR=EMPTY"),
        "With sys_env: clear, CLI env should NOT be inherited. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that restart preserves the baked config (does NOT use new CLI environment)
///
/// Note: Prior to the restart/recreate split, restart would re-bake config.
/// Now restart preserves baked config; use recreate to re-bake.
#[tokio::test]
async fn test_restart_preserves_baked_cli_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    const TEST_VAR: &str = "RESTART_TEST_VAR";
    const INITIAL_VALUE: &str = "initial_value";
    const UPDATED_VALUE: &str = "updated_value";

    harness.start_daemon().await?;

    let config_path = harness.load_config(TEST_MODULE, "test_cli_env_used_not_daemon")?;

    // Rename the variable in the config for this test
    let config_content = format!(
        r#"
kepler:
  logs:
    buffer_size: 0
services:
  env-checker:
    command: ["sh", "-c", "echo {}=${} && sleep 30"]
    sys_env: inherit
    restart: no
"#,
        TEST_VAR, TEST_VAR
    );
    harness.update_config(&config_path, &config_content)?;

    // Start with initial value
    let output = harness
        .start_services_with_env(&config_path, &[(TEST_VAR, INITIAL_VALUE)])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-checker", "running", Duration::from_secs(10))
        .await?;

    // Verify initial value
    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, INITIAL_VALUE),
            Duration::from_secs(5),
        )
        .await?;

    assert!(
        logs.stdout_contains(&format!("{}={}", TEST_VAR, INITIAL_VALUE)),
        "Initial start should have initial value. stdout: {}",
        logs.stdout
    );

    // Restart with updated value - should still use INITIAL value (baked config preserved)
    let restart_output = harness
        .run_cli_with_env(
            &["-f", config_path.to_str().unwrap(), "restart"],
            &[(TEST_VAR, UPDATED_VALUE)],
        )
        .await?;
    restart_output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-checker", "running", Duration::from_secs(10))
        .await?;

    // Wait a moment for logs to be written
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify logs still show INITIAL value (restart preserves baked config)
    let logs = harness.get_logs(&config_path, None, 100).await?;

    // Count occurrences - should have at least 2 initial values and no updated values
    let initial_count = logs.stdout.matches(&format!("{}={}", TEST_VAR, INITIAL_VALUE)).count();
    let updated_count = logs.stdout.matches(&format!("{}={}", TEST_VAR, UPDATED_VALUE)).count();

    assert!(
        initial_count >= 2,
        "Restart should preserve baked config (initial value). Expected at least 2 occurrences of '{}={}', got {}. stdout: {}",
        TEST_VAR,
        INITIAL_VALUE,
        initial_count,
        logs.stdout
    );
    assert_eq!(
        updated_count, 0,
        "Restart should NOT use new CLI env (baked config preserved). Found {} occurrences of '{}={}'. stdout: {}",
        updated_count,
        TEST_VAR,
        UPDATED_VALUE,
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that recreate uses the new CLI environment (re-bakes config)
#[tokio::test]
async fn test_recreate_uses_new_cli_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    const TEST_VAR: &str = "RECREATE_TEST_VAR";
    const INITIAL_VALUE: &str = "initial_value";
    const UPDATED_VALUE: &str = "updated_value";

    harness.start_daemon().await?;

    // Create a config that uses the test variable
    let config_content = format!(
        r#"
kepler:
  logs:
    buffer_size: 0
services:
  env-checker:
    command: ["sh", "-c", "echo {}=${} && sleep 30"]
    sys_env: inherit
    restart: no
"#,
        TEST_VAR, TEST_VAR
    );
    let config_path = harness.create_test_config(&config_content)?;

    // Start with initial value
    let output = harness
        .start_services_with_env(&config_path, &[(TEST_VAR, INITIAL_VALUE)])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-checker", "running", Duration::from_secs(10))
        .await?;

    // Verify initial value
    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, INITIAL_VALUE),
            Duration::from_secs(5),
        )
        .await?;

    assert!(
        logs.stdout_contains(&format!("{}={}", TEST_VAR, INITIAL_VALUE)),
        "Initial start should have initial value. stdout: {}",
        logs.stdout
    );

    // Stop, recreate with updated value (re-bakes config), then start
    harness.stop_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "env-checker", "stopped", Duration::from_secs(10))
        .await?;

    let recreate_output = harness
        .run_cli_with_env(
            &["-f", config_path.to_str().unwrap(), "recreate"],
            &[(TEST_VAR, UPDATED_VALUE)],
        )
        .await?;
    recreate_output.assert_success();

    let start_output = harness
        .start_services_with_env(&config_path, &[(TEST_VAR, UPDATED_VALUE)])
        .await?;
    start_output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-checker", "running", Duration::from_secs(10))
        .await?;

    // Wait for logs to contain the updated value
    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, UPDATED_VALUE),
            Duration::from_secs(5),
        )
        .await?;

    assert!(
        logs.stdout_contains(&format!("{}={}", TEST_VAR, UPDATED_VALUE)),
        "Recreate should use new CLI env (config re-baked). Expected '{}={}' in stdout: {}",
        TEST_VAR,
        UPDATED_VALUE,
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
