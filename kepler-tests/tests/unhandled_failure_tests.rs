//! Tests for unhandled service failure detection.
//!
//! Verifies that `ConfigEvent::UnhandledFailure` is emitted when a service
//! reaches a terminal failure state (Failed/Killed), won't restart, and no
//! other service has a `service_failed` or `service_stopped` dependency on it.

use kepler_daemon::config::{DependencyCondition, DependencyConfig, DependencyEntry, DependsOn, RestartPolicy};
use kepler_daemon::config_actor::ConfigEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use std::collections::HashMap;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;

// ============================================================================
// Helper
// ============================================================================

/// Collect UnhandledFailure events from a config event receiver, up to a timeout.
async fn collect_unhandled_failures(
    rx: &mut mpsc::UnboundedReceiver<ConfigEvent>,
    timeout: Duration,
) -> Vec<(String, Option<i32>)> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ConfigEvent::UnhandledFailure { service, exit_code })) => {
                events.push((service, exit_code));
            }
            Ok(Some(_)) => continue, // StatusChange/Ready/Quiescent — skip
            Ok(None) => break,       // Channel closed
            Err(_) => break,         // Timeout
        }
    }
    events
}

/// Wait for a specific status on a service, with timeout.
async fn wait_for_status(
    rx: &mut mpsc::UnboundedReceiver<ConfigEvent>,
    service_name: &str,
    target_status: ServiceStatus,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ConfigEvent::StatusChange(change)))
                if change.service == service_name && change.status == target_status =>
            {
                return true;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return false,
        }
    }
}

// ============================================================================
// Tests: Unhandled failure emitted
// ============================================================================

/// A service that exits with non-zero code, restart: no, no handler → UnhandledFailure emitted.
#[tokio::test]
async fn test_unhandled_failure_emitted_on_failed_service() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "failing",
            TestServiceBuilder::exit_with_code(42)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();

    // Subscribe before starting
    let mut rx = harness.handle().subscribe_state_changes();

    // Set up exit handler so process exit is recorded + status set to Failed
    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("failing").await.unwrap();

    // Wait for UnhandledFailure event
    let failures = collect_unhandled_failures(&mut rx, Duration::from_secs(5)).await;

    assert_eq!(failures.len(), 1, "Should emit exactly one UnhandledFailure");
    assert_eq!(failures[0].0, "failing");
    assert_eq!(failures[0].1, Some(42));
}

/// A service that exits with code 0 → status is Exited (not Failed) → no UnhandledFailure.
#[tokio::test]
async fn test_no_unhandled_failure_on_clean_exit() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "clean",
            TestServiceBuilder::exit_with_code(0)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("clean").await.unwrap();

    // Wait for service to reach Exited status
    assert!(
        wait_for_status(&mut rx, "clean", ServiceStatus::Exited, Duration::from_secs(5)).await,
        "Service should reach Exited status"
    );

    // Give a bit more time for any spurious UnhandledFailure
    let failures = collect_unhandled_failures(&mut rx, Duration::from_millis(500)).await;
    assert!(failures.is_empty(), "Clean exit should not emit UnhandledFailure");
}

// ============================================================================
// Tests: Failure handled by dependency
// ============================================================================

/// A failing service with a handler (service_failed dep) → no UnhandledFailure.
#[tokio::test]
async fn test_no_unhandled_failure_when_service_failed_handler_exists() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "failing",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .add_service(
            "handler",
            TestServiceBuilder::echo("handling failure")
                .with_restart(RestartPolicy::no())
                .with_depends_on_extended(DependsOn(vec![DependencyEntry::Extended(
                    HashMap::from([(
                        "failing".to_string(),
                        DependencyConfig {
                            condition: DependencyCondition::ServiceFailed,
                            ..Default::default()
                        },
                    )]),
                )]))
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("failing").await.unwrap();

    // Wait for failing to reach Failed status
    assert!(
        wait_for_status(&mut rx, "failing", ServiceStatus::Failed, Duration::from_secs(5)).await,
        "Service should reach Failed status"
    );

    // No UnhandledFailure should fire because "handler" has service_failed dep on "failing"
    let failures = collect_unhandled_failures(&mut rx, Duration::from_millis(500)).await;
    assert!(failures.is_empty(), "Failure should be handled by service_failed dependency");
}

/// A failing service with a service_stopped handler → no UnhandledFailure.
#[tokio::test]
async fn test_no_unhandled_failure_when_service_stopped_handler_exists() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "failing",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .add_service(
            "handler",
            TestServiceBuilder::echo("handling stop")
                .with_restart(RestartPolicy::no())
                .with_depends_on_extended(DependsOn(vec![DependencyEntry::Extended(
                    HashMap::from([(
                        "failing".to_string(),
                        DependencyConfig {
                            condition: DependencyCondition::ServiceStopped,
                            ..Default::default()
                        },
                    )]),
                )]))
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("failing").await.unwrap();

    // Wait for failing to reach Failed status
    assert!(
        wait_for_status(&mut rx, "failing", ServiceStatus::Failed, Duration::from_secs(5)).await,
        "Service should reach Failed status"
    );

    // No UnhandledFailure because service_stopped also counts as a handler
    let failures = collect_unhandled_failures(&mut rx, Duration::from_millis(500)).await;
    assert!(failures.is_empty(), "Failure should be handled by service_stopped dependency");
}

// ============================================================================
// Tests: Restart policy suppresses unhandled failure
// ============================================================================

/// A failing service with restart: on-failure → will restart → no UnhandledFailure.
#[tokio::test]
async fn test_no_unhandled_failure_when_restart_policy_applies() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "failing",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::on_failure())
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    // Spawn exit handler — it will restart the service
    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("failing").await.unwrap();

    // The service should restart (exit handler will set it to Running again).
    // No UnhandledFailure should be emitted because restart policy says to restart.
    // Wait a bit to collect any events.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let failures = collect_unhandled_failures(&mut rx, Duration::from_millis(500)).await;
    assert!(failures.is_empty(), "Service with restart: on-failure should not emit UnhandledFailure");
}

/// A service with restart: always → no UnhandledFailure even on failure.
#[tokio::test]
async fn test_no_unhandled_failure_with_restart_always() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "failing",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::always())
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("failing").await.unwrap();

    tokio::time::sleep(Duration::from_secs(1)).await;
    let failures = collect_unhandled_failures(&mut rx, Duration::from_millis(500)).await;
    assert!(failures.is_empty(), "Service with restart: always should not emit UnhandledFailure");
}

// ============================================================================
// Tests: Multiple services
// ============================================================================

/// Two failing services, only one has a handler → one UnhandledFailure emitted.
#[tokio::test]
async fn test_multiple_services_only_unhandled_one_emits() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "handled_fail",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .add_service(
            "unhandled_fail",
            TestServiceBuilder::exit_with_code(2)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .add_service(
            "error_handler",
            TestServiceBuilder::echo("handling")
                .with_restart(RestartPolicy::no())
                .with_depends_on_extended(DependsOn(vec![DependencyEntry::Extended(
                    HashMap::from([(
                        "handled_fail".to_string(),
                        DependencyConfig {
                            condition: DependencyCondition::ServiceFailed,
                            ..Default::default()
                        },
                    )]),
                )]))
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    // Start both failing services
    harness.start_service("handled_fail").await.unwrap();
    harness.start_service("unhandled_fail").await.unwrap();

    let failures = collect_unhandled_failures(&mut rx, Duration::from_secs(5)).await;

    assert_eq!(failures.len(), 1, "Only unhandled failure should emit");
    assert_eq!(failures[0].0, "unhandled_fail");
    assert_eq!(failures[0].1, Some(2));
}

/// A handler service that itself fails → its failure is also unhandled (nothing handles the handler).
#[tokio::test]
async fn test_handler_failure_is_independently_checked() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "failing",
            TestServiceBuilder::exit_with_code(1)
                .with_restart(RestartPolicy::no())
                .build(),
        )
        .add_service(
            "handler",
            // Handler itself fails with exit code 3
            TestServiceBuilder::exit_with_code(3)
                .with_restart(RestartPolicy::no())
                .with_depends_on_extended(DependsOn(vec![DependencyEntry::Extended(
                    HashMap::from([(
                        "failing".to_string(),
                        DependencyConfig {
                            condition: DependencyCondition::ServiceFailed,
                            ..Default::default()
                        },
                    )]),
                )]))
                .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path()).await.unwrap();
    let mut rx = harness.handle().subscribe_state_changes();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    // Start "failing" — its failure is handled by "handler"
    harness.start_service("failing").await.unwrap();
    // Start "handler" — it will also fail, and nothing handles *its* failure
    harness.start_service("handler").await.unwrap();

    let failures = collect_unhandled_failures(&mut rx, Duration::from_secs(5)).await;

    // "failing" is handled (handler has service_failed dep), but "handler" itself is unhandled
    let unhandled_names: Vec<&str> = failures.iter().map(|(s, _)| s.as_str()).collect();
    assert!(
        !unhandled_names.contains(&"failing"),
        "failing should be handled by handler's service_failed dep"
    );
    assert!(
        unhandled_names.contains(&"handler"),
        "handler's own failure should be unhandled"
    );
}
