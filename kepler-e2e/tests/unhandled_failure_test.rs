//! E2E tests for unhandled service failure detection.
//!
//! Verifies CLI behavior when services fail without a handler:
//! - `-d --wait` mode exits with code 1
//! - `-d --wait --abort-on-failure` stops services + exits 1
//! - Foreground mode stops services + exits 1 (default abort)
//! - Foreground `--no-abort-on-failure` waits for quiescence then exits 1
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

/// `-d --wait --abort-on-failure` with failing + long-running → exit 1, services stopped
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

    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--wait",
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

/// `-d --wait` (no --abort) with failing + long-running → exit 1, long-running still running
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

    // Without --abort-on-failure, long-running service should still be running
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

/// Foreground mode (no -d) with failing service → exits 1 (default abort behavior)
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

    // Foreground mode — should abort on unhandled failure and exit 1
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

/// Foreground `--no-abort-on-failure` still exits 1 at quiescence
#[tokio::test]
async fn test_foreground_no_abort_exits_1_at_quiescence() -> E2eResult<()> {
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
                "--no-abort-on-failure",
            ],
            Duration::from_secs(15),
        )
        .await?;

    assert_eq!(
        output.exit_code, 1,
        "Should exit 1 at quiescence with --no-abort-on-failure. stdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

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
