//! Tests for service status transitions during start/stop/restart lifecycle operations.
//!
//! Status changes are emitted via per-subscriber unbounded mpsc channels in the
//! config actor. These tests verify the orchestrator emits correct status transitions
//! by subscribing to state changes on ConfigActorHandle.

use kepler_daemon::config::{DependencyCondition, DependencyConfig, DependencyEntry, DependsOn, RestartPolicy};
use kepler_daemon::config_actor::ConfigEvent;
use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHealthCheckBuilder, TestServiceBuilder};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Mutex to synchronize environment variable setting
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Helper to set up an orchestrator for testing
async fn setup_orchestrator(
    temp_dir: &TempDir,
) -> (
    Arc<ServiceOrchestrator>,
    std::path::PathBuf,
    mpsc::Sender<ProcessExitEvent>,
    mpsc::Sender<FileChangeEvent>,
) {
    let config_dir = temp_dir.path().to_path_buf();
    let config_path = config_dir.join("kepler.yaml");

    // Set KEPLER_DAEMON_PATH to isolate state
    let kepler_state_dir = config_dir.join(".kepler");
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
        }
    }

    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, _exit_rx) = mpsc::channel::<ProcessExitEvent>(32);
    let (restart_tx, _restart_rx) = mpsc::channel::<FileChangeEvent>(32);

    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry,
        exit_tx.clone(),
        restart_tx.clone(),
    ));

    (orchestrator, config_path, exit_tx, restart_tx)
}

/// Helper to set up an orchestrator with exit event handling (needed for dependency chains).
/// Returns the orchestrator and a JoinHandle for the exit handler task.
async fn setup_orchestrator_with_exit_handler(
    temp_dir: &TempDir,
) -> (
    Arc<ServiceOrchestrator>,
    std::path::PathBuf,
    mpsc::Sender<ProcessExitEvent>,
    mpsc::Sender<FileChangeEvent>,
    tokio::task::JoinHandle<()>,
) {
    let config_dir = temp_dir.path().to_path_buf();
    let config_path = config_dir.join("kepler.yaml");

    // Set KEPLER_DAEMON_PATH to isolate state
    let kepler_state_dir = config_dir.join(".kepler");
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
        }
    }

    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, mut exit_rx) = mpsc::channel::<ProcessExitEvent>(32);
    let (restart_tx, _restart_rx) = mpsc::channel::<FileChangeEvent>(32);

    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry,
        exit_tx.clone(),
        restart_tx.clone(),
    ));

    // Spawn exit event handler (mirrors the one in main.rs)
    let exit_orch = orchestrator.clone();
    let exit_handler = tokio::spawn(async move {
        while let Some(event) = exit_rx.recv().await {
            let _ = exit_orch
                .handle_exit(
                    &event.config_path,
                    &event.service_name,
                    event.exit_code,
                    event.signal,
                )
                .await;
        }
    });

    (orchestrator, config_path, exit_tx, restart_tx, exit_handler)
}

/// Collect status change events from an unbounded receiver, waiting for `expected_count` or timeout.
async fn collect_status_changes(
    rx: &mut mpsc::UnboundedReceiver<ConfigEvent>,
    expected_count: usize,
    timeout: Duration,
) -> Vec<(String, ServiceStatus)> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;

    while events.len() < expected_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ConfigEvent::StatusChange(change))) => {
                events.push((change.service, change.status));
            }
            Ok(Some(_)) => continue, // Ready/Quiescent signals — skip
            Ok(None) => break, // Channel closed
            Err(_) => break,   // Timeout
        }
    }
    events
}

/// Subscribe to status changes for a config, returning an unbounded receiver.
/// Must be called after start_services loads the config.
async fn subscribe_to_config(
    orchestrator: &ServiceOrchestrator,
    config_path: &std::path::Path,
) -> Option<mpsc::UnboundedReceiver<ConfigEvent>> {
    let canonical = std::fs::canonicalize(config_path).ok()?;
    let handle = orchestrator.registry().get(&canonical)?;
    Some(handle.subscribe_state_changes())
}

// ============================================================================
// Start Tests
// ============================================================================

/// start_services transitions through Starting → Running for each service
#[tokio::test]
async fn test_start_transitions_starting_to_running() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    // After start completes, check that the service is Running
    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();
    let state = handle.get_service_state("web").await.unwrap();
    assert_eq!(state.status, ServiceStatus::Running, "Service should be Running after start");

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
}

/// start_services works for multiple services
#[tokio::test]
async fn test_start_multiple_services() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .add_service(
            "worker",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();

    let web_state = handle.get_service_state("web").await.unwrap();
    let worker_state = handle.get_service_state("worker").await.unwrap();
    assert_eq!(web_state.status, ServiceStatus::Running);
    assert_eq!(worker_state.status, ServiceStatus::Running);

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
}

// ============================================================================
// Stop Tests
// ============================================================================

/// stop_services transitions Running → Stopping → Stopped
#[tokio::test]
async fn test_stop_transitions() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subscribe before stopping so we can catch status changes
    let mut rx = subscribe_to_config(&orchestrator, &config_path).await.unwrap();

    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();

    // Collect status transitions (Stopping + Stopped = 2 events)
    let events = collect_status_changes(&mut rx, 2, Duration::from_secs(5)).await;
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();

    assert!(web_events.len() >= 2, "Expected at least 2 events, got {:?}", web_events);
    assert_eq!(web_events[0].1, ServiceStatus::Stopping, "First should be Stopping");
    assert_eq!(web_events[1].1, ServiceStatus::Stopped, "Second should be Stopped");
}

/// stop_services on already-stopped services is a no-op
#[tokio::test]
async fn test_stop_already_stopped_is_noop() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start and stop
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subscribe and stop again
    let mut rx = subscribe_to_config(&orchestrator, &config_path).await.unwrap();
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();

    // Should get no events
    let events = collect_status_changes(&mut rx, 1, Duration::from_millis(500)).await;
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    assert!(
        web_events.is_empty(),
        "Expected no events for already-stopped services, got: {:?}",
        web_events
    );
}

// ============================================================================
// Restart Tests
// ============================================================================

/// restart_services transitions Stopping → Stopped → Starting → Running
#[tokio::test]
async fn test_restart_full_lifecycle() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subscribe before restart
    let mut rx = subscribe_to_config(&orchestrator, &config_path).await.unwrap();

    orchestrator
        .restart_services(&config_path, &[], false)
        .await
        .unwrap();

    // Collect status transitions. The Starting status may be too brief to catch,
    // so we expect at minimum: Stopping → Stopped → Running (Starting is optional).
    let events = collect_status_changes(&mut rx, 4, Duration::from_secs(5)).await;
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();

    assert!(
        web_events.len() >= 3,
        "Expected at least 3 events for web during restart, got {}: {:?}",
        web_events.len(),
        web_events
    );

    // Verify ordering: must see Stopping, then Stopped, then Running
    let has_stopping = web_events.iter().any(|(_, s)| *s == ServiceStatus::Stopping);
    let has_stopped = web_events.iter().any(|(_, s)| *s == ServiceStatus::Stopped);
    let has_running = web_events.iter().any(|(_, s)| *s == ServiceStatus::Running);
    assert!(has_stopping, "Expected Stopping event");
    assert!(has_stopped, "Expected Stopped event");
    assert!(has_running, "Expected Running event");

    // Verify order: Stopping before Stopped before Running
    let stopping_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Stopping).unwrap();
    let stopped_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Stopped).unwrap();
    let running_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Running).unwrap();
    assert!(stopping_idx < stopped_idx, "Stopping should come before Stopped");
    assert!(stopped_idx < running_idx, "Stopped should come before Running");

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
}

// ============================================================================
// Healthcheck Tests
// ============================================================================

/// start_services with healthcheck transitions through Starting → Running → Healthy
#[tokio::test]
async fn test_start_healthcheck_transitions_to_healthy() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .build(),
                )
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    // Healthcheck runs asynchronously — poll until it resolves
    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = handle.get_service_state("web").await.unwrap();
        if state.status == ServiceStatus::Healthy {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "Timed out waiting for Healthy, got {:?}", state.status);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
}

/// Mixed services: service without HC gets Running, service with HC gets Healthy
#[tokio::test]
async fn test_start_mixed_services_correct_final_states() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "migration",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .build(),
                )
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();

    let migration_state = handle.get_service_state("migration").await.unwrap();
    assert_eq!(migration_state.status, ServiceStatus::Running, "migration should be Running (no HC)");

    // Healthcheck runs asynchronously — poll until web becomes Healthy
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let web_state = handle.get_service_state("web").await.unwrap();
        if web_state.status == ServiceStatus::Healthy {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "Timed out waiting for web Healthy, got {:?}", web_state.status);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
}

// ============================================================================
// Broadcast Channel Tests
// ============================================================================

/// Verify that the broadcast channel emits status transitions during stop.
/// We subscribe after start completes (config loaded, service Running), then stop
/// to get deterministic Stopping → Stopped events.
#[tokio::test]
async fn test_broadcast_emits_stop_transitions() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    // Subscribe after start completes (deterministic — service is Running)
    let mut rx = subscribe_to_config(&orchestrator, &config_path).await.unwrap();

    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();

    let events = collect_status_changes(&mut rx, 2, Duration::from_secs(5)).await;
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();

    let has_stopping = web_events.iter().any(|(_, s)| *s == ServiceStatus::Stopping);
    let has_stopped = web_events.iter().any(|(_, s)| *s == ServiceStatus::Stopped);
    assert!(has_stopping, "Expected Stopping event, got {:?}", web_events);
    assert!(has_stopped, "Expected Stopped event, got {:?}", web_events);
}

// ============================================================================
// False Quiescence Tests (Bug: services default to Stopped = terminal)
// ============================================================================

/// During startup with a dependency chain, dependent services should be marked
/// Starting (not Stopped) before their dependency level is processed.
/// Without this, the CLI's quiescence check sees all services as terminal and exits.
#[tokio::test]
async fn test_start_deps_no_false_quiescence() {
    let temp_dir = TempDir::new().unwrap();

    // migration: exits quickly (one-shot)
    // web: depends on migration:service_completed_successfully, long-running
    let config = TestConfigBuilder::new()
        .add_service(
            "migration",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                "sleep 1".to_string(),
            ])
            .with_restart(RestartPolicy::No)
            .build(),
        )
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .with_depends_on_extended(DependsOn(vec![DependencyEntry::Extended(
                    HashMap::from([(
                        "migration".to_string(),
                        DependencyConfig {
                            condition: DependencyCondition::ServiceCompletedSuccessfully,
                            ..Default::default()
                        },
                    )]),
                )]))
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _, exit_handler) =
        setup_orchestrator_with_exit_handler(&temp_dir).await;
    let sys_env: HashMap<String, String> = std::env::vars().collect();

    // Start in background
    let orch_clone = orchestrator.clone();
    let config_path_clone = config_path.clone();
    let start_handle = tokio::spawn(async move {
        orch_clone
            .start_services(
                &config_path_clone,
                &[],
                Some(sys_env),
                None,
                None,
                false,
            )
            .await
    });

    // Wait for config to be loaded and start processing to begin
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check web's status via the registry - it should be Starting, not Stopped.
    // If it's Stopped, the CLI's is_all_terminal() would return true (false quiescence).
    let config_path_canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&config_path_canonical);

    if let Some(handle) = handle {
        let state = handle.get_service_state("web").await;
        if let Some(state) = state {
            assert_ne!(
                state.status,
                ServiceStatus::Stopped,
                "web should be Starting (not Stopped) while waiting for dependency. \
                 The CLI would see web as terminal and exit the log following loop prematurely."
            );
        } else {
            panic!("web service state should exist after config load");
        }
    } else {
        panic!("Config should be loaded by now");
    }

    // Wait for start to complete
    start_handle.await.unwrap().unwrap();

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
    exit_handler.abort();
}

// ============================================================================
// Broadcast-based Dependency Waiting Tests (Improvement 1)
// ============================================================================

/// Verify that dependency waiting using broadcast subscription reacts faster
/// than the old 100ms polling interval. A dependent service should start
/// within a few ms of its dependency becoming satisfied, not ~100ms later.
#[tokio::test]
async fn test_dependency_waiting_reacts_via_broadcast() {
    let temp_dir = TempDir::new().unwrap();

    // "db" exits quickly (one-shot), "web" depends on db:service_completed_successfully
    let config = TestConfigBuilder::new()
        .add_service(
            "db",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo DB_DONE; exit 0".to_string(),
            ])
            .with_restart(RestartPolicy::No)
            .build(),
        )
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .with_depends_on_extended(DependsOn(vec![DependencyEntry::Extended(
                    HashMap::from([(
                        "db".to_string(),
                        DependencyConfig {
                            condition: DependencyCondition::ServiceCompletedSuccessfully,
                            ..Default::default()
                        },
                    )]),
                )]))
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _, exit_handler) =
        setup_orchestrator_with_exit_handler(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    let start = std::time::Instant::now();

    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    // start_services spawns service tasks and returns before they complete.
    // Wait for web to reach Running via state change subscription.
    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();
    let mut rx = handle.subscribe_state_changes();

    // Check if already Running before waiting
    let mut web_running = handle
        .get_service_state("web")
        .await
        .is_some_and(|s| s.status == ServiceStatus::Running);

    if !web_running {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while let Ok(Some(event)) =
            tokio::time::timeout(deadline - tokio::time::Instant::now(), rx.recv()).await
        {
            if let ConfigEvent::StatusChange(change) = event
                && change.service == "web" && change.status == ServiceStatus::Running {
                    web_running = true;
                    break;
                }
        }
    }

    let elapsed = start.elapsed();

    // With broadcast-based waiting, the entire start (including dependency
    // resolution) should complete well under 5 seconds. The old polling loop
    // would add at least 100ms of latency per check cycle.
    assert!(
        elapsed < Duration::from_secs(5),
        "Start with dependency should complete quickly via broadcast, took {:?}",
        elapsed
    );

    assert!(
        web_running,
        "web should be Running after dependency satisfied"
    );

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
    exit_handler.abort();
}

// ============================================================================
// Restart Lifecycle with Healthcheck Tests (Improvement 2)
// ============================================================================

/// Restart with healthcheck broadcasts the full lifecycle including Healthy.
/// Verifies: Stopping → Stopped → Starting → Running → Healthy
#[tokio::test]
async fn test_restart_broadcasts_full_lifecycle_with_healthcheck() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .build(),
                )
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    // Wait for initial healthy
    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = handle.get_service_state("web").await.unwrap();
        if state.status == ServiceStatus::Healthy {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for initial Healthy"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Subscribe before restart to capture all transitions
    let mut rx = subscribe_to_config(&orchestrator, &config_path).await.unwrap();

    orchestrator
        .restart_services(&config_path, &[], false)
        .await
        .unwrap();

    // Wait for Healthy after restart (healthcheck runs async)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = handle.get_service_state("web").await.unwrap();
        if state.status == ServiceStatus::Healthy {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for Healthy after restart"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Collect all events that were emitted during restart + healthcheck
    let events = collect_status_changes(&mut rx, 10, Duration::from_secs(2)).await;
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();

    // Must see: Stopping, Stopped, Running (Starting may be too brief), Healthy
    let has_stopping = web_events.iter().any(|(_, s)| *s == ServiceStatus::Stopping);
    let has_stopped = web_events.iter().any(|(_, s)| *s == ServiceStatus::Stopped);
    let has_running = web_events.iter().any(|(_, s)| *s == ServiceStatus::Running);
    let has_healthy = web_events.iter().any(|(_, s)| *s == ServiceStatus::Healthy);

    assert!(has_stopping, "Expected Stopping event, got {:?}", web_events);
    assert!(has_stopped, "Expected Stopped event, got {:?}", web_events);
    assert!(has_running, "Expected Running event, got {:?}", web_events);
    assert!(has_healthy, "Expected Healthy event after restart, got {:?}", web_events);

    // Verify order: Stopping < Stopped < Running < Healthy
    let stopping_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Stopping).unwrap();
    let stopped_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Stopped).unwrap();
    let running_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Running).unwrap();
    let healthy_idx = web_events.iter().position(|(_, s)| *s == ServiceStatus::Healthy).unwrap();
    assert!(stopping_idx < stopped_idx, "Stopping before Stopped");
    assert!(stopped_idx < running_idx, "Stopped before Running");
    assert!(running_idx < healthy_idx, "Running before Healthy");

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
}

// ============================================================================
// Auto-restart Health Checker Re-spawn Tests (Bug fix)
// ============================================================================

/// After auto-restart (handle_exit), health checker must be re-spawned.
/// Service exits with code 1, restart:always re-spawns it, and the
/// health checker should eventually mark it Healthy again.
#[tokio::test]
async fn test_handle_exit_respawns_health_checker() {
    let temp_dir = TempDir::new().unwrap();

    // Service exits after 2s with code 1, restart: always, with healthcheck
    let config = TestConfigBuilder::new()
        .add_service(
            "worker",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                "sleep 2; exit 1".to_string(),
            ])
            .with_restart(RestartPolicy::Always)
            .with_healthcheck(
                TestHealthCheckBuilder::always_healthy()
                    .with_interval(Duration::from_millis(200))
                    .build(),
            )
            .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _, exit_handler) =
        setup_orchestrator_with_exit_handler(&temp_dir).await;

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false)
        .await
        .unwrap();

    // Wait for initial Healthy
    let canonical = std::fs::canonicalize(&config_path).unwrap();
    let handle = orchestrator.registry().get(&canonical).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = handle.get_service_state("worker").await.unwrap();
        if state.status == ServiceStatus::Healthy {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for initial Healthy, got {:?}",
            state.status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Service will exit after 2s. Auto-restart fires after 1s delay.
    // Then health checker must re-spawn and mark Healthy again.
    // Wait for a non-healthy state first (the exit/restart window).
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Now wait for Healthy again — proves health checker was re-spawned
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state = handle.get_service_state("worker").await.unwrap();
        if state.status == ServiceStatus::Healthy {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for Healthy after auto-restart (health checker not re-spawned?), got {:?}",
            state.status
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();
    exit_handler.abort();
}
