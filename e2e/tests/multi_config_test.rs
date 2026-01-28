//! E2E tests for multiple config files running independently
//!
//! Tests that multiple configs can run with isolated state.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "multi_config_test";

/// Test that two configs can run independently
#[tokio::test]
async fn test_two_configs_independent() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_a = harness.load_config(TEST_MODULE, "test_two_configs_independent_a")?;
    let config_b = harness.load_config(TEST_MODULE, "test_two_configs_independent_b")?;

    harness.start_daemon().await?;

    // Start both configs
    let output_a = harness.start_services(&config_a).await?;
    output_a.assert_success();

    let output_b = harness.start_services(&config_b).await?;
    output_b.assert_success();

    // Wait for both to be running
    harness
        .wait_for_service_status(&config_a, "config-a-service", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_b, "config-b-service", "running", Duration::from_secs(10))
        .await?;

    // Both should have their own logs
    harness.wait_for_log_content(&config_a, "CONFIG_A_RUNNING", Duration::from_secs(5)).await?;
    harness.wait_for_log_content(&config_b, "CONFIG_B_RUNNING", Duration::from_secs(5)).await?;

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that logs are isolated per config
#[tokio::test]
async fn test_configs_isolated_logs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_a = harness.load_config(TEST_MODULE, "test_configs_isolated_logs_a")?;
    let config_b = harness.load_config(TEST_MODULE, "test_configs_isolated_logs_b")?;

    harness.start_daemon().await?;

    // Start both configs
    harness.start_services(&config_a).await?.assert_success();
    harness.start_services(&config_b).await?.assert_success();

    // Wait for both to produce logs
    harness
        .wait_for_service_status(&config_a, "isolated-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_b, "isolated-b", "running", Duration::from_secs(10))
        .await?;

    harness.wait_for_logs(&config_a, Duration::from_secs(5)).await?;
    harness.wait_for_logs(&config_b, Duration::from_secs(5)).await?;

    // Get logs for each config
    let logs_a = harness.get_logs(&config_a, None, 100).await?;
    let logs_b = harness.get_logs(&config_b, None, 100).await?;

    // Config A logs should have A's marker but not B's
    assert!(logs_a.stdout_contains("ISOLATED_LOG_A"), "Config A should have its marker");
    assert!(!logs_a.stdout_contains("ISOLATED_LOG_B"), "Config A should NOT have B's marker");

    // Config B logs should have B's marker but not A's
    assert!(logs_b.stdout_contains("ISOLATED_LOG_B"), "Config B should have its marker");
    assert!(!logs_b.stdout_contains("ISOLATED_LOG_A"), "Config B should NOT have A's marker");

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that stopping one config doesn't affect the other
#[tokio::test]
async fn test_stop_one_keeps_other() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_a = harness.load_config(TEST_MODULE, "test_stop_one_keeps_other_a")?;
    let config_b = harness.load_config(TEST_MODULE, "test_stop_one_keeps_other_b")?;

    harness.start_daemon().await?;

    // Start both configs
    harness.start_services(&config_a).await?.assert_success();
    harness.start_services(&config_b).await?.assert_success();

    // Wait for both to be running
    harness
        .wait_for_service_status(&config_a, "stop-test-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_b, "stop-test-b", "running", Duration::from_secs(10))
        .await?;

    // Stop config A only
    harness.stop_services(&config_a).await?.assert_success();

    // Wait for A to stop
    harness
        .wait_for_service_status(&config_a, "stop-test-a", "stopped", Duration::from_secs(10))
        .await?;

    // Config B should still be running
    let ps_b = harness.ps(&config_b).await?;
    assert!(
        ps_b.stdout_contains("running"),
        "Config B should still be running after stopping A. stdout: {}",
        ps_b.stdout
    );

    harness.stop_daemon().await?;

    Ok(())
}
