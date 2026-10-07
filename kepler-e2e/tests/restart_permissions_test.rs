//! Restart permissions must not authorize a service's first startup.
use kepler_e2e::{E2eHarness, E2eResult};
use serde_json::Value;
use std::time::Duration;

#[tokio::test]
async fn restart_inactive_recovers_initialized_services_without_granting_start() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().to_path_buf();
    let config = harness.create_test_config(&format!(
        r#"
services:
  narrow:
    command: [sh, -c, 'echo "$KEPLER_TOKEN" > {root}/narrow-token; sleep 300']
    permissions:
      allow: [restart, restart:no-deps, status]
  wide:
    command: [sh, -c, 'echo "$KEPLER_TOKEN" > {root}/wide-token; sleep 300']
    permissions:
      allow: [restart, restart:inactive, restart:no-deps, status]
  known:
    command: [sleep, '300']
  fresh:
    command: [sh, -c, 'touch {root}/fresh-ran; sleep 300']
  first_failure:
    command: [sleep, '300']
    hooks:
      pre_start:
        run: 'false'
"#,
        root = root.display()
    ))?;
    let path = config.to_str().unwrap();
    harness.start_daemon().await?;
    harness
        .run_cli(&["-f", path, "start", "narrow", "wide", "known", "-d"])
        .await?
        .assert_success();
    let narrow = harness
        .wait_for_file_content(&root.join("narrow-token"), "\n", Duration::from_secs(5))
        .await?;
    let wide = harness
        .wait_for_file_content(&root.join("wide-token"), "\n", Duration::from_secs(5))
        .await?;
    assert_eq!(narrow.trim().len(), 64);
    assert_eq!(wide.trim().len(), 64);
    let narrow_env = [("KEPLER_TOKEN", narrow.trim())];
    let wide_env = [("KEPLER_TOKEN", wide.trim())];
    let denied_start = harness
        .run_cli_with_env(&["-f", path, "start", "fresh", "-d"], &wide_env)
        .await?;
    assert!(!denied_start.success());
    assert!(denied_start.stderr.contains("start"));

    harness
        .stop_service(&config, "known")
        .await?
        .assert_success();
    let missing_right = harness
        .run_cli_with_env(
            &["-f", path, "restart", "known", "--states", "stopped"],
            &narrow_env,
        )
        .await?;
    assert!(!missing_right.success());
    assert!(missing_right.stderr.contains("restart:inactive"));
    harness
        .run_cli_with_env(
            &["-f", path, "restart", "known", "--states", "stopped"],
            &wide_env,
        )
        .await?
        .assert_success();
    // The base restart right still handles an active service without --states.
    harness
        .run_cli_with_env(&["-f", path, "restart", "known"], &narrow_env)
        .await?
        .assert_success();

    // Explicit names and --no-deps cannot bypass initialization, even with the new right.
    for args in [
        vec!["-f", path, "restart", "fresh", "--states", "all"],
        vec![
            "-f",
            path,
            "restart",
            "fresh",
            "--states",
            "all",
            "--no-deps",
        ],
    ] {
        let rejected = harness.run_cli_with_env(&args, &wide_env).await?;
        assert!(!rejected.success());
        assert!(rejected.stderr.contains("kepler start fresh"));
        assert!(!root.join("fresh-ran").exists());
    }
    // Root/owner permissions do not turn restart into a first-start operation.
    let root_rejected = harness
        .run_cli(&["-f", path, "restart", "fresh", "--states", "stopped"])
        .await?;
    assert!(!root_rejected.success());
    assert!(root_rejected.stderr.contains("kepler start fresh"));
    assert!(
        !harness
            .start_service(&config, "first_failure")
            .await?
            .success()
    );
    let failed_first_start = harness
        .run_cli_with_env(
            &["-f", path, "restart", "first_failure", "--states", "failed"],
            &wide_env,
        )
        .await?;
    assert!(!failed_first_start.success());
    assert!(
        failed_first_start
            .stderr
            .contains("kepler start first_failure")
    );

    // Bulk selection ignores never-initialized services, while still restarting known services.
    harness
        .run_cli(&["-f", path, "restart", "--states", "all"])
        .await?
        .assert_success();
    let output = harness.run_cli(&["-f", path, "ps", "--json"]).await?;
    output.assert_success();
    let states: Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(states["known"]["status"], "running");
    assert_eq!(states["fresh"]["status"], "stopped");
    assert_eq!(states["fresh"]["initialized"], false);
    assert_eq!(states["first_failure"]["status"], "failed");
    assert_eq!(states["first_failure"]["initialized"], false);
    assert!(!root.join("fresh-ran").exists());
    // A real start remains the legitimate bootstrap path.
    harness
        .start_service(&config, "fresh")
        .await?
        .assert_success();
    harness
        .wait_for_file_content(&root.join("fresh-ran"), "", Duration::from_secs(5))
        .await?;
    harness.stop_daemon().await?;
    Ok(())
}
