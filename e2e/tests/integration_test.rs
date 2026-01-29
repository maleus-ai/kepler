//! E2E integration tests combining multiple features
//!
//! Tests complex scenarios involving healthchecks, hooks, file watching, and dependencies.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "integration_test";

/// Test that healthcheck triggers on_healthcheck_success/fail hooks
#[tokio::test]
async fn test_healthcheck_with_hooks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let marker_file = harness.create_temp_file("healthcheck_hooks_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_healthcheck_with_hooks",
        &[("__MARKER_FILE__", marker_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to become healthy (healthcheck passes)
    harness
        .wait_for_service_status(&config_path, "healthcheck-hooks-service", "healthy", Duration::from_secs(15))
        .await?;

    // Wait for the on_healthcheck_success hook to write to the marker file
    let marker_content = harness
        .wait_for_file_content(&marker_file, "HEALTHCHECK_SUCCESS_HOOK", Duration::from_secs(10))
        .await?;

    assert!(
        marker_content.contains("HEALTHCHECK_SUCCESS_HOOK"),
        "on_healthcheck_success hook should have run. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that dependent service waits for dependency to be healthy
#[tokio::test]
async fn test_deps_with_healthcheck() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_deps_with_healthcheck")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for the dependency to be healthy first
    harness
        .wait_for_service_status(&config_path, "healthy-dependency", "healthy", Duration::from_secs(15))
        .await?;

    // Then the dependent service should start
    harness
        .wait_for_service_status(&config_path, "dependent-service", "running", Duration::from_secs(15))
        .await?;

    // Check logs to verify both started in correct order
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    // Both should have started
    assert!(logs.stdout_contains("HEALTHY_DEP_STARTED"), "Dependency should have started");
    assert!(logs.stdout_contains("DEPENDENT_SVC_STARTED"), "Dependent should have started");

    // Verify order: dependency starts before dependent
    let pos_dep = logs.stdout.find("HEALTHY_DEP_STARTED");
    let pos_dependent = logs.stdout.find("DEPENDENT_SVC_STARTED");
    assert!(
        pos_dep < pos_dependent,
        "Dependency should start before dependent service. Dep pos: {:?}, Dependent pos: {:?}",
        pos_dep, pos_dependent
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test full lifecycle: hooks + healthcheck + file watch integration
#[tokio::test]
async fn test_full_lifecycle() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create watch directory and file
    let watch_dir = harness.create_temp_dir("full_lifecycle_watch")?;
    let watched_file = watch_dir.join("config.txt");
    std::fs::write(&watched_file, "initial")?;

    // Create marker file for hooks
    let marker_file = harness.create_temp_file("full_lifecycle_marker.txt", "")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_full_lifecycle",
        &[
            ("__WATCH_DIR__", watch_dir.to_str().unwrap()),
            ("__MARKER_FILE__", marker_file.to_str().unwrap()),
        ],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to become healthy
    harness
        .wait_for_service_status(&config_path, "full-lifecycle-service", "healthy", Duration::from_secs(15))
        .await?;

    // Verify on_start hook ran (poll for content)
    let marker_content = harness
        .wait_for_file_content(&marker_file, "ON_START_HOOK", Duration::from_secs(10))
        .await?;
    assert!(
        marker_content.contains("ON_START_HOOK"),
        "on_start hook should have run. Content: {}",
        marker_content
    );

    // Wait for healthcheck success hook (poll for content)
    let marker_content = harness
        .wait_for_file_content(&marker_file, "HEALTHCHECK_SUCCESS", Duration::from_secs(10))
        .await?;
    assert!(
        marker_content.contains("HEALTHCHECK_SUCCESS"),
        "healthcheck success hook should have run. Content: {}",
        marker_content
    );

    // Modify watched file to trigger restart
    harness.modify_file(&watched_file, "modified")?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Wait for service to be running again after restart
    harness
        .wait_for_service_status(&config_path, "full-lifecycle-service", "running", Duration::from_secs(15))
        .await?;

    // Check logs for restart
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("FULL_LIFECYCLE_START").count();
    assert!(
        start_count >= 2,
        "Service should have started at least twice (initial + restart). Count: {}",
        start_count
    );

    // Stop and verify on_stop hook
    harness.stop_services(&config_path).await?;

    // Wait for on_stop hook to complete (poll for content)
    let marker_content = harness
        .wait_for_file_content(&marker_file, "ON_STOP_HOOK", Duration::from_secs(10))
        .await?;
    assert!(
        marker_content.contains("ON_STOP_HOOK"),
        "on_stop hook should have run. Content: {}",
        marker_content
    );

    harness.stop_daemon().await?;

    Ok(())
}
