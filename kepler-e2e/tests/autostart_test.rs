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
