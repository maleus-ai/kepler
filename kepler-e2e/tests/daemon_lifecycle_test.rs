//! E2E tests for daemon lifecycle operations
//!
//! Tests basic daemon start/stop/status functionality
//! and daemon restart respawn behavior.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "daemon_lifecycle_test";

/// Test that starting the daemon creates a socket file
#[tokio::test]
async fn test_daemon_start_creates_socket() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Verify socket doesn't exist before starting
    assert!(
        !harness.socket_exists(),
        "Socket should not exist before daemon start"
    );

    // Start the daemon
    harness.start_daemon().await?;

    // Verify socket exists after starting
    assert!(
        harness.socket_exists(),
        "Socket should exist after daemon start"
    );

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that stopping the daemon allows it to be restarted
#[tokio::test]
async fn test_daemon_stop_allows_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Start the daemon
    harness.start_daemon().await?;

    // Verify socket exists
    assert!(
        harness.socket_exists(),
        "Socket should exist after daemon start"
    );

    // Stop the daemon
    harness.stop_daemon().await?;

    // Give time for cleanup
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Should be able to start the daemon again
    harness.start_daemon().await?;

    // Verify it's running
    let output = harness.daemon_status().await?;
    assert!(
        output.stdout_contains("running") || output.success(),
        "Daemon should be running after restart. stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that daemon status returns success when daemon is running
#[tokio::test]
async fn test_daemon_status_when_running() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Start the daemon
    harness.start_daemon().await?;

    // Query status
    let output = harness.daemon_status().await?;

    // Should succeed when daemon is running
    assert!(
        output.success(),
        "Daemon status should succeed when running. stderr: {}",
        output.stderr
    );

    // Output should indicate running state
    assert!(
        output.stdout_contains("running") || output.stdout_contains("Running"),
        "Status should indicate daemon is running. stdout: {}",
        output.stdout
    );

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that daemon status indicates not running when daemon is stopped
#[tokio::test]
async fn test_daemon_status_when_stopped() -> E2eResult<()> {
    let harness = E2eHarness::new().await?;

    // Don't start the daemon - query status directly
    let output = harness.daemon_status().await?;

    // Status command should indicate daemon is not running
    // (exit code might still be 0, but message should indicate not running)
    assert!(
        output.stdout_contains("not running") || output.stderr_contains("not running") || !output.success(),
        "Daemon status should indicate not running. stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    Ok(())
}

// ============================================================================
// Daemon restart respawn tests
// ============================================================================

/// After a daemon kill+restart, a previously running service should be respawned.
#[tokio::test]
async fn test_daemon_restart_respawns_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_daemon_restart_respawns_services")?;

    harness.start_daemon().await?;

    // Start the service and wait for it to be running
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Kill the daemon abruptly (simulates systemctl stop)
    harness.kill_daemon().await?;

    // Restart the daemon
    harness.start_daemon().await?;

    // The service should be respawned and running again
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(15))
        .await?;

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// After a daemon kill+restart, multiple services (including those with dependencies)
/// should all be respawned.
#[tokio::test]
async fn test_daemon_restart_respawns_multiple_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_daemon_restart_respawns_multiple_services")?;

    harness.start_daemon().await?;

    // Start all services
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "backend", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "frontend", "running", Duration::from_secs(10))
        .await?;

    // Kill the daemon
    harness.kill_daemon().await?;

    // Restart the daemon
    harness.start_daemon().await?;

    // Both services should be respawned (longer timeout for dependency chain)
    harness
        .wait_for_service_status(&config_path, "backend", "running", Duration::from_secs(20))
        .await?;
    harness
        .wait_for_service_status(&config_path, "frontend", "running", Duration::from_secs(20))
        .await?;

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// After a daemon restart, services should have new PIDs (not reusing old processes).
#[tokio::test]
async fn test_daemon_restart_new_pids() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_daemon_restart_new_pids")?;

    harness.start_daemon().await?;

    // Start the service
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "pidcheck", "running", Duration::from_secs(10))
        .await?;

    // Get the PID before restart
    let ps_before = harness.ps(&config_path).await?;
    let pid_before = harness.extract_pid_from_ps(&ps_before.stdout, "pidcheck");
    assert!(pid_before.is_some(), "Should have a PID before restart. ps: {}", ps_before.stdout);

    // Kill and restart daemon
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    // Wait for service to be running again
    harness
        .wait_for_service_status(&config_path, "pidcheck", "running", Duration::from_secs(15))
        .await?;

    // Get the PID after restart
    let ps_after = harness.ps(&config_path).await?;
    let pid_after = harness.extract_pid_from_ps(&ps_after.stdout, "pidcheck");
    assert!(pid_after.is_some(), "Should have a PID after restart. ps: {}", ps_after.stdout);

    // PIDs should be different (new process)
    assert_ne!(
        pid_before.unwrap(),
        pid_after.unwrap(),
        "Service should have a new PID after daemon restart"
    );

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// Services that were stopped before the daemon restart should NOT be respawned.
#[tokio::test]
async fn test_daemon_restart_stopped_services_stay_stopped() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_daemon_restart_stopped_services_stay_stopped")?;

    harness.start_daemon().await?;

    // Start both services
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "running-service", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "stopped-service", "running", Duration::from_secs(10))
        .await?;

    // Stop one service before the daemon restart
    harness.stop_service(&config_path, "stopped-service").await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "stopped-service", "stopped", Duration::from_secs(10))
        .await?;

    // Kill and restart daemon
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    // The running service should be respawned
    harness
        .wait_for_service_status(&config_path, "running-service", "running", Duration::from_secs(15))
        .await?;

    // The stopped service should remain stopped (not respawned)
    // Give a moment for any spurious respawn to happen
    tokio::time::sleep(Duration::from_secs(2)).await;
    let ps_output = harness.ps(&config_path).await?;
    let stopped_line = ps_output.stdout.lines().find(|l| l.contains("stopped-service"));
    assert!(
        stopped_line.is_some_and(|l| !l.contains("Up ")),
        "stopped-service should NOT be running after daemon restart. ps:\n{}",
        ps_output.stdout
    );

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}
