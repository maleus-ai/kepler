//! E2E tests for `start`/`run -d --wait <services>` when the config holds services
//! that are not requested.
//!
//! Like `docker compose up -d --wait svc…`, `--wait` must return once the requested
//! services reached their target state, whatever the state of the others. An
//! unrequested service left `Stopped` used to hold the config-wide Ready forever.
//!
//! Tests:
//! - `run -d --wait` on a dependency chain, a dependent service left out
//! - `start -d --wait` on the same chain, on a fresh config and after `stop --clean`
//! - `start`/`run -d --wait` on a failing one-shot still exits non-zero: the scoped
//!   Ready must not overtake its unhandled failure

use kepler_e2e::{E2eHarness, E2eResult};
use std::path::Path;
use std::time::Duration;

const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `pg` healthy → `rec` and `mig` one-shots → `be` healthy → `fe`, never requested.
fn config(dir: &Path) -> String {
    r#"
services:
  pg:
    command: ["sh", "-c", "touch __DIR__/pg; sleep 600"]
    healthcheck:
      command: ["sh", "-c", "test -f __DIR__/pg"]
      interval: 1s
  rec:
    command: ["sh", "-c", "sleep 1"]
    restart: "no"
    depends_on:
      pg: { condition: service_healthy }
  mig:
    command: ["sh", "-c", "sleep 1"]
    restart: "no"
    depends_on:
      pg: { condition: service_healthy }
      rec: { condition: service_completed_successfully }
  be:
    command: ["sleep", "600"]
    depends_on:
      mig: { condition: service_completed_successfully }
    healthcheck:
      command: ["true"]
      interval: 1s
  fe:
    command: ["sleep", "600"]
    depends_on:
      be: { condition: service_healthy }
    healthcheck:
      command: ["true"]
      interval: 1s
"#
    .replace("__DIR__", dir.to_str().unwrap())
}

const REQUESTED: [&str; 4] = ["pg", "rec", "mig", "be"];

async fn launch_wait(harness: &E2eHarness, config_path: &Path, command: &str) -> E2eResult<kepler_e2e::CommandOutput> {
    let mut args = vec!["-f", config_path.to_str().unwrap(), command, "-d", "--wait"];
    args.extend_from_slice(&REQUESTED);
    harness.run_cli_with_timeout(&args, WAIT_TIMEOUT).await
}

fn assert_returned(output: &kepler_e2e::CommandOutput, what: &str) {
    assert_eq!(
        output.exit_code, 0,
        "{} should return once the requested services are ready. stdout: {}\nstderr: {}",
        what, output.stdout, output.stderr
    );
}

/// `run -d --wait pg rec mig be` returns once `be` is healthy, `fe` left out.
#[tokio::test]
async fn test_run_wait_subset_returns_with_unrequested_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = harness.create_temp_dir("svc")?;
    let config_path = harness.create_test_config(&config(&dir))?;
    harness.start_daemon().await?;

    let output = launch_wait(&harness, &config_path, "run").await?;
    assert_returned(&output, "`run -d --wait pg rec mig be`");
    harness.wait_for_service_status(&config_path, "be", "healthy", WAIT_TIMEOUT).await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// `start -d --wait pg rec mig be` returns once `be` is healthy, on a fresh config
/// and again after `stop --clean`.
#[tokio::test]
async fn test_start_wait_subset_returns_with_unrequested_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = harness.create_temp_dir("svc")?;
    let config_path = harness.create_test_config(&config(&dir))?;
    harness.start_daemon().await?;

    let output = launch_wait(&harness, &config_path, "start").await?;
    assert_returned(&output, "`start -d --wait pg rec mig be`");
    harness.wait_for_service_status(&config_path, "be", "healthy", WAIT_TIMEOUT).await?;

    let stop = harness.stop_services_clean(&config_path).await?;
    assert_eq!(stop.exit_code, 0, "stop --clean failed. stderr: {}", stop.stderr);

    let output = launch_wait(&harness, &config_path, "start").await?;
    assert_returned(&output, "`start -d --wait pg rec mig be` after `stop --clean`");

    harness.stop_daemon().await?;
    Ok(())
}

/// `job` fails right away; `other` is never requested.
const FAILING_CONFIG: &str = r#"
services:
  job:
    command: ["sh", "-c", "exit 3"]
    restart: "no"
  other:
    command: ["sleep", "600"]
"#;

/// `start`/`run -d --wait job` exits non-zero although `job`, being terminal,
/// counts as ready, and `other` is left out.
#[tokio::test]
async fn test_wait_subset_reports_failure_of_requested_service() -> E2eResult<()> {
    for command in ["start", "run"] {
        let mut harness = E2eHarness::new().await?;
        let config_path = harness.create_test_config(FAILING_CONFIG)?;
        harness.start_daemon().await?;

        let output = harness
            .run_cli_with_timeout(&["-f", config_path.to_str().unwrap(), command, "-d", "--wait", "job"], WAIT_TIMEOUT)
            .await?;
        assert_ne!(
            output.exit_code, 0,
            "`{} -d --wait job` should exit non-zero when job fails. stdout: {}\nstderr: {}",
            command, output.stdout, output.stderr
        );
        assert!(
            output.stderr_contains("job"),
            "`{} -d --wait job` should name the failed service. stderr: {}",
            command, output.stderr
        );

        harness.stop_daemon().await?;
    }
    Ok(())
}
