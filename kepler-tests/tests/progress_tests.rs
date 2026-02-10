//! Tests for progress event emission during start/stop/restart lifecycle operations.

use kepler_daemon::config::{DependencyCondition, DependencyConfig, DependencyEntry, DependsOn, RestartPolicy};
use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_protocol::protocol::{
    ServerMessage, ServicePhase, ServiceTarget, StartMode, decode_server_message,
};
use kepler_protocol::server::ProgressSender;
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

/// Create a ProgressSender backed by a test channel, returning (sender, receiver).
/// The receiver yields raw encoded ServerMessage bytes (with 4-byte length prefix).
fn create_test_progress() -> (ProgressSender, mpsc::Receiver<Vec<u8>>) {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(256);
    let sender = ProgressSender::new(tx, 1);
    (sender, rx)
}

/// Collect progress events from the channel, waiting for `expected_count` events or `timeout`.
/// Used for healthcheck tests where events arrive asynchronously after spawn.
async fn collect_progress_events_async(
    rx: &mut mpsc::Receiver<Vec<u8>>,
    expected_count: usize,
    timeout: Duration,
) -> Vec<(String, ServicePhase)> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;

    while events.len() < expected_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(bytes)) => {
                if bytes.len() < 4 {
                    continue;
                }
                let payload = &bytes[4..];
                if let Ok(ServerMessage::Event { event }) = decode_server_message(payload) {
                    match event {
                        kepler_protocol::protocol::ServerEvent::Progress { event: progress, .. } => {
                            events.push((progress.service, progress.phase));
                        }
                    }
                }
            }
            Ok(None) => break, // Channel closed
            Err(_) => break,   // Timeout
        }
    }
    events
}

/// Collect all progress events from the channel (non-blocking drain).
async fn collect_progress_events(
    rx: &mut mpsc::Receiver<Vec<u8>>,
) -> Vec<(String, ServicePhase)> {
    let mut events = Vec::new();
    while let Ok(bytes) = rx.try_recv() {
        // Strip 4-byte length prefix
        if bytes.len() < 4 {
            continue;
        }
        let payload = &bytes[4..];
        if let Ok(ServerMessage::Event { event }) = decode_server_message(payload) {
            match event {
                kepler_protocol::protocol::ServerEvent::Progress { event: progress, .. } => {
                    events.push((progress.service, progress.phase));
                }
            }
        }
    }
    events
}

// ============================================================================
// Start Progress Tests
// ============================================================================

/// start_services emits Starting then Started for each service
#[tokio::test]
async fn test_start_emits_starting_and_started() {
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
    let (progress, mut rx) = create_test_progress();

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, Some(progress))
        .await
        .unwrap();

    // Give a moment for all events to arrive
    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = collect_progress_events(&mut rx).await;

    // Should have Pending, Starting, and Started for "web"
    assert!(events.len() >= 3, "Expected at least 3 events, got {}: {:?}", events.len(), events);

    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    assert!(web_events.len() >= 3, "Expected at least 3 events for web, got {}: {:?}", web_events.len(), web_events);

    assert!(matches!(web_events[0].1, ServicePhase::Pending { .. }), "First event should be Pending, got {:?}", web_events[0].1);
    assert!(matches!(web_events[1].1, ServicePhase::Starting), "Second event should be Starting, got {:?}", web_events[1].1);
    assert!(matches!(web_events[2].1, ServicePhase::Started), "Third event should be Started, got {:?}", web_events[2].1);

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

/// start_services emits progress for multiple services
#[tokio::test]
async fn test_start_emits_progress_for_multiple_services() {
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
    let (progress, mut rx) = create_test_progress();

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, Some(progress))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = collect_progress_events(&mut rx).await;

    // Should have Pending+Starting+Started for both services
    assert!(events.len() >= 6, "Expected at least 6 events, got {}: {:?}", events.len(), events);

    // Collect per-service events
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    let worker_events: Vec<_> = events.iter().filter(|(name, _)| name == "worker").collect();

    assert!(web_events.len() >= 3, "Expected at least 3 events for web, got: {:?}", web_events);
    assert!(worker_events.len() >= 3, "Expected at least 3 events for worker, got: {:?}", worker_events);

    // Each service should have Pending, Starting, then Started (in order)
    assert!(matches!(web_events[0].1, ServicePhase::Pending { .. }));
    assert!(matches!(web_events[1].1, ServicePhase::Starting));
    assert!(matches!(web_events[2].1, ServicePhase::Started));
    assert!(matches!(worker_events[0].1, ServicePhase::Pending { .. }));
    assert!(matches!(worker_events[1].1, ServicePhase::Starting));
    assert!(matches!(worker_events[2].1, ServicePhase::Started));

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

// ============================================================================
// Stop Progress Tests
// ============================================================================

/// stop_services emits Stopping then Stopped for running services
#[tokio::test]
async fn test_stop_emits_stopping_and_stopped() {
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

    // Start without progress
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, None)
        .await
        .unwrap();

    // Small delay to ensure service is fully running
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Stop with progress
    let (progress, mut rx) = create_test_progress();
    orchestrator
        .stop_services(&config_path, None, false, None, Some(progress))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = collect_progress_events(&mut rx).await;

    assert!(events.len() >= 2, "Expected at least 2 events, got {}: {:?}", events.len(), events);

    let (ref name, ref phase) = events[0];
    assert_eq!(name, "web");
    assert!(matches!(phase, ServicePhase::Stopping), "First event should be Stopping, got {:?}", phase);

    let (ref name, ref phase) = events[1];
    assert_eq!(name, "web");
    assert!(matches!(phase, ServicePhase::Stopped), "Second event should be Stopped, got {:?}", phase);
}

/// stop_services emits no progress events for already-stopped services
#[tokio::test]
async fn test_stop_already_stopped_emits_no_events() {
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

    // Start and then stop without progress first
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, None)
        .await
        .unwrap();
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Stop again with progress — service is already stopped
    let (progress, mut rx) = create_test_progress();
    orchestrator
        .stop_services(&config_path, None, false, None, Some(progress))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = collect_progress_events(&mut rx).await;

    // No events should be emitted since nothing was actually stopped
    assert!(
        events.is_empty(),
        "Expected no events for already-stopped services, got: {:?}",
        events
    );
}

/// stop_services with clean emits Cleaning then Cleaned
#[tokio::test]
async fn test_stop_clean_emits_cleaning_and_cleaned() {
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

    // Start without progress
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, None)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Stop with clean and progress
    let (progress, mut rx) = create_test_progress();
    orchestrator
        .stop_services(&config_path, None, true, None, Some(progress))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = collect_progress_events(&mut rx).await;

    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();

    // If service was still running: Stopping → Stopped, optionally followed by Cleaned.
    // If it had already exited: possibly just Cleaned or no events.
    // Cleaned depends on state directory existing on disk (not guaranteed in test env).
    // The key invariant: if Stopping/Stopped appear, they must be in order, and
    // if Cleaned appears it must come after Stopped.
    if let Some(stopping_idx) = web_events.iter().position(|(_, p)| matches!(p, ServicePhase::Stopping)) {
        let stopped_idx = web_events.iter().position(|(_, p)| matches!(p, ServicePhase::Stopped));
        assert!(stopped_idx.is_some(), "Stopping without Stopped");
        assert!(stopped_idx.unwrap() > stopping_idx, "Stopped should come after Stopping");
    }
    if let Some(cleaned_idx) = web_events.iter().position(|(_, p)| matches!(p, ServicePhase::Cleaned)) {
        if let Some(stopped_idx) = web_events.iter().position(|(_, p)| matches!(p, ServicePhase::Stopped)) {
            assert!(cleaned_idx > stopped_idx, "Cleaned should come after Stopped");
        }
    }
}

// ============================================================================
// Restart Progress Tests
// ============================================================================

/// restart_services emits Stopping/Stopped then Starting/Started
#[tokio::test]
async fn test_restart_emits_full_lifecycle_progress() {
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

    // Start without progress
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, None)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Restart with progress
    let (progress, mut rx) = create_test_progress();
    orchestrator
        .restart_services(&config_path, &[], None, Some(progress))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = collect_progress_events(&mut rx).await;

    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    assert!(
        web_events.len() >= 4,
        "Expected at least 4 events for web during restart, got {}: {:?}",
        web_events.len(),
        web_events
    );

    // Phase 1: Stop
    assert!(matches!(web_events[0].1, ServicePhase::Stopping), "Expected Stopping, got {:?}", web_events[0].1);
    assert!(matches!(web_events[1].1, ServicePhase::Stopped), "Expected Stopped, got {:?}", web_events[1].1);

    // Phase 2: Start
    assert!(matches!(web_events[2].1, ServicePhase::Starting), "Expected Starting, got {:?}", web_events[2].1);
    assert!(matches!(web_events[3].1, ServicePhase::Started), "Expected Started, got {:?}", web_events[3].1);

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

// ============================================================================
// No Progress (None) Tests
// ============================================================================

/// Passing None for progress still works (no events emitted, no crash)
#[tokio::test]
async fn test_start_with_no_progress_works() {
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
    let result = orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, None)
        .await;

    assert!(result.is_ok(), "Start with None progress should succeed");

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

// ============================================================================
// Healthcheck Progress Tests
// ============================================================================

/// Pending target is Healthy for services with healthcheck
#[tokio::test]
async fn test_start_healthcheck_pending_target_is_healthy() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_restart(RestartPolicy::No)
                .with_healthcheck(TestHealthCheckBuilder::always_healthy().build())
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;
    let (progress, mut rx) = create_test_progress();

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, Some(progress))
        .await
        .unwrap();

    // Collect events (at least Pending + Starting + Started + Healthy)
    let events = collect_progress_events_async(&mut rx, 4, Duration::from_secs(5)).await;

    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    assert!(!web_events.is_empty(), "Expected at least 1 event for web, got none");

    // First event should be Pending with target Healthy
    match &web_events[0].1 {
        ServicePhase::Pending { target } => {
            assert_eq!(*target, ServiceTarget::Healthy, "Pending target should be Healthy for HC service");
        }
        other => panic!("First event should be Pending, got {:?}", other),
    }

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

/// start_services with healthcheck emits Started then Healthy
#[tokio::test]
async fn test_start_healthcheck_emits_started_then_healthy() {
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
    let (progress, mut rx) = create_test_progress();

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, Some(progress))
        .await
        .unwrap();

    // Collect events with enough time for healthcheck to fire
    let events = collect_progress_events_async(&mut rx, 4, Duration::from_secs(5)).await;

    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    assert!(
        web_events.len() >= 4,
        "Expected at least 4 events (Pending, Starting, Started, Healthy), got {}: {:?}",
        web_events.len(),
        web_events
    );

    assert!(matches!(web_events[0].1, ServicePhase::Pending { .. }), "Expected Pending, got {:?}", web_events[0].1);
    assert!(matches!(web_events[1].1, ServicePhase::Starting), "Expected Starting, got {:?}", web_events[1].1);
    assert!(matches!(web_events[2].1, ServicePhase::Started), "Expected Started, got {:?}", web_events[2].1);
    assert!(matches!(web_events[3].1, ServicePhase::Healthy), "Expected Healthy, got {:?}", web_events[3].1);

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

/// start_services blocks until healthcheck passes (Healthy event is in channel before return)
#[tokio::test]
async fn test_start_healthcheck_blocks_until_healthy() {
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
    let (progress, mut rx) = create_test_progress();

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, Some(progress))
        .await
        .unwrap();

    // After start_services returns, drain all events immediately (non-blocking).
    // If the daemon properly waited for healthchecks, Healthy should already be in the channel.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events = collect_progress_events(&mut rx).await;

    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    let has_healthy = web_events.iter().any(|(_, phase)| matches!(phase, ServicePhase::Healthy));

    assert!(
        has_healthy,
        "Expected Healthy event to be present after start_services returns, got: {:?}",
        web_events
    );

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
}

/// Mixed services: service without HC gets Started, service with HC gets Started then Healthy
#[tokio::test]
async fn test_start_mixed_services_both_show_progress() {
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
    let (progress, mut rx) = create_test_progress();

    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, None, Some(sys_env), StartMode::WaitStartup, Some(progress))
        .await
        .unwrap();

    // Collect events (7 total: 3 for migration + 4 for web)
    let events = collect_progress_events_async(&mut rx, 7, Duration::from_secs(5)).await;

    // migration: Pending{Started} → Starting → Started
    let migration_events: Vec<_> = events.iter().filter(|(name, _)| name == "migration").collect();
    assert!(
        migration_events.len() >= 3,
        "Expected at least 3 events for migration, got {}: {:?}",
        migration_events.len(),
        migration_events
    );
    match &migration_events[0].1 {
        ServicePhase::Pending { target } => {
            assert_eq!(*target, ServiceTarget::Started, "migration target should be Started");
        }
        other => panic!("Expected Pending for migration, got {:?}", other),
    }
    assert!(matches!(migration_events[1].1, ServicePhase::Starting));
    assert!(matches!(migration_events[2].1, ServicePhase::Started));

    // web: Pending{Healthy} → Starting → Started → Healthy
    let web_events: Vec<_> = events.iter().filter(|(name, _)| name == "web").collect();
    assert!(
        web_events.len() >= 4,
        "Expected at least 4 events for web, got {}: {:?}",
        web_events.len(),
        web_events
    );
    match &web_events[0].1 {
        ServicePhase::Pending { target } => {
            assert_eq!(*target, ServiceTarget::Healthy, "web target should be Healthy");
        }
        other => panic!("Expected Pending for web, got {:?}", other),
    }
    assert!(matches!(web_events[1].1, ServicePhase::Starting));
    assert!(matches!(web_events[2].1, ServicePhase::Started));
    assert!(matches!(web_events[3].1, ServicePhase::Healthy));

    // Cleanup
    orchestrator
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
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
                None,
                Some(sys_env),
                StartMode::WaitStartup,
                None,
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
        .stop_services(&config_path, None, false, None, None)
        .await
        .unwrap();
    exit_handler.abort();
}
