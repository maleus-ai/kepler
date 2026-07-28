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

/// Attached `kepler start` must stream only the current run's logs.
///
/// Regression: the stream started at entry ID 0, so every previous run stored
/// in the log database was replayed before the new output.
#[tokio::test]
async fn test_attached_start_does_not_replay_previous_run() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_no_replay")?;
    let config_str = config_path.to_str().unwrap().to_string();

    harness.start_daemon().await?;

    // Run 1: the service prints once and exits, so the attached start returns
    // when quiescence is reached.
    let run1 = harness
        .run_cli_with_timeout(&["-f", &config_str, "start"], Duration::from_secs(20))
        .await?;
    assert_eq!(
        run1.stdout.matches("START_MARKER").count(),
        1,
        "run 1 should print the marker once. stdout: {}",
        run1.stdout
    );

    // Run 2: the service is restarted and prints again — run 1's entry is still
    // in the log database but must not be streamed.
    let run2 = harness
        .run_cli_with_timeout(&["-f", &config_str, "start"], Duration::from_secs(20))
        .await?;
    assert_eq!(
        run2.stdout.matches("START_MARKER").count(),
        1,
        "run 2 should print only the new line, not replay run 1. stdout: {}",
        run2.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler restart --follow` must stream only the restarted run's logs.
#[tokio::test]
async fn test_restart_follow_does_not_replay_previous_run() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_follow_no_replay")?;
    let config_str = config_path.to_str().unwrap().to_string();

    harness.start_daemon().await?;

    // First run writes one marker.
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_log_content(&config_path, "RESTART_MARKER", Duration::from_secs(10))
        .await?;

    // `restart --follow` streams until interrupted — run it in the background,
    // give the restarted service time to log, then stop it and read the output.
    let mut child = harness.spawn_cli_background(&["-f", &config_str, "restart", "--follow"])?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let _ = child.kill();
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    assert_eq!(
        stdout.matches("RESTART_MARKER").count(),
        1,
        "restart --follow should print only the restarted run's line. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
