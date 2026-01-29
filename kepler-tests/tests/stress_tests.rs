//! Stress tests for Kepler daemon
//!
//! These tests verify system behavior under load conditions:
//! - Large log file handling
//! - Rapid process exits
//! - Many concurrent configs
//! - Rapid file changes

use kepler_daemon::logs::{LogBuffer, LogStream, SharedLogBuffer, DEFAULT_MAX_BYTES};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use std::time::Duration;
use tempfile::TempDir;

/// Test that large log files don't cause OOM when reading
#[test]
fn test_large_log_file_bounded_read() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create a log buffer
    let mut buffer = LogBuffer::new(logs_dir.clone());

    // Write many log lines (simulate a service that outputs a lot)
    let num_lines = 50_000;
    for i in 0..num_lines {
        buffer.push(
            "test-service".to_string(),
            format!("Log line {} with some content to make it realistic", i),
            LogStream::Stdout,
        );
    }

    // Reading with bounded tail should not OOM
    let entries = buffer.tail(1000, Some("test-service"));
    assert_eq!(entries.len(), 1000);

    // Reading with bounds should respect the limit
    let entries = buffer.tail_bounded(100, Some("test-service"), Some(1024 * 1024));
    assert!(entries.len() <= 100);
}

/// Test that log rotation works correctly under heavy write load
#[test]
fn test_log_rotation_under_load() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create a log buffer with small rotation size (10KB)
    let mut buffer = LogBuffer::with_rotation(
        logs_dir.clone(),
        10 * 1024,  // 10KB max size
        3,          // Keep 3 rotated files
    );

    // Write enough data to trigger multiple rotations
    for i in 0..1000 {
        buffer.push(
            "test-service".to_string(),
            format!("Log line {} with some padding to fill up the file quickly: {}", i, "x".repeat(100)),
            LogStream::Stdout,
        );
    }

    // Check that rotated files exist
    let log_file = logs_dir.join("test-service.log");
    assert!(log_file.exists(), "Main log file should exist");

    // At least one rotated file should exist
    let rotated_1 = format!("{}.1", log_file.display());
    assert!(
        std::path::Path::new(&rotated_1).exists(),
        "At least one rotated file should exist"
    );

    // Old rotations beyond max_files should be cleaned up
    let rotated_4 = format!("{}.4", log_file.display());
    assert!(
        !std::path::Path::new(&rotated_4).exists(),
        "Rotated file beyond max should not exist"
    );
}

/// Test that sequence metadata is persisted and loaded correctly
#[test]
fn test_sequence_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create and populate a buffer
    {
        let mut buffer = LogBuffer::new(logs_dir.clone());
        for i in 0..500 {
            buffer.push(
                "test-service".to_string(),
                format!("Line {}", i),
                LogStream::Stdout,
            );
        }
    }

    // Create a new buffer - should load sequence from file
    let buffer = LogBuffer::new(logs_dir);
    let seq = buffer.current_sequence();

    // Sequence should be approximately right (uses estimation if no metadata file)
    assert!(seq > 0, "Sequence should be restored from metadata or estimated");
}

/// Test rapid start/stop cycles don't cause issues
#[tokio::test]
async fn test_rapid_start_stop_cycles() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("test", TestServiceBuilder::new(vec!["true".to_string()]).build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Perform rapid start/stop cycles
    for i in 0..20 {
        harness.start_service("test").await.unwrap();
        // Small delay to let it start
        tokio::time::sleep(Duration::from_millis(50)).await;
        harness.stop_service("test").await.unwrap();

        if i % 5 == 0 {
            // Occasional longer pause
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Should complete without panicking or deadlocking
}

/// Test multiple concurrent services with heavy logging
#[tokio::test]
async fn test_concurrent_services_heavy_logging() {
    let temp_dir = TempDir::new().unwrap();

    // Create multiple services
    let mut builder = TestConfigBuilder::new();
    for i in 0..10 {
        builder = builder.add_service(
            &format!("service{}", i),
            TestServiceBuilder::long_running().build(),
        );
    }
    let config = builder.build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start all services
    for i in 0..10 {
        harness.start_service(&format!("service{}", i)).await.unwrap();
    }

    // Let them run briefly
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stop all services
    harness.stop_all().await.unwrap();
}

/// Test bounded log reading with very large max_lines request
#[test]
fn test_bounded_read_respects_limits() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let shared_buffer = SharedLogBuffer::new(logs_dir);

    // Write some logs
    for i in 0..1000 {
        shared_buffer.push(
            "test-service".to_string(),
            format!("Line {}", i),
            LogStream::Stdout,
        );
    }

    // Request more lines than the default limit - should be capped
    let entries = shared_buffer.tail_bounded(
        100_000, // Way more than DEFAULT_MAX_LINES
        Some("test-service"),
        Some(DEFAULT_MAX_BYTES),
    );

    // Should be capped at the internal limit (10,000 lines)
    assert!(
        entries.len() <= 10_000,
        "Should respect internal line limit"
    );
}

/// Test that clearing logs during heavy writes doesn't cause issues
#[test]
fn test_clear_during_heavy_writes() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let shared_buffer = SharedLogBuffer::new(logs_dir);

    // Write logs and clear repeatedly
    for cycle in 0..10 {
        for i in 0..100 {
            shared_buffer.push(
                "test-service".to_string(),
                format!("Cycle {} Line {}", cycle, i),
                LogStream::Stdout,
            );
        }
        shared_buffer.clear();
    }

    // Should have 0 entries after final clear
    let entries = shared_buffer.tail(1000, None);
    assert_eq!(entries.len(), 0, "Buffer should be empty after clear");
}

/// Test dependency level calculation for parallel starts
#[test]
fn test_dependency_levels() {
    use kepler_daemon::deps::get_start_levels;
    use kepler_daemon::config::ServiceConfig;
    use std::collections::HashMap;

    fn make_service(deps: Vec<&str>) -> ServiceConfig {
        ServiceConfig {
            command: vec!["test".to_string()],
            working_dir: None,
            environment: vec![],
            env_file: None,
            sys_env: Default::default(),
            restart: Default::default(),
            depends_on: deps.into_iter().map(String::from).collect(),
            healthcheck: None,
            hooks: None,
            logs: None,
            user: None,
            group: None,
            limits: None,
        }
    }

    // Create a dependency graph:
    // Level 0: a, b (no deps)
    // Level 1: c (depends on a), d (depends on b)
    // Level 2: e (depends on c and d)
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec![]));
    services.insert("c".to_string(), make_service(vec!["a"]));
    services.insert("d".to_string(), make_service(vec!["b"]));
    services.insert("e".to_string(), make_service(vec!["c", "d"]));

    let levels = get_start_levels(&services).unwrap();

    assert_eq!(levels.len(), 3, "Should have 3 levels");

    // Level 0: a and b
    assert!(levels[0].contains(&"a".to_string()));
    assert!(levels[0].contains(&"b".to_string()));

    // Level 1: c and d
    assert!(levels[1].contains(&"c".to_string()));
    assert!(levels[1].contains(&"d".to_string()));

    // Level 2: e
    assert!(levels[2].contains(&"e".to_string()));
}

/// Test that pre-allocated format buffer reduces allocations
#[test]
fn test_format_buffer_reuse() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let mut buffer = LogBuffer::new(logs_dir);

    // Write many entries - format buffer should be reused
    for i in 0..1000 {
        buffer.push(
            "test".to_string(),
            format!("Line {}", i),
            LogStream::Stdout,
        );
    }

    // This test mainly verifies no panics occur
    // The optimization benefit is in reduced allocations (not easily measurable in test)
}
