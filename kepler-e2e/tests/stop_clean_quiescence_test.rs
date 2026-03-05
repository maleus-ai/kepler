//! E2E test: foreground `kepler start` must not hang when `kepler stop --clean`
//! is called from another terminal.
//!
//! When `stop --clean` is called, it stops all services and then unloads the
//! config from the daemon. The foreground `kepler start` polls for quiescence
//! via `CheckQuiescence`, which must return true when the config is no longer
//! loaded (since there are no services left to wait on).

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "stop_clean_quiescence_test";

/// Test that a foreground `kepler start` exits cleanly when `kepler stop --clean`
/// is called concurrently. Previously, `stop --clean` would unload the config,
/// causing `CheckQuiescence` to return an error instead of true, making the
/// foreground CLI poll forever.
#[tokio::test]
async fn test_foreground_start_exits_on_stop_clean() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path =
        harness.load_config(TEST_MODULE, "test_foreground_start_exits_on_stop_clean")?;

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

    // The foreground start must still be alive at this point
    let status = fg_child.try_wait().expect("try_wait failed");
    assert!(
        status.is_none(),
        "Foreground start should still be alive before stop --clean"
    );

    // Stop services with --clean — this unloads the config from the daemon
    let output = harness.stop_services_clean(&config_path).await?;
    output.assert_success();

    // The foreground start must exit within a reasonable timeout.
    // Before the fix, this would hang forever because CheckQuiescence
    // returned an error when the config was unloaded.
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
        "Foreground start did not exit within 15s after stop --clean (hung due to quiescence bug)"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Same scenario but with multiple services including dependencies, to verify
/// quiescence works correctly when stop --clean removes a multi-service config.
#[tokio::test]
async fn test_foreground_start_exits_on_stop_clean_multiple_services() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(
        TEST_MODULE,
        "test_foreground_start_exits_on_stop_clean_multiple_services",
    )?;

    harness.start_daemon().await?;

    // Start services in detached mode first
    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "web", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_service_status(&config_path, "worker", "running", Duration::from_secs(10))
        .await?;

    // Spawn `kepler start` (foreground) as a background process
    let config_str = config_path.to_str().unwrap().to_string();
    let mut fg_child = harness.spawn_cli_background(&["-f", &config_str, "start"])?;

    // Give the foreground start time to connect and subscribe to events
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The foreground start must still be alive
    let status = fg_child.try_wait().expect("try_wait failed");
    assert!(
        status.is_none(),
        "Foreground start should still be alive before stop --clean"
    );

    // Stop services with --clean
    let output = harness.stop_services_clean(&config_path).await?;
    output.assert_success();

    // The foreground start must exit within a reasonable timeout
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
        "Foreground start did not exit within 15s after stop --clean (hung due to quiescence bug)"
    );

    harness.stop_daemon().await?;
    Ok(())
}
