// MutexGuard held across await is intentional for env safety in tests
#![allow(clippy::await_holding_lock)]
//! KEPLER_SOCKET_PATH injection tests
//!
//! Tests that:
//! - Services with `permissions` receive KEPLER_SOCKET_PATH
//! - Services without `permissions` do NOT receive KEPLER_SOCKET_PATH
//! - Hooks of services with `permissions` receive KEPLER_SOCKET_PATH
//! - Stale KEPLER_SOCKET_PATH does not leak from the caller environment
//! - KEPLER_SOCKET_PATH matches the daemon's resolved socket path

use kepler_daemon::config::{HookList, ServiceHooks};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHookBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::{TestDaemonHarness, ENV_LOCK};
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

/// Compute the socket path the harness would use for a given config_dir.
/// Mirrors the harness: KEPLER_DAEMON_PATH = config_dir/.kepler → socket = .kepler/kepler.sock
fn expected_socket_path(config_dir: &Path) -> String {
    config_dir
        .join(".kepler")
        .join("kepler.sock")
        .to_string_lossy()
        .into_owned()
}

/// Service with `permissions` receives KEPLER_SOCKET_PATH in its environment
#[tokio::test]
async fn test_service_with_permissions_gets_socket_path() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("socket_path");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"SOCKET=${{KEPLER_SOCKET_PATH}}\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_permissions(vec!["start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("socket_path", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written marker");

    let content = marker.read_marker("socket_path").unwrap();
    let expected = expected_socket_path(temp_dir.path());
    assert!(
        content.contains(&format!("SOCKET={}", expected)),
        "Service with permissions should have KEPLER_SOCKET_PATH set to '{}'. Got: {}",
        expected,
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Service without `permissions` does NOT receive KEPLER_SOCKET_PATH
#[tokio::test]
async fn test_service_without_permissions_has_no_socket_path() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("no_socket");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"HAS_SOCKET=${{KEPLER_SOCKET_PATH:+yes}}\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("no_socket", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written marker");

    let content = marker.read_marker("no_socket").unwrap();
    assert!(
        !content.contains("HAS_SOCKET=yes"),
        "KEPLER_SOCKET_PATH should NOT be set for permissionless service. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// pre_start hook of a service with `permissions` receives KEPLER_SOCKET_PATH
#[tokio::test]
async fn test_hook_with_permissions_gets_socket_path() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("hook_socket");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script(&format!(
            "echo \"HOOK_SOCKET=${{KEPLER_SOCKET_PATH}}\" >> {}",
            marker_path.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec!["sleep".to_string(), "3600".to_string()])
                .with_hooks(hooks)
                .with_permissions(vec!["start"])
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("hook_socket", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Pre-start hook should have run");

    let content = marker.read_marker("hook_socket").unwrap();
    let expected = expected_socket_path(temp_dir.path());
    assert!(
        content.contains(&format!("HOOK_SOCKET={}", expected)),
        "Hook should have KEPLER_SOCKET_PATH set to '{}'. Got: {}",
        expected,
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Stale KEPLER_SOCKET_PATH from the caller environment does not leak into services
/// without permissions. The orchestrator strips it during env computation.
#[tokio::test]
async fn test_stale_socket_path_stripped_from_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("stale_stripped");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"HAS_SOCKET=${{KEPLER_SOCKET_PATH:+yes}}\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .build(),
        )
        .build();

    // Set a stale KEPLER_SOCKET_PATH in the environment before creating the harness
    let harness = {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("KEPLER_SOCKET_PATH", "/stale/path.sock") };
        let h = TestDaemonHarness::new_with_env_lock_held(config, temp_dir.path())
            .await
            .unwrap();
        unsafe { std::env::remove_var("KEPLER_SOCKET_PATH") };
        h
    };

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("stale_stripped", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written marker");

    let content = marker.read_marker("stale_stripped").unwrap();
    assert!(
        !content.contains("HAS_SOCKET=yes"),
        "Stale KEPLER_SOCKET_PATH should be stripped for permissionless service. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}
