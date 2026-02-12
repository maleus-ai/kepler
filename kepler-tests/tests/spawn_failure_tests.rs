//! Tests for spawn failure cleanup behavior
//!
//! Verifies that when a service fails to start (e.g. nonexistent working directory),
//! the config and service state are properly cleaned up so that retrying does not hang.

use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
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

/// When a service has a nonexistent working directory, the first start should fail.
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
    let (orchestrator, _registry, _exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: std::collections::HashMap<String, String> = std::env::vars().collect();

    // First start should fail with a spawn error
    let result = orchestrator
        .start_services(&config_path, None, Some(sys_env.clone()))
        .await;
    assert!(result.is_err(), "First start should fail due to nonexistent working directory");

    // Second start must complete (fail or succeed) within a reasonable time — NOT hang.
    let result2 = tokio::time::timeout(
        Duration::from_secs(5),
        orchestrator.start_services(&config_path, None, Some(sys_env.clone())),
    )
    .await;

    assert!(
        result2.is_ok(),
        "Second start should complete within timeout (not hang)"
    );
    // It should still fail (the directory still doesn't exist)
    assert!(
        result2.unwrap().is_err(),
        "Second start should still fail since directory doesn't exist"
    );
}

/// When a config fails to start any services, it should be cleaned up from the registry
/// so that a fresh retry re-loads the config from scratch.
#[tokio::test]
async fn test_failed_config_is_cleaned_from_registry() {
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

    // Start should fail
    let _result = orchestrator
        .start_services(&config_path, None, Some(sys_env.clone()))
        .await;

    // Config should have been cleaned up from the registry
    assert!(
        registry.get(&config_path).is_none(),
        "Failed config should be removed from registry"
    );
}
