//! E2E tests for restart policies
//!
//! Tests the different restart behaviors: no, always, on-failure.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "restart_policy_test";

/// Test that restart: no does not restart after exit
#[tokio::test]
async fn test_restart_no_does_not_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_no_does_not_restart")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Service exits after 1 second, wait for it
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Should be stopped/exited, not running
    let status = harness
        .wait_for_service_status_any(&config_path, "no-restart-service", &["stopped", "exited"], Duration::from_secs(5))
        .await?;

    // Wait a bit more to ensure it doesn't restart
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Still should be stopped
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        !ps_output.stdout_contains("Up "),
        "Service should not be restarted (restart: no). Status was: {}, stdout: {}",
        status, ps_output.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that restart: always restarts on successful exit (exit 0)
#[tokio::test]
async fn test_restart_always_on_success() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_always_on_success")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for it to start
    harness
        .wait_for_service_status(&config_path, "always-restart-success", "running", Duration::from_secs(10))
        .await?;

    // Wait for it to exit and restart (service exits after 1s with exit 0)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Should be running again due to restart: always
    harness
        .wait_for_service_status(&config_path, "always-restart-success", "running", Duration::from_secs(10))
        .await?;

    // Check logs to confirm it restarted (should have multiple start messages)
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let restart_count = logs.stdout.matches("ALWAYS_SUCCESS_START").count();
    assert!(
        restart_count >= 2,
        "Service should have started at least twice. Count: {}, stdout: {}",
        restart_count, logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that restart: always restarts on failure (exit 1)
#[tokio::test]
async fn test_restart_always_on_failure() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_always_on_failure")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for it to start
    harness
        .wait_for_service_status(&config_path, "always-restart-failure", "running", Duration::from_secs(10))
        .await?;

    // Wait for it to exit and restart (service exits after 1s with exit 1)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Should be running again due to restart: always
    harness
        .wait_for_service_status(&config_path, "always-restart-failure", "running", Duration::from_secs(10))
        .await?;

    // Check logs to confirm it restarted
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let restart_count = logs.stdout.matches("ALWAYS_FAILURE_START").count();
    assert!(
        restart_count >= 2,
        "Service should have started at least twice. Count: {}, stdout: {}",
        restart_count, logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that restart: on-failure restarts when process exits with non-zero
#[tokio::test]
async fn test_restart_on_failure_exit_1() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_on_failure_exit_1")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for it to start
    harness
        .wait_for_service_status(&config_path, "on-failure-exit-1", "running", Duration::from_secs(10))
        .await?;

    // Wait for it to exit and restart (service exits after 1s with exit 1)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Should be running again due to restart: on-failure with exit 1
    harness
        .wait_for_service_status(&config_path, "on-failure-exit-1", "running", Duration::from_secs(10))
        .await?;

    // Check logs to confirm it restarted
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let restart_count = logs.stdout.matches("ON_FAILURE_EXIT_1_START").count();
    assert!(
        restart_count >= 2,
        "Service should have started at least twice. Count: {}, stdout: {}",
        restart_count, logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that restart: on-failure does NOT restart when process exits with 0
#[tokio::test]
async fn test_restart_on_failure_exit_0() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_on_failure_exit_0")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Service exits after 1 second with exit 0, wait for it
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Should be stopped/exited, not running (on-failure doesn't restart on success)
    harness
        .wait_for_service_status_any(&config_path, "on-failure-exit-0", &["stopped", "exited"], Duration::from_secs(5))
        .await?;

    // Wait a bit more to ensure it doesn't restart
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check that it only started once
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let restart_count = logs.stdout.matches("ON_FAILURE_EXIT_0_START").count();
    assert_eq!(
        restart_count, 1,
        "Service should have started only once (no restart on success). Count: {}, stdout: {}",
        restart_count, logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
