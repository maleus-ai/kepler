//! E2E tests for the root owner-takeover guard on `start`, `run` and `recreate`.
//!
//! Loading a config records its caller as owner. A root reload of another user's
//! config therefore locks that user out of it, so the CLI warns — always — and asks
//! for confirmation on a terminal unless `--force` is given.

use kepler_e2e::{E2eHarness, E2eResult};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const TEST_MODULE: &str = "owner_takeover_test";
const WARNING: &str = "Launching it as root makes root its owner";

/// Output of a CLI run under a pseudo-terminal: stdout and stderr share the pty.
struct PtyOutput {
    output: String,
    exit_code: i32,
}

/// Runs the CLI as root under a pty (`script`), so stdin and stderr are terminals,
/// feeding `input` as what the operator types.
async fn run_root_cli_in_pty(harness: &E2eHarness, args: &[&str], input: &str) -> E2eResult<PtyOutput> {
    let command_line = std::iter::once(harness.kepler_bin().to_str().unwrap())
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");

    let mut child = Command::new("script")
        .args(["-qec", &command_line, "/dev/null"])
        .env("KEPLER_DAEMON_PATH", harness.temp_dir().path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).await?;
    drop(stdin);

    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("pty CLI run timed out")?;

    Ok(PtyOutput {
        output: format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Starts the config as testuser1, who becomes its owner.
async fn start_as_testuser1(harness: &mut E2eHarness, config_path: &Path) -> E2eResult<()> {
    harness.start_daemon().await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(config_path, "takeover-svc", "running", Duration::from_secs(10)).await?;
    Ok(())
}

/// Without a terminal, a root `run` still proceeds — after the warning — and the
/// previous owner is locked out, which is what the warning is about.
#[tokio::test]
async fn test_root_run_without_terminal_warns_and_takes_ownership() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    let output = harness.run_cli(&["-f", config, "run", "-d", "--wait"]).await?;
    output.assert_success();
    assert!(output.stderr_contains(WARNING), "Root takeover should warn. stderr: {}", output.stderr);
    assert!(output.stderr_contains("is owned by UID"), "Warning should name the displaced owner. stderr: {}", output.stderr);

    let output = harness.run_cli_as_user("testuser1", &["-f", config, "ps"]).await?;
    assert!(!output.success(), "The displaced owner should have lost access. stdout: {}", output.stdout);

    harness.stop_daemon().await?;
    Ok(())
}

/// On a terminal, declining the prompt aborts with exit 1 and leaves the owner in place.
#[tokio::test]
async fn test_root_run_on_terminal_declined_aborts() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    let output = run_root_cli_in_pty(&harness, &["-f", config, "run", "-d", "--wait"], "n\n").await?;
    assert_eq!(output.exit_code, 1, "Declined takeover should exit 1. output: {}", output.output);
    assert!(output.output.contains(WARNING), "Warning should precede the prompt. output: {}", output.output);
    assert!(output.output.contains("Aborted."), "output: {}", output.output);

    let output = harness.run_cli_as_user("testuser1", &["-f", config, "ps"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// On a terminal, confirming the prompt proceeds, and the warning stays in the output.
#[tokio::test]
async fn test_root_run_on_terminal_confirmed_proceeds() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    let output = run_root_cli_in_pty(&harness, &["-f", config, "run", "-d", "--wait"], "y\n").await?;
    assert_eq!(output.exit_code, 0, "Confirmed takeover should proceed. output: {}", output.output);
    assert!(output.output.contains(WARNING), "output: {}", output.output);
    assert!(output.output.contains("Taking ownership as root (confirmed)."), "output: {}", output.output);

    harness.stop_daemon().await?;
    Ok(())
}

/// `--force` skips the prompt on a terminal but never the warning.
#[tokio::test]
async fn test_root_run_with_force_skips_prompt_but_warns() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    let output = run_root_cli_in_pty(&harness, &["-f", config, "run", "-d", "--wait", "--force"], "").await?;
    assert_eq!(output.exit_code, 0, "--force should not prompt. output: {}", output.output);
    assert!(output.output.contains(WARNING), "output: {}", output.output);
    assert!(output.output.contains("Taking ownership as root (--force)."), "output: {}", output.output);
    assert!(!output.output.contains("[y/N]"), "--force should not prompt. output: {}", output.output);

    harness.stop_daemon().await?;
    Ok(())
}

/// `recreate` reloads the config, so it is guarded like `run`.
#[tokio::test]
async fn test_root_recreate_warns() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    let output = harness.run_cli(&["-f", config, "recreate"]).await?;
    output.assert_success();
    assert!(output.stderr_contains(WARNING), "stderr: {}", output.stderr);

    harness.stop_daemon().await?;
    Ok(())
}

/// A root process holding a service token authenticates as a token caller, not as
/// root — its takeover must warn all the same.
#[tokio::test]
async fn test_root_service_with_token_warns() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let target_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let target = target_path.to_str().unwrap().to_string();
    start_as_testuser1(&mut harness, &target_path).await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_takeover_result.txt");
    let result = result_file.to_str().unwrap().to_string();
    let controller_path = harness.create_named_config(
        "token_controller.kepler.yaml",
        &format!(
            r#"services:
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{target}' run -d --wait > '{result}' 2>&1
        echo "EXIT_CODE=$?" >> '{result}'
        sleep 300
    permissions: [run]
"#
        ),
    )?;

    let output = harness.run_cli(&["-f", controller_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_file_content(&result_file, "EXIT_CODE=", Duration::from_secs(15)).await?;
    let content = std::fs::read_to_string(&result_file)?;
    assert!(content.contains("EXIT_CODE=0"), "Token-authenticated run should succeed. Result: {}", content);
    assert!(content.contains(WARNING), "Token-authenticated root takeover should warn. Result: {}", content);

    harness.stop_daemon().await?;
    Ok(())
}

/// A root `start` on a config the daemon already holds does not reload it, so the
/// owner is untouched and there is nothing to warn about.
#[tokio::test]
async fn test_root_start_on_loaded_config_does_not_warn() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    let output = harness.run_cli(&["-f", config, "start", "-d"]).await?;
    output.assert_success();
    assert!(!output.stderr_contains(WARNING), "stderr: {}", output.stderr);

    harness.stop_daemon().await?;
    Ok(())
}

/// A root `start` on an unloaded config loads it — the case where the daemon
/// restarted and nobody has touched the config since.
#[tokio::test]
async fn test_root_start_on_unloaded_config_warns() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_takeover")?;
    let config = config_path.to_str().unwrap();
    start_as_testuser1(&mut harness, &config_path).await?;

    // No autostart: the restarted daemon keeps the state on disk but does not reload the config.
    harness.kill_daemon().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", config, "start", "-d"]).await?;
    output.assert_success();
    assert!(output.stderr_contains(WARNING), "stderr: {}", output.stderr);

    harness.stop_daemon().await?;
    Ok(())
}

/// No warning when root reloads its own config, nor when a user reloads theirs.
#[tokio::test]
async fn test_no_warning_without_takeover() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let root_config = harness.load_config(TEST_MODULE, "test_takeover")?;
    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", root_config.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    let output = harness.run_cli(&["-f", root_config.to_str().unwrap(), "run", "-d", "--wait"]).await?;
    output.assert_success();
    assert!(!output.stderr_contains(WARNING), "Root reloading its own config. stderr: {}", output.stderr);

    let user_config = harness.create_named_config("user_takeover.kepler.yaml", &std::fs::read_to_string(&root_config)?)?;
    let user_config = user_config.to_str().unwrap();
    let output = harness.run_cli_as_user("testuser1", &["-f", user_config, "start", "-d"]).await?;
    output.assert_success();
    let output = harness.run_cli_as_user("testuser1", &["-f", user_config, "run", "-d", "--wait"]).await?;
    output.assert_success();
    assert!(!output.stderr_contains(WARNING), "User reloading their own config. stderr: {}", output.stderr);

    harness.stop_daemon().await?;
    Ok(())
}
