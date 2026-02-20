//! E2E tests for restart progress events and related improvements.
//!
//! Tests cover:
//! - Restart --wait completes with progress
//! - Restart with healthcheck reaches healthy after restart
//! - Restart multi-service with dependencies preserves ordering
//! - Auto-restart (handle_exit) re-spawns health checker (bug fix)
//! - Default restart returns immediately (always detached)

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "restart_progress_test";

/// Test that `kepler restart --wait` completes successfully
/// and the service is running after restart.
#[tokio::test]
async fn test_restart_wait_completes() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_wait_completes")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(10))
        .await?;

    // Restart with --wait — should complete and return
    let output = harness.restart_services(&config_path).await?;
    output.assert_success();

    // Service should be running after restart
    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(10))
        .await?;

    // Verify logs show at least two starts (original + restart)
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("WEB_STARTED").count();
    assert!(
        start_count >= 2,
        "Service should have started at least twice after restart. Count: {}, stdout: {}",
        start_count, logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that restart with healthcheck reaches healthy status.
/// The daemon must send progress events through Starting → Running → Healthy
/// for -d --wait to see the service as ready.
#[tokio::test]
async fn test_restart_healthcheck_reaches_healthy() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_healthcheck")?;

    harness.start_daemon().await?;

    // Start and wait for healthy
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "web", "healthy", Duration::from_secs(15))
        .await?;

    // Restart — should go through Stopping → Stopped → Starting → Running → Healthy
    let output = harness.restart_services(&config_path).await?;
    output.assert_success();

    // After restart --wait completes, service should be healthy again
    harness
        .wait_for_service_status(&config_path, "web", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Test restart with multiple services and a dependency chain.
/// db (with healthcheck) → web (depends_on db:service_healthy).
/// After restart, both services should be running and db should be healthy.
#[tokio::test]
async fn test_restart_multi_service_with_deps() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_multi_service")?;

    harness.start_daemon().await?;

    // Start services — web waits for db to be healthy
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "db", "healthy", Duration::from_secs(15))
        .await?;
    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(15))
        .await?;

    // Restart all — stop order: web then db, start order: db then web
    let output = harness.restart_services(&config_path).await?;
    output.assert_success();

    // Both services should be back to their expected states
    harness
        .wait_for_service_status(&config_path, "db", "healthy", Duration::from_secs(15))
        .await?;
    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(15))
        .await?;

    // Verify both restarted (at least 2 start markers each)
    let logs = harness.get_logs(&config_path, None, 200).await?;
    let db_starts = logs.stdout.matches("DB_STARTED").count();
    let web_starts = logs.stdout.matches("WEB_STARTED").count();
    assert!(
        db_starts >= 2,
        "db should have started at least twice. Count: {}, stdout: {}",
        db_starts, logs.stdout
    );
    assert!(
        web_starts >= 2,
        "web should have started at least twice. Count: {}, stdout: {}",
        web_starts, logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that auto-restart (via restart policy) re-spawns the health checker.
/// This verifies the handle_exit() bug fix where spawn_auxiliary_tasks()
/// was not called after auto-restart, leaving health checkers dead.
///
/// Service: exits after 1s with code 1, restart: always, healthcheck: true.
/// After auto-restart, the service should reach healthy again (proving
/// the health checker was re-spawned).
#[tokio::test]
async fn test_auto_restart_respawns_health_checker() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_auto_restart_healthcheck")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for initial healthy status
    harness
        .wait_for_service_status(&config_path, "worker", "healthy", Duration::from_secs(15))
        .await?;

    // Service will exit after 1s, auto-restart kicks in (restart: always).
    // After restart, healthcheck must work again — wait for healthy a second time.
    // First wait for a non-healthy state (the exit/restart transition)
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Now wait for healthy again — this proves health checker was re-spawned
    harness
        .wait_for_service_status(&config_path, "worker", "healthy", Duration::from_secs(15))
        .await?;

    // Confirm it restarted at least once
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("WORKER_STARTED").count();
    assert!(
        start_count >= 2,
        "Service should have auto-restarted at least once. Count: {}, stdout: {}",
        start_count, logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler restart` (default, no flags) returns after progress bars
/// without blocking for log following.
#[tokio::test]
async fn test_restart_default_returns_after_progress() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_wait_completes")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(10))
        .await?;

    // Restart with no flags — should return after progress bars complete
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_path.to_str().unwrap(), "restart"],
            Duration::from_secs(5),
        )
        .await?;
    output.assert_success();

    // Wait for service to come back
    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}
