//! E2E tests for autostart and autostart.environment behavior
//!
//! Tests:
//! - autostart: true enables snapshot persistence and daemon restart respawn
//! - autostart: false (or absent) disables snapshot and respawn
//! - autostart.environment resolves declared env vars
//! - kepler.env is accessible in Lua
//! - autostart + environment persists only declared vars across restart

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "autostart_test";

/// Config with autostart: true. After daemon kill+restart, service should be respawned.
#[tokio::test]
async fn test_autostart_enabled_respawns_on_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_enabled")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Kill the daemon abruptly
    harness.kill_daemon().await?;

    // Restart the daemon
    harness.start_daemon().await?;

    // The service should be respawned (snapshot was written because autostart: true)
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with no autostart (default false). After daemon kill+restart, service should NOT
/// be respawned because no snapshot was written.
#[tokio::test]
async fn test_autostart_disabled_no_respawn_on_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Kill the daemon abruptly
    harness.kill_daemon().await?;

    // Restart the daemon
    harness.start_daemon().await?;

    // Give time for any spurious respawn to happen
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The daemon should have no configs loaded (no snapshot was written)
    let status = harness.daemon_status().await?;
    // The config should not be discovered — either the status shows no services
    // or the config path is not found
    let ps_output = harness.ps(&config_path).await?;
    // ps should fail or show no running services for this config
    assert!(
        !ps_output.success() || !ps_output.stdout_contains("Up "),
        "Service should NOT be respawned after daemon restart (autostart disabled). ps:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with explicit autostart: false. Same as disabled — no respawn.
#[tokio::test]
async fn test_autostart_explicit_false_no_respawn() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_explicit_false")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Kill the daemon abruptly
    harness.kill_daemon().await?;

    // Restart the daemon
    harness.start_daemon().await?;

    // Give time for any spurious respawn to happen
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The config should not be discovered
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        !ps_output.success() || !ps_output.stdout_contains("Up "),
        "Service should NOT be respawned (autostart: false). ps:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with autostart.environment: [HOME, CUSTOM_VAR=custom_value].
/// Verify HOME resolves from CLI env and CUSTOM_VAR resolves to "custom_value".
#[tokio::test]
async fn test_kepler_environment_restricts_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_kepler_environment_resolution")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "env-checker", "running", Duration::from_secs(10))
        .await?;

    // Check logs for resolved values
    let logs = harness
        .wait_for_log_content(&config_path, "CUSTOM_VAR=custom_value", Duration::from_secs(5))
        .await?;

    // HOME should resolve (bare key from CLI env)
    assert!(
        logs.stdout_contains("HOME=/"),
        "HOME should resolve from CLI env. stdout: {}",
        logs.stdout
    );

    // CUSTOM_VAR should be "custom_value" (KEY=VALUE)
    assert!(
        logs.stdout_contains("CUSTOM_VAR=custom_value"),
        "CUSTOM_VAR should resolve to custom_value. stdout: {}",
        logs.stdout
    );

    // SHOULD_NOT_EXIST should be MISSING (not in autostart.environment)
    assert!(
        logs.stdout_contains("SHOULD_MISS=MISSING"),
        "Undeclared vars should not be available. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with autostart.environment. Verify kepler.env.VAR is accessible in Lua !lua command.
#[tokio::test]
async fn test_kepler_environment_lua_access() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_kepler_environment_lua_access")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "lua-checker", "running", Duration::from_secs(10))
        .await?;

    // Check logs for Lua-resolved value
    let logs = harness
        .wait_for_log_content(&config_path, "LUA_SAW=lua_test_value", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("LUA_SAW=lua_test_value"),
        "Lua should see kepler.env.KEPLER_TEST_VAR. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with autostart: true and explicit environment. Start, kill, restart.
/// Verify only declared env vars survive the restart.
#[tokio::test]
async fn test_autostart_with_environment_persists_only_declared() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_with_environment")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Verify PERSIST_VAR is present
    let logs = harness
        .wait_for_log_content(&config_path, "PERSIST_VAR=persisted_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("PERSIST_VAR=persisted_value"),
        "First run should have PERSIST_VAR. stdout: {}",
        logs.stdout
    );

    // Kill and restart the daemon
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    // Service should be respawned (autostart: true)
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(15))
        .await?;

    // After restart, declared env vars should still be available
    let logs = harness
        .wait_for_log_content(&config_path, "PERSIST_VAR=persisted_value", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("PERSIST_VAR=persisted_value"),
        "After restart, declared vars should survive. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Config with no autostart (default false). After daemon kill+restart:
/// - `kepler logs` should still return previous log output from disk
/// - `kepler ps` should show service state from persisted state.json
/// - Services should NOT be running (no respawn)
#[tokio::test]
async fn test_autostart_disabled_logs_and_ps_after_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_logs_after_restart")?;

    harness.start_daemon().await?;

    // Start services and wait for them to be running
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Wait for the log marker to appear
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    // Kill the daemon abruptly (simulates crash/restart)
    harness.kill_daemon().await?;

    // Restart the daemon
    harness.start_daemon().await?;

    // Give time for any spurious respawn to happen
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Logs should still be readable from disk even though config is not loaded
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(
        logs.success(),
        "kepler logs should succeed after restart. stderr: {}",
        logs.stderr
    );
    assert!(
        logs.stdout_contains("WORKER_LOG_MARKER"),
        "Logs should contain previous output after restart. stdout: {}",
        logs.stdout
    );

    // ps should show persisted service state
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.success(),
        "kepler ps should succeed after restart. stderr: {}",
        ps_output.stderr
    );
    // The service should show as stopped (orphan was killed during discovery)
    assert!(
        ps_output.stdout_contains("worker"),
        "ps should show worker service from persisted state. stdout: {}",
        ps_output.stdout
    );
    assert!(
        ps_output.stdout_contains("Stopped"),
        "ps should show worker as Stopped after orphan cleanup. stdout: {}",
        ps_output.stdout
    );
    assert!(
        !ps_output.stdout_contains("Up "),
        "Service should NOT be running after restart (autostart disabled). stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Multi-service config with no autostart. After daemon restart:
/// - Logs filtered by service name should only return that service's logs
/// - ps should show ALL services from persisted state
#[tokio::test]
async fn test_autostart_disabled_multi_service_logs_and_ps_after_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_multi_service")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "alpha", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "beta", "running", Duration::from_secs(10))
        .await?;

    // Wait for both log markers
    harness
        .wait_for_log_content(&config_path, "ALPHA_LOG_MARKER", Duration::from_secs(5))
        .await?;
    harness
        .wait_for_log_content(&config_path, "BETA_LOG_MARKER", Duration::from_secs(5))
        .await?;

    harness.kill_daemon().await?;
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Service-filtered logs: only alpha's logs
    let alpha_logs = harness.get_logs(&config_path, Some("alpha"), 100).await?;
    assert!(
        alpha_logs.success(),
        "kepler logs alpha should succeed. stderr: {}",
        alpha_logs.stderr
    );
    assert!(
        alpha_logs.stdout_contains("ALPHA_LOG_MARKER"),
        "Alpha logs should contain alpha's marker. stdout: {}",
        alpha_logs.stdout
    );
    assert!(
        !alpha_logs.stdout_contains("BETA_LOG_MARKER"),
        "Alpha logs should NOT contain beta's marker. stdout: {}",
        alpha_logs.stdout
    );

    // Service-filtered logs: only beta's logs
    let beta_logs = harness.get_logs(&config_path, Some("beta"), 100).await?;
    assert!(
        beta_logs.success(),
        "kepler logs beta should succeed. stderr: {}",
        beta_logs.stderr
    );
    assert!(
        beta_logs.stdout_contains("BETA_LOG_MARKER"),
        "Beta logs should contain beta's marker. stdout: {}",
        beta_logs.stdout
    );
    assert!(
        !beta_logs.stdout_contains("ALPHA_LOG_MARKER"),
        "Beta logs should NOT contain alpha's marker. stdout: {}",
        beta_logs.stdout
    );

    // Unfiltered logs should contain both
    let all_logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(
        all_logs.stdout_contains("ALPHA_LOG_MARKER") && all_logs.stdout_contains("BETA_LOG_MARKER"),
        "Unfiltered logs should contain both markers. stdout: {}",
        all_logs.stdout
    );

    // ps should show both services
    let ps_output = harness.ps(&config_path).await?;
    assert!(ps_output.success(), "ps should succeed. stderr: {}", ps_output.stderr);
    assert!(
        ps_output.stdout_contains("alpha"),
        "ps should show alpha. stdout: {}",
        ps_output.stdout
    );
    assert!(
        ps_output.stdout_contains("beta"),
        "ps should show beta. stdout: {}",
        ps_output.stdout
    );
    // Neither should be running
    assert!(
        !ps_output.stdout_contains("Up "),
        "No service should be running after restart. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// After graceful stop (services stopped normally), then daemon restart:
/// - Logs should still be readable from disk (requires on_stop: keep)
/// - ps should show stopped/exited status (not running)
#[tokio::test]
async fn test_autostart_disabled_stopped_then_restart_logs_and_ps() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    // Uses config with on_stop: keep so logs survive graceful stop
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_logs_keep")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    // Gracefully stop services (this transitions them to stopped/exited state)
    harness.stop_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "stopped", Duration::from_secs(10))
        .await?;

    // Kill daemon and restart
    harness.kill_daemon().await?;
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Logs should still be readable
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(
        logs.success(),
        "Logs should succeed after stop + restart. stderr: {}",
        logs.stderr
    );
    assert!(
        logs.stdout_contains("WORKER_LOG_MARKER"),
        "Logs should contain marker after stop + restart. stdout: {}",
        logs.stdout
    );

    // ps should show the service in stopped state (not running)
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.success(),
        "ps should succeed after stop + restart. stderr: {}",
        ps_output.stderr
    );
    assert!(
        ps_output.stdout_contains("worker"),
        "ps should show worker. stdout: {}",
        ps_output.stdout
    );
    assert!(
        !ps_output.stdout_contains("Up "),
        "Worker should not be running. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// After `stop --clean` (removes state/logs), then daemon restart:
/// - Logs should return empty (no error)
/// - ps should return empty (no error)
#[tokio::test]
async fn test_autostart_disabled_clean_stop_no_data_after_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_logs_after_restart")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    // Stop with --clean to remove state directory and logs
    harness.stop_services_clean(&config_path).await?.assert_success();

    // Kill daemon and restart
    harness.kill_daemon().await?;
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Logs should return empty, not an error
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(
        logs.success(),
        "Logs should succeed (empty) after clean stop + restart. stderr: {}",
        logs.stderr
    );
    assert!(
        !logs.stdout_contains("WORKER_LOG_MARKER"),
        "Logs should NOT contain marker after clean stop. stdout: {}",
        logs.stdout
    );

    // ps should return empty, not an error
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.success(),
        "ps should succeed (empty) after clean stop + restart. stderr: {}",
        ps_output.stderr
    );
    assert!(
        !ps_output.stdout_contains("worker"),
        "ps should NOT show worker after clean stop. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// After daemon restart with autostart:false, `kepler start` should still work.
/// The disk-read fallback must not interfere with subsequent config loading.
#[tokio::test]
async fn test_autostart_disabled_start_works_after_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_logs_after_restart")?;

    harness.start_daemon().await?;

    // First run: start, verify running, kill daemon
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    harness.kill_daemon().await?;
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify logs/ps work from disk (config not loaded)
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(logs.success(), "Disk fallback logs should work. stderr: {}", logs.stderr);

    // Now start services again — this should load the config actor normally
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Verify logs from the new run are available (new marker written)
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    // ps should show running
    let ps_output = harness.ps(&config_path).await?;
    assert!(ps_output.success(), "ps should succeed. stderr: {}", ps_output.stderr);
    assert!(
        ps_output.stdout_contains("Up "),
        "Worker should be running after re-start. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Verify default `kepler logs` mode (LogsCursor streaming) works from disk fallback after restart.
/// This is the most common code path: `kepler logs` without --head/--tail uses cursor-based
/// streaming via Request::LogsCursor, which falls back to disk when config is not loaded.
#[tokio::test]
async fn test_autostart_disabled_logs_default_mode_after_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_logs_after_restart")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    harness.kill_daemon().await?;
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Use default mode (no --head/--tail) which triggers LogsCursor path
    let config_str = config_path.to_str().unwrap();
    let default_logs = harness
        .run_cli_with_timeout(
            &["-f", config_str, "logs"],
            Duration::from_secs(10),
        )
        .await?;
    assert!(
        default_logs.success(),
        "kepler logs (default mode) should succeed from disk. stderr: {}",
        default_logs.stderr
    );
    assert!(
        default_logs.stdout_contains("WORKER_LOG_MARKER"),
        "Default mode logs should contain marker from disk. stdout: {}",
        default_logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Verify --head mode logs work from disk fallback after restart.
#[tokio::test]
async fn test_autostart_disabled_logs_head_mode_after_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_autostart_disabled_logs_after_restart")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "WORKER_LOG_MARKER", Duration::from_secs(5))
        .await?;

    harness.kill_daemon().await?;
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Use --head mode to read logs from disk
    let config_str = config_path.to_str().unwrap();
    let head_logs = harness.run_cli(&["-f", config_str, "logs", "--head", "10"]).await?;
    assert!(
        head_logs.success(),
        "kepler logs --head should succeed from disk. stderr: {}",
        head_logs.stderr
    );
    assert!(
        head_logs.stdout_contains("WORKER_LOG_MARKER"),
        "Head mode logs should contain marker from disk. stdout: {}",
        head_logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// After daemon restart with autostart:false, `kepler start` should preserve
/// service.initialized from the previous run. The init hook (guarded by
/// `if: ${{ not service.initialized }}$`) must NOT re-fire on the second start.
#[tokio::test]
async fn test_autostart_disabled_preserves_initialized_across_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    // Create a marker file for tracking hook executions
    let marker_file = harness
        .temp_dir()
        .path()
        .join("markers")
        .join("init_persist_marker.txt");
    std::fs::create_dir_all(marker_file.parent().unwrap())?;

    // Load config (no autostart) with marker file path substituted
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_autostart_disabled_preserves_initialized",
        &[("MARKER_FILE", marker_file.to_str().unwrap())],
    )?;

    // First run: start services, init hook should fire
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(
            &config_path,
            "init-service",
            "running",
            Duration::from_secs(10),
        )
        .await?;
    harness
        .wait_for_file_content(&marker_file, "STARTED", Duration::from_secs(5))
        .await?;

    // Verify ON_INIT fired exactly once
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let init_count = content.matches("ON_INIT").count();
    assert_eq!(
        init_count, 1,
        "Init hook should fire once on first start. Content: {}",
        content
    );

    // Kill daemon abruptly (simulates systemctl stop / crash)
    harness.kill_daemon().await?;

    // Restart daemon — config is discovered on disk but NOT loaded (autostart: false)
    harness.start_daemon().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Start services again — config is loaded fresh from source
    harness.start_services(&config_path).await?.assert_success();
    harness
        .wait_for_service_status(
            &config_path,
            "init-service",
            "running",
            Duration::from_secs(10),
        )
        .await?;

    // Wait for the second STARTED marker to confirm service actually re-ran
    tokio::time::sleep(Duration::from_secs(1)).await;
    let content = std::fs::read_to_string(&marker_file).unwrap_or_default();
    let started_count = content.matches("STARTED").count();
    assert_eq!(
        started_count, 2,
        "Service should have started twice. Content: {}",
        content
    );

    // ON_INIT should still be 1 — initialized flag should have persisted from state.json
    let init_count = content.matches("ON_INIT").count();
    assert_eq!(
        init_count, 1,
        "Init hook should NOT re-fire after daemon restart (service.initialized should persist). Content: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}
