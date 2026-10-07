//! Regressions found during review of manual restart state selection.
use kepler_e2e::{E2eHarness, E2eResult};
use serde_json::Value;
use std::time::Duration;

async fn service_states(harness: &E2eHarness, config: &std::path::Path) -> E2eResult<Value> {
    let output = harness
        .run_cli(&["-f", config.to_str().unwrap(), "ps", "--json"])
        .await?;
    output.assert_success();
    Ok(serde_json::from_str(&output.stdout).unwrap())
}

fn persisted_states(harness: &E2eHarness, config: &std::path::Path) -> Value {
    let state_dir = harness.get_config_state_dir(config).unwrap();
    let state = std::fs::read_to_string(state_dir.join("state.json")).unwrap();
    serde_json::from_str::<Value>(&state).unwrap()["services"].clone()
}

#[tokio::test]
async fn failed_restart_dependency_reports_error_and_unblocks_dependents() -> E2eResult<()> {
    for policy in ["on-failure", "always"] {
        let mut harness = E2eHarness::new().await?;
        let root = harness.temp_dir().path().to_path_buf();
        let config = harness.create_test_config(&format!(
            r#"
services:
  dependency:
    command: [sleep, '300']
    restart: {policy}
    hooks:
      pre_start:
        run: 'test ! -f {root}/fail'
  worker:
    command: [sleep, '300']
    depends_on:
      dependency:
        condition: service_started
  fallback:
    command: [sleep, '300']
    depends_on:
      dependency:
        condition: service_failed
"#,
            root = root.display()
        ))?;
        harness.start_daemon().await?;
        // Leave the failure handler stopped until the restart selects it.
        harness
            .run_cli(&[
                "-f",
                config.to_str().unwrap(),
                "start",
                "dependency",
                "worker",
                "-d",
            ])
            .await?
            .assert_success();
        harness
            .wait_for_service_status(&config, "worker", "running", Duration::from_secs(5))
            .await?;
        harness
            .run_cli(&[
                "-f",
                config.to_str().unwrap(),
                "start",
                "fallback",
                "-d",
                "--no-deps",
            ])
            .await?
            .assert_success();
        harness
            .stop_service(&config, "fallback")
            .await?
            .assert_success();
        std::fs::write(root.join("fail"), "fail").unwrap();
        // No dependency timeout: the terminal startup failure itself must finish this request.
        let output = harness
            .run_cli_with_timeout(
                &[
                    "-f",
                    config.to_str().unwrap(),
                    "restart",
                    "--states",
                    "stopped",
                ],
                Duration::from_secs(5),
            )
            .await?;
        assert!(!output.success());
        assert!(
            output.stderr.contains("pre_start"),
            "missing hook failure: {}",
            output.stderr
        );
        let states = service_states(&harness, &config).await?;
        assert_eq!(states["dependency"]["status"], "failed");
        assert_eq!(states["worker"]["status"], "skipped");
        assert!(states["worker"]["pid"].is_null());
        harness
            .wait_for_service_status(&config, "fallback", "running", Duration::from_secs(5))
            .await?;
        harness.stop_daemon().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_post_stop_cleanup_preserves_the_replacement_token() -> E2eResult<()> {
    for operation in ["restart", "stop"] {
        let mut harness = E2eHarness::new().await?;
        let root = harness.temp_dir().path().to_path_buf();
        let config = harness.create_test_config(&format!(
            r#"
services:
  worker:
    command: [sh, -c, 'echo "$KEPLER_TOKEN" > {root}/token; sleep 300']
    permissions:
      allow: [status]
    hooks:
      post_stop:
        run: |
          if mkdir {root}/hook-once 2>/dev/null; then
            touch {root}/old-post-stop
            while [ ! -f {root}/release-old ]; do sleep 0.02; done
          fi
"#,
            root = root.display()
        ))?;
        harness.start_daemon().await?;
        harness.start_services_wait(&config).await?.assert_success();
        let mut old_stop = tokio::process::Command::new(harness.kepler_bin())
            .args(["-f", config.to_str().unwrap(), operation, "worker"])
            .env("KEPLER_DAEMON_PATH", harness.state_dir())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        harness
            .wait_for_file_content(&root.join("old-post-stop"), "", Duration::from_secs(5))
            .await?;
        harness
            .stop_service(&config, "worker")
            .await?
            .assert_success();
        std::fs::remove_file(root.join("token")).unwrap();
        harness
            .start_service(&config, "worker")
            .await?
            .assert_success();
        let token = harness
            .wait_for_file_content(&root.join("token"), "\n", Duration::from_secs(5))
            .await?;
        assert_eq!(token.trim().len(), 64);
        let commands = ["-f", config.to_str().unwrap(), "logs"];
        let env = [("KEPLER_TOKEN", token.trim())];
        // Revocation would make this root client fall back to unrestricted root rights.
        assert!(!harness.run_cli_with_env(&commands, &env).await?.success());
        let replacement = service_states(&harness, &config).await?;
        std::fs::write(root.join("release-old"), "release").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), old_stop.wait())
                .await
                .expect("cancelled stop did not finish")?
                .success()
        );
        assert!(
            !harness.run_cli_with_env(&commands, &env).await?.success(),
            "{operation} cleanup revoked the replacement token"
        );
        let after = service_states(&harness, &config).await?;
        assert_eq!(after["worker"]["status"], "running");
        assert_eq!(after["worker"]["pid"], replacement["worker"]["pid"]);
        harness.stop_daemon().await?;
    }
    Ok(())
}

#[tokio::test]
async fn overlapping_restart_requests_launch_each_terminal_service_once() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().to_path_buf();
    let mut yaml = format!(
        r#"
services:
  active:
    command: [sleep, '300']
    hooks:
      pre_restart:
        run: |
          touch {root}/active-restart
          while [ ! -f {root}/release-active ]; do sleep 0.02; done
  fresh:
    command: [sh, -c, 'echo $$ >> {root}/pids; sleep 300']
    hooks:
      pre_start:
        run: |
          if [ -f {root}/armed ]; then
            echo entering >> {root}/pre-starts
            while [ ! -f {root}/release-starts ]; do sleep 0.02; done
          fi
"#,
        root = root.display()
    );
    // Widen the selection window so the two real requests can both observe
    // terminal services before claims. These services never spawn processes.
    for i in 0..64 {
        yaml.push_str(&format!(
            "  extra_{i}:\n    if: false\n    command: [sleep, '300']\n"
        ));
    }
    let config = harness.create_test_config(&yaml)?;
    harness.start_daemon().await?;
    harness
        .start_service(&config, "active")
        .await?
        .assert_success();
    harness
        .start_service(&config, "fresh")
        .await?
        .assert_success();
    harness
        .stop_service(&config, "fresh")
        .await?
        .assert_success();
    std::fs::remove_file(root.join("pids")).unwrap();
    std::fs::write(root.join("armed"), "armed").unwrap();
    let spawn_restart = || {
        tokio::process::Command::new(harness.kepler_bin())
            .args(["-f", config.to_str().unwrap(), "restart", "--states", "all"])
            .env("KEPLER_DAEMON_PATH", harness.state_dir())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
    };
    let mut first = spawn_restart()?;
    let mut second = spawn_restart()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness
            .daemon_logs()
            .matches("Restarting services for")
            .count()
            < 2
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both restart requests must reach the daemon");
    harness
        .wait_for_file_content(&root.join("active-restart"), "", Duration::from_secs(5))
        .await?;
    std::fs::write(root.join("release-active"), "release").unwrap();
    harness
        .wait_for_file_content(&root.join("pre-starts"), "entering", Duration::from_secs(5))
        .await?;
    harness
        .wait_for_service_status(&config, "active", "running", Duration::from_secs(5))
        .await?;
    // Keep pre_start blocked while both requests finish their claim/stop phases.
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(root.join("release-starts"), "release").unwrap();
    let (first, second) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(first.wait(), second.wait())
    })
    .await
    .expect("overlapping restart requests did not finish");
    assert!(first?.success());
    assert!(second?.success());
    assert_eq!(
        std::fs::read_to_string(root.join("pre-starts")).unwrap(),
        "entering\n",
        "pre_start ran more than once for one terminal service"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("pids"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        service_states(&harness, &config).await?["fresh"]["status"],
        "running"
    );
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn running_restart_and_terminal_start_have_distinct_hooks_and_counters() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().to_path_buf();
    let names = ["running", "stopped", "exited", "killed", "failed", "fresh"];
    let mut yaml = String::from("services:\n");
    for name in names {
        let command = match name {
            "exited" => "[sh, -c, 'exit 0']",
            "killed" => "[sh, -c, 'kill -KILL $$']",
            _ => "[sleep, '300']",
        };
        yaml.push_str(&format!("  {name}:\n    command: {command}\n    hooks:\n"));
        for hook in [
            "pre_restart",
            "pre_stop",
            "post_stop",
            "pre_start",
            "post_start",
            "post_restart",
        ] {
            let guard = if name == "failed" && hook == "pre_start" {
                format!("test -f {}/repaired && ", root.display())
            } else {
                String::new()
            };
            yaml.push_str(&format!(
                "      {hook}:\n        run: '{guard}echo {hook} >> {}/{name}.hooks'\n",
                root.display(),
            ));
        }
    }
    let config = harness.create_test_config(&yaml)?;
    harness.start_daemon().await?;
    std::fs::write(root.join("repaired"), "ready").unwrap();
    for name in ["running", "stopped", "exited", "killed", "failed"] {
        let output = harness
            .run_cli(&["-f", config.to_str().unwrap(), "start", name, "-d"])
            .await?;
        output.assert_success();
    }
    harness
        .stop_service(&config, "failed")
        .await?
        .assert_success();
    std::fs::remove_file(root.join("repaired")).unwrap();
    assert!(!harness.start_service(&config, "failed").await?.success());
    for (name, expected) in [
        ("running", "running"),
        ("stopped", "running"),
        ("exited", "exited"),
        ("killed", "killed"),
        ("failed", "failed"),
    ] {
        harness
            .wait_for_service_status(&config, name, expected, Duration::from_secs(5))
            .await?;
    }
    harness
        .stop_service(&config, "stopped")
        .await?
        .assert_success();
    std::fs::write(root.join("repaired"), "ready").unwrap();
    let before = persisted_states(&harness, &config);
    for name in names {
        let path = root.join(format!("{name}.hooks"));
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }

    harness
        .run_cli(&["-f", config.to_str().unwrap(), "restart", "--states", "all"])
        .await?
        .assert_success();
    let after = persisted_states(&harness, &config);
    assert_eq!(
        std::fs::read_to_string(root.join("running.hooks")).unwrap(),
        "pre_restart\npre_stop\npost_stop\npre_start\npost_start\npost_restart\n",
    );
    assert_eq!(
        after["running"]["restart_count"].as_u64().unwrap(),
        before["running"]["restart_count"].as_u64().unwrap() + 1
    );
    for name in ["stopped", "exited", "killed", "failed"] {
        assert_eq!(
            std::fs::read_to_string(root.join(format!("{name}.hooks"))).unwrap(),
            "pre_start\npost_start\n",
            "incorrect lifecycle for {name}"
        );
        assert_eq!(
            after[name]["restart_count"], before[name]["restart_count"],
            "start incremented restart count for {name}"
        );
    }
    assert_eq!(after["fresh"]["status"], "stopped");
    assert_eq!(after["fresh"]["initialized"], false);
    assert!(!root.join("fresh.hooks").exists());
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn restart_hook_outputs_survive_until_start_and_post_restart_hooks() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().display().to_string();
    let config = harness.create_test_config(&format!(
        r#"
services:
  worker:
    command: [sleep, '300']
    hooks:
      pre_restart:
        run: "echo '::output::token=from-restart'"
        output: setup
      post_stop:
        run: "echo '::output::token=from-stop'"
        output: cleanup
      pre_start:
        if: ${{{{ service.restart_count > 0 }}}}$
        run: "echo '${{{{ service.hooks.pre_restart.outputs.setup.token }}}}$' >> {root}/observed"
      post_restart:
        run: "echo '${{{{ service.hooks.post_stop.outputs.cleanup.token }}}}$' >> {root}/observed"
"#
    ))?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness.restart_services(&config).await?.assert_success();
    assert_eq!(
        std::fs::read_to_string(harness.temp_dir().path().join("observed")).unwrap(),
        "from-restart\nfrom-stop\n"
    );
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn stop_cancels_dependency_waits_for_both_restart_and_terminal_start() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().display().to_string();
    let config = harness.create_test_config(&format!(
        r#"
services:
  source:
    command: [sleep, '300']
    healthcheck:
      command: [test, -f, '{root}/healthy']
      interval: 100ms
      retries: 1
  worker:
    command: [sleep, '300']
    depends_on:
      source:
        condition: service_healthy
"#
    ))?;
    harness.start_daemon().await?;
    harness
        .run_cli(&[
            "-f",
            config.to_str().unwrap(),
            "start",
            "source",
            "worker",
            "-d",
            "--no-deps",
        ])
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "worker", "running", Duration::from_secs(5))
        .await?;

    for expected in ["restarting", "waiting"] {
        let marker = harness.temp_dir().path().join("healthy");
        if marker.exists() {
            std::fs::remove_file(&marker).unwrap();
        }
        harness
            .wait_for_service_status(&config, "source", "unhealthy", Duration::from_secs(5))
            .await?;
        let mut child = tokio::process::Command::new(harness.kepler_bin())
            .args([
                "-f",
                config.to_str().unwrap(),
                "restart",
                "worker",
                "--states",
                "stopped",
            ])
            .env("KEPLER_DAEMON_PATH", harness.state_dir())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if service_states(&harness, &config).await.unwrap()["worker"]["status"] == expected
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("worker did not enter the selected lifecycle");
        if expected == "restarting" {
            // Count advances after the stop phase and restart delay, just before dependency waiting.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if persisted_states(&harness, &config)["worker"]["restart_count"] == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("restart never reached dependency waiting");
        }
        harness
            .stop_service(&config, "worker")
            .await?
            .assert_success();
        let result = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("stopping worker did not cancel its dependency wait")?;
        assert!(result.success());
        std::fs::write(&marker, "healthy").unwrap();
        harness
            .wait_for_service_status(&config, "source", "healthy", Duration::from_secs(5))
            .await?;
        assert_eq!(
            service_states(&harness, &config).await?["worker"]["status"],
            "stopped"
        );
    }
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn cancelled_startup_cleanup_preserves_a_new_process_token() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().to_path_buf();
    let config = harness.create_test_config(&format!(
        r#"
services:
  worker:
    command: [sh, -c, 'echo "$KEPLER_TOKEN" > {root}/token; sleep 300']
    permissions:
      allow: [status]
    hooks:
      pre_start:
        run: |
          if [ "$MODE" = old ]; then
            touch {root}/old-hook
            while [ ! -f {root}/release-old ]; do sleep 0.02; done
            test ! -f {root}/fail-old
          fi
"#,
        root = root.display()
    ))?;
    harness.start_daemon().await?;

    for fail_old_hook in [false, true] {
        for marker in ["old-hook", "release-old", "token"] {
            let path = root.join(marker);
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        if fail_old_hook {
            std::fs::write(root.join("fail-old"), "fail").unwrap();
        }
        let mut old_start = tokio::process::Command::new(harness.kepler_bin())
            .args([
                "-f",
                config.to_str().unwrap(),
                "start",
                "worker",
                "-d",
                "-e",
                "MODE=old",
            ])
            .env("KEPLER_DAEMON_PATH", harness.state_dir())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        harness
            .wait_for_file_content(&root.join("old-hook"), "", Duration::from_secs(5))
            .await?;
        harness
            .stop_service(&config, "worker")
            .await?
            .assert_success();
        harness
            .run_cli(&[
                "-f",
                config.to_str().unwrap(),
                "start",
                "worker",
                "-d",
                "-e",
                "MODE=new",
            ])
            .await?
            .assert_success();
        let token = harness
            .wait_for_file_content(&root.join("token"), "\n", Duration::from_secs(5))
            .await?;
        assert_eq!(token.trim().len(), 64);
        let commands = ["-f", config.to_str().unwrap(), "logs"];
        let env = [("KEPLER_TOKEN", token.trim())];
        // A valid status-only token must deny logs. If revoked, this root test
        // client falls back to root authentication and logs would succeed.
        assert!(!harness.run_cli_with_env(&commands, &env).await?.success());
        std::fs::write(root.join("release-old"), "release").unwrap();
        tokio::time::timeout(Duration::from_secs(5), old_start.wait())
            .await
            .expect("old startup did not finish")?;
        assert!(
            !harness.run_cli_with_env(&commands, &env).await?.success(),
            "old hook cleanup revoked the newer process's token"
        );
        assert_eq!(
            service_states(&harness, &config).await?["worker"]["status"],
            "running"
        );
        harness
            .stop_service(&config, "worker")
            .await?
            .assert_success();
    }
    harness.stop_daemon().await?;
    Ok(())
}

#[tokio::test]
async fn restart_cancels_old_health_checks_and_new_instance_controls_health() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root = harness.temp_dir().path().to_path_buf();
    let config = harness.create_test_config(&format!(
        r#"
services:
  worker:
    command: [sleep, '300']
    healthcheck:
      run: |
        if [ -f {root}/armed ] && [ ! -f {root}/new-instance ]; then
          echo $$ > {root}/old-health-pid
          while [ ! -f {root}/release-health ]; do sleep 0.02; done
          touch {root}/old-result
          exit 1
        fi
        test ! -f {root}/fail-new
      interval: 100ms
      timeout: 10s
      retries: 1
    hooks:
      pre_restart:
        run: |
          touch {root}/restart-hook
          while [ ! -f {root}/release-restart ]; do sleep 0.02; done
"#,
        root = root.display()
    ))?;
    harness.start_daemon().await?;
    harness.start_services_wait(&config).await?.assert_success();
    harness
        .wait_for_service_status(&config, "worker", "healthy", Duration::from_secs(5))
        .await?;
    let before = service_states(&harness, &config).await?;
    std::fs::write(root.join("armed"), "armed").unwrap();
    let old_health_pid = harness
        .wait_for_file_content(&root.join("old-health-pid"), "\n", Duration::from_secs(5))
        .await?;
    let mut restart = tokio::process::Command::new(harness.kepler_bin())
        .args(["-f", config.to_str().unwrap(), "restart", "worker"])
        .env("KEPLER_DAEMON_PATH", harness.state_dir())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    harness
        .wait_for_file_content(&root.join("restart-hook"), "", Duration::from_secs(5))
        .await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !std::process::Command::new("sh")
                .args(["-c", "kill -0 \"$1\"", "check", old_health_pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("old health-check process was not cancelled before restart hooks");
    assert_eq!(
        service_states(&harness, &config).await?["worker"]["status"],
        "restarting"
    );
    std::fs::write(root.join("new-instance"), "new").unwrap();
    std::fs::write(root.join("release-health"), "release").unwrap();
    std::fs::write(root.join("release-restart"), "release").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), restart.wait())
            .await
            .expect("restart did not complete")?
            .success()
    );
    assert!(!root.join("old-result").exists());
    harness
        .wait_for_service_status(&config, "worker", "healthy", Duration::from_secs(5))
        .await?;
    let after = service_states(&harness, &config).await?;
    assert_ne!(before["worker"]["pid"], after["worker"]["pid"]);

    // Only the new checker can mark this process unhealthy; no automatic policy is configured.
    std::fs::write(root.join("fail-new"), "fail").unwrap();
    harness
        .wait_for_service_status(&config, "worker", "unhealthy", Duration::from_secs(5))
        .await?;
    let unhealthy = service_states(&harness, &config).await?;
    assert_eq!(unhealthy["worker"]["pid"], after["worker"]["pid"]);
    harness
        .restart_service(&config, "worker")
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "worker", "unhealthy", Duration::from_secs(5))
        .await?;
    assert_ne!(
        service_states(&harness, &config).await?["worker"]["pid"],
        unhealthy["worker"]["pid"]
    );
    harness.stop_daemon().await?;
    Ok(())
}

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
        .run_cli(&[
            "-f",
            path,
            "start",
            "z_source",
            "a_parent",
            "a_blocker",
            "-d",
            "--no-deps",
        ])
        .await?
        .assert_success();
    harness
        .wait_for_service_status(&config, "z_source", "running", Duration::from_secs(5))
        .await?;
    harness
        .wait_for_service_status(&config, "a_parent", "running", Duration::from_secs(5))
        .await?;
    harness
        .stop_service(&config, "a_blocker")
        .await?
        .assert_success();
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
    assert_eq!(states["a_blocker"]["status"], "waiting");
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
