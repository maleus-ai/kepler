//! E2E tests for restart vs recreate command semantics
//!
//! restart: Preserves baked config, runs restart hooks
//! recreate: Re-bakes config, clears state, starts fresh

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "recreate_test";

/// Test that restart preserves baked config (doesn't re-expand env vars)
#[tokio::test]
async fn test_restart_preserves_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Start daemon with clean env (excluding TEST_RESTART_VAR)
    harness.start_daemon_with_clean_env(&["TEST_RESTART_VAR"]).await?;

    let config_path = harness.load_config(TEST_MODULE, "test_restart_preserves_config")?;

    // Start services with original env value
    let output = harness
        .start_services_with_env(&config_path, &[("TEST_RESTART_VAR", "original_value")])
        .await?;
    output.assert_success();

    // Wait for service to be running
    harness
        .wait_for_service_status(&config_path, "env-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for initial log output
    let logs = harness
        .wait_for_log_content(&config_path, "VAR=original_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("VAR=original_value"),
        "First run should have original value. stdout: {}",
        logs.stdout
    );

    // Restart with different env value (should still use original baked value)
    let output = harness
        .run_cli_with_env(
            &["-f", config_path.to_str().unwrap(), "restart"],
            &[("TEST_RESTART_VAR", "changed_value")],
        )
        .await?;
    output.assert_success();

    // Wait for restart to complete
    harness
        .wait_for_service_status(&config_path, "env-service", "running", Duration::from_secs(10))
        .await?;

    // Give time for logs to be written
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check logs - should still have original value because restart preserves baked config
    let logs = harness.get_logs(&config_path, None, 100).await?;

    // Count occurrences of each value
    let original_count = logs.stdout.matches("VAR=original_value").count();
    let changed_count = logs.stdout.matches("VAR=changed_value").count();

    // There should be at least 2 original values (first start + restart) and no changed values
    assert!(
        original_count >= 2,
        "Should have at least 2 occurrences of original value after restart. stdout: {}",
        logs.stdout
    );
    assert_eq!(
        changed_count, 0,
        "Should NOT have any occurrences of changed value (restart preserves baked config). stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that recreate re-bakes config (re-expands env vars)
#[tokio::test]
async fn test_recreate_rebakes_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Start daemon with clean env (excluding TEST_RECREATE_VAR)
    harness.start_daemon_with_clean_env(&["TEST_RECREATE_VAR"]).await?;

    let config_path = harness.load_config(TEST_MODULE, "test_recreate_rebakes_config")?;

    // Start services with original env value
    let output = harness
        .start_services_with_env(&config_path, &[("TEST_RECREATE_VAR", "original_value")])
        .await?;
    output.assert_success();

    // Wait for service to be running
    harness
        .wait_for_service_status(&config_path, "env-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for initial log output
    let logs = harness
        .wait_for_log_content(&config_path, "VAR=original_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("VAR=original_value"),
        "First run should have original value. stdout: {}",
        logs.stdout
    );

    // Recreate with different env value (should use new value)
    let output = harness
        .recreate_services_with_env(&config_path, &[("TEST_RECREATE_VAR", "changed_value")])
        .await?;
    output.assert_success();

    // Wait for recreate to complete
    harness
        .wait_for_service_status(&config_path, "env-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for new log output
    let logs = harness
        .wait_for_log_content(&config_path, "VAR=changed_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("VAR=changed_value"),
        "After recreate, should have new value. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that restart calls pre_restart and post_restart hooks
#[tokio::test]
async fn test_restart_calls_hooks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    // Create a marker file path
    let marker_file = harness.temp_dir().path().join("markers").join("hook_marker.txt");
    std::fs::create_dir_all(marker_file.parent().unwrap())?;

    // Load config with marker file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_restart_calls_hooks",
        &[("MARKER_FILE", marker_file.to_str().unwrap())],
    )?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to be running
    harness
        .wait_for_service_status(&config_path, "hook-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for initial marker
    harness
        .wait_for_file_content(&marker_file, "STARTED", Duration::from_secs(5))
        .await?;

    // Verify pre_restart hook has NOT fired yet
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    assert!(
        !content.contains("PRE_RESTART"),
        "pre_restart hook should not fire before restart. Content: {}",
        content
    );

    // Restart the service
    let output = harness.restart_services(&config_path).await?;
    output.assert_success();

    // Wait for restart to complete
    harness
        .wait_for_service_status(&config_path, "hook-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for restart hooks
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify pre_restart hook fired
    let content = harness
        .wait_for_file_content(&marker_file, "PRE_RESTART", Duration::from_secs(5))
        .await?;
    assert!(
        content.contains("PRE_RESTART"),
        "pre_restart hook should fire on restart. Content: {}",
        content
    );

    // Verify post_restart hook fired
    let content = harness
        .wait_for_file_content(&marker_file, "POST_RESTART", Duration::from_secs(5))
        .await?;
    assert!(
        content.contains("POST_RESTART"),
        "post_restart hook should fire on restart. Content: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that recreate runs pre_init hooks (because state is cleared)
#[tokio::test]
async fn test_recreate_runs_init_hooks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    // Create a marker file path
    let marker_file = harness.temp_dir().path().join("markers").join("init_marker.txt");
    std::fs::create_dir_all(marker_file.parent().unwrap())?;

    // Load config with marker file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_recreate_runs_init_hooks",
        &[("MARKER_FILE", marker_file.to_str().unwrap())],
    )?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to be running
    harness
        .wait_for_service_status(&config_path, "init-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for initial marker
    harness
        .wait_for_file_content(&marker_file, "STARTED", Duration::from_secs(5))
        .await?;

    // Count initial PRE_INIT calls
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let init_count_1 = content.matches("PRE_INIT").count();
    assert_eq!(init_count_1, 1, "pre_init should fire once on first start. Content: {}", content);

    // Recreate the service (should clear state and run pre_init again)
    let output = harness.recreate_services(&config_path).await?;
    output.assert_success();

    // Wait for recreate to complete
    harness
        .wait_for_service_status(&config_path, "init-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for new init marker
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Count PRE_INIT calls after recreate
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let init_count_2 = content.matches("PRE_INIT").count();
    assert_eq!(
        init_count_2, 2,
        "pre_init should fire again after recreate (state cleared). Content: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that stop respects reverse dependency order (dependents stop first)
#[tokio::test]
async fn test_stop_reverse_dependency_order() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    // Create a marker file path
    let marker_file = harness.temp_dir().path().join("markers").join("stop_order.txt");
    std::fs::create_dir_all(marker_file.parent().unwrap())?;

    // Load config with marker file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_stop_reverse_order",
        &[("MARKER_FILE", marker_file.to_str().unwrap())],
    )?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all services to be running
    for svc in &["db", "api", "web"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    // Stop all services
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for stop to complete
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Check the order
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    // Expected order: web -> api -> db (reverse dependency order)
    let stop_web_pos = lines.iter().position(|l| *l == "STOP_WEB");
    let stop_api_pos = lines.iter().position(|l| *l == "STOP_API");
    let stop_db_pos = lines.iter().position(|l| *l == "STOP_DB");

    assert!(
        stop_web_pos.is_some() && stop_api_pos.is_some() && stop_db_pos.is_some(),
        "All services should be stopped. Lines: {:?}",
        lines
    );

    assert!(
        stop_web_pos.unwrap() < stop_api_pos.unwrap(),
        "Web should stop BEFORE api. Lines: {:?}",
        lines
    );

    assert!(
        stop_api_pos.unwrap() < stop_db_pos.unwrap(),
        "Api should stop BEFORE db. Lines: {:?}",
        lines
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that recreate respects dependency order (stop reverse, start forward)
#[tokio::test]
async fn test_recreate_respects_dependency_order() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    // Create a marker file path
    let marker_file = harness.temp_dir().path().join("markers").join("recreate_order.txt");
    std::fs::create_dir_all(marker_file.parent().unwrap())?;

    // Load config with marker file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_recreate_order",
        &[("MARKER_FILE", marker_file.to_str().unwrap())],
    )?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for services to be running
    for svc in &["backend", "frontend"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    // Clear the marker file
    std::fs::remove_file(&marker_file).ok();

    // Recreate services
    let output = harness.recreate_services(&config_path).await?;
    output.assert_success();

    // Wait for recreate to complete
    for svc in &["backend", "frontend"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check the order
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    let stop_frontend_pos = lines.iter().position(|l| *l == "STOP_FRONTEND");
    let stop_backend_pos = lines.iter().position(|l| *l == "STOP_BACKEND");
    let start_backend_pos = lines.iter().position(|l| *l == "START_BACKEND");
    let start_frontend_pos = lines.iter().position(|l| *l == "START_FRONTEND");

    assert!(
        stop_frontend_pos.is_some() && stop_backend_pos.is_some(),
        "Both services should be stopped. Lines: {:?}",
        lines
    );
    assert!(
        start_backend_pos.is_some() && start_frontend_pos.is_some(),
        "Both services should be started. Lines: {:?}",
        lines
    );

    // Stop order: frontend before backend
    assert!(
        stop_frontend_pos.unwrap() < stop_backend_pos.unwrap(),
        "Frontend should stop BEFORE backend. Lines: {:?}",
        lines
    );

    // Start order: backend before frontend
    assert!(
        start_backend_pos.unwrap() < start_frontend_pos.unwrap(),
        "Backend should start BEFORE frontend. Lines: {:?}",
        lines
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that restart calls all hooks in correct order
#[tokio::test]
async fn test_restart_hook_order() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    // Create a marker file path
    let marker_file = harness.temp_dir().path().join("markers").join("restart_hook_order.txt");
    std::fs::create_dir_all(marker_file.parent().unwrap())?;

    // Load config with marker file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_restart_hook_order",
        &[("MARKER_FILE", marker_file.to_str().unwrap())],
    )?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-svc", "running", Duration::from_secs(10))
        .await?;

    // Clear the marker file
    std::fs::remove_file(&marker_file).ok();

    // Restart service
    let output = harness.restart_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-svc", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check all hooks were called in order
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    // Expected order: PRE_RESTART -> PRE_STOP -> POST_STOP -> PRE_START -> POST_START -> POST_RESTART
    let pre_restart_pos = lines.iter().position(|l| *l == "PRE_RESTART");
    let pre_stop_pos = lines.iter().position(|l| *l == "PRE_STOP");
    let post_stop_pos = lines.iter().position(|l| *l == "POST_STOP");
    let pre_start_pos = lines.iter().position(|l| *l == "PRE_START");
    let post_start_pos = lines.iter().position(|l| *l == "POST_START");
    let post_restart_pos = lines.iter().position(|l| *l == "POST_RESTART");

    assert!(pre_restart_pos.is_some(), "PRE_RESTART should be called. Lines: {:?}", lines);
    assert!(pre_stop_pos.is_some(), "PRE_STOP should be called. Lines: {:?}", lines);
    assert!(post_stop_pos.is_some(), "POST_STOP should be called. Lines: {:?}", lines);
    assert!(pre_start_pos.is_some(), "PRE_START should be called. Lines: {:?}", lines);
    assert!(post_start_pos.is_some(), "POST_START should be called. Lines: {:?}", lines);
    assert!(post_restart_pos.is_some(), "POST_RESTART should be called. Lines: {:?}", lines);

    // Verify order
    assert!(
        pre_restart_pos.unwrap() < pre_stop_pos.unwrap(),
        "PRE_RESTART should come before PRE_STOP. Lines: {:?}",
        lines
    );
    assert!(
        pre_stop_pos.unwrap() < post_stop_pos.unwrap(),
        "PRE_STOP should come before POST_STOP. Lines: {:?}",
        lines
    );
    assert!(
        post_stop_pos.unwrap() < pre_start_pos.unwrap(),
        "POST_STOP should come before PRE_START. Lines: {:?}",
        lines
    );
    assert!(
        pre_start_pos.unwrap() < post_start_pos.unwrap(),
        "PRE_START should come before POST_START. Lines: {:?}",
        lines
    );
    assert!(
        post_start_pos.unwrap() < post_restart_pos.unwrap(),
        "POST_START should come before POST_RESTART. Lines: {:?}",
        lines
    );

    harness.stop_daemon().await?;
    Ok(())
}
