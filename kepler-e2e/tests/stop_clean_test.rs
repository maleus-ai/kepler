//! E2E tests for `stop --clean` functionality
//!
//! Tests that `stop --clean` properly removes the state directory for a config.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "stop_clean_test";

/// Test that `stop --clean` removes the state directory
#[tokio::test]
async fn test_stop_clean_removes_state_directory() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_clean_removes_state_directory")?;

    // Start the daemon
    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for the service to start
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Verify state directory exists
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should exist after starting services"
    );

    // Stop with --clean
    let output = harness.stop_services_clean(&config_path).await?;
    output.assert_success();

    // Give the daemon time to clean up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state directory is REMOVED
    assert!(
        !harness.config_state_exists(&config_path),
        "State directory should be removed after stop --clean"
    );

    // Stop daemon
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that regular `stop` (without --clean) keeps the state directory
#[tokio::test]
async fn test_stop_without_clean_keeps_state_directory() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_without_clean_keeps_state_directory")?;

    // Start the daemon
    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for the service to start
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Verify state directory exists
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should exist after starting services"
    );

    // Stop WITHOUT --clean
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Give the daemon time to process
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state directory still exists
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should remain after regular stop"
    );

    // Stop daemon
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `stop --clean` handles non-running services gracefully
#[tokio::test]
async fn test_stop_clean_with_stopped_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_clean_with_stopped_services")?;

    // Start the daemon
    harness.start_daemon().await?;

    // Start services (it will exit quickly)
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait a bit for the service to exit
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Verify state directory exists even if service exited
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should exist even if service exited"
    );

    // Stop with --clean should still work
    let _output = harness.stop_services_clean(&config_path).await?;
    // Don't assert success as the service might already be stopped
    // The important thing is that the state directory is cleaned

    // Give the daemon time to clean up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state directory is removed
    assert!(
        !harness.config_state_exists(&config_path),
        "State directory should be removed after stop --clean"
    );

    // Stop daemon
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that `stop --clean` removes the state directory when the config is not
/// loaded in memory (daemon restarted, autostart disabled — state is orphaned on disk).
#[tokio::test]
async fn test_stop_clean_config_not_loaded() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    // Reuse the basic config (no autostart) so the config won't be rediscovered
    let config_path = harness.load_config(TEST_MODULE, "test_stop_clean_removes_state_directory")?;

    // Start daemon and services
    harness.start_daemon().await?;
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Verify state directory exists
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should exist after starting services"
    );

    // Kill daemon abruptly — config has no autostart, so on restart
    // the daemon will NOT rediscover or reload this config.
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    // Wait for daemon to settle
    tokio::time::sleep(Duration::from_secs(1)).await;

    // State directory should still exist on disk (orphaned)
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should still exist after daemon restart (no autostart)"
    );

    // stop --clean should still clean up the state directory even though
    // the config is not loaded in the registry at all.
    let output = harness.stop_services_clean(&config_path).await?;
    output.assert_success();

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state directory is REMOVED
    assert!(
        !harness.config_state_exists(&config_path),
        "State directory should be removed after stop --clean on unloaded config"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `stop --clean` works with multiple services
#[tokio::test]
async fn test_stop_clean_multiple_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_clean_multiple_services")?;

    // Start the daemon
    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all services to start
    for service in &["service-a", "service-b", "service-c"] {
        harness
            .wait_for_service_status(&config_path, service, "running", Duration::from_secs(10))
            .await?;
    }

    // Verify state directory exists
    assert!(
        harness.config_state_exists(&config_path),
        "State directory should exist after starting services"
    );

    // Stop with --clean
    let output = harness.stop_services_clean(&config_path).await?;
    output.assert_success();

    // Give the daemon time to clean up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state directory is removed
    assert!(
        !harness.config_state_exists(&config_path),
        "State directory should be removed after stop --clean"
    );

    // Stop daemon
    harness.stop_daemon().await?;

    Ok(())
}
