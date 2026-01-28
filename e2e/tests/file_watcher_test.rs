//! E2E tests for file watcher auto-restart functionality
//!
//! Tests that services restart when watched files change.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "file_watcher_test";

/// Test that modifying a watched file triggers service restart
#[tokio::test]
async fn test_file_watch_triggers_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create the watched directory and initial file
    let watch_dir = harness.create_temp_dir("watched")?;
    let watched_file = watch_dir.join("test.txt");
    std::fs::write(&watched_file, "initial content")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_file_watch_triggers_restart",
        &[("WATCH_DIR", watch_dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "file-watch-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for initial log
    harness.wait_for_log_content(&config_path, "FILE_WATCH_START", Duration::from_secs(5)).await?;

    // Get initial PID (unused but documents we could track it)
    let ps_output = harness.ps(&config_path).await?;
    let _initial_pid = harness.extract_pid_from_ps(&ps_output.stdout, "file-watch-service");

    // Modify the watched file
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.modify_file(&watched_file, "modified content")?;

    // Wait for restart (file watcher should trigger it)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Wait for service to be running again
    harness
        .wait_for_service_status(&config_path, "file-watch-service", "running", Duration::from_secs(10))
        .await?;

    // Check for multiple start messages in logs (indicates restart)
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("FILE_WATCH_START").count();

    // Should have restarted at least once
    assert!(
        start_count >= 2,
        "Service should have restarted after file change. Start count: {}, stdout: {}",
        start_count, logs.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that glob patterns work for file watching
#[tokio::test]
async fn test_file_watch_glob_pattern() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create the watched directory
    let watch_dir = harness.create_temp_dir("glob_watched")?;
    let txt_file = watch_dir.join("test.txt");
    std::fs::write(&txt_file, "initial txt")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_file_watch_glob_pattern",
        &[("WATCH_DIR", watch_dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "glob-watch-service", "running", Duration::from_secs(10))
        .await?;

    harness.wait_for_log_content(&config_path, "GLOB_WATCH_START", Duration::from_secs(5)).await?;

    // Modify a .txt file (should match **/*.txt pattern)
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.modify_file(&txt_file, "modified txt content")?;

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify restart happened
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("GLOB_WATCH_START").count();

    assert!(
        start_count >= 2,
        "Service should have restarted after .txt file change. Start count: {}",
        start_count
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that non-matching files don't trigger restart
#[tokio::test]
async fn test_file_watch_no_restart_unmatched() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create the watched directory
    let watch_dir = harness.create_temp_dir("unmatched_watched")?;
    let txt_file = watch_dir.join("test.txt");
    let json_file = watch_dir.join("test.json");
    std::fs::write(&txt_file, "initial txt")?;
    std::fs::write(&json_file, "{}")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_file_watch_no_restart_unmatched",
        &[("WATCH_DIR", watch_dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "unmatched-watch-service", "running", Duration::from_secs(10))
        .await?;

    harness.wait_for_log_content(&config_path, "UNMATCHED_WATCH_START", Duration::from_secs(5)).await?;

    // Get start count before modifying unmatched file
    let logs_before = harness.get_logs(&config_path, None, 100).await?;
    let start_count_before = logs_before.stdout.matches("UNMATCHED_WATCH_START").count();

    // Modify a .json file (should NOT match *.txt pattern)
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.modify_file(&json_file, "{\"modified\": true}")?;

    // Wait a bit
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify NO restart happened
    let logs_after = harness.get_logs(&config_path, None, 100).await?;
    let start_count_after = logs_after.stdout.matches("UNMATCHED_WATCH_START").count();

    assert_eq!(
        start_count_before, start_count_after,
        "Service should NOT restart for unmatched file. Before: {}, After: {}",
        start_count_before, start_count_after
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that multiple rapid changes result in a single restart (debounce)
#[tokio::test]
async fn test_file_watch_debounce() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create the watched directory
    let watch_dir = harness.create_temp_dir("debounce_watched")?;
    let watched_file = watch_dir.join("test.txt");
    std::fs::write(&watched_file, "initial")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_file_watch_debounce",
        &[("WATCH_DIR", watch_dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&config_path, "debounce-service", "running", Duration::from_secs(10))
        .await?;

    harness.wait_for_log_content(&config_path, "DEBOUNCE_START", Duration::from_secs(5)).await?;

    // Make multiple rapid changes
    tokio::time::sleep(Duration::from_millis(500)).await;
    for i in 0..5 {
        harness.modify_file(&watched_file, &format!("change {}", i))?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Wait for debounce period + restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check that we didn't get 5 restarts (debounce should collapse them)
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("DEBOUNCE_START").count();

    // Should be 2 (initial + 1 debounced restart), not 6
    assert!(
        start_count <= 3,
        "Debounce should collapse multiple changes. Start count: {}, expected <= 3",
        start_count
    );
    assert!(
        start_count >= 2,
        "Should have at least one restart. Start count: {}",
        start_count
    );

    harness.stop_daemon().await?;

    Ok(())
}
