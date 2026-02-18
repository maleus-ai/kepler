//! E2E tests for service `run` field
//!
//! Tests that services can use `run: "shell script"` as an alternative to `command: [...]`.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "run_field_test";

/// Service with `run` field starts and runs
#[tokio::test]
async fn test_run_field_starts_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_field_starts_service")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "run-service", "running", Duration::from_secs(10))
        .await?;

    // Verify the service output
    let logs = harness
        .wait_for_log_content(&config_path, "run field works", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("run field works"),
        "Service should produce output via sh -c. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Service with `run` field supports shell features (pipes, etc.)
#[tokio::test]
async fn test_run_field_shell_features() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_field_shell_features")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "pipe-service", "running", Duration::from_secs(10))
        .await?;

    // Verify pipe worked: HELLO -> hello
    let logs = harness
        .wait_for_log_content(&config_path, "hello", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("hello"),
        "Pipe should lowercase HELLO. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with both `run` and `command` fails to start
#[tokio::test]
async fn test_run_and_command_mutually_exclusive() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_and_command_mutually_exclusive")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    assert!(
        !output.success(),
        "Starting with both run and command should fail"
    );
    assert!(
        output.stderr_contains("mutually exclusive"),
        "Error should mention mutual exclusivity. stderr: {}",
        output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}
