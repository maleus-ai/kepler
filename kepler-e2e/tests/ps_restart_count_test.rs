use kepler_e2e::{E2eHarness, E2eResult};
use serde_json::Value;
use std::time::Duration;

#[tokio::test]
async fn ps_reports_restarts_after_recovery_in_json_and_tables() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config = harness.create_test_config(
        r#"
services:
  worker:
    command: ${{ service.restart_count < 2 and {"sh", "-c", "exit 1"} or {"sleep", "300"} }}$
    restart: always
    healthcheck:
      command: ["true"]
      interval: 100ms
      start_period: 0s
"#,
    )?;
    let path = config.to_str().unwrap();
    harness.start_daemon().await?;
    harness.start_services(&config).await?.assert_success();
    harness
        .wait_for_service_status(&config, "worker", "healthy", Duration::from_secs(10))
        .await?;

    let output = harness.run_cli(&["-f", path, "ps", "--json"]).await?;
    output.assert_success();
    let services: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(services["worker"]["restart_count"], 2);
    assert_eq!(services["worker"]["status"], "healthy");

    let output = harness.run_cli(&["ps", "--all", "--json"]).await?;
    output.assert_success();
    let configs: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(configs[0]["services"]["worker"]["restart_count"], 2);

    for args in [vec!["-f", path, "ps"], vec!["ps", "--all"]] {
        let output = harness.run_cli(&args).await?;
        output.assert_success();
        let header = output.stdout.lines().next().unwrap();
        assert_eq!(header.split_whitespace().last(), Some("RESTARTS"));
        let row = output
            .stdout
            .lines()
            .find(|line| line.split_whitespace().any(|cell| cell == "worker"))
            .unwrap();
        assert_eq!(row.split_whitespace().last(), Some("2"));
    }

    let output = harness.run_cli(&["-f", path, "inspect", "worker"]).await?;
    output.assert_success();
    let inspected: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(inspected["services"]["worker"]["state"]["restart_count"], 2);

    harness
        .restart_service(&config, "worker")
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "worker", "healthy", Duration::from_secs(10))
        .await?;
    let output = harness.run_cli(&["-f", path, "ps", "--json"]).await?;
    output.assert_success();
    let services: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(services["worker"]["restart_count"], 3);

    harness.stop_daemon().await?;
    Ok(())
}
