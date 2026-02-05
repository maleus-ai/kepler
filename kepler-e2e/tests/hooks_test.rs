//! E2E tests for lifecycle hooks
//!
//! Tests global and service-level hooks at various lifecycle events.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "hooks_test";

/// Test that global on_init runs once at first start
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
        "on_init should have run. Content: {}",
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

    // on_init should NOT run again (only runs once)
    let marker_content_2 = std::fs::read_to_string(&marker_file)?;
    let init_count_2 = marker_content_2.matches("ON_INIT_RAN").count();

    assert_eq!(
        init_count_1, init_count_2,
        "on_init should only run once. First: {}, Second: {}",
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
