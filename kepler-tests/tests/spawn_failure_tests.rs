//! Tests for spawn failure cleanup behavior
//!
//! Verifies that when a service fails to start (e.g. nonexistent working directory),
//! the config and service state are properly cleaned up so that retrying does not hang.

use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::ENV_LOCK;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Helper to create an orchestrator with a fresh registry for testing.
fn create_test_orchestrator(
    _temp_dir: &std::path::Path,
) -> (
    ServiceOrchestrator,
    Arc<ConfigRegistry>,
    mpsc::Receiver<ProcessExitEvent>,
    mpsc::Receiver<FileChangeEvent>,
) {
    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, exit_rx) = mpsc::channel(32);
    let (restart_tx, restart_rx) = mpsc::channel(32);
    let orchestrator = ServiceOrchestrator::new(registry.clone(), exit_tx, restart_tx);
    (orchestrator, registry, exit_rx, restart_rx)
}

/// Write a config to a file and return its path.
fn write_config(
    config: &kepler_daemon::config::KeplerConfig,
    dir: &std::path::Path,
) -> PathBuf {
    let path = dir.join("kepler.yaml");
    let yaml = serde_yaml::to_string(config).unwrap();
    std::fs::write(&path, &yaml).unwrap();
    path
}

/// When a service has a nonexistent working directory, the service should eventually fail.
/// A subsequent start must NOT hang — it should fail cleanly within a reasonable timeout.
#[tokio::test]
async fn test_start_with_nonexistent_working_dir_retries_without_hanging() {
    let temp_dir = TempDir::new().unwrap();

    // Set KEPLER_DAEMON_PATH for this test
    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "bad-workdir",
            TestServiceBuilder::long_running()
                .with_working_dir(PathBuf::from("/nonexistent/path/that/doesnt/exist"))
                .build(),
        )
        .build();

    let config_path = write_config(&config, temp_dir.path());
    let (orchestrator, registry, _exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: std::collections::HashMap<String, String> = std::env::vars().collect();

    // First start — spawns the service which fails asynchronously
    let _result = orchestrator
        .start_services(&config_path, None, Some(sys_env.clone()), None, None)
        .await;

    // Wait for the service to reach Failed status
    let handle = registry.get(&config_path).expect("Config should be loaded");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = handle.get_service_state("bad-workdir").await {
            if state.status == ServiceStatus::Failed {
                break;
            }
        }
        if tokio::time::Instant::now() > deadline {
            panic!("Service should have reached Failed status within timeout");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Second start must complete (fail or succeed) within a reasonable time — NOT hang.
    let result2 = tokio::time::timeout(
        Duration::from_secs(5),
        orchestrator.start_services(&config_path, None, Some(sys_env.clone()), None, None),
    )
    .await;

    assert!(
        result2.is_ok(),
        "Second start should complete within timeout (not hang)"
    );
}

/// When a config fails to start services, it should remain in the registry
/// so that users can inspect logs and service status after a failed start.
#[tokio::test]
async fn test_failed_config_remains_in_registry() {
    let temp_dir = TempDir::new().unwrap();

    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "bad-workdir",
            TestServiceBuilder::long_running()
                .with_working_dir(PathBuf::from("/nonexistent/path/that/doesnt/exist"))
                .build(),
        )
        .build();

    let config_path = write_config(&config, temp_dir.path());
    let (orchestrator, registry, _exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: std::collections::HashMap<String, String> = std::env::vars().collect();

    // Start — spawns service which fails asynchronously
    let _result = orchestrator
        .start_services(&config_path, None, Some(sys_env.clone()), None, None)
        .await;

    // Config should be in the registry immediately after start_services returns
    assert!(
        registry.get(&config_path).is_some(),
        "Config should remain in registry for diagnostic access"
    );

    // Wait for the service to reach Failed status
    let handle = registry.get(&config_path).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = handle.get_service_state("bad-workdir").await {
            if state.status == ServiceStatus::Failed {
                break;
            }
        }
        if tokio::time::Instant::now() > deadline {
            panic!("Service should have reached Failed status within timeout");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Config should still be in the registry after failure
    assert!(
        registry.get(&config_path).is_some(),
        "Failed config should remain in registry for diagnostic access"
    );
}
