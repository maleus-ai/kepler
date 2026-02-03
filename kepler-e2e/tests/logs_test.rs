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

/// Test multi-service log merging and ordering with truncation
///
/// This test verifies that:
/// 1. Multiple services can write logs concurrently with truncation enabled
/// 2. Logs from all services are properly merged in chronological order
/// 3. Both stdout and stderr are properly separated and reconciled
/// 4. No rotation files are created (truncation model)
#[tokio::test]
async fn test_multi_service_rotation_reconciliation() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_multi_service_rotation")?;

    harness.start_daemon().await?;

    // Start all services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all services to start
    for service in ["log-rotation-a", "log-rotation-b", "log-rotation-c"] {
        harness
            .wait_for_service_status(&config_path, service, "running", Duration::from_secs(10))
            .await?;
    }

    // Wait for logs from all services (they write 200 lines each, may trigger truncation)
    // Wait for DONE marker indicating all lines were written
    harness
        .wait_for_log_content(&config_path, "SRVA_DONE", Duration::from_secs(20))
        .await?;
    harness
        .wait_for_log_content(&config_path, "SRVB_DONE", Duration::from_secs(20))
        .await?;
    harness
        .wait_for_log_content(&config_path, "SRVC_DONE", Duration::from_secs(20))
        .await?;

    // Get all logs from all services
    let all_logs = harness.get_logs(&config_path, None, 1000).await?;
    all_logs.assert_success();

    // Verify logs from all three services are present
    assert!(
        all_logs.stdout_contains("SRVA_"),
        "Should contain logs from service A. stdout: {}",
        all_logs.stdout
    );
    assert!(
        all_logs.stdout_contains("SRVB_"),
        "Should contain logs from service B. stdout: {}",
        all_logs.stdout
    );
    assert!(
        all_logs.stdout_contains("SRVC_"),
        "Should contain logs from service C (stderr). stdout: {}",
        all_logs.stdout
    );

    // Verify filtering by individual service works
    let logs_a = harness.get_logs(&config_path, Some("log-rotation-a"), 500).await?;
    logs_a.assert_success();
    assert!(
        logs_a.stdout_contains("SRVA_"),
        "Filtered logs should contain service A. stdout: {}",
        logs_a.stdout
    );
    assert!(
        !logs_a.stdout_contains("SRVB_"),
        "Filtered logs should NOT contain service B. stdout: {}",
        logs_a.stdout
    );
    assert!(
        !logs_a.stdout_contains("SRVC_"),
        "Filtered logs should NOT contain service C. stdout: {}",
        logs_a.stdout
    );

    // Verify truncation model: no rotation files should exist
    if let Some(state_dir) = harness.get_config_state_dir(&config_path) {
        let logs_dir = state_dir.join("logs");

        // List all log files in the directory
        let log_files: Vec<_> = std::fs::read_dir(&logs_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();

        // No rotation files should exist with truncation model
        let rotated_files: Vec<_> = log_files
            .iter()
            .filter(|f| f.contains(".log.1") || f.contains(".log.2") || f.contains(".log.3"))
            .collect();

        assert!(
            rotated_files.is_empty(),
            "No rotation files should exist with truncation model. Found: {:?}",
            rotated_files
        );

        // Main log files should exist
        let main_log_exists = logs_dir.join("log-rotation-a.stdout.log").exists();
        assert!(
            main_log_exists,
            "Main log file should exist. logs_dir: {:?}, files: {:?}",
            logs_dir,
            log_files
        );
    }

    // Verify logs are in chronological order by checking timestamps
    // The timestamps in the output should be monotonically increasing
    let lines: Vec<&str> = all_logs.stdout.lines().collect();
    let mut prev_timestamp: Option<&str> = None;
    for line in &lines {
        // Timestamp format: "YYYY-MM-DD HH:MM:SS"
        if line.len() >= 19 && line.chars().nth(4) == Some('-') {
            let timestamp = &line[..19];
            if let Some(prev) = prev_timestamp {
                // Timestamps should be >= previous (chronological order)
                assert!(
                    timestamp >= prev,
                    "Logs should be in chronological order. '{}' should be >= '{}'",
                    timestamp,
                    prev
                );
            }
            prev_timestamp = Some(timestamp);
        }
    }

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that log truncation works correctly when max_size is reached
///
/// This test verifies that:
/// 1. Log files are truncated when they reach max_size
/// 2. No rotation files (.1, .2, etc) are created
/// 3. The service can continue writing logs after truncation
/// 4. Recent logs are preserved after truncation
#[tokio::test]
async fn test_log_truncation_behavior() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_truncation_behavior")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to produce logs and complete
    harness
        .wait_for_service_status(&config_path, "truncation-test", "running", Duration::from_secs(10))
        .await?;

    // Wait for DONE marker
    harness
        .wait_for_log_content(&config_path, "TRUNCATION_DONE", Duration::from_secs(30))
        .await?;

    // Verify log file exists and is truncated
    if let Some(state_dir) = harness.get_config_state_dir(&config_path) {
        let logs_dir = state_dir.join("logs");
        let log_file = logs_dir.join("truncation-test.stdout.log");

        assert!(
            log_file.exists(),
            "Log file should exist at {:?}",
            log_file
        );

        // Check file size - should be around max_size (2KB) or less
        let metadata = std::fs::metadata(&log_file)?;
        let file_size = metadata.len();

        // Max size is 2KB (2048 bytes), but with some margin for the last line
        assert!(
            file_size <= 4096,
            "Log file should be truncated to around max_size. Got {} bytes",
            file_size
        );

        // No rotation files should exist
        let rotated_1 = logs_dir.join("truncation-test.stdout.log.1");
        assert!(
            !rotated_1.exists(),
            "Rotation file .1 should NOT exist with truncation model"
        );
    }

    // Verify we can still read logs and they contain the DONE marker
    let logs = harness.get_logs(&config_path, Some("truncation-test"), 100).await?;
    logs.assert_success();

    assert!(
        logs.stdout_contains("TRUNCATION_DONE"),
        "Logs should contain DONE marker (recent logs preserved). stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that logs from multiple services are properly merged in chronological order
///
/// This test verifies that:
/// 1. Logs from multiple concurrent services are interleaved correctly
/// 2. Timestamps are monotonically increasing across all services
/// 3. Both stdout and stderr streams are properly merged
#[tokio::test]
async fn test_log_chronological_ordering() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_log_ordering")?;

    harness.start_daemon().await?;

    // Start all services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all services to complete
    for service in ["ordering-svc-a", "ordering-svc-b", "ordering-svc-c"] {
        harness
            .wait_for_service_status(&config_path, service, "running", Duration::from_secs(10))
            .await?;
    }

    // Wait for all DONE markers
    harness
        .wait_for_log_content(&config_path, "ORDER_A_DONE", Duration::from_secs(15))
        .await?;
    harness
        .wait_for_log_content(&config_path, "ORDER_B_DONE", Duration::from_secs(15))
        .await?;
    harness
        .wait_for_log_content(&config_path, "ORDER_C_DONE", Duration::from_secs(15))
        .await?;

    // Get all logs merged
    let all_logs = harness.get_logs(&config_path, None, 500).await?;
    all_logs.assert_success();

    // Verify logs from all services are present
    assert!(
        all_logs.stdout_contains("ORDER_A_"),
        "Should contain logs from service A"
    );
    assert!(
        all_logs.stdout_contains("ORDER_B_"),
        "Should contain logs from service B"
    );
    assert!(
        all_logs.stdout_contains("ORDER_C_"),
        "Should contain logs from service C (stderr)"
    );

    // Verify timestamps are monotonically increasing
    let lines: Vec<&str> = all_logs.stdout.lines().collect();
    let mut prev_timestamp: Option<&str> = None;
    let mut order_violations = 0;

    for line in &lines {
        // Timestamp format: "YYYY-MM-DD HH:MM:SS"
        if line.len() >= 19 && line.chars().nth(4) == Some('-') {
            let timestamp = &line[..19];
            if let Some(prev) = prev_timestamp {
                if timestamp < prev {
                    order_violations += 1;
                }
            }
            prev_timestamp = Some(timestamp);
        }
    }

    assert_eq!(
        order_violations, 0,
        "Logs should be in strict chronological order. Found {} violations",
        order_violations
    );

    // Verify logs are interleaved (not grouped by service)
    // At least some logs from different services should be adjacent
    let mut found_interleaving = false;
    let mut prev_service: Option<&str> = None;

    for line in &lines {
        let current_service = if line.contains("ORDER_A_") {
            Some("A")
        } else if line.contains("ORDER_B_") {
            Some("B")
        } else if line.contains("ORDER_C_") {
            Some("C")
        } else {
            None
        };

        if let (Some(prev), Some(curr)) = (prev_service, current_service) {
            if prev != curr {
                found_interleaving = true;
                break;
            }
        }
        prev_service = current_service;
    }

    assert!(
        found_interleaving,
        "Logs should be interleaved between services (chronological merge)"
    );

    harness.stop_daemon().await?;

    Ok(())
}
