//! Health check transition tests

use kepler_daemon::config::{HookList, ServiceHooks};
use kepler_daemon::state::ServiceStatus;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHealthCheckBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use kepler_tests::helpers::wait_utils::{wait_for_healthy, wait_for_unhealthy};
use std::time::Duration;
use tempfile::TempDir;

/// Running → Healthy on first health check pass
#[tokio::test]
async fn test_running_to_healthy_transition() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
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

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Running));

    harness.start_health_checker("test").await.unwrap();

    // Wait for healthy status
    let result = wait_for_healthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;

    assert!(result.is_ok(), "Service should become healthy");
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Healthy));

    harness.stop_service("test").await.unwrap();
}

/// Running → Unhealthy after retries exhausted
#[tokio::test]
async fn test_running_to_unhealthy_after_retries() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_unhealthy()
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

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    // Wait for unhealthy status (after 2 retries)
    let result = wait_for_unhealthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;

    assert!(result.is_ok(), "Service should become unhealthy after retries exhausted");
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Unhealthy));

    harness.stop_service("test").await.unwrap();
}

/// Unhealthy → Healthy when check starts passing
#[tokio::test]
async fn test_unhealthy_to_healthy_recovery() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    // Health check passes if marker file exists
    let marker_path = marker.marker_path("health");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::file_exists(&marker_path.to_string_lossy())
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

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    // Wait for unhealthy (file doesn't exist yet)
    let result = wait_for_unhealthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;
    assert!(result.is_ok(), "Service should become unhealthy initially");

    // Create the marker file so health checks pass
    std::fs::write(&marker_path, "").unwrap();

    // Wait for healthy recovery
    let result = wait_for_healthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;
    assert!(result.is_ok(), "Service should recover to healthy");
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Healthy));

    harness.stop_service("test").await.unwrap();
}

/// Health checks wait for start_period
#[tokio::test]
async fn test_start_period_delay() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_start_period(Duration::from_millis(500))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    // Immediately after starting, should still be Running (not Healthy yet)
    // because of start_period delay
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Running));

    // After start_period + interval, should be healthy
    let result = wait_for_healthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;
    assert!(result.is_ok());

    harness.stop_service("test").await.unwrap();
}

/// Slow checks fail due to timeout
#[tokio::test]
async fn test_health_check_timeout() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    // Check sleeps for 2 seconds, but timeout is 200ms
                    TestHealthCheckBuilder::slow_check(2)
                        .with_timeout(Duration::from_millis(200))
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

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    // Wait for unhealthy (health check should timeout)
    let result = wait_for_unhealthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;

    assert!(result.is_ok(), "Service should become unhealthy due to timeout");
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Unhealthy));

    harness.stop_service("test").await.unwrap();
}

/// Direct command format works (e.g., `["true"]`)
#[tokio::test]
async fn test_direct_command_health_check() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::command("true", &[])
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    let result = wait_for_healthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;

    assert!(result.is_ok(), "Direct command health check should work");

    harness.stop_service("test").await.unwrap();
}

/// Shell script format works (e.g., `["sh", "-c", "exit 0"]`)
#[tokio::test]
async fn test_shell_health_check() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::shell("exit 0")
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    let result = wait_for_healthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;

    assert!(result.is_ok(), "Shell health check should work");

    harness.stop_service("test").await.unwrap();
}

/// Hook fires on transition to Healthy
#[tokio::test]
async fn test_on_healthcheck_success_hook() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        post_healthcheck_success: Some(HookList(vec![marker.create_marker_hook("success")])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
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
    harness.start_health_checker("test").await.unwrap();

    // Wait for the hook marker to appear
    let hook_fired = marker
        .wait_for_marker("success", Duration::from_secs(5))
        .await;

    assert!(hook_fired, "on_healthcheck_success hook should fire");

    harness.stop_service("test").await.unwrap();
}

/// Hook fires on transition to Unhealthy
#[tokio::test]
async fn test_on_healthcheck_fail_hook() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        post_healthcheck_fail: Some(HookList(vec![marker.create_marker_hook("fail")])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_unhealthy()
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
    harness.start_health_checker("test").await.unwrap();

    // Wait for the hook marker to appear
    let hook_fired = marker
        .wait_for_marker("fail", Duration::from_secs(5))
        .await;

    assert!(hook_fired, "on_healthcheck_fail hook should fire");

    harness.stop_service("test").await.unwrap();
}

/// Hook fires once per transition, not every check
#[tokio::test]
async fn test_healthcheck_hook_fires_once_per_transition() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        post_healthcheck_success: Some(HookList(vec![marker.create_timestamped_marker_hook("success")])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_healthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(1)
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
    harness.start_health_checker("test").await.unwrap();

    // Wait for the service to become healthy
    wait_for_healthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Wait a bit longer for multiple health check cycles
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The hook should have fired exactly once (on transition to Healthy)
    let line_count = marker.count_marker_lines("success");
    assert_eq!(
        line_count, 1,
        "Hook should fire exactly once per transition, not on every check. Got {} fires",
        line_count
    );

    harness.stop_service("test").await.unwrap();
}

/// Health check failure counter resets when service restarts
#[tokio::test]
async fn test_health_check_failures_reset_on_restart() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(
                    TestHealthCheckBuilder::always_unhealthy()
                        .with_interval(Duration::from_millis(100))
                        .with_retries(3)
                        .build(),
                )
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await.unwrap();

    // Wait until service becomes unhealthy (failures >= retries)
    let result = wait_for_unhealthy(
        harness.handle(),
        "test",
        Duration::from_secs(5),
    )
    .await;
    assert!(result.is_ok(), "Service should become unhealthy");

    // Verify failures accumulated
    let state = harness.handle().get_service_state("test").await.unwrap();
    assert!(
        state.health_check_failures >= 3,
        "Should have at least 3 failures, got {}",
        state.health_check_failures
    );

    // Stop and restart the service
    harness.stop_service("test").await.unwrap();
    harness.start_service("test").await.unwrap();

    // Failure counter should be reset after restart
    let state = harness.handle().get_service_state("test").await.unwrap();
    assert_eq!(
        state.health_check_failures, 0,
        "Health check failure counter should reset on restart, got {}",
        state.health_check_failures
    );

    harness.stop_service("test").await.unwrap();
}
