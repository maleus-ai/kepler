//! E2E tests for attached start mode
//!
//! Tests that `kepler start` (without -d) blocks following logs,
//! and `kepler start -d` returns immediately.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "attached_start_test";

/// Test that `kepler start -d` returns immediately
#[tokio::test]
async fn test_start_detached_returns_immediately() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // Start in detached mode with a tight timeout — should return quickly
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
            ],
            Duration::from_secs(5),
        )
        .await?;

    output.assert_success();

    // Verify service is actually running
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `kepler start` (no -d) blocks (follows logs)
/// We verify this by running with a short timeout and expecting it to time out.
#[tokio::test]
async fn test_start_attached_blocks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // Start in attached mode with a 3s timeout — should time out because it blocks
    let result = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
            ],
            Duration::from_secs(3),
        )
        .await;

    // The command should have timed out (proving it blocks/follows logs)
    assert!(
        result.is_err(),
        "Attached start should block and time out, but it returned: {:?}",
        result
    );

    // Cleanup: stop services separately
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `kepler restart` (no flags) returns immediately after progress bars
#[tokio::test]
async fn test_restart_returns_immediately() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // First start services
    harness.start_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Restart with no flags — should return after progress bars complete
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "restart",
            ],
            Duration::from_secs(5),
        )
        .await?;

    output.assert_success();

    // Service should eventually come back to running
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `kepler recreate` returns quickly (it only re-bakes config, no start/stop)
#[tokio::test]
async fn test_recreate_returns_immediately() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // Start and then stop services
    harness.start_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;
    harness.stop_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "stopped", Duration::from_secs(10))
        .await?;

    // Recreate with a tight timeout — should return quickly (no start/stop)
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_path.to_str().unwrap(), "recreate"],
            Duration::from_secs(5),
        )
        .await?;

    output.assert_success();

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `kepler restart --follow` blocks following logs after restart.
/// Progress bars show the stop+start lifecycle, then log following begins
/// and blocks until Ctrl+C — services keep running.
#[tokio::test]
async fn test_restart_follow_blocks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // First start services
    harness.start_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Restart with --follow and a short timeout — should time out because it
    // follows logs after the restart completes
    let result = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "restart",
                "--follow",
            ],
            Duration::from_secs(5),
        )
        .await;

    // The command should have timed out (proving it blocks/follows logs)
    assert!(
        result.is_err(),
        "Restart --follow should block and time out, but it returned: {:?}",
        result
    );

    // Service should still be running after restart
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;

    Ok(())
}
