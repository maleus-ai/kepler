//! E2E authorization tests for per-request access control.
//!
//! Authorization model: any kepler group member can perform all operations.
//! No ownership checks, no root-only tier.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "authorization_test";

// =========================================================================
// Any kepler group member can perform operations (no root-only tier)
// =========================================================================

/// Non-root user can shut down the daemon
#[tokio::test]
async fn test_non_root_can_shutdown() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["daemon", "stop"]).await?;
    output.assert_success();

    Ok(())
}

/// Non-root user can prune
#[tokio::test]
async fn test_non_root_can_prune() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    harness.start_daemon().await?;

    let output = harness.run_cli_as_user("testuser1", &["prune"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Any kepler group member can perform write operations on any config
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

/// Any kepler group member can stop any config
#[tokio::test]
async fn test_any_user_can_stop_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1 (they become the owner)
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 can stop testuser1's config
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "stop"]).await?;
    output.assert_success();

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

/// Any kepler group member can restart any config
#[tokio::test]
async fn test_any_user_can_restart_config() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 can restart testuser1's config
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "restart", "--wait"]).await?;
    output.assert_success();

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Any kepler group member can perform read operations on any config
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

/// Any kepler group member can read logs of any config
#[tokio::test]
async fn test_any_user_can_read_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // Wait for log output
    tokio::time::sleep(Duration::from_millis(500)).await;

    // testuser2 can read testuser1's logs
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "logs", "--tail", "10"]).await?;
    output.assert_success();

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

/// Any kepler group member can view status of any config
#[tokio::test]
async fn test_any_user_can_view_status() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_config_a")?;

    harness.start_daemon().await?;

    // Start as testuser1
    let output = harness.run_cli_as_user("testuser1", &["-f", config_path.to_str().unwrap(), "start", "-d"]).await?;
    output.assert_success();

    harness.wait_for_service_status(&config_path, "auth-svc-a", "running", Duration::from_secs(10)).await?;

    // testuser2 can view testuser1's status
    let output = harness.run_cli_as_user("testuser2", &["-f", config_path.to_str().unwrap(), "ps"]).await?;
    output.assert_success();
    assert!(output.stdout_contains("auth-svc-a"), "Should see service from other user");

    harness.stop_daemon().await?;
    Ok(())
}

// =========================================================================
// Listing operations show all configs to all kepler group members
// =========================================================================

/// List configs shows all configs to any kepler group member
#[tokio::test]
async fn test_list_configs_shows_all() -> E2eResult<()> {
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

    // testuser1 lists configs — should see both
    let output = harness.run_cli_as_user("testuser1", &["daemon", "status"]).await?;
    output.assert_success();
    assert!(
        output.stdout_contains("test_config_a"),
        "testuser1 should see config_a. stdout: {}",
        output.stdout
    );
    assert!(
        output.stdout_contains("test_config_b"),
        "testuser1 should see config_b. stdout: {}",
        output.stdout
    );

    // Root lists configs — should also see both
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

/// Status (all configs) shows all services to any kepler group member
#[tokio::test]
async fn test_status_all_shows_all() -> E2eResult<()> {
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

    // testuser1 views all-config status — should see all services
    let output = harness.run_cli_as_user("testuser1", &["ps", "--all"]).await?;
    output.assert_success();
    assert!(
        output.stdout_contains("auth-svc-a"),
        "testuser1 should see auth-svc-a. stdout: {}",
        output.stdout
    );
    assert!(
        output.stdout_contains("auth-svc-b"),
        "testuser1 should see auth-svc-b. stdout: {}",
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
