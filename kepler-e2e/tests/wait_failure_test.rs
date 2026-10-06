//! E2E tests for the exit code of `start`/`run -d --wait` when a service fails to start.
//!
//! A service whose `pre_start` hook fails ends in `Failed`. The restart policy only
//! applies to process exits, so nothing restarts it: `--wait` must exit non-zero and
//! name the service and the failed hook, whatever the restart policy.
//!
//! Tests:
//! - `start`/`run -d --wait`, `pre_start` failing, without restart policy and with `on-failure`
//! - `start -d --wait <service>` on a service failed by a file-watch restart
//! - a dependent of the failed service does not hold `--wait` until the timeout
//! - non-regression: healthy services exit 0, `start` on healthy services is a no-op exiting 0

use kepler_e2e::{E2eHarness, E2eResult};
use std::path::{Path, PathBuf};
use std::time::Duration;

const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `a` is healthy; `b` depends on it and runs a `pre_start` hook that fails while
/// the `fail` marker exists in `dir`.
fn config(dir: &Path, restart: &str) -> String {
    r#"
services:
  a:
    command: ["sleep", "1000"]
    healthcheck: { command: ["true"], interval: 1s, timeout: 1s, retries: 2 }
  b:
    command: ["sleep", "1000"]
    working_dir: __DIR__
    restart: __RESTART__
    hooks:
      pre_start:
        - run: test ! -f fail
    healthcheck: { command: ["true"], interval: 1s, timeout: 1s, retries: 2 }
    depends_on:
      a: { condition: service_healthy }
"#
    .replace("__DIR__", dir.to_str().unwrap())
    .replace("__RESTART__", restart)
}

const RESTART_NO: &str = r#""no""#;
const RESTART_ON_FAILURE: &str = r#"{ policy: "on-failure|on-unhealthy" }"#;

async fn launch_wait(harness: &E2eHarness, config_path: &Path, args: &[&str]) -> E2eResult<kepler_e2e::CommandOutput> {
    let mut full = vec!["-f", config_path.to_str().unwrap()];
    full.extend_from_slice(args);
    harness.run_cli_with_timeout(&full, WAIT_TIMEOUT).await
}

fn assert_pre_start_failure(output: &kepler_e2e::CommandOutput, what: &str) {
    assert_ne!(
        output.exit_code, 0,
        "{} should exit non-zero when b's pre_start fails. stdout: {}\nstderr: {}",
        what, output.stdout, output.stderr
    );
    assert!(
        output.stderr_contains("Error: ") && output.stderr_contains("b pre_start hook failed"),
        "{} should report an error naming b and its failed hook. stderr: {}",
        what, output.stderr
    );
}

fn setup(harness: &E2eHarness, restart: &str) -> E2eResult<(PathBuf, PathBuf)> {
    let dir = harness.create_temp_dir("svc")?;
    let config_path = harness.create_test_config(&config(&dir, restart))?;
    Ok((dir, config_path))
}

async fn assert_launch_wait_fails_on_pre_start(restart: &str) -> E2eResult<()> {
    for command in ["start", "run"] {
        let mut harness = E2eHarness::new().await?;
        let (dir, config_path) = setup(&harness, restart)?;
        std::fs::write(dir.join("fail"), "")?;

        harness.start_daemon().await?;

        let output = launch_wait(&harness, &config_path, &[command, "-d", "--wait"]).await?;
        assert_pre_start_failure(&output, &format!("`{} -d --wait` (restart: {})", command, restart));

        harness.stop_daemon().await?;
    }
    Ok(())
}

/// `start -d --wait` and `run -d --wait` on the whole config, `b`'s pre_start failing.
#[tokio::test]
async fn test_launch_wait_pre_start_failure_without_restart_policy() -> E2eResult<()> {
    assert_launch_wait_fails_on_pre_start(RESTART_NO).await
}

/// Same with `on-failure`: no process exited, so the policy restarts nothing.
#[tokio::test]
async fn test_launch_wait_pre_start_failure_with_on_failure_policy() -> E2eResult<()> {
    assert_launch_wait_fails_on_pre_start(RESTART_ON_FAILURE).await
}

/// `start -d --wait b` after a file-watch restart left `b` failed on its pre_start.
#[tokio::test]
async fn test_start_wait_service_failed_by_watch_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = harness.create_temp_dir("svc")?;
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(dir.join("src/x.ts"), "1")?;
    let content = config(&dir, r#"{ policy: "on-failure|on-unhealthy", watch: ["src/**/*.ts"] }"#);
    let config_path = harness.create_test_config(&content)?;

    harness.start_daemon().await?;

    launch_wait(&harness, &config_path, &["run", "-d", "--wait"]).await?.assert_success();

    std::fs::write(dir.join("fail"), "")?;
    std::fs::write(dir.join("src/x.ts"), "2")?;
    harness.wait_for_service_status(&config_path, "b", "failed", Duration::from_secs(15)).await?;

    let output = launch_wait(&harness, &config_path, &["start", "-d", "--wait", "--timeout", "10s", "b"]).await?;
    assert_pre_start_failure(&output, "`start -d --wait b`");

    // Once the cause is fixed, the same command brings b back
    std::fs::remove_file(dir.join("fail"))?;
    let output = launch_wait(&harness, &config_path, &["start", "-d", "--wait", "--timeout", "10s", "b"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "b", "healthy", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// A dependent of the failed service waits on a dependency that will never restart:
/// `--wait` must report the failure instead of waiting for the timeout.
#[tokio::test]
async fn test_launch_wait_with_dependent_of_failed_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = harness.create_temp_dir("svc")?;
    let mut content = config(&dir, r#"{ policy: "on-failure" }"#);
    content.push_str(
        r#"  c:
    command: ["sleep", "1000"]
    depends_on:
      b: { condition: service_healthy }
"#,
    );
    let config_path = harness.create_test_config(&content)?;
    std::fs::write(dir.join("fail"), "")?;

    harness.start_daemon().await?;

    let output = launch_wait(
        &harness,
        &config_path,
        &["start", "-d", "--wait", "--no-abort-on-failure", "--timeout", "20s"],
    ).await?;
    assert_pre_start_failure(&output, "`start -d --wait --no-abort-on-failure`");
    assert!(
        !output.stderr_contains("Timeout"),
        "--wait should end on b's failure, not on the timeout. stderr: {}",
        output.stderr
    );
    harness.wait_for_service_status(&config_path, "a", "healthy", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthy services exit 0, and `start -d --wait` on services already healthy stays a no-op.
#[tokio::test]
async fn test_launch_wait_healthy_services_exit_0() -> E2eResult<()> {
    for restart in [RESTART_NO, RESTART_ON_FAILURE] {
        let mut harness = E2eHarness::new().await?;
        let (_dir, config_path) = setup(&harness, restart)?;

        harness.start_daemon().await?;

        launch_wait(&harness, &config_path, &["run", "-d", "--wait"]).await?.assert_success();
        harness.wait_for_service_status(&config_path, "b", "healthy", Duration::from_secs(5)).await?;

        launch_wait(&harness, &config_path, &["start", "-d", "--wait"]).await?.assert_success();
        launch_wait(&harness, &config_path, &["start", "-d", "--wait", "b"]).await?.assert_success();
        harness.wait_for_service_status(&config_path, "b", "healthy", Duration::from_secs(5)).await?;

        harness.stop_daemon().await?;
    }
    Ok(())
}
