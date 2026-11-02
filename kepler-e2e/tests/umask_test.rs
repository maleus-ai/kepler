//! E2E tests for `kepler.services.default_umask` and `services.<name>.umask`.
//!
//! The umask is applied by kepler-exec right before exec, so it must cover the
//! service command, every hook (whatever its `user:`) and the healthcheck,
//! while the daemon keeps its own `007`.

use kepler_e2e::{E2eHarness, E2eResult};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TEST_MODULE: &str = "umask_test";

/// Directory every test user can write to, reachable from the harness temp dir.
fn shared_dir(harness: &E2eHarness) -> E2eResult<PathBuf> {
    std::fs::set_permissions(harness.temp_dir().path(), std::fs::Permissions::from_mode(0o755))?;
    let dir = harness.create_temp_dir("umask_out")?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))?;
    Ok(dir)
}

/// Waits for `path` to exist and returns its metadata.
async fn wait_for_file(path: &Path) -> std::fs::Metadata {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(meta) = std::fs::metadata(path) {
            return meta;
        }
        assert!(tokio::time::Instant::now() < deadline, "{} was never created", path.display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_mode(path: &Path, expected: u32) {
    let mode = wait_for_file(path).await.permissions().mode() & 0o777;
    assert_eq!(mode, expected, "{}: mode {:04o}, expected {:04o}", path.display(), mode, expected);
}

fn uid_of(user: &str) -> u32 {
    let out = std::process::Command::new("id").args(["-u", user]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// `default_umask: '027'` applies to `run:`, to a `pre_start` hook running as
/// another user, and to the healthcheck.
#[tokio::test]
async fn test_default_umask_applies_everywhere() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = shared_dir(&harness)?;
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_default_umask_applies_everywhere",
        &[("__DIR__", dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;
    harness.start_services(&config_path).await?.assert_success();

    assert_mode(&dir.join("hook_file"), 0o640).await;
    assert_mode(&dir.join("run_file"), 0o640).await;
    assert_mode(&dir.join("healthcheck_file"), 0o640).await;

    // The hook and the command really ran as different users.
    assert_eq!(std::fs::metadata(dir.join("hook_file"))?.uid(), uid_of("testuser2"));
    assert_eq!(std::fs::metadata(dir.join("run_file"))?.uid(), uid_of("testuser1"));

    harness.stop_daemon().await?;
    Ok(())
}

/// A service `umask` overrides `default_umask`; other services keep the default.
#[tokio::test]
async fn test_service_umask_overrides_default() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = shared_dir(&harness)?;
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_service_umask_overrides_default",
        &[("__DIR__", dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;
    harness.start_services(&config_path).await?.assert_success();

    assert_mode(&dir.join("overridden_file"), 0o600).await;
    assert_mode(&dir.join("defaulted_file"), 0o640).await;

    harness.stop_daemon().await?;
    Ok(())
}

/// Without any umask setting, services inherit the daemon's `007`.
#[tokio::test]
async fn test_no_umask_inherits_daemon() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = shared_dir(&harness)?;
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_no_umask_inherits_daemon",
        &[("__DIR__", dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;
    harness.start_services(&config_path).await?.assert_success();

    assert_mode(&dir.join("plain_file"), 0o660).await;

    harness.stop_daemon().await?;
    Ok(())
}

/// A config loaded by a non-root user applies its umask to the service and its hooks.
#[tokio::test]
async fn test_non_root_config_umask() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let dir = shared_dir(&harness)?;
    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_non_root_config_umask",
        &[("__DIR__", dir.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;
    let output = harness
        .run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"])
        .await?;
    output.assert_success();

    assert_mode(&dir.join("hook_file"), 0o640).await;
    assert_mode(&dir.join("run_file"), 0o640).await;
    assert_eq!(std::fs::metadata(dir.join("run_file"))?.uid(), uid_of("testuser1"));

    harness.stop_daemon().await?;
    Ok(())
}

/// An unquoted YAML number (here `777`) is refused with a hint to quote it,
/// rather than being applied as a possibly decimal value. (An unquoted `027`
/// is a different case: serde_yaml reads it as the string "027", so it is
/// accepted as 0o027 — see `yaml_unquoted_leading_zero_is_never_misread`.)
#[tokio::test]
async fn test_unquoted_umask_rejected() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_unquoted_umask_rejected")?;

    harness.start_daemon().await?;
    let output = harness.start_services(&config_path).await?;
    assert!(!output.success(), "unquoted umask must be rejected. stdout: {}", output.stdout);
    assert!(
        output.stderr_contains("quoted octal string"),
        "error should tell to quote the value. stderr: {}",
        output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}
