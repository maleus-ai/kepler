//! E2E tests for daemon lifecycle operations
//!
//! Tests basic daemon start/stop/status functionality.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

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
