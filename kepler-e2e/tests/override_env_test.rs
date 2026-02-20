//! E2E tests for --override-envs (-e) and --refresh-env flags
//!
//! These tests verify that:
//! - `-e KEY=VALUE` overrides specific sys_env variables
//! - `--refresh-env` replaces all sys_env with the current CLI environment
//! - Overrides persist across restarts (baked into snapshot)
//! - Both start and restart support these flags

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "override_env_test";

/// Test that `start -e KEY=VALUE` overrides specific env vars.
#[tokio::test]
async fn test_start_with_override_envs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let config_path = harness.load_config(TEST_MODULE, "test_override_env")?;

    // Start with -e overrides via run_cli_with_env (passing CLI args with -e flags)
    let output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "start",
            "-d",
            "-e",
            "OVERRIDE_A=hello",
            "-e",
            "OVERRIDE_B=world",
        ])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    // Check logs - the service should see the overridden values
    let logs = harness
        .wait_for_log_content(&config_path, "OVERRIDE_A=hello", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("OVERRIDE_A=hello"),
        "Service should see overridden OVERRIDE_A. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("OVERRIDE_B=world"),
        "Service should see overridden OVERRIDE_B. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `restart -e KEY=VALUE` overrides env vars on restart.
#[tokio::test]
async fn test_restart_with_override_envs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let config_path = harness.load_config(TEST_MODULE, "test_override_env")?;

    // Initial start (no overrides - env vars should be EMPTY)
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    // Verify initial values are EMPTY
    let logs = harness
        .wait_for_log_content(&config_path, "OVERRIDE_A=EMPTY", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("OVERRIDE_A=EMPTY"),
        "Initial start should have EMPTY values. stdout: {}",
        logs.stdout
    );

    // Restart with -e overrides
    let restart_output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "restart",
            "--wait",
            "-e",
            "OVERRIDE_A=restarted",
        ])
        .await?;
    restart_output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    // Wait for new log output
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check that the restarted service sees the overridden value
    let logs = harness
        .wait_for_log_content(&config_path, "OVERRIDE_A=restarted", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("OVERRIDE_A=restarted"),
        "Restarted service should see overridden OVERRIDE_A. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that overrides persist across plain restarts (no -e flag).
/// After an initial `start -e KEY=VALUE`, a plain `restart` should keep the overridden value.
#[tokio::test]
async fn test_override_persists_across_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let config_path = harness.load_config(TEST_MODULE, "test_override_env")?;

    // Start with override
    let output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "start",
            "-d",
            "-e",
            "OVERRIDE_A=persisted",
        ])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "OVERRIDE_A=persisted", Duration::from_secs(5))
        .await?;
    assert!(logs.stdout_contains("OVERRIDE_A=persisted"));

    // Plain restart (no -e) — should still have the overridden value
    let restart_output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "restart",
            "--wait",
        ])
        .await?;
    restart_output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let logs = harness
        .wait_for_log_content(&config_path, "OVERRIDE_A=persisted", Duration::from_secs(5))
        .await?;
    assert!(
        logs.stdout_contains("OVERRIDE_A=persisted"),
        "Override should persist across plain restart. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `start --refresh-env` replaces all sys_env with the current CLI env.
#[tokio::test]
async fn test_start_with_refresh_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    const TEST_VAR: &str = "OVERRIDE_A";
    const INITIAL_VALUE: &str = "initial";
    const REFRESHED_VALUE: &str = "refreshed";

    harness.start_daemon().await?;

    let config_path = harness.load_config(TEST_MODULE, "test_override_env")?;

    // Start with initial value in CLI env
    let output = harness
        .run_cli_with_env(
            &["-f", config_path.to_str().unwrap(), "start", "-d"],
            &[(TEST_VAR, INITIAL_VALUE)],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, INITIAL_VALUE),
            Duration::from_secs(5),
        )
        .await?;
    assert!(logs.stdout_contains(&format!("{}={}", TEST_VAR, INITIAL_VALUE)));

    // Stop and restart with --refresh-env and a different CLI env
    harness.stop_services(&config_path).await?;
    harness
        .wait_for_service_status(&config_path, "env-printer", "stopped", Duration::from_secs(10))
        .await?;

    let refresh_output = harness
        .run_cli_with_env(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--refresh-env",
            ],
            &[(TEST_VAR, REFRESHED_VALUE)],
        )
        .await?;
    refresh_output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, REFRESHED_VALUE),
            Duration::from_secs(5),
        )
        .await?;
    assert!(
        logs.stdout_contains(&format!("{}={}", TEST_VAR, REFRESHED_VALUE)),
        "--refresh-env should replace sys_env with current CLI env. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `restart --refresh-env` refreshes env vars on restart.
#[tokio::test]
async fn test_restart_with_refresh_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    const TEST_VAR: &str = "OVERRIDE_A";
    const INITIAL_VALUE: &str = "initial";
    const REFRESHED_VALUE: &str = "refreshed";

    harness.start_daemon().await?;

    let config_path = harness.load_config(TEST_MODULE, "test_override_env")?;

    // Start with initial value
    let output = harness
        .run_cli_with_env(
            &["-f", config_path.to_str().unwrap(), "start", "-d"],
            &[(TEST_VAR, INITIAL_VALUE)],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, INITIAL_VALUE),
            Duration::from_secs(5),
        )
        .await?;
    assert!(logs.stdout_contains(&format!("{}={}", TEST_VAR, INITIAL_VALUE)));

    // Restart with --refresh-env and updated CLI env
    let restart_output = harness
        .run_cli_with_env(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "restart",
                "--wait",
                "--refresh-env",
            ],
            &[(TEST_VAR, REFRESHED_VALUE)],
        )
        .await?;
    restart_output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-printer", "running", Duration::from_secs(10))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let logs = harness
        .wait_for_log_content(
            &config_path,
            &format!("{}={}", TEST_VAR, REFRESHED_VALUE),
            Duration::from_secs(5),
        )
        .await?;
    assert!(
        logs.stdout_contains(&format!("{}={}", TEST_VAR, REFRESHED_VALUE)),
        "restart --refresh-env should use new CLI env. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
