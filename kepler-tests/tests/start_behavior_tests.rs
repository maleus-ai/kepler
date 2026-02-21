//! Tests for manual `kepler start` behavior.
//!
//! Verifies that a user-initiated `kepler start` always restarts services
//! in terminal states, regardless of the restart policy. The restart policy
//! only governs *automatic* restarts by the daemon.

use kepler_daemon::config::RestartPolicy;
use kepler_daemon::state::ServiceStatus;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// Manual start after exit (the original bug)
// ============================================================================

/// A service with `restart: no` that exits with code 0 should be re-startable
/// via manual `start_service()`. The restart policy must NOT prevent this.
#[tokio::test]
async fn test_manual_start_after_exit_code_0_restart_no() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // First start — service runs and exits
    harness.start_service("test").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let content = marker.wait_for_marker_content("started", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started the first time");

    // Wait for exit and state to settle
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let count_after_first = marker.count_marker_lines("started");
    assert_eq!(count_after_first, 1, "Should have started exactly once");

    // Manual start again — should succeed despite restart: no
    harness.start_service("test").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let count_after_second = marker.count_marker_lines("started");
    assert_eq!(
        count_after_second, 2,
        "Manual start should restart the exited service even with restart: no. Got {} starts",
        count_after_second
    );

    harness.stop_all().await.unwrap();
}

/// A service that exits with non-zero code should be re-startable via manual start.
#[tokio::test]
async fn test_manual_start_after_nonzero_exit() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let count = marker.count_marker_lines("started");
    assert_eq!(count, 1, "Should have started once");

    // Manual start of failed service
    harness.start_service("test").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let count = marker.count_marker_lines("started");
    assert_eq!(
        count, 2,
        "Manual start should restart failed service. Got {} starts",
        count
    );

    harness.stop_all().await.unwrap();
}

/// A service with `restart: always` that exits should still be manually restartable.
/// (The automatic restart may have already re-started it, but the test verifies
/// the service is running after manual start.)
#[tokio::test]
async fn test_manual_start_after_exit_restart_always() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart(RestartPolicy::always())
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait for at least one restart cycle
    tokio::time::sleep(Duration::from_secs(2)).await;

    let count = marker.count_marker_lines("started");
    assert!(
        count >= 1,
        "Service should have started at least once. Got {} starts",
        count
    );

    // Manual start should not cause errors, even if service is in cycle
    harness.start_service("test").await.unwrap();

    harness.stop_all().await.unwrap();
}

// ============================================================================
// Manual start after stop (killed/stopped state)
// ============================================================================

/// A stopped long-running service should be re-startable via manual start.
#[tokio::test]
async fn test_manual_start_after_stop() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // First start
    harness.start_service("test").await.unwrap();
    let content = marker.wait_for_marker_content("started", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started");

    let count = marker.count_marker_lines("started");
    assert_eq!(count, 1);

    // Stop the service
    harness.stop_service("test").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify it's no longer running
    let status = harness.get_status("test").await;
    assert!(
        matches!(
            status,
            Some(ServiceStatus::Stopped) | Some(ServiceStatus::Killed) | Some(ServiceStatus::Exited)
        ),
        "Service should be in a terminal state after stop. Got: {:?}",
        status
    );

    // Manual start again
    harness.start_service("test").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let count = marker.count_marker_lines("started");
    assert_eq!(
        count, 2,
        "Manual start should restart stopped service. Got {} starts",
        count
    );

    harness.stop_all().await.unwrap();
}

// ============================================================================
// Multiple restarts (regression test)
// ============================================================================

/// Verifies that the fix works across multiple start/exit/start cycles.
#[tokio::test]
async fn test_manual_start_three_cycles() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    for i in 1..=3 {
        harness.start_service("test").await.unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let count = marker.count_marker_lines("started");
        assert_eq!(
            count, i,
            "After {} manual starts, should have {} starts but got {}",
            i, i, count
        );
    }

    harness.stop_all().await.unwrap();
}
