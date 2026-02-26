//! E2E authorization tests for per-request access control.
//!
//! Authorization model: without ACL, only the config owner and root have access.
//! Non-owner kepler group members are denied unless an explicit ACL grants them scopes.

use kepler_e2e::{E2eHarness, E2eResult};
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

    // testuser2 has config:status in ACL — should be able to view status
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

    // testuser2 has service:logs in ACL — should be able to view logs
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

    // testuser2 has config:status + service:logs but NOT service:stop
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
