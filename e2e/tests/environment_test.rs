//! E2E tests for environment variable handling
//!
//! Tests inline environment, env_file loading, and shell expansion.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "environment_test";

/// Test that inline environment variables are available to the process
#[tokio::test]
async fn test_environment_inline() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_environment_inline")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start and produce logs
    harness
        .wait_for_service_status(&config_path, "env-inline-service", "running", Duration::from_secs(10))
        .await?;

    // Check logs for the environment variable
    harness.wait_for_log_content(&config_path, "MY_VAR=hello_world", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that env_file loads variables correctly
#[tokio::test]
async fn test_env_file_loading() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create the env file first
    let env_file_path = harness.create_temp_file("test.env", "ENV_FILE_VAR=from_env_file\nANOTHER_VAR=another_value\n")?;

    // Load config with the env file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_env_file_loading",
        &[("__ENV_FILE_PATH__", env_file_path.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "env-file-service", "running", Duration::from_secs(10))
        .await?;

    // Check logs for the environment variable from env_file
    harness.wait_for_log_content(&config_path, "ENV_FILE_VAR=from_env_file", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that shell expansion ${VAR} works in environment values
#[tokio::test]
async fn test_shell_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_shell_expansion")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "shell-expansion-service", "running", Duration::from_secs(10))
        .await?;

    // The HOME variable should be expanded from the system environment
    // Check that EXPANDED_VAR contains something (not the literal ${HOME})
    let logs = harness.wait_for_log_content(&config_path, "EXPANDED_VAR=", Duration::from_secs(5)).await?;

    // Should not contain the literal "${HOME}"
    assert!(
        !logs.stdout_contains("EXPANDED_VAR=${HOME}"),
        "Shell expansion should work. stdout: {}", logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that environment array takes priority over env_file
#[tokio::test]
async fn test_env_priority() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with a variable
    let env_file_path = harness.create_temp_file("priority.env", "PRIORITY_VAR=from_file\n")?;

    // Load config with the env file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_env_priority",
        &[("__ENV_FILE_PATH__", env_file_path.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "env-priority-service", "running", Duration::from_secs(10))
        .await?;

    // The inline environment should override the env_file
    harness.wait_for_log_content(&config_path, "PRIORITY_VAR=from_inline", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}
