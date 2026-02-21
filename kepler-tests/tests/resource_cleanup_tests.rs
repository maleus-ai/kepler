//! Resource cleanup tests — verify that all internal resources are properly
//! released when services stop, subscribers disconnect, or the actor shuts down.

use kepler_daemon::config::RestartPolicy;
use kepler_daemon::state::ServiceStatus;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHealthCheckBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::wait_utils::{wait_for_healthy, wait_for_status};
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// Category 1: Service Stop Cleanup
// ============================================================================

/// Stopping a service removes its process handle.
#[tokio::test]
async fn test_stop_removes_process_handle() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    harness.start_service("svc").await.unwrap();
    assert_eq!(harness.get_status("svc").await, Some(ServiceStatus::Running));

    let state = harness.handle().get_service_state("svc").await.unwrap();
    assert!(state.pid.is_some(), "Running service should have a PID");

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 1, "Should have 1 process handle while running");

    harness.stop_service("svc").await.unwrap();
    assert_eq!(harness.get_status("svc").await, Some(ServiceStatus::Stopped));

    let state = harness.handle().get_service_state("svc").await.unwrap();
    assert!(state.pid.is_none(), "Stopped service should have no PID");

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0, "No process handles after stop");
}

/// Stopping a service cancels its health check task.
#[tokio::test]
async fn test_stop_cancels_health_check() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    harness.start_service("svc").await.unwrap();
    harness.start_health_checker("svc").await.unwrap();

    wait_for_healthy(harness.handle(), "svc", Duration::from_secs(5))
        .await
        .expect("Service should become healthy");

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.health_checks, 1, "Should have 1 health check while running");

    harness.stop_service("svc").await.unwrap();

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.health_checks, 0, "No health checks after stop");
}

/// Stopping a service cancels its file watcher task.
#[tokio::test]
async fn test_stop_cancels_file_watcher() {
    let temp_dir = TempDir::new().unwrap();

    // Create watched directory structure
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.ts"), "// source").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc",
            TestServiceBuilder::long_running()
                .with_working_dir(temp_dir.path().to_path_buf())
                .with_restart_and_watch(
                    RestartPolicy::always(),
                    vec!["src/**/*.ts".to_string()],
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    harness.start_service("svc").await.unwrap();

    // Give the file watcher a moment to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.file_watchers, 1, "Should have 1 file watcher while running");

    harness.stop_service("svc").await.unwrap();

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.file_watchers, 0, "No file watchers after stop");
}

/// Stopping a service with healthcheck + watcher cleans all resources.
#[tokio::test]
async fn test_stop_cleans_all_resources() {
    let temp_dir = TempDir::new().unwrap();

    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.ts"), "// source").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc",
            TestServiceBuilder::long_running()
                .with_working_dir(temp_dir.path().to_path_buf())
                .with_restart_and_watch(
                    RestartPolicy::always(),
                    vec!["src/**/*.ts".to_string()],
                )
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    harness.start_service("svc").await.unwrap();
    harness.start_health_checker("svc").await.unwrap();

    wait_for_healthy(harness.handle(), "svc", Duration::from_secs(5))
        .await
        .expect("Service should become healthy");

    // Give file watcher time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 1);
    assert_eq!(counts.health_checks, 1);
    assert_eq!(counts.file_watchers, 1);

    harness.stop_service("svc").await.unwrap();

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0, "No process handles after stop");
    assert_eq!(counts.health_checks, 0, "No health checks after stop");
    assert_eq!(counts.file_watchers, 0, "No file watchers after stop");
}

// ============================================================================
// Category 2: Subscriber Cleanup
// ============================================================================

/// Dropping a subscriber receiver eventually prunes it from the list.
#[tokio::test]
async fn test_subscriber_pruned_on_receiver_drop() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let rx = harness.handle().subscribe_state_changes();
    assert_eq!(harness.handle().subscriber_count(), 1);

    drop(rx);

    // Trigger a status change to prune dead subscribers
    harness.start_service("svc").await.unwrap();
    // Small delay for the notification to propagate through the actor
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(harness.handle().subscriber_count(), 0, "Dead subscriber should be pruned");

    harness.stop_service("svc").await.unwrap();
}

/// Adding a new subscriber prunes dead ones.
#[tokio::test]
async fn test_subscriber_pruned_on_new_subscribe() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let rx1 = harness.handle().subscribe_state_changes();
    assert_eq!(harness.handle().subscriber_count(), 1);

    drop(rx1);

    // Subscribe again — should prune the dead one and add new one
    let _rx2 = harness.handle().subscribe_state_changes();
    assert_eq!(harness.handle().subscriber_count(), 1, "Dead subscriber pruned on new subscribe");
}

/// Multiple subscribers pruned selectively — only dead ones removed.
#[tokio::test]
async fn test_multiple_subscribers_pruned_selectively() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let _rx1 = harness.handle().subscribe_state_changes();
    let rx2 = harness.handle().subscribe_state_changes();
    let _rx3 = harness.handle().subscribe_state_changes();
    let rx4 = harness.handle().subscribe_state_changes();
    let _rx5 = harness.handle().subscribe_state_changes();

    assert_eq!(harness.handle().subscriber_count(), 5);

    // Drop 2 of the 5 receivers
    drop(rx2);
    drop(rx4);

    // Trigger a status change to prune dead subscribers
    harness.start_service("svc").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(harness.handle().subscriber_count(), 3, "Only live subscribers remain");

    harness.stop_service("svc").await.unwrap();
}

/// Shutdown clears all subscribers.
#[tokio::test]
async fn test_subscribers_cleared_on_shutdown() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let _rx1 = harness.handle().subscribe_state_changes();
    let _rx2 = harness.handle().subscribe_state_changes();
    let _rx3 = harness.handle().subscribe_state_changes();

    assert_eq!(harness.handle().subscriber_count(), 3);

    harness.handle().shutdown().await;

    // After shutdown the actor clears all subscribers
    assert_eq!(harness.handle().subscriber_count(), 0, "All subscribers cleared on shutdown");
}

// ============================================================================
// Category 3: Dep Watcher Cleanup
// ============================================================================

/// Dropping a dep watcher receiver prunes it on the next status change.
#[tokio::test]
async fn test_dep_watcher_pruned_on_receiver_drop() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let rx = harness.handle().watch_dep("svc");
    assert_eq!(harness.handle().dep_watcher_count(), 1);

    drop(rx);

    // Trigger a status change on 'svc' to prune dead dep watchers
    harness.start_service("svc").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(harness.handle().dep_watcher_count(), 0, "Dead dep watcher pruned");

    harness.stop_service("svc").await.unwrap();
}

/// Adding a new dep watcher prunes dead ones for the same service.
#[tokio::test]
async fn test_dep_watcher_pruned_on_new_watch() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let rx1 = harness.handle().watch_dep("svc");
    assert_eq!(harness.handle().dep_watcher_count(), 1);

    drop(rx1);

    let _rx2 = harness.handle().watch_dep("svc");
    assert_eq!(harness.handle().dep_watcher_count(), 1, "Dead watcher pruned on new watch");
}

/// Multiple watchers on the same dep, dropping one leaves the others.
#[tokio::test]
async fn test_multiple_watchers_same_dep() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc-a", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let _rx1 = harness.handle().watch_dep("svc-a");
    let rx2 = harness.handle().watch_dep("svc-a");
    let _rx3 = harness.handle().watch_dep("svc-a");

    assert_eq!(harness.handle().dep_watcher_count(), 3);

    drop(rx2);

    // Trigger status change to prune
    harness.start_service("svc-a").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(harness.handle().dep_watcher_count(), 2, "Only live watchers remain");

    harness.stop_service("svc-a").await.unwrap();
}

/// Shutdown clears all dep watchers.
#[tokio::test]
async fn test_dep_watchers_cleared_on_shutdown() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc-a", TestServiceBuilder::long_running().build())
        .add_service("svc-b", TestServiceBuilder::long_running().build())
        .add_service("svc-c", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    let _rx1 = harness.handle().watch_dep("svc-a");
    let _rx2 = harness.handle().watch_dep("svc-b");
    let _rx3 = harness.handle().watch_dep("svc-c");

    assert_eq!(harness.handle().dep_watcher_count(), 3);

    harness.handle().shutdown().await;

    assert_eq!(harness.handle().dep_watcher_count(), 0, "All dep watchers cleared on shutdown");
}

// ============================================================================
// Category 4: Skipped Service Cleanup
// ============================================================================

/// A skipped service should have no process, health check, or watcher resources.
#[tokio::test]
async fn test_skipped_service_has_no_resources() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc-a",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .build(),
                )
                .build(),
        )
        .add_service(
            "svc-b",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["svc-a".to_string()])
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Set both services to Skipped
    harness.handle().set_service_status("svc-a", ServiceStatus::Skipped).await.unwrap();
    harness.handle().set_service_status("svc-b", ServiceStatus::Skipped).await.unwrap();

    let state_a = harness.handle().get_service_state("svc-a").await.unwrap();
    assert!(state_a.pid.is_none(), "Skipped service should have no PID");

    let state_b = harness.handle().get_service_state("svc-b").await.unwrap();
    assert!(state_b.pid.is_none(), "Skipped service should have no PID");

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0);
    assert_eq!(counts.health_checks, 0);
    assert_eq!(counts.file_watchers, 0);
}

/// A chain A → B → C where A is skipped, cascading skip to B and C.
#[tokio::test]
async fn test_cascading_skip_chain() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("a", TestServiceBuilder::long_running().build())
        .add_service(
            "b",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["a".to_string()])
                .build(),
        )
        .add_service(
            "c",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["b".to_string()])
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Skip the whole chain
    harness.handle().set_service_status("a", ServiceStatus::Skipped).await.unwrap();
    harness.handle().set_service_status("b", ServiceStatus::Skipped).await.unwrap();
    harness.handle().set_service_status("c", ServiceStatus::Skipped).await.unwrap();

    assert_eq!(harness.get_status("a").await, Some(ServiceStatus::Skipped));
    assert_eq!(harness.get_status("b").await, Some(ServiceStatus::Skipped));
    assert_eq!(harness.get_status("c").await, Some(ServiceStatus::Skipped));

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0);
    assert_eq!(counts.health_checks, 0);
    assert_eq!(counts.file_watchers, 0);
    assert_eq!(counts.event_senders, 0);
}

// ============================================================================
// Category 5: Shutdown Cleanup
// ============================================================================

/// Full shutdown clears all resources: processes, health checks, watchers,
/// subscribers, dep watchers, event senders.
#[tokio::test]
async fn test_shutdown_full_cleanup() {
    let temp_dir = TempDir::new().unwrap();

    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.ts"), "// source").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc-a",
            TestServiceBuilder::long_running()
                .with_working_dir(temp_dir.path().to_path_buf())
                .with_restart_and_watch(
                    RestartPolicy::always(),
                    vec!["src/**/*.ts".to_string()],
                )
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .add_service(
            "svc-b",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Start services
    harness.start_service("svc-a").await.unwrap();
    harness.start_health_checker("svc-a").await.unwrap();
    harness.start_service("svc-b").await.unwrap();
    harness.start_health_checker("svc-b").await.unwrap();

    wait_for_healthy(harness.handle(), "svc-a", Duration::from_secs(5))
        .await
        .expect("svc-a should become healthy");
    wait_for_healthy(harness.handle(), "svc-b", Duration::from_secs(5))
        .await
        .expect("svc-b should become healthy");

    // Give file watcher time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create subscribers and dep watchers
    let _rx1 = harness.handle().subscribe_state_changes();
    let _rx2 = harness.handle().subscribe_state_changes();
    let _rx3 = harness.handle().subscribe_state_changes();
    let _dep_rx1 = harness.handle().watch_dep("svc-a");
    let _dep_rx2 = harness.handle().watch_dep("svc-b");

    assert_eq!(harness.handle().subscriber_count(), 3);
    assert_eq!(harness.handle().dep_watcher_count(), 2);

    // Verify resources are present before shutdown
    let counts = harness.handle().diagnostic_counts().await;
    assert!(counts.process_handles >= 2, "Should have process handles");
    assert!(counts.health_checks >= 2, "Should have health checks");

    // Stop services first (otherwise process handles remain)
    harness.stop_service("svc-a").await.unwrap();
    harness.stop_service("svc-b").await.unwrap();

    // Shutdown the actor
    harness.handle().shutdown().await;

    // Verify everything is cleaned up
    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0, "No process handles after shutdown");
    assert_eq!(counts.health_checks, 0, "No health checks after shutdown");
    assert_eq!(counts.file_watchers, 0, "No file watchers after shutdown");
    assert_eq!(counts.event_senders, 0, "No event senders after shutdown");

    assert_eq!(harness.handle().subscriber_count(), 0, "No subscribers after shutdown");
    assert_eq!(harness.handle().dep_watcher_count(), 0, "No dep watchers after shutdown");
}

// ============================================================================
// Category 6: Edge Cases
// ============================================================================

/// Rapid start/stop cycles should not leak resources.
#[tokio::test]
async fn test_rapid_start_stop_cycles_no_leak() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    for i in 0..5 {
        harness.start_service("svc").await.unwrap();
        harness.start_health_checker("svc").await.unwrap();

        wait_for_healthy(harness.handle(), "svc", Duration::from_secs(5))
            .await
            .unwrap_or_else(|_| panic!("Cycle {}: service should become healthy", i));

        harness.stop_service("svc").await.unwrap();

        wait_for_status(harness.handle(), "svc", ServiceStatus::Stopped, Duration::from_secs(5))
            .await
            .unwrap_or_else(|_| panic!("Cycle {}: service should become stopped", i));
    }

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0, "No leaked process handles after cycles");
    assert_eq!(counts.health_checks, 0, "No leaked health checks after cycles");
    assert_eq!(counts.file_watchers, 0, "No leaked file watchers after cycles");

    assert_eq!(harness.handle().subscriber_count(), 0, "No leaked subscribers");
    assert_eq!(harness.handle().dep_watcher_count(), 0, "No leaked dep watchers");
}

/// Event senders are properly managed: create, remove, and shutdown.
#[tokio::test]
async fn test_event_sender_cleanup() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("svc-a", TestServiceBuilder::long_running().build())
        .add_service("svc-b", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Create event channels for both services
    let _rx_a = harness.handle().create_event_channel("svc-a").await;
    let _rx_b = harness.handle().create_event_channel("svc-b").await;

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.event_senders, 2, "Should have 2 event senders");

    // Remove one event channel
    harness.handle().remove_event_channel("svc-a").await;

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.event_senders, 1, "Should have 1 event sender after remove");

    // Shutdown clears remaining
    harness.handle().shutdown().await;

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.event_senders, 0, "No event senders after shutdown");
}

/// Stopping all services cleans up all resources.
#[tokio::test]
async fn test_stop_all_services_cleans_resources() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc-a",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .add_service(
            "svc-b",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .add_service(
            "svc-c",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Start all services with health checks
    for svc in &["svc-a", "svc-b", "svc-c"] {
        harness.start_service(svc).await.unwrap();
        harness.start_health_checker(svc).await.unwrap();
    }

    for svc in &["svc-a", "svc-b", "svc-c"] {
        wait_for_healthy(harness.handle(), svc, Duration::from_secs(5))
            .await
            .unwrap_or_else(|_| panic!("{} should become healthy", svc));
    }

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 3);
    assert_eq!(counts.health_checks, 3);

    // Stop all
    harness.stop_all().await.unwrap();

    for svc in &["svc-a", "svc-b", "svc-c"] {
        assert_eq!(
            harness.get_status(svc).await,
            Some(ServiceStatus::Stopped),
            "{} should be stopped",
            svc
        );
    }

    let counts = harness.handle().diagnostic_counts().await;
    assert_eq!(counts.process_handles, 0, "No process handles after stop_all");
    assert_eq!(counts.health_checks, 0, "No health checks after stop_all");
}

/// Subscribers are not leaked when a service restarts (on-failure policy).
#[tokio::test]
async fn test_subscriber_not_leaked_across_restart() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "svc",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::on_failure())
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Set up process exit handler for auto-restart
    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    // Subscribe before starting
    let _rx = harness.handle().subscribe_state_changes();
    let initial_sub_count = harness.handle().subscriber_count();

    harness.start_service("svc").await.unwrap();

    // Wait for the restart cycle (exit code 1 → restart → exit again)
    // The service exits with code 1, gets restarted, then exits again
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Subscriber count should be stable (not growing)
    let final_sub_count = harness.handle().subscriber_count();
    assert_eq!(
        initial_sub_count, final_sub_count,
        "Subscriber count should remain stable across restarts"
    );
    assert_eq!(harness.handle().dep_watcher_count(), 0, "No dep watchers leaked");
}
