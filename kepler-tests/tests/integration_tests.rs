//! Full lifecycle integration tests

use kepler_daemon::config::ServiceHooks;
use kepler_daemon::state::ServiceStatus;
use kepler_tests::helpers::config_builder::{
    TestConfigBuilder, TestHealthCheckBuilder, TestServiceBuilder,
};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use kepler_tests::helpers::wait_utils::{wait_for_healthy, wait_for_unhealthy};
use std::time::Duration;
use tempfile::TempDir;

/// Start → unhealthy → healthy → stop with all hooks
#[tokio::test]
async fn test_full_lifecycle_with_healthcheck_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    // Health check uses a marker file
    let health_marker_path = marker.marker_path("health_status");

    let hooks = ServiceHooks {
        on_init: Some(marker.create_timestamped_marker_hook("on_init")),
        on_start: Some(marker.create_timestamped_marker_hook("on_start")),
        on_stop: Some(marker.create_timestamped_marker_hook("on_stop")),
        on_healthcheck_success: Some(marker.create_timestamped_marker_hook("on_healthy")),
        on_healthcheck_fail: Some(marker.create_timestamped_marker_hook("on_unhealthy")),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::file_exists(&health_marker_path.to_string_lossy())
                        .with_interval(Duration::from_millis(100))
                        .with_retries(2)
                        .build(),
                )
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // 1. Start the service
    harness.start_service("test").await.unwrap();
    assert!(
        marker.wait_for_marker("on_init", Duration::from_secs(2)).await,
        "on_init should fire"
    );
    assert!(
        marker.wait_for_marker("on_start", Duration::from_secs(2)).await,
        "on_start should fire"
    );

    // 2. Start health checker - service should become unhealthy (no marker file)
    harness.start_health_checker("test").unwrap();

    wait_for_unhealthy(
        harness.state(),
        harness.config_path(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    assert!(
        marker.wait_for_marker("on_unhealthy", Duration::from_secs(2)).await,
        "on_healthcheck_fail should fire"
    );

    // 3. Create the marker file - service should become healthy
    std::fs::write(&health_marker_path, "ok").unwrap();

    wait_for_healthy(
        harness.state(),
        harness.config_path(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    assert!(
        marker.wait_for_marker("on_healthy", Duration::from_secs(2)).await,
        "on_healthcheck_success should fire"
    );

    // 4. Stop the service
    harness.stop_service("test").await.unwrap();

    assert!(
        marker.wait_for_marker("on_stop", Duration::from_secs(2)).await,
        "on_stop should fire"
    );

    // Verify final status
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Stopped));

    // Check hook counts - each should fire exactly once
    assert_eq!(marker.count_marker_lines("on_init"), 1);
    assert_eq!(marker.count_marker_lines("on_start"), 1);
    assert_eq!(marker.count_marker_lines("on_unhealthy"), 1);
    assert_eq!(marker.count_marker_lines("on_healthy"), 1);
    assert_eq!(marker.count_marker_lines("on_stop"), 1);
}

/// Services with depends_on start in correct order
#[tokio::test]
async fn test_service_dependencies_order() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("start_order.txt");

    // Create services with dependencies: frontend -> backend -> database
    let frontend_hooks = ServiceHooks {
        on_start: Some(kepler_daemon::config::HookCommand::Script {
            run: format!("echo 'frontend' >> {}", order_file.display()),
        }),
        ..Default::default()
    };

    let backend_hooks = ServiceHooks {
        on_start: Some(kepler_daemon::config::HookCommand::Script {
            run: format!("echo 'backend' >> {}", order_file.display()),
        }),
        ..Default::default()
    };

    let database_hooks = ServiceHooks {
        on_start: Some(kepler_daemon::config::HookCommand::Script {
            run: format!("echo 'database' >> {}", order_file.display()),
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "frontend",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["backend".to_string()])
                .with_hooks(frontend_hooks)
                .build(),
        )
        .add_service(
            "backend",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["database".to_string()])
                .with_hooks(backend_hooks)
                .build(),
        )
        .add_service(
            "database",
            TestServiceBuilder::long_running()
                .with_hooks(database_hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start in correct order: database first, then backend, then frontend
    harness.start_service("database").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.start_service("backend").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.start_service("frontend").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify order in file
    let order_content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let order: Vec<&str> = order_content.lines().collect();

    assert_eq!(order.len(), 3, "All three services should have started");
    assert_eq!(order[0], "database", "Database should start first");
    assert_eq!(order[1], "backend", "Backend should start second");
    assert_eq!(order[2], "frontend", "Frontend should start third");

    // Cleanup
    harness.stop_all().await.unwrap();
}

/// Multiple unhealthy↔healthy transitions fire hooks correctly
#[tokio::test]
async fn test_multiple_health_transitions() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    // Health check uses a marker file
    let health_marker_path = marker.marker_path("health_status");

    let hooks = ServiceHooks {
        on_healthcheck_success: Some(marker.create_timestamped_marker_hook("healthy")),
        on_healthcheck_fail: Some(marker.create_timestamped_marker_hook("unhealthy")),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::file_exists(&health_marker_path.to_string_lossy())
                        .with_interval(Duration::from_millis(100))
                        .with_retries(2)
                        .build(),
                )
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").unwrap();

    // Transition 1: Running -> Unhealthy (no marker file)
    wait_for_unhealthy(
        harness.state(),
        harness.config_path(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Transition 2: Unhealthy -> Healthy (create marker)
    std::fs::write(&health_marker_path, "ok").unwrap();
    wait_for_healthy(
        harness.state(),
        harness.config_path(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Transition 3: Healthy -> Unhealthy (remove marker)
    std::fs::remove_file(&health_marker_path).unwrap();
    wait_for_unhealthy(
        harness.state(),
        harness.config_path(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Transition 4: Unhealthy -> Healthy (create marker again)
    std::fs::write(&health_marker_path, "ok").unwrap();
    wait_for_healthy(
        harness.state(),
        harness.config_path(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Give hooks time to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify hook counts
    let healthy_count = marker.count_marker_lines("healthy");
    let unhealthy_count = marker.count_marker_lines("unhealthy");

    assert_eq!(
        healthy_count, 2,
        "on_healthcheck_success should fire twice (2 transitions to healthy)"
    );
    assert_eq!(
        unhealthy_count, 2,
        "on_healthcheck_fail should fire twice (2 transitions to unhealthy)"
    );

    harness.stop_service("test").await.unwrap();
}

/// Service can be stopped and restarted multiple times
#[tokio::test]
async fn test_service_stop_restart_cycle() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        on_init: Some(marker.create_timestamped_marker_hook("init")),
        on_start: Some(marker.create_timestamped_marker_hook("start")),
        on_stop: Some(marker.create_timestamped_marker_hook("stop")),
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

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Cycle 1
    harness.start_service("test").await.unwrap();
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Running));
    harness.stop_service("test").await.unwrap();
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Stopped));

    // Cycle 2
    harness.start_service("test").await.unwrap();
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Running));
    harness.stop_service("test").await.unwrap();
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Stopped));

    // Cycle 3
    harness.start_service("test").await.unwrap();
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Running));
    harness.stop_service("test").await.unwrap();
    assert_eq!(harness.get_status("test"), Some(ServiceStatus::Stopped));

    // Wait for hooks to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify counts
    assert_eq!(marker.count_marker_lines("init"), 1, "on_init only fires once");
    assert_eq!(marker.count_marker_lines("start"), 3, "on_start fires each start");
    assert_eq!(marker.count_marker_lines("stop"), 3, "on_stop fires each stop");
}

/// Multiple services can run concurrently
#[tokio::test]
async fn test_multiple_concurrent_services() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("service1", TestServiceBuilder::long_running().build())
        .add_service("service2", TestServiceBuilder::long_running().build())
        .add_service("service3", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start all services
    harness.start_service("service1").await.unwrap();
    harness.start_service("service2").await.unwrap();
    harness.start_service("service3").await.unwrap();

    // All should be running
    assert_eq!(harness.get_status("service1"), Some(ServiceStatus::Running));
    assert_eq!(harness.get_status("service2"), Some(ServiceStatus::Running));
    assert_eq!(harness.get_status("service3"), Some(ServiceStatus::Running));

    // Stop all
    harness.stop_all().await.unwrap();

    // All should be stopped
    assert_eq!(harness.get_status("service1"), Some(ServiceStatus::Stopped));
    assert_eq!(harness.get_status("service2"), Some(ServiceStatus::Stopped));
    assert_eq!(harness.get_status("service3"), Some(ServiceStatus::Stopped));
}

/// Health checks work correctly with multiple services
#[tokio::test]
async fn test_multiple_services_with_healthchecks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let marker1_path = marker.marker_path("health1");
    let marker2_path = marker.marker_path("health2");

    let config = TestConfigBuilder::new()
        .add_service(
            "service1",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::file_exists(&marker1_path.to_string_lossy())
                        .with_interval(Duration::from_millis(100))
                        .with_retries(2)
                        .build(),
                )
                .build(),
        )
        .add_service(
            "service2",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::file_exists(&marker2_path.to_string_lossy())
                        .with_interval(Duration::from_millis(100))
                        .with_retries(2)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start both services
    harness.start_service("service1").await.unwrap();
    harness.start_service("service2").await.unwrap();
    harness.start_health_checker("service1").unwrap();
    harness.start_health_checker("service2").unwrap();

    // Both should become unhealthy (no marker files)
    wait_for_unhealthy(
        harness.state(),
        harness.config_path(),
        "service1",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    wait_for_unhealthy(
        harness.state(),
        harness.config_path(),
        "service2",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Make service1 healthy
    std::fs::write(&marker1_path, "ok").unwrap();

    wait_for_healthy(
        harness.state(),
        harness.config_path(),
        "service1",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // service1 should be healthy, service2 still unhealthy
    assert_eq!(harness.get_status("service1"), Some(ServiceStatus::Healthy));
    assert_eq!(harness.get_status("service2"), Some(ServiceStatus::Unhealthy));

    // Make service2 healthy
    std::fs::write(&marker2_path, "ok").unwrap();

    wait_for_healthy(
        harness.state(),
        harness.config_path(),
        "service2",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Both should now be healthy
    assert_eq!(harness.get_status("service1"), Some(ServiceStatus::Healthy));
    assert_eq!(harness.get_status("service2"), Some(ServiceStatus::Healthy));

    harness.stop_all().await.unwrap();
}
