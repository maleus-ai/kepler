// MutexGuard held across await is intentional for env safety in tests
#![allow(clippy::await_holding_lock)]
//! Tests for file change event handling:
//! - matched_files are populated in FileChangeEvent
//! - stopped services ignore file change events
//! - duplicate in-flight file change events are deduplicated

use kepler_daemon::config::{HookCommand, HookList, RestartPolicy, ServiceHooks};
use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::ENV_LOCK;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
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
    Arc<ServiceOrchestrator>,
    Arc<ConfigRegistry>,
    mpsc::Receiver<ProcessExitEvent>,
    mpsc::Receiver<FileChangeEvent>,
) {
    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, exit_rx) = mpsc::channel(32);
    let (restart_tx, restart_rx) = mpsc::channel(32);
    let cursor_manager = Arc::new(kepler_daemon::cursor::CursorManager::new(300));
    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry.clone(),
        exit_tx,
        restart_tx,
        cursor_manager,
        kepler_daemon::hardening::HardeningLevel::None,
        None,
    ));
    (orchestrator, registry, exit_rx, restart_rx)
}

/// Write a typed config to a file and return its path.
fn write_config(config: &kepler_daemon::config::KeplerConfig, dir: &std::path::Path) -> PathBuf {
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

/// Wait for the marker file to reach the expected number of lines within a timeout.
async fn wait_for_start_count(
    marker: &MarkerFileHelper,
    marker_name: &str,
    expected: usize,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if marker.count_marker_lines(marker_name) >= expected {
            return true;
        }
        if tokio::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ============================================================================
// handle_file_change: inactive service guard
// ============================================================================

/// File change event for a stopped service should be ignored
#[tokio::test]
async fn test_file_change_ignored_for_stopped_service() {
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
            .with_restart(RestartPolicy::always())
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

    // Start the service
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service should reach Running status"
    );

    // Wait for initial start marker
    let content = marker.wait_for_marker_content("started", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started");

    // Stop the service
    orchestrator
        .stop_services(&config_path, Some("test"), false, None)
        .await
        .unwrap();

    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Stopped, Duration::from_secs(5)).await,
        "Service should reach Stopped status"
    );

    let count_after_stop = marker.count_marker_lines("started");

    // Send a file change event — it should be ignored since the service is stopped
    let event = FileChangeEvent {
        config_path: config_path.clone(),
        service_name: "test".to_string(),
        matched_files: vec![PathBuf::from("/some/file.ts")],
    };
    orchestrator.handle_file_change(event).await;

    // Give time for any potential restart
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Service should still be stopped
    let state = handle
        .get_service_state("test")
        .await
        .expect("Service state should exist");
    assert_eq!(
        state.status,
        ServiceStatus::Stopped,
        "Service should remain Stopped after file change event"
    );

    // Start count should not have increased
    let count_after_event = marker.count_marker_lines("started");
    assert_eq!(
        count_after_stop, count_after_event,
        "Service should not have restarted. Starts before: {}, after: {}",
        count_after_stop, count_after_event
    );
}

/// File change event for an exited service should be ignored
#[tokio::test]
async fn test_file_change_ignored_for_exited_service() {
    let temp_dir = TempDir::new().unwrap();

    // Service that exits immediately (no restart policy)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let config_path = write_config(&config, temp_dir.path());

    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    let (orchestrator, registry, mut exit_rx, _restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");

    // Wait for process exit event and let the orchestrator handle it
    if let Some(exit_event) = exit_rx.recv().await {
        let _ = orchestrator
            .handle_exit(
                &exit_event.config_path,
                &exit_event.service_name,
                exit_event.exit_code,
                exit_event.signal,
            )
            .await;
    }

    // Wait for the service to reach Exited status
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Exited, Duration::from_secs(5)).await,
        "Service should reach Exited status"
    );

    // Send a file change event — it should be ignored
    let event = FileChangeEvent {
        config_path: config_path.clone(),
        service_name: "test".to_string(),
        matched_files: vec![PathBuf::from("/some/file.ts")],
    };
    orchestrator.handle_file_change(event).await;

    // Give time for any potential restart
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Service should still be exited
    let state = handle
        .get_service_state("test")
        .await
        .expect("Service state should exist");
    assert_eq!(
        state.status,
        ServiceStatus::Exited,
        "Service should remain Exited after file change event"
    );
}

// ============================================================================
// spawn_file_change_handler: deduplication
// ============================================================================

/// Duplicate file change events for the same service are deduplicated
#[tokio::test]
async fn test_file_change_handler_deduplicates_events() {
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
            .with_restart(RestartPolicy::always())
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

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service should reach Running status"
    );

    // Wait for initial start marker
    let content = marker.wait_for_marker_content("started", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started");
    assert_eq!(marker.count_marker_lines("started"), 1);

    // Create a channel we control and spawn the orchestrator's file change handler
    let (tx, restart_rx) = mpsc::channel::<FileChangeEvent>(32);
    let handler = orchestrator.clone().spawn_file_change_handler(restart_rx);

    // Send 5 events rapidly — the first should trigger a restart,
    // the rest should be deduplicated while the restart is in-flight
    for i in 0..5 {
        let event = FileChangeEvent {
            config_path: config_path.clone(),
            service_name: "test".to_string(),
            matched_files: vec![PathBuf::from(format!("/some/file{}.ts", i))],
        };
        tx.send(event).await.unwrap();
    }

    // Wait for restart(s) to complete
    tokio::time::sleep(Duration::from_secs(5)).await;

    // The service should have restarted at most twice (initial + one restart).
    // Without deduplication, it could restart up to 5 times.
    let final_count = marker.count_marker_lines("started");
    assert!(
        final_count <= 2,
        "Service should restart at most once due to deduplication (initial start + 1 restart). Got {} starts",
        final_count
    );

    handler.abort();
}

/// File change events for different services are handled independently
#[tokio::test]
async fn test_file_change_handler_independent_services() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_a = marker.marker_path("started_a");
    let marker_b = marker.marker_path("started_b");

    let config = TestConfigBuilder::new()
        .add_service(
            "service_a",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_a.display()),
            ])
            .with_restart(RestartPolicy::always())
            .build(),
        )
        .add_service(
            "service_b",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_b.display()),
            ])
            .with_restart(RestartPolicy::always())
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

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "service_a", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service A should reach Running status"
    );
    assert!(
        wait_for_status(&handle, "service_b", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service B should reach Running status"
    );

    // Wait for initial start markers
    marker.wait_for_marker_content("started_a", Duration::from_secs(2)).await;
    marker.wait_for_marker_content("started_b", Duration::from_secs(2)).await;

    // Stop service_a, leave service_b running
    orchestrator
        .stop_services(&config_path, Some("service_a"), false, None)
        .await
        .unwrap();

    assert!(
        wait_for_status(&handle, "service_a", ServiceStatus::Stopped, Duration::from_secs(5)).await,
        "Service A should reach Stopped status"
    );

    let count_a_after_stop = marker.count_marker_lines("started_a");
    let count_b_before = marker.count_marker_lines("started_b");

    // Send file change for stopped service_a — should be ignored
    let event_a = FileChangeEvent {
        config_path: config_path.clone(),
        service_name: "service_a".to_string(),
        matched_files: vec![PathBuf::from("/some/file.ts")],
    };
    orchestrator.handle_file_change(event_a).await;

    // Send file change for running service_b — should trigger restart
    let event_b = FileChangeEvent {
        config_path: config_path.clone(),
        service_name: "service_b".to_string(),
        matched_files: vec![PathBuf::from("/some/file.ts")],
    };
    orchestrator.handle_file_change(event_b).await;

    // Wait for restart to complete
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Service A should still be stopped and not restarted
    let state_a = handle
        .get_service_state("service_a")
        .await
        .expect("Service A state should exist");
    assert_eq!(
        state_a.status,
        ServiceStatus::Stopped,
        "Service A should remain Stopped"
    );
    assert_eq!(
        marker.count_marker_lines("started_a"),
        count_a_after_stop,
        "Service A should not have restarted"
    );

    // Service B should have been restarted
    let count_b_after = marker.count_marker_lines("started_b");
    assert!(
        count_b_after > count_b_before,
        "Service B should have restarted. Before: {}, After: {}",
        count_b_before,
        count_b_after
    );
}

// ============================================================================
// Watcher lifecycle: not active during hooks, no duplicates
// ============================================================================

/// Watcher should not be active during pre_start hook on initial start.
/// A pre_start hook that modifies watched files should not trigger a restart.
#[tokio::test]
async fn test_watcher_not_active_during_pre_start() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create source directory with an initial file
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("app.ts"), "// initial").unwrap();

    // pre_start hook writes to a file that matches the watch pattern
    let hook_target = src_dir.join("hook_output.ts");
    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!(
            "echo 'hook ran' >> {}",
            hook_target.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
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

    let (orchestrator, registry, _exit_rx, restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Spawn the file change handler before starting services
    let handler = orchestrator.clone().spawn_file_change_handler(restart_rx);

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service should reach Running status"
    );

    // Wait for initial start
    assert!(
        wait_for_start_count(&marker, "started", 1, Duration::from_secs(2)).await,
        "Service should start initially"
    );

    // Verify the hook ran (file was created)
    assert!(
        hook_target.exists(),
        "pre_start hook should have created the file"
    );

    // Wait for any potential watcher-triggered restart
    // (watcher debounce is 500ms, wait 3s to be safe)
    tokio::time::sleep(Duration::from_secs(3)).await;

    let count = marker.count_marker_lines("started");
    assert_eq!(
        count, 1,
        "pre_start hook modifying watched files should not trigger restart. Got {} starts",
        count
    );

    handler.abort();
}

/// During file-change restart, the old watcher is cancelled before hooks run.
/// A pre_start hook modifying watched files during restart should not trigger another restart.
#[tokio::test]
async fn test_watcher_not_active_during_restart_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create source directory with a file to trigger restart
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let trigger_file = src_dir.join("app.ts");
    std::fs::write(&trigger_file, "// initial").unwrap();

    // pre_start hook writes to a file that matches the watch pattern
    let hook_target = src_dir.join("hook_output.ts");
    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!(
            "echo 'hook ran' >> {}",
            hook_target.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
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

    let (orchestrator, registry, _exit_rx, restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Spawn the file change handler
    let handler = orchestrator.clone().spawn_file_change_handler(restart_rx);

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service should reach Running status"
    );

    // Wait for initial start
    assert!(
        wait_for_start_count(&marker, "started", 1, Duration::from_secs(2)).await,
        "Service should start initially"
    );

    // Allow watcher to fully initialize
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Modify the watched file to trigger a restart
    std::fs::write(&trigger_file, "// modified").unwrap();

    // Wait for restart to complete (count reaches 2)
    assert!(
        wait_for_start_count(&marker, "started", 2, Duration::from_secs(10)).await,
        "Service should restart after file change. Got {} starts",
        marker.count_marker_lines("started")
    );

    // Wait extra time for any potential cascade restart from the pre_start hook's
    // file modification during the restart cycle
    tokio::time::sleep(Duration::from_secs(3)).await;

    let final_count = marker.count_marker_lines("started");
    assert_eq!(
        final_count, 2,
        "pre_start hook modifying watched files during restart should not trigger another restart. Got {} starts",
        final_count
    );

    handler.abort();
}

/// After a file-change restart, the watcher is properly re-established (no watcher leak).
/// Modifying a watched file after restart should trigger exactly one more restart.
#[tokio::test]
async fn test_watcher_reestablished_after_restart() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create source directory
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let trigger_file = src_dir.join("app.ts");
    std::fs::write(&trigger_file, "// initial").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
            .build(),
        )
        .build();

    let config_path = write_config(&config, temp_dir.path());

    let kepler_state_dir = temp_dir.path().join(".kepler");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
    }

    let (orchestrator, registry, _exit_rx, restart_rx) =
        create_test_orchestrator(temp_dir.path());

    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Spawn the file change handler
    let handler = orchestrator.clone().spawn_file_change_handler(restart_rx);

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    let handle = registry.get(&config_path).expect("Config should be loaded");
    assert!(
        wait_for_status(&handle, "test", ServiceStatus::Running, Duration::from_secs(5)).await,
        "Service should reach Running status"
    );

    // Wait for initial start
    assert!(
        wait_for_start_count(&marker, "started", 1, Duration::from_secs(2)).await,
        "Service should start initially"
    );

    // Allow watcher to fully initialize
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // First file change → triggers first restart
    std::fs::write(&trigger_file, "// modified v1").unwrap();

    assert!(
        wait_for_start_count(&marker, "started", 2, Duration::from_secs(10)).await,
        "Service should restart after first file change. Got {} starts",
        marker.count_marker_lines("started")
    );

    // Wait for new watcher to initialize after restart
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Second file change → should trigger another restart (watcher was re-established)
    std::fs::write(&trigger_file, "// modified v2").unwrap();

    assert!(
        wait_for_start_count(&marker, "started", 3, Duration::from_secs(10)).await,
        "Service should restart after second file change (watcher re-established). Got {} starts",
        marker.count_marker_lines("started")
    );

    // Wait extra time to make sure no additional restarts happen
    tokio::time::sleep(Duration::from_secs(3)).await;

    let final_count = marker.count_marker_lines("started");
    assert_eq!(
        final_count, 3,
        "Should have exactly 3 starts (initial + 2 restarts). Got {} starts",
        final_count
    );

    handler.abort();
}

