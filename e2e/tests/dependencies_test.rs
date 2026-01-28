//! E2E tests for service dependency ordering
//!
//! Tests that services with dependencies start correctly.
//! Note: Log output order may not match start order due to sorting by service name.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "dependencies_test";

/// Test simple dependency: B depends on A, A starts first
#[tokio::test]
async fn test_simple_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_simple_dependency")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for both to be running
    harness
        .wait_for_service_status(&config_path, "service-a", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "service-b", "running", Duration::from_secs(10))
        .await?;

    // Check logs to verify both started and order is correct
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    // Both markers should be present
    assert!(
        logs.stdout_contains("SERVICE_A_STARTED"),
        "Service A should have logged its start marker. stdout: {}",
        logs.stdout
    );
    assert!(
        logs.stdout_contains("SERVICE_B_STARTED"),
        "Service B should have logged its start marker. stdout: {}",
        logs.stdout
    );

    // A should start before B (check log order)
    let pos_a = logs.stdout.find("SERVICE_A_STARTED");
    let pos_b = logs.stdout.find("SERVICE_B_STARTED");
    assert!(
        pos_a < pos_b,
        "Service A should start before Service B. A pos: {:?}, B pos: {:?}",
        pos_a, pos_b
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test chain dependency: C → B → A (A starts first, then B, then C)
#[tokio::test]
async fn test_chain_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_chain_dependency")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["chain-a", "chain-b", "chain-c"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(15))
            .await?;
    }

    // Check logs - all should have started and in correct order
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    assert!(logs.stdout_contains("CHAIN_A_STARTED"), "Chain A should have started");
    assert!(logs.stdout_contains("CHAIN_B_STARTED"), "Chain B should have started");
    assert!(logs.stdout_contains("CHAIN_C_STARTED"), "Chain C should have started");

    // Verify order: A → B → C
    let pos_a = logs.stdout.find("CHAIN_A_STARTED");
    let pos_b = logs.stdout.find("CHAIN_B_STARTED");
    let pos_c = logs.stdout.find("CHAIN_C_STARTED");
    assert!(
        pos_a < pos_b && pos_b < pos_c,
        "Start order should be A → B → C. A pos: {:?}, B pos: {:?}, C pos: {:?}",
        pos_a, pos_b, pos_c
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test diamond dependency: D depends on B and C, both depend on A
/// A starts first, then B and C (either order), then D
#[tokio::test]
async fn test_diamond_dependency() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_diamond_dependency")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["diamond-a", "diamond-b", "diamond-c", "diamond-d"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(15))
            .await?;
    }

    // Check logs - all should have started
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;

    assert!(logs.stdout_contains("DIAMOND_A_STARTED"), "Diamond A should have started");
    assert!(logs.stdout_contains("DIAMOND_B_STARTED"), "Diamond B should have started");
    assert!(logs.stdout_contains("DIAMOND_C_STARTED"), "Diamond C should have started");
    assert!(logs.stdout_contains("DIAMOND_D_STARTED"), "Diamond D should have started");

    // Verify order: A first, then B and C (either order), then D last
    let pos_a = logs.stdout.find("DIAMOND_A_STARTED");
    let pos_b = logs.stdout.find("DIAMOND_B_STARTED");
    let pos_c = logs.stdout.find("DIAMOND_C_STARTED");
    let pos_d = logs.stdout.find("DIAMOND_D_STARTED");

    // A must be before B and C
    assert!(
        pos_a < pos_b && pos_a < pos_c,
        "A should start before B and C. A pos: {:?}, B pos: {:?}, C pos: {:?}",
        pos_a, pos_b, pos_c
    );
    // D must be after B and C
    assert!(
        pos_d > pos_b && pos_d > pos_c,
        "D should start after B and C. B pos: {:?}, C pos: {:?}, D pos: {:?}",
        pos_b, pos_c, pos_d
    );

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that dependent service and its dependency both stop
#[tokio::test]
async fn test_stop_with_dependencies() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_stop_order_reverse")?;

    harness.start_daemon().await?;

    // Start services
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for all to be running
    for svc in &["stop-order-a", "stop-order-b"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }

    // Verify both started (check logs before stop - logs are cleared on stop)
    harness.wait_for_logs(&config_path, Duration::from_secs(5)).await?;
    let logs = harness.get_logs(&config_path, None, 100).await?;
    assert!(logs.stdout_contains("STOP_ORDER_A_STARTED"), "A should have started");
    assert!(logs.stdout_contains("STOP_ORDER_B_STARTED"), "B should have started");

    // Stop services
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for both to stop
    for svc in &["stop-order-a", "stop-order-b"] {
        harness
            .wait_for_service_status(&config_path, svc, "stopped", Duration::from_secs(10))
            .await?;
    }

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that circular dependencies are detected as an error
#[tokio::test]
async fn test_cycle_detection() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_cycle_detection")?;

    harness.start_daemon().await?;

    // Try to start services - should fail due to cycle
    let output = harness.start_services(&config_path).await?;

    // Should fail with an error about circular dependency
    assert!(
        !output.success() || output.stderr_contains("cycle") || output.stderr_contains("circular"),
        "Should detect circular dependency. exit_code: {}, stderr: {}",
        output.exit_code, output.stderr
    );

    harness.stop_daemon().await?;

    Ok(())
}
