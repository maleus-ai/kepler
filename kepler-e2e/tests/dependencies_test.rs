//! E2E tests for service dependency ordering
//!
//! Tests that services with dependencies start correctly.
//! Uses timestamps for reliable ordering verification (log positions can be racy).

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "dependencies_test";

/// Extract timestamp from a log line containing a marker.
/// Log format with timestamps: "YYYY-MM-DD HH:MM:SS ..."
fn extract_timestamp<'a>(logs: &'a str, marker: &str) -> Option<&'a str> {
    for line in logs.lines() {
        if line.contains(marker) {
            // Timestamp is at the start: "2024-01-15 10:30:45 ..."
            return line.get(0..19);
        }
    }
    None
}

/// Test simple dependency: B depends on A, A starts first
#[tokio::test]
async fn test_simple_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_simple_dependency")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for both to be running
    harness
        .wait_for_service_status(&config_path, "service-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "service-b", "running", Duration::from_secs(10))
        .await?;

    // Check logs to verify both started
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    // Both markers should be present
    assert!(
        logs.stdout_contains("SERVICE_A_STARTED"),
        "Service A should have logged its start marker. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("SERVICE_B_STARTED"),
        "Service B should have logged its start marker. stdout: {}",
        logs.stdout
    );

    // Use timestamps for reliable ordering (log positions can be racy)
    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;
    let ts_a = extract_timestamp(&logs_with_ts.stdout, "SERVICE_A_STARTED");
    let ts_b = extract_timestamp(&logs_with_ts.stdout, "SERVICE_B_STARTED");

    // A should start before or at the same time as B
    assert!(
        ts_a <= ts_b,
        "Service A should start before Service B. A timestamp: {:?}, B timestamp: {:?}",
        ts_a, ts_b
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test chain dependency: C → B → A (A starts first, then B, then C)
#[tokio::test]
async fn test_chain_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_chain_dependency")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["chain-a", "chain-b", "chain-c"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(15))
            .await?;
    }

    // Check logs - all should have started
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    assert!(logs.stdout_contains("CHAIN_A_STARTED"), "Chain A should have started");
    assert!(logs.stdout_contains("CHAIN_B_STARTED"), "Chain B should have started");
    assert!(logs.stdout_contains("CHAIN_C_STARTED"), "Chain C should have started");

    // Use timestamps for reliable ordering (log positions can be racy)
    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;
    let ts_a = extract_timestamp(&logs_with_ts.stdout, "CHAIN_A_STARTED");
    let ts_b = extract_timestamp(&logs_with_ts.stdout, "CHAIN_B_STARTED");
    let ts_c = extract_timestamp(&logs_with_ts.stdout, "CHAIN_C_STARTED");

    // Verify order: A → B → C
    assert!(
        ts_a <= ts_b && ts_b <= ts_c,
        "Start order should be A → B → C. A ts: {:?}, B ts: {:?}, C ts: {:?}",
        ts_a, ts_b, ts_c
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test diamond dependency: D depends on B and C, both depend on A
/// A starts first, then B and C (either order), then D
#[tokio::test]
async fn test_diamond_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_diamond_dependency")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["diamond-a", "diamond-b", "diamond-c", "diamond-d"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(15))
            .await?;
    }

    // Check logs - all should have started
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    assert!(logs.stdout_contains("DIAMOND_A_STARTED"), "Diamond A should have started");
    assert!(logs.stdout_contains("DIAMOND_B_STARTED"), "Diamond B should have started");
    assert!(logs.stdout_contains("DIAMOND_C_STARTED"), "Diamond C should have started");
    assert!(logs.stdout_contains("DIAMOND_D_STARTED"), "Diamond D should have started");

    // Use timestamps for reliable ordering (log positions can be racy)
    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;
    let ts_a = extract_timestamp(&logs_with_ts.stdout, "DIAMOND_A_STARTED");
    let ts_b = extract_timestamp(&logs_with_ts.stdout, "DIAMOND_B_STARTED");
    let ts_c = extract_timestamp(&logs_with_ts.stdout, "DIAMOND_C_STARTED");
    let ts_d = extract_timestamp(&logs_with_ts.stdout, "DIAMOND_D_STARTED");

    // A must be before or equal to B and C
    assert!(
        ts_a <= ts_b && ts_a <= ts_c,
        "A should start before B and C. A ts: {:?}, B ts: {:?}, C ts: {:?}",
        ts_a, ts_b, ts_c
    );
    // D must be after or equal to B and C
    assert!(
        ts_d >= ts_b && ts_d >= ts_c,
        "D should start after B and C. B ts: {:?}, C ts: {:?}, D ts: {:?}",
        ts_b, ts_c, ts_d
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that dependent service and its dependency both stop
#[tokio::test]
async fn test_stop_with_dependencies() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_order_reverse")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["stop-order-a", "stop-order-b"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    // Verify both started (check logs before stop - logs are cleared on stop)
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(logs.stdout_contains("STOP_ORDER_A_STARTED"), "A should have started");
    assert!(logs.stdout_contains("STOP_ORDER_B_STARTED"), "B should have started");

    // Stop services
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for both to stop
    for svc in &["stop-order-a", "stop-order-b"] {
        harness
            .wait_for_service_status(&config_path, svc, "stopped", Duration::from_secs(10))
            .await?;
    }

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that circular dependencies are detected as an error
#[tokio::test]
async fn test_cycle_detection() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_cycle_detection")?;

    harness.start_daemon().await?;

    // Try to start services - should fail due to cycle
    let output = harness.start_services(&config_path).await?;

    // Should fail with an error about circular dependency
    assert!(
        !output.success() || output.stderr_contains("cycle") || output.stderr_contains("circular"),
        "Should detect circular dependency. exit_code: {}, stderr: {}",
        output.exit_code, output.stderr
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on with service_healthy condition
/// Backend should wait for database healthcheck to pass before starting
#[tokio::test]
async fn test_depends_on_service_healthy() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_depends_on_service_healthy")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    assert!(
        output.success(),
        "start_services should succeed. exit_code: {}, stdout: {}, stderr: {}",
        output.exit_code, output.stdout, output.stderr
    );

    // Wait for database to be healthy first
    harness
        .wait_for_service_status(&config_path, "database", "healthy", Duration::from_secs(10))
        .await?;

    // Backend should start after database is healthy
    harness
        .wait_for_service_status(&config_path, "backend", "running", Duration::from_secs(10))
        .await?;

    // Get logs (timestamps are included via logs config)
    harness.wait_for_log_content(&config_path, "DATABASE_STARTED", Duration::from_secs(5)).await?;

    let logs = harness.get_logs(&config_path, None, 100).await?;

    // Both should have started
    assert!(
        logs.stdout_contains("DATABASE_STARTED"),
        "Database should have started. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("BACKEND_STARTED"),
        "Backend should have started. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on with service_completed_successfully condition
/// App should wait for init container to exit with code 0 before starting
#[tokio::test]
async fn test_depends_on_completed_successfully() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_depends_on_completed_successfully")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for init-container to complete (it exits with code 0)
    harness
        .wait_for_service_status(&config_path, "init-container", "stopped", Duration::from_secs(10))
        .await?;

    // App should start after init completes successfully
    harness
        .wait_for_service_status(&config_path, "app", "running", Duration::from_secs(10))
        .await?;

    // Check logs
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;

    // Both should have logged
    assert!(
        logs_with_ts.stdout_contains("INIT_STARTED"),
        "Init should have started. stdout: {}",
        logs_with_ts.stdout
    );
    assert!(
        logs_with_ts.stdout_contains("INIT_COMPLETED"),
        "Init should have completed. stdout: {}",
        logs_with_ts.stdout
    );
    assert!(
        logs_with_ts.stdout_contains("APP_STARTED"),
        "App should have started. stdout: {}",
        logs_with_ts.stdout
    );

    // App should start after init completed
    let ts_init = extract_timestamp(&logs_with_ts.stdout, "INIT_COMPLETED");
    let ts_app = extract_timestamp(&logs_with_ts.stdout, "APP_STARTED");

    assert!(
        ts_init <= ts_app,
        "Init should complete before app starts. Init ts: {:?}, App ts: {:?}",
        ts_init, ts_app
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test restart propagation via depends_on restart: true
/// When database restarts, backend should also restart
#[tokio::test]
async fn test_restart_propagation() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_propagation")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    assert!(
        output.success(),
        "start_services should succeed. exit_code: {}, stdout: {}, stderr: {}",
        output.exit_code, output.stdout, output.stderr
    );

    // Wait for database to be healthy
    harness
        .wait_for_service_status(&config_path, "database", "healthy", Duration::from_secs(10))
        .await?;

    // Wait for backend to be running (after dependency condition is met)
    harness
        .wait_for_service_status(&config_path, "backend", "running", Duration::from_secs(10))
        .await?;

    // Get initial logs to capture start markers
    harness.wait_for_logs(&config_path, Duration::from_secs(2)).await?;
    let initial_logs = harness.get_logs(&config_path, None, 100).await?;

    // Count initial start markers
    let initial_backend_starts = initial_logs.stdout.matches("BACKEND_STARTED").count();
    assert_eq!(
        initial_backend_starts, 1,
        "Backend should have started once initially. stdout: {}",
        initial_logs.stdout
    );

    // Restart the database
    let output = harness.restart_service(&config_path, "database").await?;
    output.assert_success();

    // Wait for database to be healthy again
    harness
        .wait_for_service_status(&config_path, "database", "healthy", Duration::from_secs(10))
        .await?;

    // Wait for backend to restart (due to restart propagation)
    // Give it some time for the restart to propagate
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Backend should have restarted and be running again
    harness
        .wait_for_service_status(&config_path, "backend", "running", Duration::from_secs(10))
        .await?;

    // Get logs and check that backend started again
    harness.wait_for_logs(&config_path, Duration::from_secs(2)).await?;
    let final_logs = harness.get_logs(&config_path, None, 200).await?;

    // Backend should have started twice now (initial + restart propagation)
    let final_backend_starts = final_logs.stdout.matches("BACKEND_STARTED").count();
    assert!(
        final_backend_starts >= 2,
        "Backend should have restarted due to restart propagation. Expected >= 2 starts, got {}. stdout: {}",
        final_backend_starts,
        final_logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on timeout - backend should fail when dependency doesn't meet condition in time
#[tokio::test]
async fn test_depends_on_timeout() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_depends_on_timeout")?;

    harness.start_daemon().await?;

    // Start services - backend should eventually fail due to timeout
    let _output = harness.start_services(&config_path).await?;
    // The start command itself may succeed, but backend should fail to start

    // Wait for database to be running (but unhealthy)
    harness
        .wait_for_service_status(&config_path, "database", "unhealthy", Duration::from_secs(10))
        .await?;

    // Wait for the timeout to expire (2s) plus some buffer
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Check backend status using ps
    let status = harness.ps(&config_path).await?;

    // Backend should either:
    // 1. Not be running at all (never started due to timeout)
    // 2. Be in a failed/error state
    let backend_status = status.stdout.lines()
        .find(|line| line.contains("backend"))
        .unwrap_or("");

    // The backend should NOT be in a healthy running state
    assert!(
        !backend_status.contains("running") || backend_status.contains("error") || backend_status.contains("failed") || backend_status.is_empty(),
        "Backend should not be running successfully due to dependency timeout. Status: {}",
        status.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
