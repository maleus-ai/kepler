//! E2E tests for service lifecycle operations
//!
//! Tests basic service start/stop/restart functionality.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "service_lifecycle_test";

/// Test starting a single service
#[tokio::test]
async fn test_start_single_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_single_service")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to be running
    harness
        .wait_for_service_status(&config_path, "simple-service", "running", Duration::from_secs(10))
        .await?;

    // Verify via ps
    let ps_output = harness.ps(&config_path).await?;
    ps_output.assert_success();
    assert!(
        ps_output.stdout_contains("simple-service") && ps_output.stdout_contains("running"),
        "Service should be listed as running. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test stopping a single service
#[tokio::test]
async fn test_stop_single_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_single_service")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "stoppable-service", "running", Duration::from_secs(10))
        .await?;

    // Stop the service
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for service to be stopped
    harness
        .wait_for_service_status(&config_path, "stoppable-service", "stopped", Duration::from_secs(10))
        .await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test restarting a service gets a new PID
#[tokio::test]
async fn test_restart_single_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_single_service")?;

    harness.start_daemon().await?;

    // Start the service
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "restartable-service", "running", Duration::from_secs(10))
        .await?;

    // Get the original PID
    let ps_output = harness.ps(&config_path).await?;
    let original_pid = harness.extract_pid_from_ps(&ps_output.stdout, "restartable-service");

    // Restart the service
    let output = harness.restart_service(&config_path, "restartable-service").await?;
    output.assert_success();

    // Wait for it to be running again
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness
        .wait_for_service_status(&config_path, "restartable-service", "running", Duration::from_secs(10))
        .await?;

    // Get the new PID
    let ps_output = harness.ps(&config_path).await?;
    let new_pid = harness.extract_pid_from_ps(&ps_output.stdout, "restartable-service");

    // PIDs should be different
    assert!(
        original_pid.is_some() && new_pid.is_some(),
        "Should be able to extract PIDs"
    );
    assert_ne!(
        original_pid, new_pid,
        "Service should have a new PID after restart. Original: {:?}, New: {:?}",
        original_pid, new_pid
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test starting multiple services together
#[tokio::test]
async fn test_start_all_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_start_all_services")?;

    harness.start_daemon().await?;

    // Start all services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all services to be running
    for service in &["service-alpha", "service-beta", "service-gamma"] {
        harness
            .wait_for_service_status(&config_path, service, "running", Duration::from_secs(10))
            .await?;
    }

    // Verify all are running
    let ps_output = harness.ps(&config_path).await?;
    ps_output.assert_success();
    for service in &["service-alpha", "service-beta", "service-gamma"] {
        assert!(
            ps_output.stdout_contains(service),
            "Service {} should be listed. stdout: {}",
            service, ps_output.stdout
        );
    }

    harness.stop_daemon().await?;

    Ok(())
}

/// Test stopping all services
#[tokio::test]
async fn test_stop_all_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_all_services")?;

    harness.start_daemon().await?;

    // Start all services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all services to be running
    for service in &["svc-one", "svc-two"] {
        harness
            .wait_for_service_status(&config_path, service, "running", Duration::from_secs(10))
            .await?;
    }

    // Stop all services
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be stopped
    for service in &["svc-one", "svc-two"] {
        harness
            .wait_for_service_status(&config_path, service, "stopped", Duration::from_secs(10))
            .await?;
    }

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that ps command shows all services with correct info
#[tokio::test]
async fn test_ps_shows_all_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_ps_shows_all_services")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for services to start
    for service in &["ps-service-a", "ps-service-b"] {
        harness
            .wait_for_service_status(&config_path, service, "running", Duration::from_secs(10))
            .await?;
    }

    // Run ps command
    let ps_output = harness.ps(&config_path).await?;
    ps_output.assert_success();

    // Verify output contains both services
    assert!(
        ps_output.stdout_contains("ps-service-a"),
        "ps output should contain ps-service-a. stdout: {}",
        ps_output.stdout
    );
    assert!(
        ps_output.stdout_contains("ps-service-b"),
        "ps output should contain ps-service-b. stdout: {}",
        ps_output.stdout
    );
    assert!(
        ps_output.stdout_contains("running"),
        "ps output should show running status. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
