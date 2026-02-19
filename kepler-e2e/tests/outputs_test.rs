//! E2E tests for the output capture mechanism.
//!
//! Tests hook output capture, service process output capture, dependent service
//! output access, output marker filtering from logs, daemon restart persistence,
//! and combined process + declared outputs.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "outputs_test";

/// Test that hook output capture works: hook step with `output: setup` captures
/// ::output:: markers. The hook's regular output should appear in logs but
/// marker lines should be filtered.
#[tokio::test]
async fn test_hook_output_capture() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_hook_output_capture")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(
            &config_path,
            "hook-output-service",
            "running",
            Duration::from_secs(10),
        )
        .await?;

    // Verify the service is running (hook completed)
    let logs = harness
        .wait_for_log_content(&config_path, "SERVICE_RUNNING", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("SERVICE_RUNNING"),
        "Service should be running after hook. logs: {}",
        logs.stdout
    );

    // Hook's regular output should appear in logs
    let all_logs = harness.get_logs(&config_path, None, 1000).await?;
    assert!(
        all_logs.stdout_contains("HOOK_VISIBLE_LINE"),
        "Hook's regular output should appear in logs. logs: {}",
        all_logs.stdout
    );

    // ::output:: markers should NOT appear in logs
    assert!(
        !all_logs.stdout_contains("::output::"),
        "::output:: markers should be filtered from logs. logs: {}",
        all_logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test service process output capture: service with `output: true` captures
/// ::output:: markers from stdout.
#[tokio::test]
async fn test_service_process_output() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_service_process_output")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to complete (restart: no)
    harness
        .wait_for_service_status(
            &config_path,
            "output-service",
            "exited",
            Duration::from_secs(10),
        )
        .await?;

    // Verify regular output appears in logs
    let logs = harness.get_logs(&config_path, None, 1000).await?;
    assert!(
        logs.stdout_contains("PROCESS_DONE"),
        "Regular process output should appear in logs. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test dependent service reads producer's outputs via ${{ deps['producer'].outputs.port }}.
#[tokio::test]
async fn test_dependent_reads_outputs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_dependent_reads_outputs")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for consumer to be running (producer must complete first)
    harness
        .wait_for_service_status(
            &config_path,
            "consumer",
            "running",
            Duration::from_secs(15),
        )
        .await?;

    // Verify consumer reads producer's output
    let logs = harness
        .wait_for_log_content(&config_path, "CONSUMER_PORT=8080", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("CONSUMER_PORT=8080"),
        "Consumer should read producer's output. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test hook outputs → service outputs declaration → dependent service reads them.
/// Setup service has a hook that emits outputs, an `outputs:` map references them,
/// and the app service reads the declared outputs.
#[tokio::test]
async fn test_hook_outputs_to_service_outputs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_hook_outputs_to_service_outputs")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for app to be running (setup must complete first)
    harness
        .wait_for_service_status(&config_path, "app", "running", Duration::from_secs(15))
        .await?;

    // Verify app reads declared outputs
    let logs = harness
        .wait_for_log_content(&config_path, "APP_TOKEN=abc-123", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("APP_TOKEN=abc-123"),
        "App should read declared token output. logs: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("APP_URL=http://localhost:8080"),
        "App should read declared URL output. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that ::output:: marker lines are NOT present in service logs,
/// but regular output lines are.
#[tokio::test]
async fn test_output_markers_filtered() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path =
        harness.load_config(TEST_MODULE, "test_output_markers_filtered_from_logs")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to complete
    harness
        .wait_for_service_status(
            &config_path,
            "marker-service",
            "exited",
            Duration::from_secs(10),
        )
        .await?;

    // Give a moment for logs to flush
    tokio::time::sleep(Duration::from_millis(500)).await;

    let logs = harness.get_logs(&config_path, None, 1000).await?;

    // Regular output should appear
    assert!(
        logs.stdout_contains("REGULAR_OUTPUT"),
        "Regular output should appear in logs. logs: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("ANOTHER_LINE"),
        "Another regular line should appear in logs. logs: {}",
        logs.stdout
    );

    // ::output:: markers should NOT appear
    assert!(
        !logs.stdout_contains("::output::"),
        "::output:: markers should be filtered from logs. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that outputs persist across daemon restart.
/// Producer exits with outputs, consumer reads them. Kill daemon, restart.
/// Consumer should still have access to producer's outputs.
#[tokio::test]
async fn test_daemon_restart_preserves_outputs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path =
        harness.load_config(TEST_MODULE, "test_daemon_restart_preserves_outputs")?;

    harness.start_daemon().await?;

    // Start services — producer completes, consumer starts
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for consumer to be running
    harness
        .wait_for_service_status(
            &config_path,
            "consumer",
            "running",
            Duration::from_secs(15),
        )
        .await?;

    // Verify consumer reads producer's output
    let logs = harness
        .wait_for_log_content(&config_path, "CONSUMER_PORT=9090", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("CONSUMER_PORT=9090"),
        "Consumer should read producer's output before restart. logs: {}",
        logs.stdout
    );

    // Kill and restart daemon
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    // Start services again
    let output2 = harness.start_services(&config_path).await?;
    output2.assert_success();

    // Wait for consumer to be running again
    harness
        .wait_for_service_status(
            &config_path,
            "consumer",
            "running",
            Duration::from_secs(15),
        )
        .await?;

    // Consumer should still read producer's outputs (persisted on disk)
    let logs2 = harness
        .wait_for_log_content(&config_path, "CONSUMER_PORT=9090", Duration::from_secs(5))
        .await?;
    assert!(
        logs2.stdout_contains("CONSUMER_PORT=9090"),
        "Consumer should read producer's outputs after daemon restart. logs: {}",
        logs2.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test combined process outputs and declared outputs.
/// Process emits outputs via ::output::, service has `outputs:` map.
/// Declared outputs override process outputs on conflict.
/// Both are available to dependent services.
#[tokio::test]
async fn test_combined_process_and_declared_outputs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path =
        harness.load_config(TEST_MODULE, "test_combined_process_and_declared_outputs")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for reader to be running (combined must complete first)
    harness
        .wait_for_service_status(&config_path, "reader", "running", Duration::from_secs(15))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "PROCESS_KEY=", Duration::from_secs(5))
        .await?;

    // Process output should be available
    assert!(
        logs.stdout_contains("PROCESS_KEY=process_val"),
        "Process output should be available to dependent. logs: {}",
        logs.stdout
    );

    // Declared output should be available
    assert!(
        logs.stdout_contains("DECLARED_KEY=declared_val"),
        "Declared output should be available to dependent. logs: {}",
        logs.stdout
    );

    // On conflict, declared outputs should win
    assert!(
        logs.stdout_contains("SHARED_KEY=from_declaration"),
        "Declared outputs should override process outputs on conflict. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
