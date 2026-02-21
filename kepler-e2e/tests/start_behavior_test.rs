//! E2E tests for `kepler start` behavior with services in various states.
//!
//! These tests reproduce the original bug where `kepler start` on an exited
//! service would show the old log line instead of re-running the service.
//! Each test verifies the service **actually ran again** by counting log lines.
//!
//! Tests:
//! - `start` re-runs an exited one-shot service (exit 0) — the original bug
//! - `start` re-runs an exited one-shot service with `restart: no` — same bug, explicit policy
//! - `start` re-runs an exited service (exit non-zero)
//! - `start` re-runs a killed service (after stop)
//! - `start` all services re-evaluates Skipped services (`if: false` stays skipped)
//! - `start <service>` on a Skipped service bypasses `if:` condition
//! - `start --no-deps` on a specific service skips dependency waiting
//! - repeated `start` always re-runs exited service (regression)

use kepler_e2e::{E2eHarness, E2eResult};
use std::path::Path;
use std::time::Duration;

const TEST_MODULE: &str = "start_behavior_test";

/// Count occurrences of a marker string in log output.
fn count_marker(logs: &str, marker: &str) -> usize {
    logs.lines().filter(|l| l.contains(marker)).count()
}

/// Wait until the log output contains at least `expected` occurrences of `marker`.
async fn wait_for_marker_count(
    harness: &E2eHarness,
    config_path: &Path,
    marker: &str,
    expected: usize,
    timeout_duration: Duration,
) -> E2eResult<usize> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout_duration {
        let logs = harness.get_logs(config_path, None, 1000).await?;
        let count = count_marker(&logs.stdout, marker);
        if count >= expected {
            return Ok(count);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let logs = harness.get_logs(config_path, None, 1000).await?;
    let count = count_marker(&logs.stdout, marker);
    Ok(count)
}

/// `kepler start` should restart a service that exited with code 0.
/// This is the original bug: one-shot services with exit code 0 would not
/// restart on manual `kepler start` because the restart policy check prevented it.
#[tokio::test]
async fn test_start_restarts_exited_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_restarts_exited_service")?;

    harness.start_daemon().await?;

    // First start — service runs and exits with code 0
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "oneshot", "exited", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "ONESHOT_RAN", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs(&config_path, None, 1000).await?;
    let count_after_first = count_marker(&logs.stdout, "ONESHOT_RAN");
    assert_eq!(count_after_first, 1, "Should have 1 log line after first start");

    // Second start — should restart the exited service and produce a NEW log line
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "oneshot", "exited", Duration::from_secs(10))
        .await?;

    // Wait for the second log line to appear
    let count_after_second = wait_for_marker_count(
        &harness, &config_path, "ONESHOT_RAN", 2, Duration::from_secs(5),
    ).await?;
    assert_eq!(
        count_after_second, 2,
        "Service should have run twice (got {} log lines). The original bug would show 1.",
        count_after_second
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Same as above but with explicit `restart: no` — the restart policy should
/// only affect automatic restarts, not user-initiated `kepler start`.
#[tokio::test]
async fn test_start_exited_with_restart_no() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_exited_with_restart_no")?;

    harness.start_daemon().await?;

    // First start
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "oneshot-no-restart", "exited", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "ONESHOT_NO_RESTART_RAN", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs(&config_path, None, 1000).await?;
    let count_after_first = count_marker(&logs.stdout, "ONESHOT_NO_RESTART_RAN");
    assert_eq!(count_after_first, 1);

    // Second start — should restart despite restart: no
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "oneshot-no-restart", "exited", Duration::from_secs(10))
        .await?;

    let count_after_second = wait_for_marker_count(
        &harness, &config_path, "ONESHOT_NO_RESTART_RAN", 2, Duration::from_secs(5),
    ).await?;
    assert_eq!(
        count_after_second, 2,
        "restart:no should not prevent manual start. Got {} runs instead of 2.",
        count_after_second
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start` should restart a service that exited with a non-zero code.
/// Note: non-zero exit codes result in "Exited" status (not "Failed").
/// "Failed" is only used for spawn failures.
#[tokio::test]
async fn test_start_restarts_nonzero_exit_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_restarts_failed_service")?;

    harness.start_daemon().await?;

    // First start — exits with code 1
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "failing", "exited", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "FAILING_RAN", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs(&config_path, None, 1000).await?;
    let count_after_first = count_marker(&logs.stdout, "FAILING_RAN");
    assert_eq!(count_after_first, 1);

    // Second start — should restart
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "failing", "exited", Duration::from_secs(10))
        .await?;

    let count_after_second = wait_for_marker_count(
        &harness, &config_path, "FAILING_RAN", 2, Duration::from_secs(5),
    ).await?;
    assert_eq!(
        count_after_second, 2,
        "Exited service (non-zero) should restart on manual start. Got {} runs.",
        count_after_second
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start` should restart a service that was killed (Stopped/Killed state).
/// Note: the default log retention on stop is Clear, so logs from the first run
/// are gone after `kepler stop`. We verify the service ran again by checking
/// that new log output appears after the second start.
#[tokio::test]
async fn test_start_restarts_killed_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_restarts_killed_service")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "killable", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "KILLABLE_STARTED", Duration::from_secs(5))
        .await?;

    // Stop the service (default retention clears logs on stop)
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status_any(
            &config_path,
            "killable",
            &["stopped", "killed"],
            Duration::from_secs(10),
        )
        .await?;

    // Start again — should restart and produce NEW log output
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    assert!(
        !output.stdout_contains("All services already running"),
        "Killed/stopped service should restart on manual start. stdout: {}",
        output.stdout
    );

    harness
        .wait_for_service_status(&config_path, "killable", "running", Duration::from_secs(10))
        .await?;

    // Verify the service produced output after the second start
    // (logs from the first run were cleared by stop's default retention)
    let count = wait_for_marker_count(
        &harness, &config_path, "KILLABLE_STARTED", 1, Duration::from_secs(5),
    ).await?;
    assert_eq!(
        count, 1,
        "Service should have produced output after restart. Got {} log lines.",
        count
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start` (all services) should re-evaluate Skipped services.
/// If the `if:` condition is still false, the service should remain Skipped.
#[tokio::test]
async fn test_start_reevaluates_skipped_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_reevaluates_skipped_service")?;

    harness.start_daemon().await?;

    // Start all — conditional has if: false, so it should be Skipped
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "always-run", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "conditional", "skipped", Duration::from_secs(10))
        .await?;

    // Start again — should re-evaluate but still skip (if: false is static)
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // The conditional service should still be skipped (if: false is constant)
    harness
        .wait_for_service_status(&config_path, "conditional", "skipped", Duration::from_secs(10))
        .await?;

    // Verify the conditional service never produced any output
    let logs = harness.get_logs(&config_path, None, 1000).await?;
    assert!(
        !logs.stdout_contains("CONDITIONAL_STARTED"),
        "Skipped service should not have produced output. logs: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start <service>` on a Skipped service should bypass the `if:` condition
/// and start the service regardless.
#[tokio::test]
async fn test_start_specific_skipped_bypasses_if() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_specific_skipped_bypasses_if")?;

    harness.start_daemon().await?;

    // Start all — disabled has if: false, should be Skipped
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "normal", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "disabled", "skipped", Duration::from_secs(10))
        .await?;

    // Explicitly start the disabled service — should bypass if: false
    let output = harness.start_service(&config_path, "disabled").await?;
    output.assert_success();

    // disabled should now be running
    harness
        .wait_for_service_status(&config_path, "disabled", "running", Duration::from_secs(10))
        .await?;

    // Verify it produced output
    harness
        .wait_for_log_content(&config_path, "DISABLED_STARTED", Duration::from_secs(5))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start` (all) with a dependency chain: stop, then start again.
/// Both services must actually restart. Since the default log retention on stop
/// clears logs, we verify by checking that new output appears after the second start.
#[tokio::test]
async fn test_start_all_follows_dependency_chain() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_all_no_deps")?;

    harness.start_daemon().await?;

    // Start all
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "base", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "dependent", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "BASE_STARTED", Duration::from_secs(5))
        .await?;
    harness
        .wait_for_log_content(&config_path, "DEPENDENT_STARTED", Duration::from_secs(5))
        .await?;

    // Stop all (default retention clears logs)
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    for svc in &["base", "dependent"] {
        harness
            .wait_for_service_status_any(
                &config_path,
                svc,
                &["stopped", "killed", "exited"],
                Duration::from_secs(10),
            )
            .await?;
    }

    // Restart all — should restart in dependency order
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    assert!(
        !output.stdout_contains("All services already running"),
        "Stopped services should restart. stdout: {}",
        output.stdout
    );

    harness
        .wait_for_service_status(&config_path, "base", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "dependent", "running", Duration::from_secs(10))
        .await?;

    // Verify both services produced new output after the second start
    // (logs from first run were cleared by stop's default retention)
    let base_count = wait_for_marker_count(
        &harness, &config_path, "BASE_STARTED", 1, Duration::from_secs(5),
    ).await?;
    let dep_count = wait_for_marker_count(
        &harness, &config_path, "DEPENDENT_STARTED", 1, Duration::from_secs(5),
    ).await?;
    assert_eq!(base_count, 1, "base should have produced output after restart");
    assert_eq!(dep_count, 1, "dependent should have produced output after restart");

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start <dependent> --no-deps` should start the dependent service
/// without waiting for its base dependency.
#[tokio::test]
async fn test_start_specific_no_deps_skips_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_all_no_deps")?;

    harness.start_daemon().await?;

    // Start only "dependent" with --no-deps
    let config_str = config_path.to_str().unwrap();
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start", "-d", "--no-deps", "dependent"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();

    // dependent should start without base
    harness
        .wait_for_service_status(&config_path, "dependent", "running", Duration::from_secs(10))
        .await?;

    // base should NOT be running (we didn't start it)
    let ps_output = harness.ps(&config_path).await?;
    let base_running = ps_output
        .stdout
        .lines()
        .any(|l| l.contains("base") && l.contains("Up "));
    assert!(
        !base_running,
        "base should not be running. ps output:\n{}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Calling `kepler start` multiple times should always restart exited services.
/// This is the core regression test — verifies service runs N times, not once.
#[tokio::test]
async fn test_start_multiple_times_always_restarts_exited() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_restarts_exited_service")?;

    harness.start_daemon().await?;

    for i in 1..=3 {
        let output = harness.start_services(&config_path).await?;
        output.assert_success();

        // Wait for service to exit
        harness
            .wait_for_service_status(&config_path, "oneshot", "exited", Duration::from_secs(10))
            .await?;

        // Wait for the i-th log line to appear
        let run_count = wait_for_marker_count(
            &harness, &config_path, "ONESHOT_RAN", i, Duration::from_secs(5),
        ).await?;
        assert_eq!(
            run_count, i,
            "After {} starts, service should have run {} times but ran {} times. \
             The original bug would keep showing 1.",
            i, i, run_count
        );
    }

    harness.stop_daemon().await?;
    Ok(())
}

/// `kepler start svc` on an exited service should also restart it.
/// Tests the specific-service path (skip_condition=true).
#[tokio::test]
async fn test_start_specific_restarts_exited_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_restarts_exited_service")?;

    harness.start_daemon().await?;

    // First start (all)
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "oneshot", "exited", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "ONESHOT_RAN", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs(&config_path, None, 1000).await?;
    assert_eq!(count_marker(&logs.stdout, "ONESHOT_RAN"), 1);

    // Second start by name — should also restart
    let output = harness.start_service(&config_path, "oneshot").await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "oneshot", "exited", Duration::from_secs(10))
        .await?;

    let run_count = wait_for_marker_count(
        &harness, &config_path, "ONESHOT_RAN", 2, Duration::from_secs(5),
    ).await?;
    assert_eq!(
        run_count, 2,
        "`start svc` on exited service should restart it. Got {} runs.",
        run_count
    );

    harness.stop_daemon().await?;
    Ok(())
}

// ============================================================================
// Foreground (attached) start — verifies CLI shows current run's output
// ============================================================================

/// `kepler start` (foreground/attached mode) on a oneshot service must display
/// the current run's log output. Reproduces the bug where the second foreground
/// start showed only historical logs and missed the new run's output because
/// the CLI received a premature Quiescent signal (from the subscription recheck)
/// before the Start request was processed by the daemon.
#[tokio::test]
async fn test_foreground_start_shows_current_run_output() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_foreground_start_shows_output")?;
    let config_str = config_path.to_str().unwrap();

    harness.start_daemon().await?;

    // First foreground start — oneshot runs and exits, CLI should show the marker
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();
    let count_1 = count_marker(&output.stdout, "FOREGROUND_MARKER");
    assert_eq!(
        count_1, 1,
        "First foreground start should show 1 log line. stdout:\n{}",
        output.stdout
    );

    // Wait for service to fully settle
    harness
        .wait_for_service_status(&config_path, "oneshot", "exited", Duration::from_secs(5))
        .await?;

    // Second foreground start — must show the NEW run's output (2 lines total:
    // 1 from first run + 1 from this run). The bug showed only 1 line (old).
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();
    let count_2 = count_marker(&output.stdout, "FOREGROUND_MARKER");
    assert_eq!(
        count_2, 2,
        "Second foreground start should show 2 log lines (old + new). \
         Got {} — if 1, the current run's output was missed. stdout:\n{}",
        count_2, output.stdout
    );

    // Third foreground start — same pattern, 3 lines total
    let output = harness
        .run_cli_with_timeout(
            &["-f", config_str, "start"],
            Duration::from_secs(10),
        )
        .await?;
    output.assert_success();
    let count_3 = count_marker(&output.stdout, "FOREGROUND_MARKER");
    assert_eq!(
        count_3, 3,
        "Third foreground start should show 3 log lines. Got {}. stdout:\n{}",
        count_3, output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
