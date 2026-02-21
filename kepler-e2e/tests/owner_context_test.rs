//! E2E tests for the `owner` Lua context and `os.getgroups()`.
//!
//! Tests that Lua scripts can access config owner info (uid, gid, user)
//! and query supplementary groups, with hardening restrictions enforced.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "owner_context_test";

// =========================================================================
// owner context availability
// =========================================================================

/// owner.uid, owner.gid, owner.user are accessible when config loaded by non-root user
#[tokio::test]
async fn test_owner_context_available_for_non_root() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_owner_context_available")?;

    harness.start_daemon().await?;

    // Start as testuser1 — they become the config owner
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user("testuser1", &["-f", config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "owner-ctx-svc", "running", Duration::from_secs(10))
        .await?;

    // Check that owner fields are populated
    let logs = harness.wait_for_log_content(&config_path, "OWNER_UID=", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    // owner.uid should be testuser1's uid (non-zero)
    assert!(
        stdout.contains("OWNER_UID="),
        "Should contain OWNER_UID. stdout: {}", stdout
    );
    assert!(
        !stdout.contains("OWNER_UID=nil"),
        "OWNER_UID should not be nil. stdout: {}", stdout
    );

    // owner.user should be testuser1
    assert!(
        stdout.contains("OWNER_USER=testuser1"),
        "OWNER_USER should be testuser1. stdout: {}", stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// owner is populated with uid=0 when config loaded by root
#[tokio::test]
async fn test_owner_present_for_root() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_owner_nil_for_root")?;

    harness.start_daemon().await?;

    // Start as root (default) — owner should have uid=0
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "owner-root-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "ROOT_OWNER_UID=0", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    // owner.uid should be 0 for root
    assert!(
        stdout.contains("ROOT_OWNER_UID=0"),
        "Root should have owner.uid=0. stdout: {}", stdout
    );
    // Don't assert on username — uid 0 is "root" on Linux but may differ on other platforms

    harness.stop_daemon().await?;
    Ok(())
}

/// owner fields work in ${{ }}$ inline expressions
#[tokio::test]
async fn test_owner_inline_expressions() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_owner_inline_expr")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user("testuser1", &["-f", config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "owner-inline-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "INLINE_UID=", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    // Should have non-'none' uid
    assert!(
        !stdout.contains("INLINE_UID=none"),
        "INLINE_UID should not be 'none'. stdout: {}", stdout
    );

    // Should have testuser1 as user
    assert!(
        stdout.contains("INLINE_USER=testuser1"),
        "INLINE_USER should be testuser1. stdout: {}", stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// os.getgroups()
// =========================================================================

/// os.getgroups() returns group names for the config owner
#[tokio::test]
async fn test_os_getgroups_returns_groups() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_os_getgroups")?;

    harness.start_daemon().await?;

    // Start as testuser1 (who is in the 'kepler' group)
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user("testuser1", &["-f", config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "getgroups-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "GROUPS=", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    // testuser1 is in the 'kepler' group (set up in Dockerfile)
    assert!(
        stdout.contains("kepler"),
        "Groups should include 'kepler'. stdout: {}", stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// os.clock() still works via metatable fallback
#[tokio::test]
async fn test_os_clock_metatable_fallback() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_os_clock_fallback")?;

    harness.start_daemon().await?;

    // Start as testuser1 (so owner context is active and os table is overridden)
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user("testuser1", &["-f", config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "clock-fallback-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "OS_CLOCK_WORKS", Duration::from_secs(5)).await?;
    assert!(
        logs.stdout.contains("OS_CLOCK_WORKS"),
        "os.clock() should work via metatable fallback. stdout: {}", logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// os.getgroups() hardening restrictions
// =========================================================================

/// os.getgroups("root") is blocked with --hardening no-root
#[tokio::test]
async fn test_os_getgroups_blocked_noroot_hardening() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_os_getgroups_noroot")?;

    harness.start_daemon().await?;

    // Start as testuser1 with no-root hardening
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user(
        "testuser1",
        &["-f", config_str, "start", "-d", "--hardening", "no-root"],
    ).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "getgroups-noroot-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "GETGROUPS_ROOT_BLOCKED", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    assert!(
        stdout.contains("GETGROUPS_ROOT_BLOCKED"),
        "os.getgroups('root') should be blocked. stdout: {}", stdout
    );
    assert!(
        !stdout.contains("SHOULD_HAVE_FAILED"),
        "os.getgroups('root') should not succeed. stdout: {}", stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// os.getgroups("testuser2") is blocked with --hardening strict (only own uid allowed)
#[tokio::test]
async fn test_os_getgroups_blocked_strict_other_user() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_os_getgroups_strict")?;

    harness.start_daemon().await?;

    // Start as testuser1 with strict hardening — querying testuser2 should fail
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user(
        "testuser1",
        &["-f", config_str, "start", "-d", "--hardening", "strict"],
    ).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "getgroups-strict-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "GETGROUPS_OTHER_USER_BLOCKED", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    assert!(
        stdout.contains("GETGROUPS_OTHER_USER_BLOCKED"),
        "os.getgroups('testuser2') should be blocked with strict hardening. stdout: {}", stdout
    );
    assert!(
        !stdout.contains("SHOULD_HAVE_FAILED"),
        "os.getgroups('testuser2') should not succeed. stdout: {}", stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// os.getgroups(own_user) succeeds with --hardening strict
#[tokio::test]
async fn test_os_getgroups_allowed_strict_self() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_os_getgroups_strict_self")?;

    harness.start_daemon().await?;

    // Start as testuser1 with strict hardening — querying own user should succeed
    let config_str = config_path.to_str().unwrap();
    let output = harness.run_cli_as_user(
        "testuser1",
        &["-f", config_str, "start", "-d", "--hardening", "strict"],
    ).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "getgroups-strict-self-svc", "running", Duration::from_secs(10))
        .await?;

    let logs = harness.wait_for_log_content(&config_path, "SELF_GROUPS=", Duration::from_secs(5)).await?;
    let stdout = &logs.stdout;

    // Should have own groups (including 'kepler')
    assert!(
        stdout.contains("SELF_GROUPS="),
        "Should contain SELF_GROUPS. stdout: {}", stdout
    );
    assert!(
        stdout.contains("kepler"),
        "Own groups should include 'kepler'. stdout: {}", stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}
