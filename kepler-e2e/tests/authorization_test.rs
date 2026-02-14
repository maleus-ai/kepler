//! E2E authorization tests for per-request access control.
//!
//! Tests the three-tier authorization model:
//! - Tier 3 (root-only): Shutdown, Prune
//! - Tier 2 (owner): Stop, Restart, Logs, Status(specific)
//! - Tier 2-filter: Status(all), ListConfigs

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "authorization_test";

// =========================================================================
// Tier 3: root-only operations
// =========================================================================

/// Non-root user cannot shut down the daemon
#[tokio::test]
async fn test_non_root_cannot_shutdown() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["daemon", "stop"]).await?;
    assert!(!output.success(), "Non-root shutdown should fail");
    assert!(
        output.stderr.contains("Permission denied") || output.stdout.contains("Permission denied"),
        "Should contain 'Permission denied', got stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    // Daemon should still be running
    let status = harness.run_cli(&["daemon", "status"]).await?;
    status.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-root user cannot prune
#[tokio::test]
async fn test_non_root_cannot_prune() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["prune"]).await?;
    assert!(!output.success(), "Non-root prune should fail");
    assert!(
        output.stderr.contains("Permission denied") || output.stdout.contains("Permission denied"),
        "Should contain 'Permission denied', got stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Tier 2: owner-restricted write operations
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

/// Non-owner cannot stop someone else's config
#[tokio::test]
async fn test_non_owner_cannot_stop_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1 (they become the owner)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 cannot stop testuser1's config
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    assert!(!output.success(), "Non-owner stop should fail");
    assert!(
        output.stderr.contains("Permission denied") || output.stdout.contains("Permission denied"),
        "Should contain 'Permission denied', got stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    // Service should still be running
    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(5)).await?;

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
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "restart", "-d", "--wait"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

/// Non-owner cannot restart someone else's config
#[tokio::test]
async fn test_non_owner_cannot_restart_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 cannot restart testuser1's config
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "restart", "-d"]).await?;
    assert!(!output.success(), "Non-owner restart should fail");
    assert!(
        output.stderr.contains("Permission denied") || output.stdout.contains("Permission denied"),
        "Should contain 'Permission denied', got stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Tier 2: owner-restricted read operations
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

/// Non-owner cannot read logs of someone else's config
#[tokio::test]
async fn test_non_owner_cannot_read_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 cannot read testuser1's logs
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "logs", "--tail", "10"]).await?;
    assert!(!output.success(), "Non-owner logs should fail");
    assert!(
        output.stderr.contains("Permission denied") || output.stdout.contains("Permission denied"),
        "Should contain 'Permission denied', got stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

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

/// Non-owner cannot view status of someone else's config
#[tokio::test]
async fn test_non_owner_cannot_view_status() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 cannot view testuser1's status
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    assert!(!output.success(), "Non-owner status should fail");
    assert!(
        output.stderr.contains("Permission denied") || output.stdout.contains("Permission denied"),
        "Should contain 'Permission denied', got stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Tier 2-filter: listing operations filtered by ownership
// =========================================================================

/// List configs shows only owned configs (non-root)
#[tokio::test]
async fn test_list_configs_filtered_by_owner() -> E2eResult<()> {
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

    // testuser1 lists configs — should only see config_a
    // `daemon status` uses ListConfigs which is filtered by ownership
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

/// Status (all configs) filtered by ownership
#[tokio::test]
async fn test_status_all_filtered_by_owner() -> E2eResult<()> {
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

    // testuser1 views all-config status — should only see their services
    // `ps --all` uses Status(None) which is filtered by ownership
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
