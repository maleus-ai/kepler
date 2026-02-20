//! E2E tests for lifecycle hooks
//!
//! Tests global and service-level hooks at various lifecycle events.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "hooks_test";

/// Test that global pre_start with `if: "not service.initialized"` runs once at first start
#[tokio::test]
async fn test_global_on_init_runs_once() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create marker file location
    let marker_file = harness.create_temp_file("init_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_on_init_runs_once",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // First start
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "init-hook-service", "running", Duration::from_secs(10))
        .await?;

    // Check marker file was written
    tokio::time::sleep(Duration::from_millis(500)).await;
    let marker_content = std::fs::read_to_string(&marker_file)?;
    let init_count_1 = marker_content.matches("ON_INIT_RAN").count();

    assert!(
        init_count_1 >= 1,
        "pre_start with if condition should have run on first start. Content: {}",
        marker_content
    );

    // Stop and start again
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "init-hook-service", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Hook should NOT run again (if condition prevents it after initialization)
    let marker_content_2 = std::fs::read_to_string(&marker_file)?;
    let init_count_2 = marker_content_2.matches("ON_INIT_RAN").count();

    assert_eq!(
        init_count_1, init_count_2,
        "pre_start with if condition should only run once. First: {}, Second: {}",
        init_count_1, init_count_2
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that global pre_start runs when services start
#[tokio::test]
async fn test_global_on_start_runs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("start_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_on_start_runs",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "start-hook-service", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("PRE_START_RAN"),
        "Global pre_start hook should have run. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that global pre_stop runs when services stop
#[tokio::test]
async fn test_global_on_stop_runs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("stop_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_on_stop_runs",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "stop-hook-service", "running", Duration::from_secs(10))
        .await?;

    // Stop services
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("PRE_STOP_RAN"),
        "Global pre_stop hook should have run. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that global pre_cleanup runs with --clean flag
#[tokio::test]
async fn test_global_on_cleanup_runs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("cleanup_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_on_cleanup_runs",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "cleanup-hook-service", "running", Duration::from_secs(10))
        .await?;

    // Stop with --clean flag
    harness.stop_services_clean(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("PRE_CLEANUP_RAN"),
        "Global pre_cleanup hook should have run with --clean. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that service pre_start hook runs on each service start
#[tokio::test]
async fn test_service_on_start_runs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("svc_start_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_service_on_start_runs",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // First start
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "svc-start-hook-service", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;
    let marker_content = std::fs::read_to_string(&marker_file)?;
    let count_1 = marker_content.matches("SERVICE_PRE_START_RAN").count();

    assert!(count_1 >= 1, "Service pre_start should have run. Content: {}", marker_content);

    // Restart the service
    harness.restart_service(&config_path, "svc-start-hook-service").await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    harness
        .wait_for_service_status(&config_path, "svc-start-hook-service", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;
    let marker_content = std::fs::read_to_string(&marker_file)?;
    let count_2 = marker_content.matches("SERVICE_PRE_START_RAN").count();

    assert!(
        count_2 > count_1,
        "Service pre_start should run on each start. Count: {} -> {}",
        count_1, count_2
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that service pre_stop hook runs when service stops
#[tokio::test]
async fn test_service_on_stop_runs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("svc_stop_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_service_on_stop_runs",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "svc-stop-hook-service", "running", Duration::from_secs(10))
        .await?;

    // Stop the service
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("SERVICE_PRE_STOP_RAN"),
        "Service pre_stop hook should have run. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that service post_exit hook runs when process exits
#[tokio::test]
async fn test_service_on_exit_runs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("svc_exit_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_service_on_exit_runs",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Service exits after 1 second, wait for it
    tokio::time::sleep(Duration::from_secs(3)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("SERVICE_POST_EXIT_RAN"),
        "Service post_exit hook should have run when process exited. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that global post_stop runs even when services are already terminal (e.g. failed)
/// before `stop` is called. Previously, `stopped.is_empty()` caused the hook to be skipped.
#[tokio::test]
async fn test_global_post_stop_runs_when_services_already_stopped() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("post_stop_already_stopped_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_post_stop_runs_when_services_already_stopped",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start in detached mode — service exits immediately with exit 1
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to reach a terminal status
    harness
        .wait_for_service_status_any(&config_path, "failing-service", &["failed", "exited"], Duration::from_secs(10))
        .await?;

    // Now call stop — service is already terminal
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("PRE_STOP_RAN"),
        "Global pre_stop hook should have run. Content: {}",
        marker_content
    );
    assert!(
        marker_content.contains("POST_STOP_RAN"),
        "Global post_stop hook should have run even when services were already stopped. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that global pre_stop and post_stop hooks fire on quiescence
/// (when all services naturally exit without an explicit `stop` call).
#[tokio::test]
async fn test_global_hooks_on_quiescence() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("quiescence_hooks_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_hooks_on_quiescence",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start in foreground mode (no -d) — CLI blocks until quiescence then exits
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start"],
            Duration::from_secs(15),
        )
        .await?;

    output.assert_success();

    // Give hooks a moment to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("PRE_STOP_RAN"),
        "Global pre_stop hook should have run on quiescence. Content: {}",
        marker_content
    );
    assert!(
        marker_content.contains("POST_STOP_RAN"),
        "Global post_stop hook should have run on quiescence. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that global hooks fire on quiescence even when the service runs for a
/// while before exiting (race condition: the quiescent signal must not reach
/// the CLI before hooks have completed).
#[tokio::test]
async fn test_global_hooks_on_delayed_quiescence() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("delayed_quiescence_hooks_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_global_hooks_on_delayed_quiescence",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start in foreground mode (no -d) — service runs for 1s then exits
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start"],
            Duration::from_secs(15),
        )
        .await?;

    output.assert_success();

    // Hooks must have completed before the CLI exited
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("PRE_STOP_RAN"),
        "Global pre_stop hook should have run on delayed quiescence. Content: {}",
        marker_content
    );
    assert!(
        marker_content.contains("POST_STOP_RAN"),
        "Global post_stop hook should have run on delayed quiescence. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that quiescence hook logs are cleared after quiescence (retention policy).
/// Without retention, pre_stop/post_stop logs would accumulate across runs.
#[tokio::test]
async fn test_quiescence_hook_logs_cleared_on_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("retention_quiescence_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_quiescence_hook_logs_cleared_on_restart",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // First foreground start — service exits after 1s, quiescence hooks fire
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(&["-f", config_str, "start"], Duration::from_secs(15))
        .await?;
    output.assert_success();

    // Verify hooks ran
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(marker_content.contains("PRE_STOP_RAN"), "First run hooks should fire");

    // After quiescence, pre_stop/post_stop logs should be cleared by retention
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let pre_stop_count = logs.stdout.matches("global.pre_stop").count();
    assert_eq!(
        pre_stop_count, 0,
        "After quiescence, pre_stop logs should be cleared by retention. Logs:\n{}",
        logs.stdout
    );

    // Second foreground start — no stale hook logs to accumulate
    let output = harness
        .run_cli_with_timeout(&["-f", config_str, "start"], Duration::from_secs(15))
        .await?;
    output.assert_success();

    // Still 0 — no accumulation
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let pre_stop_count = logs.stdout.matches("global.pre_stop").count();
    assert_eq!(
        pre_stop_count, 0,
        "After second run, pre_stop logs should still be cleared. Logs:\n{}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that on_stop log retention applies to global hook logs even when services
/// are already terminal before `stop` is called. Previously, the `!stopped.is_empty()`
/// condition caused retention to be skipped, so hook logs accumulated.
#[tokio::test]
async fn test_stop_hook_logs_retention_when_already_stopped() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("retention_stop_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_stop_hook_logs_cleared_on_next_stop",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start detached — service exits immediately with exit 1
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for terminal status
    harness
        .wait_for_service_status_any(&config_path, "failing-service", &["failed", "exited"], Duration::from_secs(10))
        .await?;

    // First stop — service already terminal, hooks fire then retention clears
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify hooks ran (marker file)
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(marker_content.contains("PRE_STOP_RAN"), "Hooks should have fired");

    // on_stop retention (default: clear) clears global hook logs after hooks run
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let pre_stop_count = logs.stdout.matches("global.pre_stop").count();
    assert_eq!(
        pre_stop_count, 0,
        "After stop, global hook logs should be cleared by on_stop retention. Logs:\n{}",
        logs.stdout
    );

    // Start again, wait for terminal, stop again
    let output = harness.start_services(&config_path).await?;
    output.assert_success();
    harness
        .wait_for_service_status_any(&config_path, "failing-service", &["failed", "exited"], Duration::from_secs(10))
        .await?;
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Still 0 — no accumulation
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let pre_stop_count = logs.stdout.matches("global.pre_stop").count();
    assert_eq!(
        pre_stop_count, 0,
        "After second stop, global hook logs should still be cleared. Logs:\n{}",
        logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
