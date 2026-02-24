//! E2E tests for user-specific environment variable injection.
//!
//! When a service specifies `user:`, HOME/USER/LOGNAME/SHELL should be
//! auto-injected from the target user's /etc/passwd entry, not the CLI caller's.
//! Explicit `environment:` or `env_file:` values always take priority.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "user_env_test";

/// Extract the value of a `KEY=value` line from log output.
fn extract_env_value<'a>(stdout: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{}=", key);
    stdout.lines()
        .find_map(|line| line.find(&prefix).map(|pos| &line[pos + prefix.len()..]))
}

/// When `user: testuser1` is set with inherit_env: true, HOME/USER/LOGNAME/SHELL
/// should reflect testuser1's passwd entry, not the CLI caller (root).
#[tokio::test]
async fn test_user_env_injected() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_user_env_injected")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "user-env-svc", "running", Duration::from_secs(10))
        .await?;

    // Wait for all env lines to appear
    let logs = harness
        .wait_for_log_content(&config_path, "SHELL=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;

    // HOME should be testuser1's home directory (platform-specific path),
    // not the CLI caller's (root) home
    let home = extract_env_value(stdout, "HOME").expect("HOME not found in output");
    assert!(
        home.contains("testuser1"),
        "HOME should be testuser1's home dir, not root's. Got HOME={}. stdout: {}",
        home, stdout
    );

    // USER and LOGNAME should be testuser1
    assert!(
        stdout.contains("USER=testuser1"),
        "USER should be testuser1. stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("LOGNAME=testuser1"),
        "LOGNAME should be testuser1. stdout: {}",
        stdout
    );

    // SHELL should be set (value is platform-specific)
    assert!(
        extract_env_value(stdout, "SHELL").is_some_and(|s| !s.is_empty()),
        "SHELL should be set from passwd. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// With inherit_env: false and user: testuser1, user env vars should still
/// be injected even though CLI environment is excluded.
#[tokio::test]
async fn test_user_env_injected_without_inherit() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_user_env_no_inherit")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "user-env-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "SHELL=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;

    // Even without inherit_env, user vars should be injected
    let home = extract_env_value(stdout, "HOME").expect("HOME not found in output");
    assert!(
        home.contains("testuser1"),
        "HOME should be testuser1's home dir even with inherit_env: false. Got HOME={}. stdout: {}",
        home, stdout
    );
    assert!(
        stdout.contains("USER=testuser1"),
        "USER should be testuser1 even with inherit_env: false. stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("LOGNAME=testuser1"),
        "LOGNAME should be testuser1 even with inherit_env: false. stdout: {}",
        stdout
    );
    assert!(
        extract_env_value(stdout, "SHELL").is_some_and(|s| !s.is_empty()),
        "SHELL should be set even with inherit_env: false. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Explicit `environment: [HOME=/custom/home]` should override the auto-injected value.
#[tokio::test]
async fn test_user_env_explicit_override() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_user_env_explicit_override")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "user-env-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "USER=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;

    // HOME was explicitly set — should keep the explicit value
    assert!(
        stdout.contains("HOME=/custom/home"),
        "Explicit HOME should override injected value. stdout: {}",
        stdout
    );

    // USER was NOT explicitly set — should be injected from passwd
    assert!(
        stdout.contains("USER=testuser1"),
        "USER should be auto-injected when not explicitly set. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// With `inject_user_env: after` and explicit `environment: [HOME=/custom/home]`,
/// HOME should be overridden to match testuser1's /etc/passwd entry.
#[tokio::test]
async fn test_override_user_env_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_override_user_env_service")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "user-env-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "SHELL=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;

    // HOME was explicitly set to /custom/home, but inject_user_env: after
    // should force it to match testuser1's passwd entry
    let home = extract_env_value(stdout, "HOME").expect("HOME not found in output");
    assert!(
        home.contains("testuser1"),
        "inject_user_env: after should override explicit HOME. Got HOME={}. stdout: {}",
        home, stdout
    );

    // USER/LOGNAME/SHELL should all match testuser1
    assert!(
        stdout.contains("USER=testuser1"),
        "USER should be testuser1 with inject_user_env: after. stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("LOGNAME=testuser1"),
        "LOGNAME should be testuser1 with inject_user_env: after. stdout: {}",
        stdout
    );
    assert!(
        extract_env_value(stdout, "SHELL").is_some_and(|s| !s.is_empty()),
        "SHELL should be set with inject_user_env: after. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Hook with `inject_user_env: after` should see target user's env
/// even when the hook explicitly sets HOME.
#[tokio::test]
async fn test_override_user_env_hook() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_override_user_env_hook")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "user-env-svc", "running", Duration::from_secs(10))
        .await?;

    // Hook output goes to hook logs — wait for the hook log content
    let logs = harness
        .wait_for_log_content(&config_path, "USER=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;

    // HOME was explicitly set in the hook, but inject_user_env: after forces it
    let home = extract_env_value(stdout, "HOME");
    if let Some(home) = home {
        assert!(
            home.contains("testuser1"),
            "Hook inject_user_env: after should override explicit HOME. Got HOME={}. stdout: {}",
            home, stdout
        );
    }

    // USER should be testuser1
    assert!(
        stdout.contains("USER=testuser1"),
        "Hook USER should be testuser1 with inject_user_env: after. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthcheck with `inject_user_env: after` should see target user's env
/// even when the service explicitly sets USER=custom_user.
/// The healthcheck runs `test "$USER" = testuser1` — if inject_user_env: after
/// correctly forces USER from /etc/passwd, the check passes → healthy.
#[tokio::test]
async fn test_override_user_env_healthcheck() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_override_user_env_healthcheck")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // If inject_user_env: after works, the healthcheck assertion succeeds → healthy
    harness
        .wait_for_service_status(&config_path, "user-env-svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthcheck with default inject_user_env (None → Before) should inject
/// the effective user's env, overriding the service's explicit USER=custom_user.
#[tokio::test]
async fn test_healthcheck_default_inject() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_default_inject")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Default inject_user_env (= before) should inject testuser1's env into
    // the healthcheck, overriding the service's USER=custom_user.
    // The healthcheck asserts USER == testuser1 → healthy if injection works.
    harness
        .wait_for_service_status(&config_path, "user-env-svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthcheck with `inject_user_env: none` should NOT inject user env.
/// The healthcheck should see the service's explicit USER=custom_user.
#[tokio::test]
async fn test_healthcheck_disabled_inject() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_disabled_inject")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // inject_user_env: none means no injection → healthcheck sees USER=custom_user
    // from the service's computed_env. The healthcheck asserts USER == custom_user → healthy.
    harness
        .wait_for_service_status(&config_path, "user-env-svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthcheck with a different user than the service should inject that
/// user's env vars, not the service user's.
#[tokio::test]
async fn test_healthcheck_different_user() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_different_user")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Service runs as testuser1, healthcheck runs as testuser2.
    // Default injection should inject testuser2's env into the healthcheck.
    // The healthcheck asserts USER == testuser2 → healthy.
    harness
        .wait_for_service_status(&config_path, "user-env-svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Hook with `inject_user_env: none` should not get user env injected
/// at the hook level. It still inherits the service's computed_env.
#[tokio::test]
async fn test_hook_disabled_inject() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_hook_disabled_inject")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "user-env-svc", "running", Duration::from_secs(10))
        .await?;

    // Hook inherits service computed_env (which has testuser1's values from
    // service-level injection). inject_user_env: none on the hook prevents
    // hook-level re-injection, but the service values are still inherited.
    let logs = harness
        .wait_for_log_content(&config_path, "USER=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;
    assert!(
        stdout.contains("USER=testuser1"),
        "Hook should inherit service env with testuser1. stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Without `user:`, HOME/USER should come from the CLI caller's environment
/// (transparent inheritance, no injection).
#[tokio::test]
async fn test_no_user_inherits_cli_env() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_no_user_inherits_cli_env")?;

    harness.start_daemon().await?;

    // Start with explicit CLI env to verify the service sees the CLI's values
    let output = harness
        .start_services_with_env(
            &config_path,
            &[("HOME", "/root"), ("USER", "root")],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "no-user-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness
        .wait_for_log_content(&config_path, "USER=", Duration::from_secs(5))
        .await?;

    let stdout = &logs.stdout;

    // Without user:, the service should inherit the CLI caller's env
    assert!(
        stdout.contains("HOME=/root"),
        "Without user:, HOME should come from CLI env (/root). stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("USER=root"),
        "Without user:, USER should come from CLI env (root). stdout: {}",
        stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
