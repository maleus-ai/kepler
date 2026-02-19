//! E2E tests for --no-deps flag and implicit `if:` skip on explicit start.
//!
//! Tests:
//! - `start svc --no-deps` skips dependency waiting
//! - `restart svc --no-deps` uses user-specified order instead of topological sort
//! - `start svc` (explicit service) skips `if: false` condition
//! - `start` (all services) still respects `if: false` condition
//! - `restart svc` ignores `if: false` condition
//! - `start --no-deps` (no service) is rejected by clap
//! - `restart --no-deps` (no services) is rejected at runtime

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "no_deps_test";

/// `start dependent --no-deps` should start the service immediately
/// without waiting for its dependency to be running.
///
/// Setup: "dependent" depends_on "dependency". We only start "dependent"
/// with --no-deps, never starting "dependency". The service should start
/// successfully despite its dependency being absent.
#[tokio::test]
async fn test_start_no_deps_skips_dependency_wait() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_no_deps_skips_dep_wait")?;

    harness.start_daemon().await?;

    // Start only "dependent" with --no-deps — should NOT block on "dependency"
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start", "-d", "--no-deps", "dependent"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();

    // "dependent" should be running even though "dependency" was never started
    harness
        .wait_for_service_status(&config_path, "dependent", "running", Duration::from_secs(10))
        .await?;

    // Verify "dependency" is NOT running (never started)
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        !ps_output.stdout.contains("Up ") || !ps_output.stdout.lines().any(|l| l.contains("dependency") && l.contains("Up ")),
        "dependency should not be running since we never started it. ps output:\n{}",
        ps_output.stdout
    );

    // Verify dependent actually produced output
    harness.wait_for_log_content(&config_path, "DEPENDENT_STARTED", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `start dependent` (without --no-deps) should still wait for dependencies.
/// Since we only request "dependent" and "dependency" is never started,
/// "dependent" should remain in "waiting" state.
#[tokio::test]
async fn test_start_specific_without_no_deps_still_waits() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_no_deps_skips_dep_wait")?;

    harness.start_daemon().await?;

    // Start all services first so they're loaded, then stop them
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for dependency to be running, then stop all
    harness
        .wait_for_service_status(&config_path, "dependency", "running", Duration::from_secs(10))
        .await?;
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for both to be stopped
    harness
        .wait_for_service_status_any(&config_path, "dependency", &["stopped", "exited"], Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status_any(&config_path, "dependent", &["stopped", "exited", "waiting"], Duration::from_secs(10))
        .await?;

    // Now start only "dependent" WITHOUT --no-deps — should block on dependency
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start", "-d", "dependent"],
            Duration::from_secs(5),
        )
        .await?;
    output.assert_success();

    // Give it a moment to settle
    tokio::time::sleep(Duration::from_millis(500)).await;

    // "dependent" should be waiting (blocked on dependency)
    let ps_output = harness.ps(&config_path).await?;
    // It should NOT be "Up" (running) since dependency isn't started
    let dependent_running = ps_output.stdout.lines().any(|l| l.contains("dependent") && !l.contains("dependency") && l.contains("Up "));
    assert!(
        !dependent_running,
        "dependent should not be running without its dependency. ps output:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `restart top mid --no-deps` should restart services in the user-specified
/// order (top, mid) rather than topological order (mid, top).
///
/// We verify this by checking that after restart, both services are running.
/// The key is that the command completes successfully without errors,
/// even though the topological order would be different.
#[tokio::test]
async fn test_restart_no_deps_user_order() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_no_deps_user_order")?;

    harness.start_daemon().await?;

    // Start all services normally
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["base", "mid", "top"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    // Restart "top" and "mid" with --no-deps (user order: top first, then mid)
    // Topological order would be: mid first (dependency), then top (dependent)
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "restart", "-d", "--wait", "--no-deps", "top", "mid"],
            Duration::from_secs(15),
        )
        .await?;
    output.assert_success();

    // Both should be running after restart
    harness
        .wait_for_service_status(&config_path, "top", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "mid", "running", Duration::from_secs(10))
        .await?;

    // "base" should still be running (not restarted)
    harness
        .wait_for_service_status(&config_path, "base", "running", Duration::from_secs(5))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `start never-run` (explicit service name) should skip the `if: false`
/// condition and start the service anyway.
#[tokio::test]
async fn test_start_specific_service_skips_if_condition() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_specific_skips_if_condition")?;

    harness.start_daemon().await?;

    // Start only "never-run" explicitly — should ignore `if: false`
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start", "-d", "never-run"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();

    // "never-run" should be running despite if: false
    harness
        .wait_for_service_status(&config_path, "never-run", "running", Duration::from_secs(10))
        .await?;

    // Verify it actually produced output
    harness.wait_for_log_content(&config_path, "NEVER_RUN_STARTED", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `start` (all services) should still respect `if: false` and skip the
/// disabled service.
#[tokio::test]
async fn test_start_all_respects_if_condition() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_all_respects_if_condition")?;

    harness.start_daemon().await?;

    // Start all services — disabled-svc has `if: false`
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // normal-svc should be running
    harness
        .wait_for_service_status(&config_path, "normal-svc", "running", Duration::from_secs(10))
        .await?;

    // disabled-svc should be skipped
    harness
        .wait_for_service_status(&config_path, "disabled-svc", "skipped", Duration::from_secs(10))
        .await?;

    // Verify normal-svc produced output but disabled-svc did not
    harness.wait_for_log_content(&config_path, "NORMAL_SVC_STARTED", Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(
        !logs.stdout_contains("DISABLED_SVC_STARTED"),
        "disabled-svc should not have started. stdout:\n{}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `restart never-run` should succeed even though the service has `if: false`.
/// Restart inherently ignores `if:` conditions since it operates on already-known
/// services (bypassing the startup condition check).
///
/// Flow: explicitly start "never-run" (which skips `if:`), then restart it.
#[tokio::test]
async fn test_restart_service_ignores_if_condition() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_specific_skips_if_condition")?;

    harness.start_daemon().await?;

    // First, explicitly start "never-run" (skips if: false)
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start", "-d", "never-run"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();

    // Wait for it to be running
    harness
        .wait_for_service_status(&config_path, "never-run", "running", Duration::from_secs(10))
        .await?;

    // Now restart "never-run" — should succeed despite if: false
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "restart", "-d", "--wait", "never-run"],
            Duration::from_secs(15),
        )
        .await?;
    output.assert_success();

    // Should be running again after restart
    harness
        .wait_for_service_status(&config_path, "never-run", "running", Duration::from_secs(10))
        .await?;

    // Verify it produced output again (restart means new process)
    harness.wait_for_log_content(&config_path, "NEVER_RUN_STARTED", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `start --no-deps` (without specifying a service) should fail with a
/// clap validation error because --no-deps requires a service argument.
#[tokio::test]
async fn test_start_no_deps_without_service_fails() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_no_deps_skips_dep_wait")?;

    harness.start_daemon().await?;

    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli(&["-f", config_str, "start", "-d", "--no-deps"])
        .await?;

    // Should fail (non-zero exit code)
    assert!(
        !output.success(),
        "start --no-deps without a service should fail. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );
    assert!(
        output.stderr_contains("--no-deps requires"),
        "Error message should mention --no-deps requires services. stderr: {}",
        output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `restart --no-deps` (without specifying services) should fail with an
/// error because --no-deps requires at least one service name.
#[tokio::test]
async fn test_restart_no_deps_without_services_fails() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_no_deps_skips_dep_wait")?;

    harness.start_daemon().await?;

    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli(&["-f", config_str, "restart", "-d", "--no-deps"])
        .await?;

    // Should fail (non-zero exit code)
    assert!(
        !output.success(),
        "restart --no-deps without services should fail. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );
    assert!(
        output.stderr_contains("--no-deps requires"),
        "Error message should mention --no-deps requires services. stderr: {}",
        output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}
