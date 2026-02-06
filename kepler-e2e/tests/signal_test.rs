//! E2E tests for stop --signal feature
//!
//! Tests that services can be stopped with specific signals.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "signal_test";

/// Test stopping a service with --signal=SIGKILL
#[tokio::test]
async fn test_stop_with_sigkill() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_with_signal")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Stop with SIGKILL
    let output = harness.stop_services_with_signal(&config_path, "SIGKILL").await?;
    output.assert_success();

    // Verify service is stopped
    harness
        .wait_for_service_status_any(&config_path, "test-service", &["stopped", "failed"], Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test stopping a service with a numeric signal (--signal=9)
#[tokio::test]
async fn test_stop_with_numeric_signal() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_with_signal")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Stop with numeric signal 9 (SIGKILL)
    let output = harness.stop_services_with_signal(&config_path, "9").await?;
    output.assert_success();

    harness
        .wait_for_service_status_any(&config_path, "test-service", &["stopped", "failed"], Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test stopping a service with --signal=SIGTERM (same as default)
#[tokio::test]
async fn test_stop_with_sigterm() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_with_signal")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Stop with SIGTERM (should behave same as default)
    let output = harness.stop_services_with_signal(&config_path, "SIGTERM").await?;
    output.assert_success();

    harness
        .wait_for_service_status_any(&config_path, "test-service", &["stopped", "failed"], Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test stopping with an invalid signal name produces an error
#[tokio::test]
async fn test_stop_with_invalid_signal() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_with_signal")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Stop with invalid signal
    let output = harness.stop_services_with_signal(&config_path, "INVALID").await?;

    // Should fail with an error about invalid signal
    assert!(
        !output.success() || output.stderr_contains("nvalid signal"),
        "Invalid signal should produce an error. exit_code: {}, stdout: {}, stderr: {}",
        output.exit_code, output.stdout, output.stderr
    );

    // Cleanup: stop normally
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;

    Ok(())
}
