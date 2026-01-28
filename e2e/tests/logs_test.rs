//! E2E tests for log retrieval
//!
//! Tests basic log commands, timestamps, filtering, and stderr capture.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "logs_test";

/// Test basic log retrieval
#[tokio::test]
async fn test_logs_basic() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_basic")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to produce logs
    harness
        .wait_for_service_status(&config_path, "log-basic-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for log content
    let logs = harness.wait_for_log_content(&config_path, "BASIC_LOG_MESSAGE", Duration::from_secs(5)).await?;
    logs.assert_success();

    assert!(
        logs.stdout_contains("BASIC_LOG_MESSAGE"),
        "Logs should contain the message. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that logs include timestamps in their output
#[tokio::test]
async fn test_logs_with_timestamps() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_with_timestamps")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to produce logs
    harness
        .wait_for_service_status(&config_path, "log-timestamp-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for log content
    let logs = harness.wait_for_log_content(&config_path, "TIMESTAMP_LOG_MESSAGE", Duration::from_secs(5)).await?;

    // Logs should include timestamps in format like "2026-01-28 17:30:40"
    // Check for date pattern (YYYY-MM-DD)
    let has_timestamp = logs.stdout.contains("202") &&
                        logs.stdout.contains("-") &&
                        logs.stdout.contains(":");

    assert!(
        has_timestamp,
        "Logs should include timestamps. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test filtering logs by specific service
#[tokio::test]
async fn test_logs_filter_by_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_filter_by_service")?;

    harness.start_daemon().await?;

    // Start the services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for both services to produce logs
    harness
        .wait_for_service_status(&config_path, "filter-service-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "filter-service-b", "running", Duration::from_secs(10))
        .await?;

    // Wait for logs
    harness.wait_for_log_content(&config_path, "FILTER_A_MESSAGE", Duration::from_secs(5)).await?;
    harness.wait_for_log_content(&config_path, "FILTER_B_MESSAGE", Duration::from_secs(5)).await?;

    // Get logs for only service A
    let logs_a = harness.get_logs(&config_path, Some("filter-service-a"), 100).await?;
    logs_a.assert_success();

    // Should have service A's message
    assert!(
        logs_a.stdout_contains("FILTER_A_MESSAGE"),
        "Filtered logs should contain service A's message. stdout: {}",
        logs_a.stdout
    );

    // Should NOT have service B's message
    assert!(
        !logs_a.stdout_contains("FILTER_B_MESSAGE"),
        "Filtered logs should NOT contain service B's message. stdout: {}",
        logs_a.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that stderr is captured separately
#[tokio::test]
async fn test_logs_stderr_captured() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_stderr_captured")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to produce logs
    harness
        .wait_for_service_status(&config_path, "stderr-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for logs to contain both stdout and stderr messages
    harness.wait_for_log_content(&config_path, "STDOUT_MESSAGE", Duration::from_secs(5)).await?;

    // Get all logs
    let logs = harness.get_logs(&config_path, None, 100).await?;
    logs.assert_success();

    // Both stdout and stderr messages should be captured
    assert!(
        logs.stdout_contains("STDOUT_MESSAGE"),
        "Logs should contain stdout message. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("STDERR_MESSAGE"),
        "Logs should contain stderr message. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
