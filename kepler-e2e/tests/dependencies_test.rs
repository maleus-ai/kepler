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

    // Wait for both markers to appear in logs (B depends on A, so B starts last)
    harness.wait_for_log_content(&config_path, "SERVICE_B_STARTED", Duration::from_secs(10)).await?;

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

    // Wait for last marker in chain (C depends on B depends on A)
    harness.wait_for_log_content(&config_path, "CHAIN_C_STARTED", Duration::from_secs(10)).await?;

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

    // Wait for last marker in diamond (D depends on B and C, which depend on A)
    harness.wait_for_log_content(&config_path, "DIAMOND_D_STARTED", Duration::from_secs(10)).await?;

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
    // B depends on A, so B starts last
    harness.wait_for_log_content(&config_path, "STOP_ORDER_B_STARTED", Duration::from_secs(10)).await?;

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
        .wait_for_service_status(&config_path, "init-container", "exited", Duration::from_secs(10))
        .await?;

    // App should start after init completes successfully
    harness
        .wait_for_service_status(&config_path, "app", "running", Duration::from_secs(10))
        .await?;

    // Wait for app's stdout to be captured in the log system before reading
    harness
        .wait_for_log_content(&config_path, "APP_STARTED", Duration::from_secs(5))
        .await?;
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

    // Wait for backend's stdout to be captured before reading logs
    harness
        .wait_for_log_content(&config_path, "BACKEND_STARTED", Duration::from_secs(5))
        .await?;
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

    // Wait for the second BACKEND_STARTED to appear in logs (restart propagation)
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let _final_logs = loop {
        let logs = harness.get_logs(&config_path, None, 200).await?;
        if logs.stdout.matches("BACKEND_STARTED").count() >= 2 {
            break logs;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Backend should have restarted due to restart propagation. stdout: {}",
            logs.stdout
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

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
        !backend_status.contains("Up ") || backend_status.contains("error") || backend_status.contains("Failed") || backend_status.is_empty(),
        "Backend should not be running successfully due to dependency timeout. Status: {}",
        status.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on with service_failed condition
/// Handler should wait for worker to fail before starting
#[tokio::test]
async fn test_depends_on_service_failed() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_depends_on_service_failed")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for worker to exit with non-zero code (Exited status, not Failed)
    harness
        .wait_for_service_status(&config_path, "worker", "exited", Duration::from_secs(10))
        .await?;

    // Handler should start after worker fails (service_failed matches Exited with non-zero code)
    harness
        .wait_for_service_status(&config_path, "handler", "running", Duration::from_secs(10))
        .await?;

    // Check logs
    harness
        .wait_for_log_content(&config_path, "HANDLER_STARTED", Duration::from_secs(5))
        .await?;

    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;

    assert!(
        logs_with_ts.stdout_contains("WORKER_FAILING"),
        "Worker should have logged failure. stdout: {}",
        logs_with_ts.stdout
    );
    assert!(
        logs_with_ts.stdout_contains("HANDLER_STARTED"),
        "Handler should have started. stdout: {}",
        logs_with_ts.stdout
    );

    // Handler should start after worker failed
    let ts_worker = extract_timestamp(&logs_with_ts.stdout, "WORKER_FAILING");
    let ts_handler = extract_timestamp(&logs_with_ts.stdout, "HANDLER_STARTED");

    assert!(
        ts_worker <= ts_handler,
        "Worker should fail before handler starts. Worker ts: {:?}, Handler ts: {:?}",
        ts_worker, ts_handler
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on with service_stopped condition
/// Cleanup should wait for worker to stop before starting
#[tokio::test]
async fn test_depends_on_service_stopped() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_depends_on_service_stopped")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for worker to exit naturally
    harness
        .wait_for_service_status(&config_path, "worker", "exited", Duration::from_secs(10))
        .await?;

    // Cleanup should start after worker stops
    harness
        .wait_for_service_status(&config_path, "cleanup", "running", Duration::from_secs(10))
        .await?;

    // Check logs
    harness
        .wait_for_log_content(&config_path, "CLEANUP_STARTED", Duration::from_secs(5))
        .await?;

    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;

    assert!(
        logs_with_ts.stdout_contains("WORKER_DONE"),
        "Worker should have logged done. stdout: {}",
        logs_with_ts.stdout
    );
    assert!(
        logs_with_ts.stdout_contains("CLEANUP_STARTED"),
        "Cleanup should have started. stdout: {}",
        logs_with_ts.stdout
    );

    // Cleanup should start after worker stopped
    let ts_worker = extract_timestamp(&logs_with_ts.stdout, "WORKER_DONE");
    let ts_cleanup = extract_timestamp(&logs_with_ts.stdout, "CLEANUP_STARTED");

    assert!(
        ts_worker <= ts_cleanup,
        "Worker should stop before cleanup starts. Worker ts: {:?}, Cleanup ts: {:?}",
        ts_worker, ts_cleanup
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on with service_unhealthy condition
/// Alert should wait for monitor to become healthy then transition to unhealthy
#[tokio::test]
async fn test_depends_on_service_unhealthy() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Use PID-unique marker path to avoid collisions between concurrent test runs
    let marker_path = format!("/tmp/kepler_e2e_unhealthy_marker_{}", std::process::id());
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_depends_on_service_unhealthy",
        &[("{{MARKER_PATH}}", &marker_path)],
    )?;

    harness.start_daemon().await?;

    // Clean up marker file from previous runs
    let _ = std::fs::remove_file(&marker_path);

    // Start services - with detached mode (-d), this returns immediately.
    // Deferred dependencies (service_unhealthy) are started in background.
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for alert to be running (deferred: waits for monitor healthy→unhealthy transition)
    // Use longer timeout since the unhealthy transition takes time:
    // monitor starts → becomes healthy → on_healthy hook sleeps 1s → marker created →
    // next healthcheck fails → becomes unhealthy → alert can start
    harness
        .wait_for_service_status(&config_path, "alert", "running", Duration::from_secs(60))
        .await?;

    // Check logs
    harness
        .wait_for_log_content(&config_path, "ALERT_STARTED", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(
        logs.stdout_contains("MONITOR_STARTED"),
        "Monitor should have started. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("ALERT_STARTED"),
        "Alert should have started. stdout: {}",
        logs.stdout
    );

    // Clean up marker file
    let _ = std::fs::remove_file(&marker_path);

    harness.stop_daemon().await?;

    Ok(())
}

/// Test depends_on with service_failed condition and exit_code filter
/// Handler should wait for worker to fail with exit code in range 1:10
#[tokio::test]
async fn test_depends_on_service_failed_exit_code() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path =
        harness.load_config(TEST_MODULE, "test_depends_on_service_failed_exit_code")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for worker to exit with code 5 (Exited status, not Failed)
    harness
        .wait_for_service_status(&config_path, "worker", "exited", Duration::from_secs(10))
        .await?;

    // Handler should start because exit code 5 is in range 1:10 (service_failed matches Exited with non-zero)
    harness
        .wait_for_service_status(&config_path, "handler", "running", Duration::from_secs(10))
        .await?;

    // Check logs
    harness
        .wait_for_log_content(&config_path, "HANDLER_STARTED", Duration::from_secs(5))
        .await?;

    let logs_with_ts = harness.get_logs_with_timestamps(&config_path, None, 100).await?;

    assert!(
        logs_with_ts.stdout_contains("WORKER_FAILING"),
        "Worker should have logged failure. stdout: {}",
        logs_with_ts.stdout
    );
    assert!(
        logs_with_ts.stdout_contains("HANDLER_STARTED"),
        "Handler should have started. stdout: {}",
        logs_with_ts.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

// ============================================================================
// Effective Wait Resolution E2E Tests
// ============================================================================

/// Test that --wait mode blocks until all startup services are ready
/// Config: database → backend → frontend (all startup conditions)
/// All services should be running when start_services_wait returns
#[tokio::test]
async fn test_wait_all_startup() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_wait_all_startup")?;

    harness.start_daemon().await?;

    // Start with --wait mode (blocks until startup cluster is ready)
    let output = harness.start_services_wait(&config_path).await?;
    assert!(
        output.success(),
        "start --wait should succeed. exit_code: {}, stdout: {}, stderr: {}",
        output.exit_code, output.stdout, output.stderr
    );

    // All services should be running immediately after --wait returns
    let ps = harness.ps(&config_path).await?;
    assert!(
        ps.stdout_contains("database") && (ps.stdout_contains("healthy") || ps.stdout_contains("Up ")),
        "database should be running after --wait. ps: {}",
        ps.stdout
    );
    assert!(
        ps.stdout_contains("backend") && ps.stdout_contains("Up "),
        "backend should be running after --wait. ps: {}",
        ps.stdout
    );
    assert!(
        ps.stdout_contains("frontend") && ps.stdout_contains("Up "),
        "frontend should be running after --wait. ps: {}",
        ps.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that per-dependency timeout works
/// Per-dep timeout is 2s — should timeout in ~2s
#[tokio::test]
async fn test_global_timeout_overridden_by_per_dep() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_global_timeout_overridden")?;

    harness.start_daemon().await?;

    let start = std::time::Instant::now();
    let _output = harness.start_services_wait(&config_path).await?;
    let elapsed = start.elapsed();

    // Should have waited ~2s (per-dep timeout), NOT 30s (global)
    assert!(
        elapsed < Duration::from_secs(10),
        "Per-dep timeout (2s) should override global (30s), but waited {:?}",
        elapsed
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test permanently unsatisfied dependency detection
/// "setup" exits with code 0, restart: no.
/// "monitor" depends on "setup" with condition: service_failed.
/// Since setup exited cleanly and won't restart, monitor should be marked skipped.
#[tokio::test]
async fn test_permanently_unsatisfied_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_permanently_unsatisfied")?;

    harness.start_daemon().await?;

    let _output = harness.start_services_wait(&config_path).await?;

    // Wait for setup to stop (it exits immediately with code 0)
    harness
        .wait_for_service_status(&config_path, "setup", "exited", Duration::from_secs(5))
        .await?;

    // monitor should be marked as skipped because its dependency is permanently unsatisfied
    // (setup stopped with exit 0, won't restart, and service_failed condition requires non-zero)
    harness
        .wait_for_service_status(&config_path, "monitor", "skipped", Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test foreground mode with permanently unsatisfied dependency.
///
/// `kepler start` (no -d) should detect quiescence and exit on its own
/// when all services reach a terminal state (setup=exited, monitor=skipped).
/// This verifies the CLI doesn't block forever when a deferred service's
/// dependency is permanently unsatisfied.
#[tokio::test]
async fn test_permanently_unsatisfied_foreground_exits() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_permanently_unsatisfied")?;

    harness.start_daemon().await?;

    // Run `kepler start` in foreground mode (no -d) from scratch.
    // setup exits immediately (code 0, restart:no) → exited.
    // monitor's dependency (service_failed) is permanently unsatisfied → skipped.
    // The CLI should detect quiescence and exit within a few seconds.
    let config_str = config_path.to_str().unwrap();
    let result = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start"],
            Duration::from_secs(10),
        )
        .await;

    // The CLI should exit before the timeout (quiescence detected)
    assert!(
        result.is_ok(),
        "Foreground start should exit when all services are terminal, but it timed out: {:?}",
        result.err()
    );

    // Verify final states
    harness
        .wait_for_service_status(&config_path, "setup", "exited", Duration::from_secs(2))
        .await?;
    harness
        .wait_for_service_status(&config_path, "monitor", "skipped", Duration::from_secs(2))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test parallel startup: services at the same dependency level start concurrently.
///
/// Each worker sleeps 3s then echoes a marker. If workers run in parallel the
/// markers all appear after ~3s. If they ran sequentially it would take ~9s.
/// We assert the total wall-clock time is under 7s, giving ample margin for
/// parallel execution while catching sequential execution.
#[tokio::test]
async fn test_parallel_startup_same_level() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_parallel_startup")?;

    harness.start_daemon().await?;

    let _output = harness.start_services(&config_path).await?;

    // All services should reach running quickly (process spawned)
    for svc in &["root", "worker-a", "worker-b", "worker-c", "aggregator"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    // Now wait for all 3 worker markers to appear in logs.
    // Each worker sleeps 3s then echoes. Parallel ≈ 3s, sequential ≈ 9s.
    let markers = &["WORKER_A_DONE", "WORKER_B_DONE", "WORKER_C_DONE"];
    let timer = std::time::Instant::now();
    let deadline = Duration::from_secs(15);

    loop {
        let logs = harness.get_logs(&config_path, None, 200).await?;
        let found = markers
            .iter()
            .all(|m| logs.stdout.contains(m));
        if found {
            break;
        }
        if timer.elapsed() > deadline {
            let logs = harness.get_logs(&config_path, None, 200).await?;
            panic!(
                "Timed out waiting for worker markers in logs.\nLogs:\n{}",
                logs.stdout
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let elapsed = timer.elapsed();

    // Parallel: ~3s. Sequential: ~9s. Threshold at 7s gives safe margin.
    assert!(
        elapsed < Duration::from_secs(7),
        "Workers should start in parallel (~3s), but took {:.1}s (sequential would be ~9s)",
        elapsed.as_secs_f64()
    );

    // Verify level ordering: root marker must appear before worker markers
    let logs = harness.get_logs(&config_path, None, 200).await?;
    let root_pos = logs.stdout.find("ROOT_STARTED");
    let first_worker_pos = markers
        .iter()
        .filter_map(|m| logs.stdout.find(m))
        .min();

    assert!(
        root_pos.is_some() && first_worker_pos.is_some(),
        "Expected ROOT_STARTED and worker markers in logs"
    );
    assert!(
        root_pos.unwrap() < first_worker_pos.unwrap(),
        "Root (level 0) should appear in logs before workers (level 1)"
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that exit_code appears in ps output for stopped/failed services
#[tokio::test]
async fn test_ps_shows_exit_code() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_permanently_unsatisfied")?;

    harness.start_daemon().await?;

    let _output = harness.start_services_wait(&config_path).await?;

    // Wait for setup to stop with exit 0
    harness
        .wait_for_service_status(&config_path, "setup", "exited", Duration::from_secs(5))
        .await?;

    // Check ps output includes exit code
    let ps = harness.ps(&config_path).await?;
    assert!(
        ps.stdout.contains("Exited (0)"),
        "ps should show exit code for exited service. ps output: {}",
        ps.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test transient exit filtering: when a dep has restart:always and exits,
/// conditions like service_failed should NOT be considered satisfied because
/// the dep will restart. The dependent should be Skipped (structurally
/// unreachable) instead of starting on every crash/restart cycle.
///
/// "worker" exits with code 1, restart:always (crash-loops).
/// "watcher" depends on worker with condition: service_failed.
/// Without transient exit filtering, watcher could briefly start in the
/// window between worker exiting and being restarted. With the filter,
/// watcher is properly Skipped.
#[tokio::test]
async fn test_transient_exit_not_satisfied() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_transient_exit_not_satisfied")?;

    harness.start_daemon().await?;

    let _output = harness.start_services_wait(&config_path).await?;

    // watcher should be Skipped (structurally unreachable: service_failed + restart:always).
    // Critically, the transient exit filter prevents watcher from starting during the
    // brief window between worker crashing and being restarted.
    harness
        .wait_for_service_status(&config_path, "watcher", "skipped", Duration::from_secs(10))
        .await?;

    // Verify watcher never ran by checking logs don't contain its marker
    let logs = harness.get_logs(&config_path, None, 200).await?;
    assert!(
        !logs.stdout.contains("WATCHER_SHOULD_NOT_RUN"),
        "watcher should never have started (transient exit filter). logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
