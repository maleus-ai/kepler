//! E2E tests for the --timeout flag
//!
//! Tests that `kepler start -d --wait --timeout` returns after the specified
//! duration even if services are not yet ready.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::{Duration, Instant};

const TEST_MODULE: &str = "timeout_test";

/// Test that --timeout flag causes early exit when startup takes too long.
///
/// Uses two services: `never-ready` has a healthcheck that always fails, and
/// `slow-service` depends on `never-ready` with `condition: service_healthy`.
/// Since the dependency is never satisfied, the WaitStartup mode blocks
/// in `wait_for_dependencies`. The --timeout 2s flag should cause the CLI
/// to exit after ~2s with a non-zero exit code.
#[tokio::test]
async fn test_timeout_flag_expiry() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_timeout_flag_expiry")?;

    harness.start_daemon().await?;

    let start = Instant::now();

    // Start with a 2s timeout — the dependency is never satisfied, so it will time out
    let output = harness
        .run_cli_with_timeout(
            &[
                "-f",
                config_path.to_str().unwrap(),
                "start",
                "-d",
                "--wait",
                "--timeout",
                "2s",
            ],
            Duration::from_secs(15),
        )
        .await?;

    let elapsed = start.elapsed();

    // Should have returned due to timeout (non-zero exit code)
    assert!(
        !output.success(),
        "Command should fail due to timeout, but got exit code {}. stdout: {}, stderr: {}",
        output.exit_code, output.stdout, output.stderr
    );

    // Should have returned reasonably close to the timeout duration (within 5s)
    assert!(
        elapsed < Duration::from_secs(10),
        "Command should have returned after ~2s timeout, but took {:?}",
        elapsed
    );

    // Stderr should mention timeout
    assert!(
        output.stderr_contains("imeout") || output.stderr_contains("timeout"),
        "Stderr should mention timeout. stderr: {}",
        output.stderr
    );

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;

    Ok(())
}
