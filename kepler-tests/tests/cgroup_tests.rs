//! cgroup v2 integration tests
//!
//! These tests verify that the daemon harness creates and cleans up cgroup
//! directories correctly during service start/stop. All tests are gated by
//! REQUIRE_CGROUPV2=1 (set in the test-cgroup Docker service).

use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::wait_utils::wait_for_running;
use std::time::Duration;
use tempfile::TempDir;

/// Skip helper: returns true if cgroup tests should run.
fn require_cgroupv2() -> bool {
    std::env::var("REQUIRE_CGROUPV2").as_deref() == Ok("1")
}

/// Start a service, verify its cgroup directory exists and contains its PID.
#[tokio::test]
async fn test_cgroup_created_on_spawn() {
    if !require_cgroupv2() {
        eprintln!("Skipping: REQUIRE_CGROUPV2 not set");
        return;
    }

    let temp_dir = TempDir::new().unwrap();
    let config = TestConfigBuilder::new()
        .add_service("cg-test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("cg-test").await.unwrap();
    wait_for_running(harness.handle(), "cg-test", Duration::from_secs(5))
        .await
        .unwrap();

    let config_hash = harness.handle().config_hash().to_string();
    let cgroup_path = kepler_unix::cgroup::service_cgroup_path(
        &std::path::PathBuf::from("/sys/fs/cgroup/kepler"),
        &config_hash,
        "cg-test",
    );

    assert!(cgroup_path.exists(), "cgroup directory should exist after spawn");

    let pids = kepler_unix::cgroup::enumerate_cgroup_pids(&cgroup_path);
    assert!(!pids.is_empty(), "cgroup should contain at least one PID");

    // Verify the PID in cgroup matches the service's PID
    let state = harness
        .handle()
        .get_service_state("cg-test")
        .await
        .expect("service state should exist");
    if let Some(pid) = state.pid {
        assert!(
            pids.contains(&pid),
            "cgroup should contain service PID {}, got {:?}",
            pid,
            pids,
        );
    }

    harness.stop_service("cg-test").await.unwrap();
}

/// Start then stop a service, verify its cgroup directory is cleaned up.
#[tokio::test]
async fn test_cgroup_cleanup_after_stop() {
    if !require_cgroupv2() {
        eprintln!("Skipping: REQUIRE_CGROUPV2 not set");
        return;
    }

    let temp_dir = TempDir::new().unwrap();
    let config = TestConfigBuilder::new()
        .add_service("cg-cleanup", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("cg-cleanup").await.unwrap();
    wait_for_running(harness.handle(), "cg-cleanup", Duration::from_secs(5))
        .await
        .unwrap();

    let config_hash = harness.handle().config_hash().to_string();
    let cgroup_path = kepler_unix::cgroup::service_cgroup_path(
        &std::path::PathBuf::from("/sys/fs/cgroup/kepler"),
        &config_hash,
        "cg-cleanup",
    );

    assert!(cgroup_path.exists(), "cgroup should exist while running");

    harness.stop_service("cg-cleanup").await.unwrap();

    assert!(
        !cgroup_path.exists(),
        "cgroup directory should be removed after stop",
    );
}
