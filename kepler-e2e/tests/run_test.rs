//! E2E tests for `kepler run` command (ephemeral mode)
//!
//! Tests cover: basic run, no snapshot, config re-baking, stopping prior services,
//! --start-clean, run→start transition, daemon restart, partial service run,
//! --no-deps, multi-service, suppress_snapshot auto-reset cycle, and --start-clean
//! log verification.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "run_test";

/// Test that `kepler run -d` starts a service successfully
#[tokio::test]
async fn test_run_starts_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_starts_service")?;

    harness.start_daemon().await?;

    let output = harness.run_services(&config_path).await?;
    output.assert_success();

    // Wait for the service to be running
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Verify logs show the service ran
    let logs = harness
        .wait_for_log_content(&config_path, "Running", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("Running"),
        "Service should have produced output. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler run` does NOT create a snapshot (expanded_config.yaml),
/// even when the config has autostart: true
#[tokio::test]
async fn test_run_no_snapshot() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_no_snapshot")?;

    harness.start_daemon().await?;

    // Run the config (autostart: true, but run should suppress snapshot)
    let output = harness.run_services(&config_path).await?;
    output.assert_success();

    // Wait for the service to be running
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Verify NO snapshot was created
    assert!(
        !harness.snapshot_exists(&config_path),
        "run should NOT create a snapshot (expanded_config.yaml), even with autostart: true"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler run` re-bakes config fresh each time.
/// Running twice with different env values should use the new value on second run.
#[tokio::test]
async fn test_run_rebakes_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Start daemon with clean env (excluding TEST_RUN_VAR)
    harness.start_daemon_with_clean_env(&["TEST_RUN_VAR"]).await?;

    let config_path = harness.load_config(TEST_MODULE, "test_run_rebakes_config")?;

    // First run with original value
    let output = harness
        .run_services_with_env(&config_path, &[("TEST_RUN_VAR", "original_value")])
        .await?;
    output.assert_success();

    // Wait for service to be running
    harness
        .wait_for_service_status(&config_path, "env-service", "running", Duration::from_secs(10))
        .await?;

    // Verify original value in logs
    let logs = harness
        .wait_for_log_content(&config_path, "VAR=original_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("VAR=original_value"),
        "First run should have original value. stdout: {}",
        logs.stdout
    );

    // Second run with changed value — run should reload config fresh
    let output = harness
        .run_services_with_env(&config_path, &[("TEST_RUN_VAR", "changed_value")])
        .await?;
    output.assert_success();

    // Wait for service to be running again
    harness
        .wait_for_service_status(&config_path, "env-service", "running", Duration::from_secs(10))
        .await?;

    // Verify new value in logs
    let logs = harness
        .wait_for_log_content(&config_path, "VAR=changed_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("VAR=changed_value"),
        "Second run should have changed value (re-baked config). stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler run` stops services from a previous start before starting new ones
#[tokio::test]
async fn test_run_stops_prior_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_stops_prior_services")?;

    harness.start_daemon().await?;

    // Start services using the normal start command
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "long-service", "running", Duration::from_secs(10))
        .await?;

    // Get PID of the running service
    let ps_before = harness.ps(&config_path).await?;
    let pid_before = harness.extract_pid_from_ps(&ps_before.stdout, "long-service");
    assert!(
        pid_before.is_some(),
        "Should have a PID before run. ps: {}",
        ps_before.stdout
    );

    // Now run — this should stop the prior service and start a new one
    let output = harness.run_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "long-service", "running", Duration::from_secs(10))
        .await?;

    // Get PID after run
    let ps_after = harness.ps(&config_path).await?;
    let pid_after = harness.extract_pid_from_ps(&ps_after.stdout, "long-service");
    assert!(
        pid_after.is_some(),
        "Should have a PID after run. ps: {}",
        ps_after.stdout
    );

    // PIDs should be different (old process was stopped, new one started)
    assert_ne!(
        pid_before.unwrap(),
        pid_after.unwrap(),
        "run should stop prior services and start new ones (different PIDs)"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler run --start-clean` clears old logs.
/// Uses a unique marker to verify old content is actually gone.
#[tokio::test]
async fn test_run_start_clean() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_start_clean_verifies_logs")?;

    harness.start_daemon().await?;

    // First: start normally to produce logs with a unique marker
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "marker-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for the marker to appear in logs
    harness
        .wait_for_log_content(&config_path, "UNIQUE_MARKER_12345", Duration::from_secs(5))
        .await?;

    // Verify logs exist
    assert!(
        harness.logs_exist(&config_path),
        "Logs should exist after first start"
    );

    // Now run with --start-clean — should wipe everything including old logs
    let output = harness.run_services_start_clean(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "marker-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for fresh logs to appear
    harness
        .wait_for_log_content(&config_path, "UNIQUE_MARKER_12345", Duration::from_secs(5))
        .await?;

    // Get all logs — there should be exactly 1 occurrence of the marker
    // (from the new run, not the old one which was wiped)
    let logs = harness.get_logs(&config_path, None, 1000).await?;
    let marker_count = logs.stdout.matches("UNIQUE_MARKER_12345").count();
    assert_eq!(
        marker_count, 1,
        "After --start-clean, old logs should be gone (only 1 marker from new run). Found {} occurrences. stdout: {}",
        marker_count, logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that after `run` (no snapshot), a subsequent `start` creates a snapshot
#[tokio::test]
async fn test_run_then_start_creates_snapshot() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_then_start_creates_snapshot")?;

    harness.start_daemon().await?;

    // Run first — should NOT create a snapshot
    let output = harness.run_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    assert!(
        !harness.snapshot_exists(&config_path),
        "run should NOT create a snapshot"
    );

    // Now stop and start normally — start should create a snapshot (autostart: true)
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Give time for stop to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Give time for snapshot to be written (post_startup_work)
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        harness.snapshot_exists(&config_path),
        "start should create a snapshot when autostart: true"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that after `run -d`, a daemon restart does NOT auto-start the services.
/// (Because run suppresses the snapshot, there's nothing to restore on restart.)
#[tokio::test]
async fn test_run_no_autostart_on_daemon_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_no_autostart_on_daemon_restart")?;

    harness.start_daemon().await?;

    // Run in detached mode (autostart: true in config, but run suppresses snapshot)
    let output = harness.run_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;

    // Verify no snapshot was created
    assert!(
        !harness.snapshot_exists(&config_path),
        "run should not create a snapshot"
    );

    // Kill and restart the daemon
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    // Give time for any spurious respawn to happen
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Services should NOT be running (no snapshot = no autostart)
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        !ps_output.success() || !ps_output.stdout_contains("Up "),
        "Service should NOT be auto-started after daemon restart (no snapshot from run). ps:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Additional coverage tests
// =========================================================================

/// Test that `run` with specific service names only starts those services.
#[tokio::test]
async fn test_run_specific_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_multi_service")?;

    harness.start_daemon().await?;

    // Run only "db" — api and web should NOT start
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli(&["-f", config_str, "run", "-d", "db"])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "db", "running", Duration::from_secs(10))
        .await?;

    // Give a moment for any spurious starts
    tokio::time::sleep(Duration::from_secs(1)).await;

    let ps_output = harness.ps(&config_path).await?;
    let api_line = ps_output.stdout.lines().find(|l| l.contains("api"));
    let web_line = ps_output.stdout.lines().find(|l| l.contains("web"));

    // api and web should not be running
    assert!(
        api_line.is_none() || !api_line.unwrap().contains("Up "),
        "api should NOT be running when only db was specified. ps:\n{}",
        ps_output.stdout
    );
    assert!(
        web_line.is_none() || !web_line.unwrap().contains("Up "),
        "web should NOT be running when only db was specified. ps:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `run --no-deps` skips dependency waiting.
#[tokio::test]
async fn test_run_no_deps() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_multi_service")?;

    harness.start_daemon().await?;

    // Run "api" with --no-deps — should start without waiting for "db"
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli(&["-f", config_str, "run", "-d", "--no-deps", "api"])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "api", "running", Duration::from_secs(10))
        .await?;

    // db should NOT be started (--no-deps skips pulling in dependencies)
    let ps_output = harness.ps(&config_path).await?;
    let db_line = ps_output.stdout.lines().find(|l| l.contains("db"));
    assert!(
        db_line.is_none() || !db_line.unwrap().contains("Up "),
        "db should NOT be running with --no-deps. ps:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `run` starts all services in a multi-service config.
#[tokio::test]
async fn test_run_multi_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_multi_service")?;

    harness.start_daemon().await?;

    let output = harness.run_services(&config_path).await?;
    output.assert_success();

    // All three services should start (in dependency order)
    for svc in &["db", "api", "web"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(15))
            .await?;
    }

    // Verify all produced output
    for marker in &["DB_STARTED", "API_STARTED", "WEB_STARTED"] {
        let logs = harness
            .wait_for_log_content(&config_path, marker, Duration::from_secs(5))
            .await?;
        assert!(
            logs.stdout_contains(marker),
            "Expected {} in logs. stdout: {}",
            marker,
            logs.stdout
        );
    }

    harness.stop_daemon().await?;
    Ok(())
}

/// Test the full suppress_snapshot auto-reset cycle: run → start → run → start.
/// Each `run` should suppress the snapshot, each `start` should create one.
#[tokio::test]
async fn test_run_start_run_start_snapshot_cycle() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_then_start_creates_snapshot")?;

    harness.start_daemon().await?;

    // 1. Run — no snapshot
    harness.run_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;
    assert!(
        !harness.snapshot_exists(&config_path),
        "First run: no snapshot expected"
    );

    // 2. Stop, then Start — snapshot should be created
    harness.stop_services(&config_path).await?.assert_success();
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        harness.snapshot_exists(&config_path),
        "First start: snapshot expected"
    );

    // 3. Run again — snapshot should be cleared
    harness.run_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;
    assert!(
        !harness.snapshot_exists(&config_path),
        "Second run: no snapshot expected (suppress_snapshot re-armed)"
    );

    // 4. Stop, then Start again — snapshot should be created again
    harness.stop_services(&config_path).await?.assert_success();
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(10))
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        harness.snapshot_exists(&config_path),
        "Second start: snapshot expected (auto-reset worked again)"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `run -d --wait` blocks until services are ready, then returns.
#[tokio::test]
async fn test_run_detach_wait() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_run_starts_service")?;

    harness.start_daemon().await?;

    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli(&["-f", config_str, "run", "-d", "--wait"])
        .await?;
    output.assert_success();

    // After --wait returns, service should be running
    harness
        .wait_for_service_status(&config_path, "echo-service", "running", Duration::from_secs(5))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `run -d --wait --timeout` exits with error on timeout.
#[tokio::test]
async fn test_run_detach_wait_timeout() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    // Use multi_service config — all 3 services need dependency ordering which takes time
    let config_path = harness.load_config(TEST_MODULE, "test_run_multi_service")?;

    harness.start_daemon().await?;

    let config_str = config_path.to_str().unwrap();
    // Use an extremely short timeout that should expire
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "run", "-d", "--wait", "--timeout", "1ms"],
            Duration::from_secs(10),
        )
        .await?;

    // Should fail due to timeout
    assert!(
        !output.success(),
        "run -d --wait --timeout 1ms should fail. stdout: {}, stderr: {}",
        output.stdout,
        output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}
