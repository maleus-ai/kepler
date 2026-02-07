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

/// Test that `kepler restart -d` returns immediately (fire-and-forget)
#[tokio::test]
async fn test_restart_detached_returns_immediately() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // First start services
    harness.start_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Restart in detached mode with a tight timeout — should return quickly
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "restart",
                "-d",
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

/// Test that `kepler recreate -d` returns immediately (fire-and-forget)
#[tokio::test]
async fn test_recreate_detached_returns_immediately() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // First start services
    harness.start_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Recreate in detached mode with a tight timeout — should return quickly
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "recreate",
                "-d",
            ],
            Duration::from_secs(5),
        )
        .await?;

    output.assert_success();

    // Service should eventually come back to running
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `kepler restart` (no -d) blocks following logs
#[tokio::test]
async fn test_restart_attached_blocks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_attached")?;

    harness.start_daemon().await?;

    // First start services
    harness.start_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Restart in attached mode with a 3s timeout — should time out because it follows logs
    let result = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "restart",
            ],
            Duration::from_secs(3),
        )
        .await;

    assert!(
        result.is_err(),
        "Attached restart should block and time out, but it returned: {:?}",
        result
    );

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;

    Ok(())
}
