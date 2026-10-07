//! Manual restart state selection and dependency checks through the real CLI.
use kepler_e2e::{E2eHarness, E2eResult};
use serde_json::Value;
use std::{path::Path, time::Duration};

async fn status(harness: &E2eHarness, config: &Path) -> E2eResult<Value> {
    let output = harness
        .run_cli(&["-f", config.to_str().unwrap(), "ps", "--json"])
        .await?;
    output.assert_success();
    Ok(serde_json::from_str(&output.stdout).unwrap())
}

async fn restart(harness: &E2eHarness, config: &Path, args: &[&str]) -> E2eResult<()> {
    let mut command = vec![
        "-f",
        config.to_str().unwrap(),
        "restart",
        "--wait",
        "--timeout",
        "10s",
    ];
    command.extend_from_slice(args);
    harness.run_cli(&command).await?.assert_success();
    Ok(())
}

#[tokio::test]
async fn states_add_to_running_and_all_recovers_terminal_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let recovery_marker = harness.temp_dir().path().join("recovered");
    let config = harness.create_test_config(&format!(
        r#"
services:
  active:
    command: ["sleep", "300"]
  manual:
    command: ["sleep", "300"]
  completed:
    command: ["sh", "-c", "echo COMPLETED_RUN; exit 0"]
  signaled:
    command: ["sh", "-c", "echo SIGNALED_RUN; kill -KILL $$"]
  broken:
    command: ["sleep", "300"]
    hooks:
      pre_start:
        run: "test -f {}"
"#,
        recovery_marker.display()
    ))?;
    harness.start_daemon().await?;
    // Establish a successful startup before testing recovery of a later failure.
    std::fs::write(&recovery_marker, "ready").unwrap();
    harness.start_services(&config).await?.assert_success();
    harness
        .wait_for_service_status(&config, "broken", "running", Duration::from_secs(10))
        .await?;
    harness
        .stop_service(&config, "broken")
        .await?
        .assert_success();
    std::fs::remove_file(&recovery_marker).unwrap();
    assert!(!harness.start_service(&config, "broken").await?.success());
    for (name, expected) in [
        ("active", "running"),
        ("manual", "running"),
        ("completed", "exited"),
        ("signaled", "killed"),
        ("broken", "failed"),
    ] {
        harness
            .wait_for_service_status(&config, name, expected, Duration::from_secs(10))
            .await?;
    }
    harness
        .wait_for_log_content(&config, "COMPLETED_RUN", Duration::from_secs(5))
        .await?;
    harness
        .wait_for_log_content(&config, "SIGNALED_RUN", Duration::from_secs(5))
        .await?;
    harness
        .stop_service(&config, "manual")
        .await?
        .assert_success();
    let before = status(&harness, &config).await?;

    // The default excludes every terminal state.
    restart(&harness, &config, &[]).await?;
    let default = status(&harness, &config).await?;
    assert_ne!(before["active"]["pid"], default["active"]["pid"]);
    for name in ["manual", "completed", "signaled", "broken"] {
        assert_eq!(before[name]["status"], default[name]["status"]);
    }
    let logs = harness.get_logs(&config, None, 1000).await?;
    assert_eq!(logs.stdout.matches("COMPLETED_RUN").count(), 1);
    assert_eq!(logs.stdout.matches("SIGNALED_RUN").count(), 1);

    // A specific state adds to running services without selecting other states.
    restart(&harness, &config, &["--states", "exited"]).await?;
    let selected = status(&harness, &config).await?;
    assert_ne!(default["active"]["pid"], selected["active"]["pid"]);
    assert_eq!(selected["manual"]["status"], "stopped");
    assert_eq!(selected["broken"]["status"], "failed");
    assert_eq!(selected["signaled"]["status"], "killed");

    // Repair the startup failure so all can bring it up.
    std::fs::write(&recovery_marker, "ready").unwrap();
    restart(&harness, &config, &["--states", "all"]).await?;
    for name in ["active", "manual", "broken"] {
        harness
            .wait_for_service_status(&config, name, "running", Duration::from_secs(10))
            .await?;
    }
    harness
        .wait_for_service_status(&config, "signaled", "killed", Duration::from_secs(10))
        .await?;
    let logs = harness.get_logs(&config, None, 1000).await?;
    assert_eq!(logs.stdout.matches("COMPLETED_RUN").count(), 3);
    assert_eq!(logs.stdout.matches("SIGNALED_RUN").count(), 2);
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn named_restart_checks_dependencies_without_starting_them() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config = harness.create_test_config(
        r#"
services:
  database:
    command: ["sleep", "300"]
  backend:
    command: ["sleep", "300"]
    depends_on:
      database:
        condition: service_started
"#,
    )?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness.stop_services(&config).await?.assert_success();
    restart(&harness, &config, &["backend", "--states", "all"]).await?;
    let blocked = status(&harness, &config).await?;
    assert_eq!(blocked["database"]["status"], "stopped");
    assert_eq!(blocked["backend"]["status"], "skipped");
    assert!(
        blocked["backend"]["skip_reason"]
            .as_str()
            .unwrap()
            .contains("database")
    );

    // Start resets the skipped state; no-deps explicitly permits bypassing checks.
    harness
        .run_cli(&[
            "-f",
            config.to_str().unwrap(),
            "start",
            "backend",
            "-d",
            "--no-deps",
        ])
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "backend", "running", Duration::from_secs(10))
        .await?;
    harness
        .stop_service(&config, "backend")
        .await?
        .assert_success();
    restart(
        &harness,
        &config,
        &["backend", "--states", "stopped", "--no-deps"],
    )
    .await?;
    let bypassed = status(&harness, &config).await?;
    assert_eq!(bypassed["backend"]["status"], "running");
    assert_eq!(bypassed["database"]["status"], "stopped");
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn full_restart_waits_for_new_health_and_job_completion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().display().to_string();
    let config = harness.create_test_config(&format!(
        r#"
services:
  database:
    command: ["sh", "-c", "sleep 1; touch {root}/healthy; sleep 300"]
    hooks:
      pre_start:
        run: "rm -f {root}/healthy"
    healthcheck:
      command: ["test", "-f", "{root}/healthy"]
      interval: 100ms
      retries: 1
  job:
    command: ["sh", "-c", "echo JOB_RUN; sleep 1; touch {root}/completed; exit 0"]
    hooks:
      pre_start:
        run: "rm -f {root}/completed"
  backend:
    command: ["sh", "-c", "test -f {root}/healthy && test -f {root}/completed && exec sleep 300"]
    depends_on:
      database:
        condition: service_healthy
        timeout: 5s
      job:
        condition: service_completed_successfully
        timeout: 5s
"#
    ))?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness
        .wait_for_service_status(&config, "backend", "running", Duration::from_secs(10))
        .await?;
    let before = status(&harness, &config).await?;
    // Both the running dependency and the exited job are selected for a new cycle.
    restart(&harness, &config, &["--states", "all"]).await?;
    let after = status(&harness, &config).await?;
    assert_eq!(after["database"]["status"], "healthy");
    assert_eq!(after["job"]["status"], "exited");
    assert_eq!(after["job"]["exit_code"], 0);
    assert_eq!(after["backend"]["status"], "running");
    assert_ne!(before["backend"]["pid"], after["backend"]["pid"]);
    let logs = harness.get_logs(&config, None, 1000).await?;
    assert_eq!(logs.stdout.matches("JOB_RUN").count(), 2);
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn full_restart_honors_conditions_and_excludes_skipped_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config = harness.create_test_config(
        r#"
services:
  conditional:
    if: ${{ kepler.flags.ENABLED == 'true' }}$
    command: ["sleep", "300"]
  excluded:
    if: false
    command: ["sleep", "300"]
"#,
    )?;
    harness.start_daemon().await?;
    harness
        .run_cli(&[
            "-f",
            config.to_str().unwrap(),
            "start",
            "-d",
            "--wait",
            "-D",
            "ENABLED=true",
        ])
        .await?
        .assert_success();
    harness
        .stop_service(&config, "conditional")
        .await?
        .assert_success();
    restart(
        &harness,
        &config,
        &["--states", "all", "-D", "ENABLED=false"],
    )
    .await?;
    let after = status(&harness, &config).await?;
    assert_eq!(after["conditional"]["status"], "skipped");
    assert_eq!(after["excluded"]["status"], "skipped");
    // all never selects skipped, even if its condition now becomes true.
    restart(
        &harness,
        &config,
        &["--states", "all", "-D", "ENABLED=true"],
    )
    .await?;
    assert_eq!(
        status(&harness, &config).await?["conditional"]["status"],
        "skipped"
    );
    // Explicit naming bypasses if, as start does, when the service is eligible.
    harness
        .run_cli(&["-f", config.to_str().unwrap(), "start", "conditional", "-d"])
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "conditional", "running", Duration::from_secs(10))
        .await?;
    harness
        .stop_service(&config, "conditional")
        .await?
        .assert_success();
    restart(
        &harness,
        &config,
        &["conditional", "--states", "stopped", "-D", "ENABLED=false"],
    )
    .await?;
    assert_eq!(
        status(&harness, &config).await?["conditional"]["status"],
        "running"
    );
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn dependency_timeout_leaves_failed_instead_of_restarting() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config = harness.create_test_config(
        r#"
services:
  database:
    command: ["sleep", "300"]
    healthcheck:
      command: ["false"]
      interval: 100ms
      retries: 1
  backend:
    command: ["sleep", "300"]
    depends_on:
      database:
        condition: service_healthy
        timeout: 300ms
"#,
    )?;
    harness.start_daemon().await?;
    harness
        .run_cli(&[
            "-f",
            config.to_str().unwrap(),
            "start",
            "database",
            "backend",
            "-d",
            "--no-deps",
        ])
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "backend", "running", Duration::from_secs(10))
        .await?;
    let output = harness
        .run_cli(&["-f", config.to_str().unwrap(), "restart", "--states", "all"])
        .await?;
    assert!(
        !output.success(),
        "restart must report the dependency timeout"
    );
    assert_eq!(
        status(&harness, &config).await?["backend"]["status"],
        "failed"
    );
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn states_accepts_lists_and_rejects_unknown_values() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config =
        harness.create_test_config("services:\n  worker:\n    command: [sleep, '300']\n")?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness.stop_services(&config).await?.assert_success();
    restart(
        &harness,
        &config,
        &["--states", "exited,stopped", "--states", "failed,killed"],
    )
    .await?;
    assert_eq!(
        status(&harness, &config).await?["worker"]["status"],
        "running"
    );
    let help = harness.run_cli(&["restart", "--help"]).await?;
    help.assert_success();
    assert!(
        help.stdout
            .contains("[possible values: stopped, exited, failed, killed, all]")
    );
    let invalid = harness
        .run_cli(&[
            "-f",
            config.to_str().unwrap(),
            "restart",
            "--states",
            "skipped",
        ])
        .await?;
    assert!(!invalid.success());
    assert!(
        invalid
            .stderr
            .contains("possible values: stopped, exited, failed, killed, all")
    );
    let unknown = harness
        .run_cli(&[
            "-f",
            config.to_str().unwrap(),
            "restart",
            "missing",
            "--states",
            "all",
        ])
        .await?;
    assert!(!unknown.success());
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn failed_new_job_execution_cannot_reuse_previous_success() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let failure_marker = harness.temp_dir().path().join("fail-job");
    let config = harness.create_test_config(&format!(
        r#"
services:
  job:
    command: ["sh", "-c", "sleep 0.2; test ! -f {}"]
  backend:
    command: ["sleep", "300"]
    depends_on:
      job:
        condition: service_completed_successfully
        timeout: 5s
"#,
        failure_marker.display()
    ))?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness
        .wait_for_service_status(&config, "backend", "running", Duration::from_secs(10))
        .await?;
    std::fs::write(&failure_marker, "fail").unwrap();
    restart(&harness, &config, &["--states", "all"]).await?;
    let after = status(&harness, &config).await?;
    assert_eq!(after["job"]["status"], "exited");
    assert_eq!(after["job"]["exit_code"], 1);
    assert_eq!(after["backend"]["status"], "skipped");
    assert!(after["backend"]["pid"].is_null());
    harness.stop_daemon().await?;
    Ok(())
}
