//! E2E authorization tests for per-request access control.
//!
//! Authorization model: without ACL, only the config owner and root have access.
//! Non-owner kepler group members are denied unless an explicit ACL grants them scopes.

use kepler_e2e::{E2eHarness, E2eResult};
use std::path::Path;
use std::time::Duration;

const TEST_MODULE: &str = "authorization_test";

// =========================================================================
// Daemon-level operations are root-only
// =========================================================================

/// Non-root user is denied daemon shutdown
#[tokio::test]
async fn test_non_root_denied_shutdown() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["daemon", "stop"]).await?;
    assert!(!output.success(), "Non-root should be denied shutdown");
    assert!(output.stderr_contains("Permission denied"), "Should mention Permission denied");

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-root user is denied prune
#[tokio::test]
async fn test_non_root_denied_prune() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["prune"]).await?;
    assert!(!output.success(), "Non-root should be denied prune");
    assert!(output.stderr_contains("Permission denied"), "Should mention Permission denied");

    harness.stop_daemon().await?;
    Ok(())
}

/// Root can shut down the daemon
#[tokio::test]
async fn test_root_can_shutdown() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli(&["daemon", "stop"]).await?;
    output.assert_success();

    Ok(())
}

/// Root can prune
#[tokio::test]
async fn test_root_can_prune() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli(&["prune"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Owner and root can perform write operations; non-owner is denied
// =========================================================================

/// Root can stop any config
#[tokio::test]
async fn test_root_can_stop_any_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1 (they become the owner)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // Root can stop it even though testuser1 owns it
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// Owner can stop their own config
#[tokio::test]
async fn test_owner_can_stop_own_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser1 can stop their own config
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-owner is denied stop without ACL
#[tokio::test]
async fn test_non_owner_denied_stop_without_acl() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1 (they become the owner)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 is denied stop on testuser1's config
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    assert!(!output.success(), "Non-owner should be denied");
    assert!(output.stderr_contains("permission denied"), "Should mention permission denied");

    harness.stop_daemon().await?;
    Ok(())
}

/// Owner can restart their own config
#[tokio::test]
async fn test_owner_can_restart_own_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser1 can restart their own config
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "restart", "--wait"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-owner is denied restart without ACL
#[tokio::test]
async fn test_non_owner_denied_restart_without_acl() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 is denied restart on testuser1's config
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "restart", "--wait"]).await?;
    assert!(!output.success(), "Non-owner should be denied");
    assert!(output.stderr_contains("permission denied"), "Should mention permission denied");

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Owner and root can perform read operations; non-owner is denied
// =========================================================================

/// Owner can read logs of their own config
#[tokio::test]
async fn test_owner_can_read_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // Wait for log output
    tokio::time::sleep(Duration::from_millis(500)).await;

    // testuser1 can read logs
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "logs", "--tail", "10"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-owner is denied logs without ACL
#[tokio::test]
async fn test_non_owner_denied_logs_without_acl() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // Wait for log output
    tokio::time::sleep(Duration::from_millis(500)).await;

    // testuser2 is denied reading testuser1's logs
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "logs", "--tail", "10"]).await?;
    assert!(!output.success(), "Non-owner should be denied");
    assert!(output.stderr_contains("permission denied"), "Should mention permission denied");

    harness.stop_daemon().await?;
    Ok(())
}

/// Owner can view status of their own config
#[tokio::test]
async fn test_owner_can_view_status() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser1 can view status
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    output.assert_success();
    assert!(output.stdout_contains("auth-svc-a"), "Should see own service");

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-owner is denied status without ACL
#[tokio::test]
async fn test_non_owner_denied_status_without_acl() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 is denied viewing testuser1's status
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    assert!(!output.success(), "Non-owner should be denied");
    assert!(output.stderr_contains("permission denied"), "Should mention permission denied");

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Listing operations are filtered by ownership (root sees all)
// =========================================================================

/// List configs is filtered by ownership; root sees all
#[tokio::test]
async fn test_list_configs_filtered_by_ownership() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_a = harness.load_config(TEST_MODULE, "test_config_a")?;
    let config_b = harness.load_config(TEST_MODULE, "test_config_b")?;

    harness.start_daemon().await?;

    // testuser1 starts config_a
    let output = harness.run_cli_as_user("testuser1", &["-f", config_a.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    // testuser2 starts config_b
    let output = harness.run_cli_as_user("testuser2", &["-f", config_b.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_a, "auth-svc-a", "running", Duration::from_secs(10)).await?;
    harness.wait_for_service_status(&config_b, "auth-svc-b", "running", Duration::from_secs(10)).await?;

    // testuser1 lists configs — should see only their own config
    let output = harness.run_cli_as_user("testuser1", &["daemon", "status"]).await?;
    output.assert_success();
    assert!(
        output.stdout_contains("test_config_a"),
        "testuser1 should see config_a. stdout: {}",
        output.stdout
    );
    assert!(
        !output.stdout_contains("test_config_b"),
        "testuser1 should NOT see config_b. stdout: {}",
        output.stdout
    );

    // Root lists configs — should see both
    let output = harness.run_cli(&["daemon", "status"]).await?;
    output.assert_success();
    assert!(
        output.stdout_contains("test_config_a") && output.stdout_contains("test_config_b"),
        "Root should see both configs. stdout: {}",
        output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Status all is filtered by ownership; root sees all
#[tokio::test]
async fn test_status_all_filtered_by_ownership() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_a = harness.load_config(TEST_MODULE, "test_config_a")?;
    let config_b = harness.load_config(TEST_MODULE, "test_config_b")?;

    harness.start_daemon().await?;

    // testuser1 starts config_a
    let output = harness.run_cli_as_user("testuser1", &["-f", config_a.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    // testuser2 starts config_b
    let output = harness.run_cli_as_user("testuser2", &["-f", config_b.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_a, "auth-svc-a", "running", Duration::from_secs(10)).await?;
    harness.wait_for_service_status(&config_b, "auth-svc-b", "running", Duration::from_secs(10)).await?;

    // testuser1 views all-config status — should see only their own service
    let output = harness.run_cli_as_user("testuser1", &["ps", "--all"]).await?;
    output.assert_success();
    assert!(
        output.stdout_contains("auth-svc-a"),
        "testuser1 should see auth-svc-a. stdout: {}",
        output.stdout
    );
    assert!(
        !output.stdout_contains("auth-svc-b"),
        "testuser1 should NOT see auth-svc-b. stdout: {}",
        output.stdout
    );

    // Root sees all services
    let output = harness.run_cli(&["ps", "--all"]).await?;
    output.assert_success();
    assert!(
        output.stdout_contains("auth-svc-a") && output.stdout_contains("auth-svc-b"),
        "Root should see all services. stdout: {}",
        output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Filesystem permission check: non-owner denied when lacking read access
// =========================================================================

// =========================================================================
// ACL-based access control
// =========================================================================

/// ACL grants scoped access to non-owner group members
#[tokio::test]
async fn test_acl_grants_scoped_access() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_scoped")?;

    harness.start_daemon().await?;

    // Start as testuser1 (they become the owner)
    let output = harness
        .run_cli_as_user(
            "testuser1",
            &["-f", config_path.to_str().unwrap(), "start", "-d"],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "acl-svc", "running", Duration::from_secs(10))
        .await?;

    // Wait for log output
    tokio::time::sleep(Duration::from_millis(500)).await;

    // testuser2 has status right in ACL — should be able to view status
    let output = harness
        .run_cli_as_user(
            "testuser2",
            &["-f", config_path.to_str().unwrap(), "ps"],
        )
        .await?;
    output.assert_success();
    assert!(
        output.stdout_contains("acl-svc"),
        "testuser2 should see acl-svc via ACL. stdout: {}",
        output.stdout
    );

    // testuser2 has logs right in ACL — should be able to view logs
    let output = harness
        .run_cli_as_user(
            "testuser2",
            &[
                "-f",
                config_path.to_str().unwrap(),
                "logs",
                "--tail",
                "10",
            ],
        )
        .await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// ACL denies operations not in allow list
#[tokio::test]
async fn test_acl_denies_unlisted_operations() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_scoped")?;

    harness.start_daemon().await?;

    // Start as testuser1 (owner)
    let output = harness
        .run_cli_as_user(
            "testuser1",
            &["-f", config_path.to_str().unwrap(), "start", "-d"],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "acl-svc", "running", Duration::from_secs(10))
        .await?;

    // testuser2 has status + logs but NOT stop
    let output = harness
        .run_cli_as_user(
            "testuser2",
            &["-f", config_path.to_str().unwrap(), "stop"],
        )
        .await?;
    assert!(
        !output.success(),
        "testuser2 should be denied stop (not in ACL)"
    );
    assert!(
        output.stderr_contains("permission denied"),
        "Should mention permission denied. stderr: {}",
        output.stderr
    );

    // testuser2 also cannot restart
    let output = harness
        .run_cli_as_user(
            "testuser2",
            &["-f", config_path.to_str().unwrap(), "restart", "--wait"],
        )
        .await?;
    assert!(
        !output.success(),
        "testuser2 should be denied restart (not in ACL)"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Owner bypasses ACL — full access regardless of ACL entries
#[tokio::test]
async fn test_owner_bypasses_acl() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_scoped")?;

    harness.start_daemon().await?;

    // Start as testuser1 (owner) — the ACL only lists testuser2
    let output = harness
        .run_cli_as_user(
            "testuser1",
            &["-f", config_path.to_str().unwrap(), "start", "-d"],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "acl-svc", "running", Duration::from_secs(10))
        .await?;

    // Owner can stop even though they're not listed in ACL
    let output = harness
        .run_cli_as_user(
            "testuser1",
            &["-f", config_path.to_str().unwrap(), "stop"],
        )
        .await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// User with no ACL entry is denied all scoped operations
#[tokio::test]
async fn test_user_without_acl_entry_denied() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    // test_config_a has no ACL section at all
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1 (owner)
    let output = harness
        .run_cli_as_user(
            "testuser1",
            &["-f", config_path.to_str().unwrap(), "start", "-d"],
        )
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10))
        .await?;

    // testuser2 has no ACL entry — denied status
    let output = harness
        .run_cli_as_user(
            "testuser2",
            &["-f", config_path.to_str().unwrap(), "ps"],
        )
        .await?;
    assert!(
        !output.success(),
        "User without ACL entry should be denied"
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// PID-based service authentication
// =========================================================================

/// Service with permissions config gets registered by PID
#[tokio::test]
async fn test_pid_registered_for_security_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_token_security")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();

    harness
        .wait_for_service_status(&config_path, "token-svc", "running", Duration::from_secs(10))
        .await?;

    // Wait for the echo output
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check logs to verify the service echoed its PID (confirming it's running)
    let output = harness
        .get_logs(&config_path, Some("token-svc"), 10)
        .await?;
    output.assert_success();
    assert!(
        output.stdout_contains("PID="),
        "Service should have echoed its PID. stdout: {}",
        output.stdout
    );
    // PID should be a non-empty numeric string
    let pid_line = output
        .stdout
        .lines()
        .find(|l| l.contains("PID="))
        .expect("Should find PID line");
    let pid_value = pid_line
        .split("PID=")
        .nth(1)
        .expect("Should have value after PID=")
        .trim();
    assert!(
        pid_value.parse::<u32>().is_ok(),
        "PID should be a valid number, got: '{}'",
        pid_value
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// PID registration is revoked on service exit and re-registered on restart
#[tokio::test]
async fn test_pid_revoked_on_service_exit() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_token_security")?;

    harness.start_daemon().await?;

    harness.start_services(&config_path).await?.assert_success();

    harness
        .wait_for_service_status(&config_path, "token-svc", "running", Duration::from_secs(10))
        .await?;

    // Wait for first PID echo to appear in logs
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Capture the first PID value
    let output = harness
        .get_logs(&config_path, Some("token-svc"), 10)
        .await?;
    output.assert_success();
    let first_pid: String = output
        .stdout
        .lines()
        .find(|l| l.contains("PID="))
        .and_then(|l| l.split("PID=").nth(1))
        .map(|s| s.trim().to_string())
        .expect("Should find first PID line in logs");
    assert!(first_pid.parse::<u32>().is_ok(), "First PID should be numeric");

    // Stop the service — PID registration should be revoked
    harness.stop_services(&config_path).await?.assert_success();

    harness
        .wait_for_service_status(&config_path, "token-svc", "stopped", Duration::from_secs(10))
        .await?;

    // Restart the service — it should get a new PID
    harness.start_services(&config_path).await?.assert_success();

    harness
        .wait_for_service_status(&config_path, "token-svc", "running", Duration::from_secs(10))
        .await?;

    // Wait for second PID echo
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Capture the second PID value
    let output = harness
        .get_logs(&config_path, Some("token-svc"), 10)
        .await?;
    output.assert_success();
    let second_pid: String = output
        .stdout
        .lines()
        .find(|l| l.contains("PID="))
        .and_then(|l| l.split("PID=").nth(1))
        .map(|s| s.trim().to_string())
        .expect("Should find second PID line in logs");
    assert!(second_pid.parse::<u32>().is_ok(), "Second PID should be numeric");

    // PIDs should be different (old process exited, new one spawned)
    assert_ne!(
        first_pid, second_pid,
        "New PID should differ from previous PID"
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Filesystem permission check: non-owner denied when lacking read access
// =========================================================================

// =========================================================================
// Sub-right enforcement: stop:clean, stop:signal
// =========================================================================

/// User with `stop` but not `stop:clean` is denied `stop --clean`
#[tokio::test]
async fn test_acl_stop_without_clean_subright_denies_clean_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_stop_subrights")?;

    harness.start_daemon().await?;

    // Start as testuser1 (owner)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 has `stop` but NOT `stop:clean` — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop", "--clean"]).await?;
    assert!(!output.success(), "stop --clean should be denied without stop:clean sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("stop:clean") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    // testuser2 can still plain stop (has `stop`)
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// User with `stop` but not `stop:signal` is denied `stop --signal`
#[tokio::test]
async fn test_acl_stop_without_signal_subright_denies_signal_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_stop_subrights")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 has `stop` but NOT `stop:signal` — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop", "--signal", "SIGTERM"]).await?;
    assert!(!output.success(), "stop --signal should be denied without stop:signal sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("stop:signal") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Sub-right enforcement: start:hardening, start:env-override, start:no-deps
// =========================================================================

/// User with `start` but not `start:hardening` can start without --hardening flag
#[tokio::test]
async fn test_acl_start_without_hardening_subright_succeeds_without_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_start_subrights")?;

    harness.start_daemon().await?;

    // testuser1 loads config as owner, then stops it
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts without --hardening — should succeed
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// User with `start` but not `start:hardening` is denied when explicitly passing --hardening
#[tokio::test]
async fn test_acl_start_with_hardening_flag_denied_without_subright() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_start_subrights")?;

    harness.start_daemon().await?;

    // testuser1 loads config then stops it
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts with --hardening none — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "start", "-d", "--hardening", "none"]).await?;
    assert!(!output.success(), "start --hardening should be denied without start:hardening sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("start:hardening") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// User with `start` but not `start:env-override` is denied `-e KEY=VALUE`
#[tokio::test]
async fn test_acl_start_without_env_override_denies_e_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_start_subrights")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts with -e — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "start", "-d", "-e", "TEST_KEY=test_value"]).await?;
    assert!(!output.success(), "start -e should be denied without start:env-override sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("start:env-override") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// User with `start` but not `start:no-deps` is denied `start --no-deps`
#[tokio::test]
async fn test_acl_start_without_no_deps_denies_no_deps_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_start_subrights")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts with --no-deps — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "start", "-d", "--no-deps", "subrights-svc"]).await?;
    assert!(!output.success(), "start --no-deps should be denied without start:no-deps sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("start:no-deps") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Sub-right enforcement: restart:env-override, restart:no-deps
// =========================================================================

/// User with `restart` but not `restart:env-override` is denied `restart -e`
#[tokio::test]
async fn test_acl_restart_without_env_override_denies_e_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_restart_subrights")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 restarts with -e — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "restart", "--wait", "-e", "TEST_KEY=test_value"]).await?;
    assert!(!output.success(), "restart -e should be denied without restart:env-override sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("restart:env-override") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// User with `restart` but not `restart:no-deps` is denied `restart --no-deps`
#[tokio::test]
async fn test_acl_restart_without_no_deps_denies_no_deps_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_restart_subrights")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 restarts with --no-deps — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "restart", "--wait", "--no-deps", "subrights-svc"]).await?;
    assert!(!output.success(), "restart --no-deps should be denied without restart:no-deps sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("restart:no-deps") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Sub-right enforcement: recreate:hardening
// =========================================================================

/// User with `recreate` but not `recreate:hardening` is denied `recreate --hardening`
#[tokio::test]
async fn test_acl_recreate_without_hardening_denies_flag() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_recreate_subrights")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "subrights-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 recreates with --hardening — denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "recreate", "--hardening", "none"]).await?;
    assert!(!output.success(), "recreate --hardening should be denied without recreate:hardening sub-right");
    assert!(
        output.stderr_contains("sub-right") || output.stderr_contains("recreate:hardening") || output.stderr_contains("permission denied"),
        "Should mention sub-right denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Lua authorizer enforcement
// =========================================================================

/// Lua authorizer denies a specific action (stop) while allowing others (status)
#[tokio::test]
async fn test_lua_authorizer_denies_specific_action() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_lua_deny")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "lua-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 can view status (authorizer returns true for non-stop)
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    output.assert_success();
    assert!(output.stdout_contains("lua-svc"), "testuser2 should see service. stdout: {}", output.stdout);

    // testuser2 is denied stop (authorizer returns false for stop)
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    assert!(!output.success(), "stop should be denied by Lua authorizer");
    assert!(
        output.stderr_contains("permission denied") || output.stderr_contains("denied"),
        "Should mention denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Lua authorizer can use shared functions from kepler.acl.lua block
#[tokio::test]
async fn test_lua_authorizer_shared_acl_lua_functions() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_lua_shared")?;

    harness.start_daemon().await?;

    // testuser1 loads config (owner bypasses authorizers)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "lua-shared-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 is denied everything (deny_all() returns false)
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    assert!(!output.success(), "Should be denied by shared Lua function");
    assert!(
        output.stderr_contains("permission denied") || output.stderr_contains("denied"),
        "Should mention denial. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Lua authorizer error() denies request and the error is logged in daemon logs
#[tokio::test]
async fn test_lua_authorizer_error_denies_and_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_lua_error")?;

    harness.start_daemon().await?;

    // testuser1 loads config (owner bypasses authorizers)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "lua-error-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 triggers the authorizer error — request is denied
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    assert!(!output.success(), "Should be denied by Lua error()");
    assert!(
        output.stderr_contains("permission denied"),
        "CLI should show permission denied. stderr: {}", output.stderr
    );

    // The actual Lua error message is logged in daemon logs (not sent to CLI)
    let daemon_logs = harness.daemon_logs();
    assert!(
        daemon_logs.contains("not allowed during maintenance"),
        "Daemon logs should contain the Lua error message. daemon_logs: {}", daemon_logs
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// CLI degraded mode: partial permissions
// =========================================================================

/// Foreground start without `logs` right prints warning and continues.
/// Uses stderr file capture since the foreground CLI hangs and must be killed.
#[tokio::test]
async fn test_degraded_mode_foreground_start_without_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_degraded_nologs")?;

    harness.start_daemon().await?;

    // testuser1 loads then stops (so testuser2 can start it)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts foreground (no -d) — has start+stop+status but NOT logs.
    // The CLI will hang in Ctrl+C wait after printing the warning, so we redirect
    // stderr to a temp file and poll it, then kill the background process.
    use std::os::unix::fs::PermissionsExt;
    // Make temp dir traversable by testuser2 (same as run_cli_as_user does)
    std::fs::set_permissions(harness.state_dir(), std::fs::Permissions::from_mode(0o755))?;

    let stderr_file = harness.create_temp_file("degraded_stderr.txt", "")?;
    let stderr_path = stderr_file.to_str().unwrap().to_string();
    // Make file writable by testuser2
    std::fs::set_permissions(&stderr_file, std::fs::Permissions::from_mode(0o666))?;

    let daemon_path_env = format!("KEPLER_DAEMON_PATH={}", harness.state_dir().display());
    let config_str = config_path.to_str().unwrap().to_string();
    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();

    // Spawn in background with stderr redirected to file via shell
    let mut child = tokio::process::Command::new("sudo")
        .args([
            "-u", "testuser2",
            "sh", "-c",
            &format!("env {} '{}' -f '{}' start 2>'{}'",
                daemon_path_env, kepler_bin, config_str, stderr_path),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    // Wait for the warning to appear in the file (poll for up to 15s)
    let mut found = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(content) = std::fs::read_to_string(&stderr_file) {
            if content.contains("log streaming unavailable") || content.contains("permission denied") {
                found = true;
                break;
            }
        }
    }

    // Clean up: kill the background process
    let _ = child.kill().await;
    let _ = child.wait().await;

    let stderr_content = std::fs::read_to_string(&stderr_file).unwrap_or_default();
    assert!(
        found,
        "Should warn about log streaming. stderr file content: {}", stderr_content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `-q` flag suppresses degraded mode warnings in foreground start
#[tokio::test]
async fn test_degraded_mode_quiet_suppresses_warnings() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_degraded_nologs")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts foreground with -q — warnings suppressed.
    // Same file-based stderr capture approach as the non-quiet test.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(harness.state_dir(), std::fs::Permissions::from_mode(0o755))?;

    let stderr_file = harness.create_temp_file("quiet_stderr.txt", "")?;
    let stderr_path = stderr_file.to_str().unwrap().to_string();
    std::fs::set_permissions(&stderr_file, std::fs::Permissions::from_mode(0o666))?;

    let daemon_path_env = format!("KEPLER_DAEMON_PATH={}", harness.state_dir().display());
    let config_str = config_path.to_str().unwrap().to_string();
    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();

    let mut child = tokio::process::Command::new("sudo")
        .args([
            "-u", "testuser2",
            "sh", "-c",
            &format!("env {} '{}' -q -f '{}' start 2>'{}'",
                daemon_path_env, kepler_bin, config_str, stderr_path),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    // Wait enough time for warnings to be written (if any)
    tokio::time::sleep(Duration::from_secs(10)).await;

    let _ = child.kill().await;
    let _ = child.wait().await;

    let stderr_content = std::fs::read_to_string(&stderr_file).unwrap_or_default();
    // Should NOT contain "Warning:" in stderr when -q is used
    assert!(
        !stderr_content.contains("Warning:"),
        "-q should suppress warnings. stderr file content: {}", stderr_content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `restart --follow` without `logs` right prints warning and completes
#[tokio::test]
async fn test_degraded_mode_restart_follow_without_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_degraded_restart")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 restarts with --follow — has restart+status but NOT logs
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "restart", "--follow"]).await?;

    // Restart itself should succeed; log following should warn
    assert!(
        output.stderr_contains("log") && output.stderr_contains("unavailable")
            || output.stderr_contains("log") && output.stderr_contains("permission denied"),
        "Should warn about log following unavailable. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// `start -d --wait` works without `subscribe` right (uses inline follow events)
#[tokio::test]
async fn test_detach_wait_without_subscribe() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_degraded_nosubscribe")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "running", Duration::from_secs(10)).await?;
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "degraded-svc", "stopped", Duration::from_secs(10)).await?;

    // testuser2 starts with -d --wait — has start+stop+status but NOT subscribe.
    // With inline follow, this should succeed without needing subscribe permission.
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "start", "-d", "--wait"]).await?;

    // Should succeed with progress bars (no subscribe needed)
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Alias tests: `all` and user-defined
// =========================================================================

/// `allow: [all]` grants full access including sub-right-gated operations
#[tokio::test]
async fn test_acl_all_alias_grants_full_access() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_all_alias")?;

    harness.start_daemon().await?;

    // testuser1 loads config
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "all-svc", "running", Duration::from_secs(10)).await?;

    // testuser2 has `all` — can view status
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    output.assert_success();
    assert!(output.stdout_contains("all-svc"), "testuser2 should see service. stdout: {}", output.stdout);

    // testuser2 can read logs
    tokio::time::sleep(Duration::from_millis(500)).await;
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "logs", "--tail", "10"]).await?;
    output.assert_success();

    // testuser2 can stop --clean (has stop + stop:clean via all)
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop", "--clean"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// User-defined alias expands correctly — viewer can see status, logs, inspect but not start/stop
#[tokio::test]
async fn test_acl_user_defined_alias_expands() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_user_alias")?;

    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();
    harness.wait_for_service_status(&config_path, "alias-svc", "running", Duration::from_secs(10)).await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // testuser2 has viewer alias = [status, logs, inspect]
    // Can view status
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    output.assert_success();
    assert!(output.stdout_contains("alias-svc"), "Viewer should see service. stdout: {}", output.stdout);

    // Can view logs
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "logs", "--tail", "10"]).await?;
    output.assert_success();

    // Can inspect
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "inspect"]).await?;
    output.assert_success();

    // Cannot stop
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    assert!(!output.success(), "Viewer should not be able to stop");
    assert!(output.stderr_contains("permission denied"), "Should mention permission denied. stderr: {}", output.stderr);

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Config validation: invalid rights
// =========================================================================

/// Config with invalid rights is rejected at load time
#[tokio::test]
async fn test_config_rejects_invalid_rights_at_load() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_acl_invalid_rights")?;

    harness.start_daemon().await?;

    // Attempting to load a config with invalid rights should fail
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    assert!(!output.success(), "Config with invalid rights should fail to load");
    assert!(
        output.stderr_contains("invalid") || output.stderr_contains("unknown") || output.stderr_contains("invalid_right"),
        "Should mention invalid right. stderr: {}", output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Filesystem permission check: non-owner denied when lacking read access
// =========================================================================

/// Non-owner is denied when they don't have filesystem read access to the config file
#[tokio::test]
async fn test_non_owner_denied_without_filesystem_read() -> E2eResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1 (they become the owner)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // Make the config file readable only by root (chmod 600, already owned by root)
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))?;

    // testuser2 should be denied due to filesystem permission check
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    assert!(!output.success(), "Non-owner without filesystem read should be denied");
    assert!(
        output.stderr_contains("Permission denied") || output.stderr_contains("permission denied"),
        "Should mention permission denied. stderr: {}",
        output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Token-based service authorization
// =========================================================================

/// Wait for a result file to contain specific text, polling every 100ms.
async fn wait_for_file_content(path: &Path, expected: &str, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains(expected) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Service with `permissions: [status]` can call `kepler ps` using its token
#[tokio::test]
async fn test_token_service_can_call_status() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_status.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_status_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    let config_content = format!(
        r#"services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' ps > '{result_str}' 2>&1
        echo "EXIT_CODE=$?" >> '{result_str}'
        sleep 300
    permissions: [status]
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "EXIT_CODE=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        content.contains("EXIT_CODE=0"),
        "ps should succeed with status permission. Result: {}",
        content
    );
    assert!(
        content.contains("target"),
        "ps output should contain target service name. Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Service with `permissions: [status]` is denied `kepler stop`
#[tokio::test]
async fn test_token_scope_denies_unauthorized_stop() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_deny_stop.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_deny_stop_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    let config_content = format!(
        r#"services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' stop target > '{result_str}' 2>&1
        echo "EXIT_CODE=$?" >> '{result_str}'
        sleep 300
    permissions: [status]
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "EXIT_CODE=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        !content.contains("EXIT_CODE=0"),
        "stop should be denied with only status permission. Result: {}",
        content
    );
    assert!(
        content.to_lowercase().contains("permission denied") || content.to_lowercase().contains("missing required right"),
        "Should mention permission denied or missing right. Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Service with `permissions: [status, stop]` can call `kepler stop target`
#[tokio::test]
async fn test_token_service_can_stop_with_right() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_can_stop.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_can_stop_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    let config_content = format!(
        r#"services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' stop target 2>&1
        echo "EXIT_CODE=$?" > '{result_str}'
        sleep 300
    permissions: [status, stop]
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "EXIT_CODE=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        content.contains("EXIT_CODE=0"),
        "stop should succeed with stop permission. Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Token authorizer Lua script can deny specific actions
#[tokio::test]
async fn test_token_authorizer_denies_action() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_authorizer_deny.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_authorizer_deny_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    let config_content = format!(
        r#"services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' ps > '{result_str}' 2>&1
        echo "PS_EXIT=$?" >> '{result_str}'
        '{kepler_bin}' -f '{config_str}' stop target >> '{result_str}' 2>&1
        echo "STOP_EXIT=$?" >> '{result_str}'
        sleep 300
    permissions:
      allow: [status, stop]
      authorize: |
        if request.action == 'stop' then return false end
        return true
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "STOP_EXIT=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        content.contains("PS_EXIT=0"),
        "ps should succeed (authorizer allows non-stop). Result: {}",
        content
    );
    assert!(
        !content.contains("STOP_EXIT=0"),
        "stop should be denied by Lua authorizer. Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Token authorizer returning true cannot grant actions beyond token scope
#[tokio::test]
async fn test_token_authorizer_cannot_grant_beyond_scope() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_authorizer_no_escalate.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_authorizer_no_escalate_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    let config_content = format!(
        r#"services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' stop target > '{result_str}' 2>&1
        echo "EXIT_CODE=$?" >> '{result_str}'
        sleep 300
    permissions:
      allow: [status]
      authorize: "return true"
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "EXIT_CODE=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        !content.contains("EXIT_CODE=0"),
        "stop should be denied at check_rights despite authorizer returning true. Result: {}",
        content
    );
    assert!(
        content.to_lowercase().contains("permission denied") || content.to_lowercase().contains("missing required right"),
        "Should mention permission denied or missing right. Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Token narrows effective rights below ACL for non-root users
#[tokio::test]
async fn test_token_narrows_acl_rights() -> E2eResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_narrows_acl.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_narrows_acl_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    // Pre-create result file writable by testuser2
    std::fs::write(&result_file, "")?;
    std::fs::set_permissions(&result_file, std::fs::Permissions::from_mode(0o666))?;
    std::fs::set_permissions(harness.state_dir(), std::fs::Permissions::from_mode(0o755))?;

    // ACL grants testuser2: [status, stop, logs]
    // Token has permissions: [status, logs] (narrower)
    // Effective = token ∩ ACL = [status, logs]; stop should be denied
    let config_content = format!(
        r#"kepler:
  acl:
    users:
      testuser2:
        allow: [status, stop, logs]

services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' ps > '{result_str}' 2>&1
        echo "PS_EXIT=$?" >> '{result_str}'
        '{kepler_bin}' -f '{config_str}' stop target >> '{result_str}' 2>&1
        echo "STOP_EXIT=$?" >> '{result_str}'
        sleep 300
    user: testuser2
    permissions: [status, logs]
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "STOP_EXIT=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        content.contains("PS_EXIT=0"),
        "ps should succeed (status is in both token and ACL). Result: {}",
        content
    );
    assert!(
        !content.contains("STOP_EXIT=0"),
        "stop should be denied (in ACL but not in token). Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Broad token (all) cannot escalate beyond ACL for non-root users
#[tokio::test]
async fn test_token_cannot_escalate_beyond_acl() -> E2eResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_no_escalate_acl.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_no_escalate_acl_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    // Pre-create result file writable by testuser2
    std::fs::write(&result_file, "")?;
    std::fs::set_permissions(&result_file, std::fs::Permissions::from_mode(0o666))?;
    std::fs::set_permissions(harness.state_dir(), std::fs::Permissions::from_mode(0o755))?;

    // ACL grants testuser2 only [status]
    // Token has permissions: [all] (expands to every right)
    // Effective = [all] ∩ [status] = [status]; stop should be denied
    let config_content = format!(
        r#"kepler:
  acl:
    users:
      testuser2:
        allow: [status]

services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' ps > '{result_str}' 2>&1
        echo "PS_EXIT=$?" >> '{result_str}'
        '{kepler_bin}' -f '{config_str}' stop target >> '{result_str}' 2>&1
        echo "STOP_EXIT=$?" >> '{result_str}'
        sleep 300
    user: testuser2
    permissions: [all]
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "STOP_EXIT=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        content.contains("PS_EXIT=0"),
        "ps should succeed (status is in ACL). Result: {}",
        content
    );
    assert!(
        !content.contains("STOP_EXIT=0"),
        "stop should be denied (not in ACL despite broad token). Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// ACL authorizer returning true cannot grant rights beyond effective rights for token users
#[tokio::test]
async fn test_token_acl_authorizer_cannot_grant_beyond_rights() -> E2eResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut harness = E2eHarness::new().await?;

    let kepler_bin = harness.kepler_bin().to_str().unwrap().to_string();
    let config_dir = harness.state_dir().join("test_configs");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("token_acl_authorizer_no_grant.kepler.yaml");
    let config_str = config_path.to_str().unwrap().to_string();
    let result_file = harness.state_dir().join("token_acl_authorizer_no_grant_result.txt");
    let result_str = result_file.to_str().unwrap().to_string();

    // Pre-create result file writable by testuser2
    std::fs::write(&result_file, "")?;
    std::fs::set_permissions(&result_file, std::fs::Permissions::from_mode(0o666))?;
    std::fs::set_permissions(harness.state_dir(), std::fs::Permissions::from_mode(0o755))?;

    // ACL grants testuser2 only [status] with authorizer that always allows
    // Token has permissions: [all]
    // Effective = [all] ∩ [status] = [status]; stop denied at check_rights before authorizer runs
    let config_content = format!(
        r#"kepler:
  acl:
    users:
      testuser2:
        allow: [status]
        authorize: "return true"

services:
  target:
    command: ["sleep", "300"]
  controller:
    command:
      - "sh"
      - "-c"
      - |
        '{kepler_bin}' -f '{config_str}' stop target > '{result_str}' 2>&1
        echo "EXIT_CODE=$?" >> '{result_str}'
        sleep 300
    user: testuser2
    permissions: [all]
    depends_on:
      target:
        condition: service_started
"#
    );

    std::fs::write(&config_path, &config_content)?;

    harness.start_daemon().await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "controller", "running", Duration::from_secs(15))
        .await?;

    assert!(
        wait_for_file_content(&result_file, "EXIT_CODE=", Duration::from_secs(15)).await,
        "Controller should write result file"
    );

    let content = std::fs::read_to_string(&result_file).unwrap();
    assert!(
        !content.contains("EXIT_CODE=0"),
        "stop should be denied at check_rights despite ACL authorizer returning true. Result: {}",
        content
    );
    assert!(
        content.to_lowercase().contains("permission denied") || content.to_lowercase().contains("missing required right"),
        "Should mention permission denied or missing right. Result: {}",
        content
    );

    harness.stop_daemon().await?;
    Ok(())
}
