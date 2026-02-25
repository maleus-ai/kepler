//! E2E tests for prune + start functionality
//!
//! Tests that after pruning config state, starting services again works correctly,
//! including log functionality. This tests the fix for the bug where logs wouldn't
//! work after prune + start.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "prune_start_test";

/// Test that logs work after prune + start
///
/// This was the critical bug: after running `prune --force` and then `start`,
/// logs would fail to capture because the log actor wasn't properly re-initialized.
#[tokio::test]
async fn test_prune_then_start_logs_work() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_prune_then_start_logs_work_first")?;

    // Start the daemon
    harness.start_daemon().await?;

    // === FIRST RUN ===
    println!("=== First run: Starting services ===");

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to start and produce logs
    harness
        .wait_for_service_status(&config_path, "logger-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for logs to contain our marker
    harness.wait_for_log_content(&config_path, "FIRST_RUN_MARKER", Duration::from_secs(10)).await?;

    println!("=== First run: Logs verified ===");

    // Stop services (without clean)
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for stop to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // === PRUNE ===
    println!("=== Pruning state ===");

    // Verify state exists before prune
    assert!(
        harness.config_state_exists(&config_path),
        "State should exist before prune"
    );

    // Run prune with force (since daemon knows about this config)
    let output = harness.prune(true).await?;
    println!("Prune output: {}", output.stdout);

    // Give time for prune to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state was removed
    assert!(
        !harness.config_state_exists(&config_path),
        "State should be removed after prune"
    );

    println!("=== State pruned ===");

    // === SECOND RUN (THE CRITICAL TEST) ===
    println!("=== Second run: Starting services after prune ===");

    // Load the second config (with different marker) to the same location
    let second_config = harness.load_config(TEST_MODULE, "test_prune_then_start_logs_work_second")?;

    // Start services again
    let output = harness.start_services(&second_config).await?;
    output.assert_success();

    // Wait for service to start
    harness
        .wait_for_service_status(&second_config, "logger-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for logs to contain the second run marker - THIS IS THE CRITICAL TEST
    // The bug was that logs wouldn't work after prune
    harness.wait_for_log_content(&second_config, "SECOND_RUN_MARKER", Duration::from_secs(10)).await?;

    println!("=== Second run: Logs verified (BUG FIX WORKS!) ===");

    // Cleanup
    harness.stop_daemon().await?;

    Ok(())
}

/// Test that logs work correctly through multiple prune/start cycles
#[tokio::test]
async fn test_multiple_prune_start_cycles() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Start daemon
    harness.start_daemon().await?;

    for cycle in 1..=3 {
        println!("=== Cycle {} ===", cycle);

        let marker = format!("CYCLE_{}_MARKER", cycle);

        // Load config with marker replacement
        let config_path = harness.load_config_with_replacements(
            TEST_MODULE,
            "test_multiple_prune_start_cycles",
            &[("CYCLE_MARKER", &marker)],
        )?;

        // Start services
        let output = harness.start_services(&config_path).await?;
        output.assert_success();

        // Wait for service to start
        harness
            .wait_for_service_status(&config_path, "cyclic-logger", "running", Duration::from_secs(10))
            .await?;

        // Wait for logs to contain the cycle marker
        harness.wait_for_log_content(&config_path, &marker, Duration::from_secs(10)).await?;

        println!("Cycle {}: Logs verified", cycle);

        // Stop and prune
        harness.stop_services(&config_path).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        harness.prune(true).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that prune dry-run doesn't affect logs
#[tokio::test]
async fn test_prune_dry_run_preserves_state() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_prune_dry_run_preserves_state")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "preserved-service", "running", Duration::from_secs(10))
        .await?;

    // Stop services
    harness.stop_services(&config_path).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify state exists
    assert!(
        harness.config_state_exists(&config_path),
        "State should exist before dry-run"
    );

    // Run prune with dry-run
    let output = harness.prune_dry_run().await?;
    println!("Prune dry-run output: {}", output.stdout);

    // State should still exist
    assert!(
        harness.config_state_exists(&config_path),
        "State should still exist after dry-run prune"
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that logs are properly isolated between configs
#[tokio::test]
async fn test_logs_isolated_between_configs() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let config_path_a = harness.load_config(TEST_MODULE, "test_logs_isolated_config_a")?;
    let config_path_b = harness.load_config(TEST_MODULE, "test_logs_isolated_config_b")?;

    harness.start_daemon().await?;

    // Start both configs
    harness.start_services(&config_path_a).await?.assert_success();
    harness.start_services(&config_path_b).await?.assert_success();

    // Wait for services
    harness
        .wait_for_service_status(&config_path_a, "service-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path_b, "service-b", "running", Duration::from_secs(10))
        .await?;

    // Wait for each config's marker to appear
    harness.wait_for_log_content(&config_path_a, "CONFIG_A_MARKER", Duration::from_secs(10)).await?;
    harness.wait_for_log_content(&config_path_b, "CONFIG_B_MARKER", Duration::from_secs(10)).await?;

    // Get logs for each config
    let logs_a = harness.get_logs(&config_path_a, None, 100).await?;
    let logs_b = harness.get_logs(&config_path_b, None, 100).await?;

    // Verify isolation: A's logs shouldn't have B's marker and vice versa
    assert!(
        logs_a.stdout_contains("CONFIG_A_MARKER"),
        "Config A should have its marker"
    );
    assert!(
        !logs_a.stdout_contains("CONFIG_B_MARKER"),
        "Config A should NOT have config B's marker"
    );

    assert!(
        logs_b.stdout_contains("CONFIG_B_MARKER"),
        "Config B should have its marker"
    );
    assert!(
        !logs_b.stdout_contains("CONFIG_A_MARKER"),
        "Config B should NOT have config A's marker"
    );

    // Stop config A and verify B still works (without prune)
    harness.stop_services(&config_path_a).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Config B's logs should still work
    let logs_b = harness.get_logs(&config_path_b, None, 100).await?;
    assert!(
        logs_b.stdout_contains("CONFIG_B_MARKER"),
        "Config B logs should still work after stopping config A. stdout: {}\nstderr: {}",
        logs_b.stdout,
        logs_b.stderr
    );

    harness.stop_daemon().await?;

    Ok(())
}
