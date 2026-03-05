//! E2E test: foreground `kepler start` must not exit prematurely during restart.
//!
//! When `kepler start` (foreground) follows logs and `kepler restart` is called
//! from another terminal, the foreground start should remain alive. Previously,
//! the CLI's fallback status poll could see all services as terminal during the
//! restart stop-phase and exit early.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "restart_quiescence_test";

/// Test that a foreground `kepler start` does not exit when `kepler restart`
/// is called concurrently. Repeats 3 restarts to exercise the race window.
#[tokio::test]
async fn test_restart_no_premature_exit() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_restart_no_premature_exit")?;

    harness.start_daemon().await?;

    // Start services in detached mode first
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Spawn `kepler start` (foreground) as a background process
    let config_str = config_path.to_str().unwrap().to_string();
    let mut fg_child = harness.spawn_cli_background(&["-f", &config_str, "start"])?;

    // Give the foreground start time to connect and subscribe to events
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Perform 3 restarts — each one briefly puts services in terminal state
    for i in 1..=3 {
        let output = harness.restart_services(&config_path).await?;
        output.assert_success();

        // Wait for worker to be running again after restart
        harness
            .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(15))
            .await?;

        // The foreground start must still be alive
        let status = fg_child.try_wait().expect("try_wait failed");
        assert!(
            status.is_none(),
            "Foreground start exited prematurely after restart #{i}: {:?}",
            status
        );
    }

    // Stop services — this should cause the foreground start to exit
    let output = harness.stop_services(&config_path).await?;
    output.assert_success();

    // Wait for the foreground start to exit (with timeout)
    let exit = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match fg_child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(e) => panic!("Error waiting for foreground start: {e}"),
            }
        }
    })
    .await;

    assert!(
        exit.is_ok(),
        "Foreground start did not exit within 15s after stop"
    );

    harness.stop_daemon().await?;
    Ok(())
}
