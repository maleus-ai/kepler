// MutexGuard held across await is intentional for env safety in tests
#![allow(clippy::await_holding_lock)]
//! Tests that service and hook errors are written to stderr logs.
//!
//! When a service fails to start (e.g. Lua error in config) or a hook fails,
//! the error message should appear in the service's stderr log file so that
//! it's visible via `kepler logs`, not only via `kepler ps`.

use kepler_daemon::config::{HookCommand, HookList, KeplerConfig, ServiceHooks};
use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::logs::{LogReader, LogStream};
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::ENV_LOCK;
use std::collections::HashMap;
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

/// Write a raw YAML config to a file and return its path.
fn write_raw_config(yaml: &str, dir: &std::path::Path) -> PathBuf {
    let path = dir.join("kepler.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Write a typed config to a file and return its path.
fn write_config(config: &KeplerConfig, dir: &std::path::Path) -> PathBuf {
    let path = dir.join("kepler.yaml");
    let yaml = serde_yaml::to_string(config).unwrap();
    std::fs::write(&path, &yaml).unwrap();
    path
}

/// Wait for a service to reach the expected status within a timeout.
async fn wait_for_status(
    handle: &kepler_daemon::config_actor::ConfigActorHandle,
    service_name: &str,
    expected: ServiceStatus,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(state) = handle.get_service_state(service_name).await {
            if state.status == expected {
                return true;
            }
        }
        if tokio::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// When a service's config has a Lua error in a ${{ }}$ expression,
/// the error should be written to the service's stderr log file.
#[tokio::test]
async fn test_service_lua_error_logged_to_stderr() {
    let temp_dir = TempDir::new().unwrap();

    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    // Use a ${{ }}$ expression that will fail at resolution time
    let yaml = r#"
services:
  test:
    command:
      - echo
      - ${{ error("intentional test error for stderr logging") }}$
"#;

    let config_path = write_raw_config(yaml, temp_dir.path());
    let (orchestrator, registry, _exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Start the service — resolution should fail due to Lua error
    let _result = orchestrator
        .start_services(&config_path, None, Some(sys_env), None, None)
        .await;

    // Wait for the service to reach Failed status
    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Failed, Duration::from_secs(5)).await,
        "Service should reach Failed status"
    );

    // Read the service's stderr log — the error should be there
    let log_config = handle.get_log_config().await.expect("Log config should exist");
    let reader = LogReader::new(log_config.logs_dir);
    let entries = reader.tail(100, Some("test"), true);
    let stderr_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.stream == LogStream::Stderr)
        .collect();

    assert!(
        !stderr_entries.is_empty(),
        "Service stderr log should contain at least one entry"
    );

    let has_error = stderr_entries
        .iter()
        .any(|e| e.line.contains("intentional test error for stderr logging"));
    assert!(
        has_error,
        "Service stderr log should contain the Lua error message. Got entries: {:?}",
        stderr_entries.iter().map(|e| &e.line).collect::<Vec<_>>()
    );
}

/// When a hook fails (non-zero exit code), the error should be written
/// to the service's stderr log file.
#[tokio::test]
async fn test_hook_failure_logged_to_stderr() {
    let temp_dir = TempDir::new().unwrap();

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(
            "echo 'hook stderr output' >&2 && exit 1",
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let config_path = write_config(&config, temp_dir.path());

    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    let (orchestrator, registry, _exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Start the service — pre_start hook should fail
    let _result = orchestrator
        .start_services(&config_path, None, Some(sys_env), None, None)
        .await;

    // Wait for the service to reach Failed status
    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Failed, Duration::from_secs(5)).await,
        "Service should reach Failed status due to hook failure"
    );

    // Give a moment for logs to be flushed
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Read the service's stderr log — should contain the hook failure error
    let log_config = handle.get_log_config().await.expect("Log config should exist");
    let reader = LogReader::new(log_config.logs_dir);
    let entries = reader.tail(100, Some("test"), true);
    let stderr_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.stream == LogStream::Stderr)
        .collect();

    assert!(
        !stderr_entries.is_empty(),
        "Service stderr log should contain at least one entry after hook failure"
    );

    let has_hook_error = stderr_entries
        .iter()
        .any(|e| e.line.contains("Hook") && e.line.contains("failed"));
    assert!(
        has_hook_error,
        "Service stderr log should contain the hook failure message. Got entries: {:?}",
        stderr_entries.iter().map(|e| &e.line).collect::<Vec<_>>()
    );
}

/// When a hook has a Lua error in a ${{ }}$ field, the error should be
/// written to the service's stderr log file.
#[tokio::test]
async fn test_hook_lua_error_logged_to_stderr() {
    let temp_dir = TempDir::new().unwrap();

    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    // Hook with a bad Lua expression in its run field
    let yaml = r#"
services:
  test:
    command:
      - sleep
      - "3600"
    hooks:
      pre_start:
        - run: ${{ error("hook lua error for stderr test") }}$
"#;

    let config_path = write_raw_config(yaml, temp_dir.path());
    let (orchestrator, registry, _exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Start the service — hook resolution should fail due to Lua error
    let _result = orchestrator
        .start_services(&config_path, None, Some(sys_env), None, None)
        .await;

    // Wait for the service to reach Failed status
    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Failed, Duration::from_secs(5)).await,
        "Service should reach Failed status due to hook Lua error"
    );

    // Read the service's stderr log — should contain the Lua error
    let log_config = handle.get_log_config().await.expect("Log config should exist");
    let reader = LogReader::new(log_config.logs_dir);
    let entries = reader.tail(100, Some("test"), true);
    let stderr_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.stream == LogStream::Stderr)
        .collect();

    assert!(
        !stderr_entries.is_empty(),
        "Service stderr log should contain at least one entry after hook Lua error"
    );

    let has_lua_error = stderr_entries
        .iter()
        .any(|e| e.line.contains("hook lua error for stderr test"));
    assert!(
        has_lua_error,
        "Service stderr log should contain the hook Lua error message. Got entries: {:?}",
        stderr_entries.iter().map(|e| &e.line).collect::<Vec<_>>()
    );
}
