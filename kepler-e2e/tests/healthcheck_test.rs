//! E2E tests for health check functionality
//!
//! Tests healthcheck pass/fail, retries, start_period, and timeout behavior.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "healthcheck_test";

/// Test that a passing healthcheck marks service as healthy
#[tokio::test]
async fn test_healthcheck_passes() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_passes")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to become healthy
    harness
        .wait_for_service_status(&config_path, "healthy-service", "healthy", Duration::from_secs(15))
        .await?;

    // Verify via ps that it shows as healthy
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.stdout_contains("healthy"),
        "Service should show as healthy. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that a failing healthcheck marks service as unhealthy after retries
#[tokio::test]
async fn test_healthcheck_fails() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_fails")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start running first
    harness
        .wait_for_service_status(&config_path, "unhealthy-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for healthcheck to fail (after retries)
    // Config has interval: 1s, retries: 2, so should fail within ~5 seconds
    harness
        .wait_for_service_status(&config_path, "unhealthy-service", "unhealthy", Duration::from_secs(15))
        .await?;

    // Verify via ps
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.stdout_contains("unhealthy"),
        "Service should show as unhealthy. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that healthcheck retries work - intermittent failures within retry limit
#[tokio::test]
async fn test_healthcheck_retries() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create a flag file that the healthcheck will use
    let flag_file = harness.create_temp_file("healthcheck_flag", "0")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_healthcheck_retries",
        &[("FLAG_FILE_PATH", flag_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "retry-service", "running", Duration::from_secs(10))
        .await?;

    // After a few seconds, the counter should reach threshold and become healthy
    // The healthcheck increments a counter and succeeds when count >= 3
    harness
        .wait_for_service_status(&config_path, "retry-service", "healthy", Duration::from_secs(20))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that no healthchecks run during start_period
#[tokio::test]
async fn test_healthcheck_start_period() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_start_period")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "start-period-service", "running", Duration::from_secs(10))
        .await?;

    // During start_period (5s), service should be running but not marked unhealthy yet
    // even though healthcheck will fail
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check it's still running (not marked unhealthy during start_period)
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.stdout_contains("Up ") && !ps_output.stdout_contains("unhealthy"),
        "Service should be running during start_period. stdout: {}",
        ps_output.stdout
    );

    // After start_period + healthcheck time, should become unhealthy
    harness
        .wait_for_service_status(&config_path, "start-period-service", "unhealthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that healthcheck timeout counts as failure
#[tokio::test]
async fn test_healthcheck_timeout() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_timeout")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "timeout-service", "running", Duration::from_secs(10))
        .await?;

    // Healthcheck times out (sleeps longer than timeout)
    // After retries, should become unhealthy
    harness
        .wait_for_service_status(&config_path, "timeout-service", "unhealthy", Duration::from_secs(20))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}
