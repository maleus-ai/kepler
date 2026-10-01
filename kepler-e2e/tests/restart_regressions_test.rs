//! Regressions found during review of manual restart state selection.
use kepler_e2e::{E2eHarness, E2eResult};
use serde_json::Value;
use std::time::Duration;

#[tokio::test]
async fn unrelated_service_must_not_wait_for_deferred_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config = harness.create_test_config(
        r#"
services:
  z_source:
    command: [sleep, '300']
  a_blocker:
    command: [sleep, '300']
    depends_on:
      z_source:
        condition: service_stopped
  a_parent:
    command: [sleep, '300']
"#,
    )?;
    harness.start_daemon().await?;
    let path = config.to_str().unwrap();
    harness
        .run_cli(&["-f", path, "start", "z_source", "a_parent", "-d"])
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "z_source", "running", Duration::from_secs(5))
        .await?;
    harness
        .wait_for_service_status(&config, "a_parent", "running", Duration::from_secs(5))
        .await?;
    let output = harness
        .run_cli(&[
            "-f",
            path,
            "restart",
            "--states",
            "all",
            "--wait",
            "--timeout",
            "2s",
        ])
        .await?;
    assert!(
        !output.success(),
        "deferred dependency should remain pending"
    );
    let output = harness.run_cli(&["-f", path, "ps", "--json"]).await?;
    let states: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(
        states["a_parent"]["status"], "running",
        "independent service is held down: {states}"
    );
    assert_eq!(states["a_blocker"]["status"], "restarting");
    harness
        .stop_service(&config, "z_source")
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "a_blocker", "running", Duration::from_secs(5))
        .await?;
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn post_restart_hook_changes_must_not_trigger_another_restart() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().display().to_string();
    std::fs::write(harness.temp_dir().path().join("watched.txt"), "initial").unwrap();
    let config = harness.create_test_config(&format!(
        r#"
services:
  worker:
    working_dir: '{root}'
    command: [sh, -c, 'echo WORKER_STARTED; sleep 300']
    restart:
      policy: on-failure
      watch: ['watched.txt']
    hooks:
      post_restart:
        run: 'sleep 0.3; echo changed >> {root}/watched.txt; sleep 1'
"#
    ))?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness
        .wait_for_log_content(&config, "WORKER_STARTED", Duration::from_secs(5))
        .await?;
    harness.restart_services(&config).await?.assert_success();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let logs = harness.get_logs(&config, None, 1000).await?;
    assert_eq!(
        logs.stdout.matches("WORKER_STARTED").count(),
        2,
        "restart hook caused additional restart: {}",
        logs.stdout
    );
    harness.stop_daemon().await?;
    Ok(())
}
