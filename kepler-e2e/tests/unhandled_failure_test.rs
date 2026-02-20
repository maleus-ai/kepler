//! E2E tests for unhandled service failure detection.
//!
//! Verifies CLI behavior when services fail without a handler:
//! - `-d --wait` mode stops services + exits 1 (default abort)
//! - `-d --wait --no-abort-on-failure` exits 1, leaves services running
//! - Foreground mode exits 1 at quiescence (default no-abort)
//! - Foreground `--abort-on-failure` stops services + exits 1
//! - Handled failures (service_failed dep) result in exit 0

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

// ============================================================================
// --wait mode tests
// ============================================================================

/// `-d --wait` with a failing service (no handler, restart: no) → exit 1
#[tokio::test]
async fn test_wait_mode_exits_1_on_unhandled_failure() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.create_test_config(
        r#"
services:
  failing:
    command: ["sh", "-c", "exit 42"]
    restart: "no"
"#,
    )?;

    harness.start_daemon().await?;

    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--wait",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 1,
        "Should exit 1 on unhandled failure. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `-d --wait` with a handled failure (service_failed dep) → exit 0
#[tokio::test]
async fn test_wait_mode_exits_0_when_failure_handled() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.create_test_config(
        r#"
services:
  failing:
    command: ["sh", "-c", "exit 1"]
    restart: "no"
  handler:
    command: ["sh", "-c", "echo handled"]
    restart: "no"
    depends_on:
      failing:
        condition: service_failed
"#,
    )?;

    harness.start_daemon().await?;

    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--wait",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 0,
        "Should exit 0 when failure is handled. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `-d --wait` (default abort) with failing + long-running → exit 1, services stopped
#[tokio::test]
async fn test_wait_mode_abort_on_failure_stops_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.create_test_config(
        r#"
services:
  long-running:
    command: ["sh", "-c", "sleep 300"]
  failing:
    command: ["sh", "-c", "exit 42"]
    restart: "no"
"#,
    )?;

    harness.start_daemon().await?;

    // --wait mode aborts on failure by default (no flag needed)
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--wait",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 1,
        "Should exit 1 on unhandled failure (default abort). stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    // Verify long-running service was stopped
    harness
        .wait_for_service_status(
            &config_path,
            "long-running",
            "stopped",
            Duration::from_secs(10),
        )
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `-d --wait --no-abort-on-failure` with failing + long-running → exit 1, long-running still running
#[tokio::test]
async fn test_wait_mode_no_abort_leaves_services_running() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.create_test_config(
        r#"
services:
  long-running:
    command: ["sh", "-c", "sleep 300"]
  failing:
    command: ["sh", "-c", "exit 42"]
    restart: "no"
"#,
    )?;

    harness.start_daemon().await?;

    // --no-abort-on-failure overrides the default abort behavior of --wait
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--wait",
                "--no-abort-on-failure",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 1,
        "Should exit 1 on unhandled failure. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    // With --no-abort-on-failure, long-running service should still be running
    harness
        .wait_for_service_status(
            &config_path,
            "long-running",
            "running",
            Duration::from_secs(10),
        )
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

// ============================================================================
// Foreground mode tests
// ============================================================================

/// Foreground mode (no -d) with failing service → exits 1 at quiescence (default no-abort)
#[tokio::test]
async fn test_foreground_exits_1_on_unhandled_failure() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.create_test_config(
        r#"
services:
  failing:
    command: ["sh", "-c", "exit 42"]
    restart: "no"
"#,
    )?;

    harness.start_daemon().await?;

    // Foreground mode — exits 1 at quiescence (no abort by default)
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 1,
        "Foreground mode should exit 1 on unhandled failure. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Foreground `--abort-on-failure` stops services immediately and exits 1
#[tokio::test]
async fn test_foreground_abort_on_failure_stops_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.create_test_config(
        r#"
services:
  long-running:
    command: ["sh", "-c", "sleep 300"]
  failing:
    command: ["sh", "-c", "exit 42"]
    restart: "no"
"#,
    )?;

    harness.start_daemon().await?;

    // --abort-on-failure overrides the default no-abort behavior of foreground mode
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "--abort-on-failure",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 1,
        "Should exit 1 with --abort-on-failure. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    // Verify long-running service was stopped
    harness
        .wait_for_service_status(
            &config_path,
            "long-running",
            "stopped",
            Duration::from_secs(10),
        )
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Foreground mode with handled failure (service_failed dep) → exits 0
#[tokio::test]
async fn test_foreground_exits_0_when_failure_handled() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path = harness.create_test_config(
        r#"
services:
  failing:
    command: ["sh", "-c", "exit 1"]
    restart: "no"
  handler:
    command: ["sh", "-c", "echo handled"]
    restart: "no"
    depends_on:
      failing:
        condition: service_failed
"#,
    )?;

    harness.start_daemon().await?;

    // Foreground mode with handler — should exit 0 at quiescence
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 0,
        "Foreground mode should exit 0 when failure is handled. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}
