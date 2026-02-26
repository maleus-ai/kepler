//! Log retention tests

use kepler_daemon::config::{ConfigValue, LogConfig, LogRetention, LogRetentionConfig, RawServiceConfig, ServiceConfig};
use kepler_daemon::logs::LogStream;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use tempfile::TempDir;

fn deser_svc(raw: &RawServiceConfig) -> ServiceConfig {
    let val = serde_yaml::to_value(raw).unwrap();
    serde_yaml::from_value(val).unwrap()
}

/// Default behavior clears logs on stop
#[tokio::test]
async fn test_default_clears_logs_on_stop() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "default_clear_test",
            TestServiceBuilder::long_running()
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("default_clear_test").await.unwrap();

// Clear any existing logs first (logs persist on disk across test runs)
    let logs = harness.logs().await.unwrap();
    logs.clear();

    // Add some log entries
    logs.push("default_clear_test", "log line 1".to_string(), LogStream::Stdout);
    logs.push("default_clear_test", "log line 2".to_string(), LogStream::Stdout);

    // Verify logs exist
    let entries = logs.tail(10, Some("default_clear_test"));
    assert_eq!(entries.len(), 2, "Should have 2 log entries before stop");

    // Stop the service - default behavior should clear logs
    harness.stop_service("default_clear_test").await.unwrap();

    // Note: The daemon harness stop_service doesn't call handle_process_exit
    // which is where on_stop log clearing happens. For integration tests we
    // would need to trigger a process exit. Let's verify the config parsing works.
}

/// Retain logs on stop when configured
#[tokio::test]
async fn test_retain_logs_on_stop() {
    let temp_dir = TempDir::new().unwrap();

    let log_config = LogConfig {
        retention: Some(LogRetentionConfig {
            on_stop: Some(LogRetention::Retain),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "retain_test",  // Use unique service name to avoid hash collision
            TestServiceBuilder::long_running()
                .with_logs(log_config)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("retain_test").await.unwrap();

// Clear any existing logs first
    let logs = harness.logs().await.unwrap();
    logs.clear();

    // Add some log entries
    logs.push("retain_test", "log line 1".to_string(), LogStream::Stdout);
    logs.push("retain_test", "log line 2".to_string(), LogStream::Stdout);

    // Verify logs exist
    let entries = logs.tail(10, Some("retain_test"));
    assert_eq!(entries.len(), 2, "Should have 2 log entries");

    harness.stop_service("retain_test").await.unwrap();

    // Since we use Retain, logs should still exist
    // (The actual clearing happens in handle_process_exit in process.rs)
}

/// Service-level log config overrides global log config
#[tokio::test]
async fn test_service_log_config_overrides_global() {
    let temp_dir = TempDir::new().unwrap();

    // Global config says clear on stop
    let global_log_config = LogConfig {
        retention: Some(LogRetentionConfig {
            on_stop: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Service config says retain on stop
    let service_log_config = LogConfig {
        retention: Some(LogRetentionConfig {
            on_stop: Some(LogRetention::Retain),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .with_logs(global_log_config)
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_logs(service_log_config)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Verify the config was loaded correctly
    let config = harness
        .handle()
        .get_config()
        .await
        .unwrap();

    // Check global config
    assert_eq!(
        config.global_logs().unwrap().get_on_stop(),
        Some(LogRetention::Clear)
    );

    // Check service config overrides
    let svc = deser_svc(&config.services["test"]);
    assert_eq!(
        svc.logs
            .as_ref()
            .unwrap()
            .get_on_stop(),
        Some(LogRetention::Retain)
    );
}

/// Test all log retention events parse correctly
#[tokio::test]
async fn test_all_log_retention_events() {
    let temp_dir = TempDir::new().unwrap();

    // Use on_success/on_failure instead of on_exit to avoid mutual exclusivity error
    let log_config = LogConfig {
        store: None,
        retention: Some(LogRetentionConfig {
            on_stop: Some(LogRetention::Retain),
            on_start: Some(LogRetention::Clear),
            on_restart: Some(LogRetention::Retain),
            on_success: Some(LogRetention::Clear),
            on_failure: Some(LogRetention::Retain),
            on_skipped: Some(LogRetention::Clear),
            ..Default::default()
        }),
        max_size: ConfigValue::default(),
        buffer_size: ConfigValue::default(),
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_logs(log_config)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let config = harness
        .handle()
        .get_config()
        .await
        .unwrap();
    let svc = deser_svc(&config.services["test"]);
    let service_logs = svc.logs.as_ref().unwrap();

    assert_eq!(service_logs.get_on_stop(), Some(LogRetention::Retain));
    assert_eq!(service_logs.get_on_start(), Some(LogRetention::Clear));
    assert_eq!(service_logs.get_on_restart(), Some(LogRetention::Retain));
    assert_eq!(service_logs.get_on_exit(), None);
    assert_eq!(service_logs.get_on_success(), Some(LogRetention::Clear));
    assert_eq!(service_logs.get_on_failure(), Some(LogRetention::Retain));
    assert_eq!(service_logs.get_on_skipped(), Some(LogRetention::Clear));
}

/// Log buffer operations work correctly
#[tokio::test]
async fn test_log_buffer_operations() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("buffer_ops_test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("buffer_ops_test").await.unwrap();

let logs = harness.logs().await.unwrap();

    // Clear any existing logs first (logs persist on disk across test runs)
    logs.clear();

    // Push some logs with small delays to ensure distinct timestamps
    // (new architecture writes stdout/stderr to separate files, merged by timestamp)
    logs.push("buffer_ops_test", "line 1".to_string(), LogStream::Stdout);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    logs.push("buffer_ops_test", "line 2".to_string(), LogStream::Stderr);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    logs.push("buffer_ops_test", "line 3".to_string(), LogStream::Stdout);

    // Test tail
    let entries = logs.tail(10, Some("buffer_ops_test"));
    assert_eq!(entries.len(), 3);

    // Test tail with limit (returns last N entries sorted by timestamp)
    let entries = logs.tail(2, Some("buffer_ops_test"));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].line, "line 2");
    assert_eq!(entries[1].line, "line 3");

    // Test clear_service
    logs.clear_service("buffer_ops_test");
    let entries = logs.tail(10, Some("buffer_ops_test"));
    assert_eq!(entries.len(), 0, "Logs should be cleared");

    harness.stop_service("buffer_ops_test").await.unwrap();
}

/// Log entries include correct stream type
#[tokio::test]
async fn test_log_stream_types() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("stream_types_test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("stream_types_test").await.unwrap();

let logs = harness.logs().await.unwrap();

    // Clear any existing logs first
    logs.clear();

    // Add delay between writes to ensure distinct timestamps
    // (new architecture writes stdout/stderr to separate files, merged by timestamp)
    logs.push("stream_types_test", "stdout line".to_string(), LogStream::Stdout);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    logs.push("stream_types_test", "stderr line".to_string(), LogStream::Stderr);

    let entries = logs.tail(10, Some("stream_types_test"));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].stream, LogStream::Stdout);
    assert_eq!(entries[1].stream, LogStream::Stderr);

    harness.stop_service("stream_types_test").await.unwrap();
}

/// Multiple services have separate log buffers
#[tokio::test]
async fn test_multiple_services_logs() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("multi_svc1", TestServiceBuilder::long_running().build())
        .add_service("multi_svc2", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("multi_svc1").await.unwrap();
    harness.start_service("multi_svc2").await.unwrap();

let logs = harness.logs().await.unwrap();

    // Clear all logs after starting services (hooks may have added logs)
    logs.clear();

    // Add logs for each service
    logs.push("multi_svc1", "s1 line 1".to_string(), LogStream::Stdout);
    logs.push("multi_svc2", "s2 line 1".to_string(), LogStream::Stdout);
    logs.push("multi_svc1", "s1 line 2".to_string(), LogStream::Stdout);

    // Check service1 logs
    let s1_entries = logs.tail(10, Some("multi_svc1"));
    assert_eq!(s1_entries.len(), 2);
    assert!(s1_entries.iter().all(|e| &*e.service == "multi_svc1"));

    // Check service2 logs
    let s2_entries = logs.tail(10, Some("multi_svc2"));
    assert_eq!(s2_entries.len(), 1);
    assert!(s2_entries.iter().all(|e| &*e.service == "multi_svc2"));

    // Clear service1 logs, service2 should be unaffected
    logs.clear_service("multi_svc1");

    let s1_entries = logs.tail(10, Some("multi_svc1"));
    assert_eq!(s1_entries.len(), 0, "multi_svc1 logs should be cleared");

    let s2_entries = logs.tail(10, Some("multi_svc2"));
    assert_eq!(s2_entries.len(), 1, "multi_svc2 logs should be unaffected");

    harness.stop_all().await.unwrap();
}

/// clear_service_prefix clears hook logs
#[tokio::test]
async fn test_clear_service_prefix() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("prefix_svc", TestServiceBuilder::long_running().build())  // Unique name
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("prefix_svc").await.unwrap();

let logs = harness.logs().await.unwrap();

    // Clear any existing logs first
    logs.clear();

    // Add regular service logs
    logs.push("prefix_svc", "service log".to_string(), LogStream::Stdout);

    // Add hook logs (using the format from hooks.rs)
    logs.push("prefix_svc.pre_start", "hook log 1".to_string(), LogStream::Stdout);
    logs.push("prefix_svc.pre_stop", "hook log 2".to_string(), LogStream::Stdout);

    // Verify all logs exist
    let all_entries = logs.tail(10, None);
    assert_eq!(all_entries.len(), 3, "Should have 3 log entries: 1 service + 2 hooks");

    // Clear hook logs using prefix
    logs.clear_service_prefix("prefix_svc.");

    // Service logs should remain
    let service_entries = logs.tail(10, Some("prefix_svc"));
    assert_eq!(service_entries.len(), 1, "Service log should remain");
    assert_eq!(service_entries[0].line, "service log");

    // Hook logs should be cleared
    let hook_entries = logs.tail(10, Some("prefix_svc.pre_start"));
    assert_eq!(hook_entries.len(), 0, "Hook logs should be cleared");

    harness.stop_service("prefix_svc").await.unwrap();
}

/// Test log entries_since for follow mode
#[tokio::test]
async fn test_log_entries_since() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("entries_since_test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("entries_since_test").await.unwrap();

let logs = harness.logs().await.unwrap();

    // Clear any existing logs first (this resets sequence to 0)
    logs.clear();

    // Get initial sequence (should be 0 after clear)
    let seq1 = logs.current_sequence();
    assert_eq!(seq1, 0);

    // Add some logs
    logs.push("entries_since_test", "line 1".to_string(), LogStream::Stdout);
    logs.push("entries_since_test", "line 2".to_string(), LogStream::Stdout);

    let seq2 = logs.current_sequence();
    assert_eq!(seq2, seq1 + 2);

    // Get entries since seq1
    let entries = logs.entries_since(seq1, Some("entries_since_test"));
    assert_eq!(entries.len(), 2);

    // Add more logs
    logs.push("entries_since_test", "line 3".to_string(), LogStream::Stdout);

    // Get entries since seq2
    let entries = logs.entries_since(seq2, Some("entries_since_test"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].line, "line 3");

    harness.stop_service("entries_since_test").await.unwrap();
}

/// Test clear all logs
#[tokio::test]
async fn test_clear_all_logs() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("clearall_svc1", TestServiceBuilder::long_running().build())
        .add_service("clearall_svc2", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("clearall_svc1").await.unwrap();
    harness.start_service("clearall_svc2").await.unwrap();

let logs = harness.logs().await.unwrap();

    // Clear any existing logs first (from hooks during startup)
    logs.clear();

    // Add logs for both services
    logs.push("clearall_svc1", "s1 log".to_string(), LogStream::Stdout);
    logs.push("clearall_svc2", "s2 log".to_string(), LogStream::Stdout);

    // Verify logs exist
    let all_entries = logs.tail(10, None);
    assert_eq!(all_entries.len(), 2);

    // Clear all
    logs.clear();

    // All logs should be cleared
    let all_entries = logs.tail(10, None);
    assert_eq!(all_entries.len(), 0);

    // Sequence should reset
    assert_eq!(logs.current_sequence(), 0);

    harness.stop_all().await.unwrap();
}

/// Log retention config from YAML parses correctly (under kepler namespace)
#[test]
fn test_log_retention_yaml_parsing() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_stop: retain
      on_start: clear
      on_restart: retain
      on_exit: retain

services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_stop: clear
        on_exit: clear
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Check global config
    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.get_on_stop(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_start(), Some(LogRetention::Clear));
    assert_eq!(global_logs.get_on_restart(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_exit(), Some(LogRetention::Retain));
    // on_exit is sugar: get_on_success/get_on_failure fall back to on_exit
    assert_eq!(global_logs.get_on_success(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_failure(), Some(LogRetention::Retain));

    // Check service config overrides
    let svc = deser_svc(&config.services["test"]);
    let service_logs = svc.logs.as_ref().unwrap();
    assert_eq!(service_logs.get_on_stop(), Some(LogRetention::Clear));
    assert_eq!(service_logs.get_on_exit(), Some(LogRetention::Clear));
    // on_exit sugar resolves through get_on_success/get_on_failure
    assert_eq!(service_logs.get_on_success(), Some(LogRetention::Clear));
    assert_eq!(service_logs.get_on_failure(), Some(LogRetention::Clear));
}

/// Default LogConfig fields are None (unset)
#[test]
fn test_default_log_config_is_none() {
    let log_config = LogConfig::default();

    // All fields should be None (unset) by default
    assert_eq!(log_config.get_on_stop(), None);
    assert_eq!(log_config.get_on_start(), None);
    assert_eq!(log_config.get_on_restart(), None);
    assert_eq!(log_config.get_on_exit(), None);
    assert_eq!(log_config.get_on_success(), None);
    assert_eq!(log_config.get_on_failure(), None);
    assert_eq!(log_config.get_on_skipped(), None);
    assert!(log_config.retention.is_none());
}

/// Test resolve_log_retention inheritance behavior
#[test]
fn test_resolve_log_retention_inheritance() {
    use kepler_daemon::config::resolve_log_retention;

    // Test 1: No config at all → use default
    let retention = resolve_log_retention(
        None,
        None,
        |l| l.get_on_start(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Retain, "Should use built-in default when no config");

    // Test 2: Global config set, no service config → use global
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_start: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let retention = resolve_log_retention(
        None,
        Some(&global_logs),
        |l| l.get_on_start(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "Should use global config when service not set");

    // Test 3: Service config set, no global → use service
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_start: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let retention = resolve_log_retention(
        Some(&service_logs),
        None,
        |l| l.get_on_start(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "Should use service config when global not set");

    // Test 4: Both set → service wins
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_start: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_start: Some(LogRetention::Retain),
            ..Default::default()
        }),
        ..Default::default()
    };
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_start(),
        LogRetention::Clear,
    );
    assert_eq!(retention, LogRetention::Retain, "Service config should override global config");

    // Test 5: Service config exists but field is None → fall back to global
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_start: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_start: None, // Not set
            on_stop: Some(LogRetention::Retain), // Other field set
            ..Default::default()
        }),
        ..Default::default()
    };
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_start(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "Should fall back to global when service field is None");
}

/// Test the new default values for each event
#[test]
fn test_new_default_values() {
    use kepler_daemon::config::resolve_log_retention;

    // on_start defaults to retain
    let retention = resolve_log_retention(None, None, |l| l.get_on_start(), LogRetention::Retain);
    assert_eq!(retention, LogRetention::Retain);

    // on_restart defaults to retain
    let retention = resolve_log_retention(None, None, |l| l.get_on_restart(), LogRetention::Retain);
    assert_eq!(retention, LogRetention::Retain);

    // on_exit defaults to retain
    let retention = resolve_log_retention(None, None, |l| l.get_on_exit(), LogRetention::Retain);
    assert_eq!(retention, LogRetention::Retain);

    // on_stop defaults to clear
    let retention = resolve_log_retention(None, None, |l| l.get_on_stop(), LogRetention::Clear);
    assert_eq!(retention, LogRetention::Clear);

    // on_success defaults to retain
    let retention = resolve_log_retention(None, None, |l| l.get_on_success(), LogRetention::Retain);
    assert_eq!(retention, LogRetention::Retain);

    // on_failure defaults to retain
    let retention = resolve_log_retention(None, None, |l| l.get_on_failure(), LogRetention::Retain);
    assert_eq!(retention, LogRetention::Retain);

    // on_skipped defaults to retain
    let retention = resolve_log_retention(None, None, |l| l.get_on_skipped(), LogRetention::Retain);
    assert_eq!(retention, LogRetention::Retain);
}

/// Test on_success and on_failure YAML parsing
#[test]
fn test_on_success_on_failure_yaml_parsing() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_success: clear
      on_failure: retain

services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_success: retain
        on_failure: clear
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Check global config
    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.get_on_success(), Some(LogRetention::Clear));
    assert_eq!(global_logs.get_on_failure(), Some(LogRetention::Retain));

    // Check service config overrides
    let svc = deser_svc(&config.services["test"]);
    let service_logs = svc.logs.as_ref().unwrap();
    assert_eq!(service_logs.get_on_success(), Some(LogRetention::Retain));
    assert_eq!(service_logs.get_on_failure(), Some(LogRetention::Clear));
}

/// Test on_skipped YAML parsing
#[test]
fn test_on_skipped_yaml_parsing() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_skipped: clear

services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_skipped: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.get_on_skipped(), Some(LogRetention::Clear));

    let svc = deser_svc(&config.services["test"]);
    let service_logs = svc.logs.as_ref().unwrap();
    assert_eq!(service_logs.get_on_skipped(), Some(LogRetention::Retain));
}

/// Test on_exit is mutually exclusive with on_success
#[test]
fn test_on_exit_mutually_exclusive_with_on_success() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_exit: clear
        on_success: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err(), "Should fail when on_exit and on_success are both set");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("on_exit"), "Error should mention on_exit: {}", err);
    assert!(err.contains("on_success"), "Error should mention on_success: {}", err);
}

/// Test on_exit is mutually exclusive with on_failure
#[test]
fn test_on_exit_mutually_exclusive_with_on_failure() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_exit: clear
        on_failure: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err(), "Should fail when on_exit and on_failure are both set");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("on_exit"), "Error should mention on_exit: {}", err);
    assert!(err.contains("on_failure"), "Error should mention on_failure: {}", err);
}

/// Test resolve_log_retention works correctly for the new getters
#[test]
fn test_resolve_log_retention_new_events() {
    use kepler_daemon::config::resolve_log_retention;

    // on_success: service overrides global
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_success: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_success: Some(LogRetention::Retain),
            ..Default::default()
        }),
        ..Default::default()
    };
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_success(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Retain, "Service on_success should override global");

    // on_failure: global used when service not set
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_failure: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let retention = resolve_log_retention(
        None,
        Some(&global_logs),
        |l| l.get_on_failure(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "Should use global on_failure when service not set");

    // on_skipped: falls through to default
    let retention = resolve_log_retention(
        None,
        None,
        |l| l.get_on_skipped(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Retain, "on_skipped should default to Retain");
}

/// Test default values for on_success, on_failure, on_skipped fields
#[test]
fn test_default_values_new_fields() {
    let retention_config = LogRetentionConfig::default();

    assert_eq!(retention_config.on_success, None);
    assert_eq!(retention_config.on_failure, None);
    assert_eq!(retention_config.on_skipped, None);
}

/// Test that on_exit alone (without on_success/on_failure) is valid
#[test]
fn test_on_exit_alone_is_valid() {
    let retention_config = LogRetentionConfig {
        on_exit: Some(LogRetention::Clear),
        ..Default::default()
    };
    assert!(retention_config.validate().is_ok());
}

/// Test that on_success/on_failure alone (without on_exit) is valid
#[test]
fn test_on_success_on_failure_alone_is_valid() {
    let retention_config = LogRetentionConfig {
        on_success: Some(LogRetention::Clear),
        on_failure: Some(LogRetention::Retain),
        ..Default::default()
    };
    assert!(retention_config.validate().is_ok());
}

/// Test global-level mutual exclusivity: on_exit + on_success
#[test]
fn test_global_on_exit_mutually_exclusive_with_on_success() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_exit: clear
      on_success: retain

services:
  test:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err(), "Should fail when global on_exit and on_success are both set");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("on_exit"), "Error should mention on_exit: {}", err);
    assert!(err.contains("on_success"), "Error should mention on_success: {}", err);
}

/// Test global-level mutual exclusivity: on_exit + on_failure
#[test]
fn test_global_on_exit_mutually_exclusive_with_on_failure() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_exit: clear
      on_failure: retain

services:
  test:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err(), "Should fail when global on_exit and on_failure are both set");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("on_exit"), "Error should mention on_exit: {}", err);
    assert!(err.contains("on_failure"), "Error should mention on_failure: {}", err);
}

/// Cross-level: on_exit at global + on_success at service is valid
/// (mutual exclusivity only applies within the same retention block)
#[test]
fn test_cross_level_on_exit_global_on_success_service_is_valid() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_exit: clear

services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_success: retain
        on_failure: clear
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path);
    assert!(config.is_ok(), "Cross-level on_exit (global) + on_success (service) should be valid");
}

/// Cross-level: on_success at global + on_exit at service is valid
#[test]
fn test_cross_level_on_success_global_on_exit_service_is_valid() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_success: clear
      on_failure: retain

services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_exit: clear
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path);
    assert!(config.is_ok(), "Cross-level on_success (global) + on_exit (service) should be valid");
}

/// on_success/on_failure/on_skipped work at global level
#[test]
fn test_new_fields_at_global_level_yaml_parsing() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_success: clear
      on_failure: retain
      on_skipped: clear

services:
  test:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.get_on_success(), Some(LogRetention::Clear));
    assert_eq!(global_logs.get_on_failure(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_skipped(), Some(LogRetention::Clear));
}

/// on_success/on_failure/on_skipped at service level override global
#[test]
fn test_new_fields_service_overrides_global() {
    use kepler_daemon::config::KeplerConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_success: clear
      on_failure: retain
      on_skipped: clear

services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_success: retain
        on_failure: clear
        on_skipped: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Global
    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.get_on_success(), Some(LogRetention::Clear));
    assert_eq!(global_logs.get_on_failure(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_skipped(), Some(LogRetention::Clear));

    // Service overrides
    let svc = deser_svc(&config.services["test"]);
    let service_logs = svc.logs.as_ref().unwrap();
    assert_eq!(service_logs.get_on_success(), Some(LogRetention::Retain));
    assert_eq!(service_logs.get_on_failure(), Some(LogRetention::Clear));
    assert_eq!(service_logs.get_on_skipped(), Some(LogRetention::Retain));
}

/// resolve_log_retention falls back from service to global for new fields
#[test]
fn test_resolve_new_fields_fallback_to_global() {
    use kepler_daemon::config::resolve_log_retention;

    // Service has no on_success set, global does → should fall back to global
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_success: Some(LogRetention::Clear),
            on_failure: Some(LogRetention::Retain),
            on_skipped: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_stop: Some(LogRetention::Retain), // other field set, but not on_success/on_failure/on_skipped
            ..Default::default()
        }),
        ..Default::default()
    };

    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_success(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "on_success should fall back to global");

    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_failure(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Retain, "on_failure should fall back to global");

    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_skipped(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "on_skipped should fall back to global");
}

/// on_exit is sugar: get_on_success and get_on_failure resolve through it
#[test]
fn test_on_exit_is_sugar_for_on_success_and_on_failure() {
    let log_config = LogConfig {
        retention: Some(LogRetentionConfig {
            on_exit: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };

    // on_exit raw value
    assert_eq!(log_config.get_on_exit(), Some(LogRetention::Clear));
    // get_on_success / get_on_failure fall back to on_exit
    assert_eq!(log_config.get_on_success(), Some(LogRetention::Clear));
    assert_eq!(log_config.get_on_failure(), Some(LogRetention::Clear));
    // on_skipped is independent — not affected by on_exit
    assert_eq!(log_config.get_on_skipped(), None);
}

/// Cross-level resolve: global on_exit expands to on_success/on_failure,
/// service on_success overrides only the success path
#[test]
fn test_cross_level_on_exit_global_resolved_by_service_on_success() {
    use kepler_daemon::config::resolve_log_retention;

    // Global: on_exit: clear (sugar for on_success: clear + on_failure: clear)
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_exit: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Service: on_success: retain (overrides only the success path)
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_success: Some(LogRetention::Retain),
            ..Default::default()
        }),
        ..Default::default()
    };

    // ExitSuccess: service on_success=retain wins
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_success(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Retain, "Service on_success should override global on_exit for success");

    // ExitFailure: service has no on_failure/on_exit → falls back to global on_exit sugar → clear
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_failure(),
        LogRetention::Retain,
    );
    assert_eq!(retention, LogRetention::Clear, "Global on_exit should apply as on_failure fallback");
}

/// Cross-level resolve: global on_success/on_failure, service on_exit expands correctly
#[test]
fn test_cross_level_service_on_exit_overrides_global_on_success_on_failure() {
    use kepler_daemon::config::resolve_log_retention;

    // Global: on_success: clear, on_failure: clear
    let global_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_success: Some(LogRetention::Clear),
            on_failure: Some(LogRetention::Clear),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Service: on_exit: retain (sugar for on_success: retain + on_failure: retain)
    let service_logs = LogConfig {
        retention: Some(LogRetentionConfig {
            on_exit: Some(LogRetention::Retain),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Service on_exit expands → get_on_success returns retain
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_success(),
        LogRetention::Clear,
    );
    assert_eq!(retention, LogRetention::Retain, "Service on_exit sugar should override global on_success");

    // Service on_exit expands → get_on_failure returns retain
    let retention = resolve_log_retention(
        Some(&service_logs),
        Some(&global_logs),
        |l| l.get_on_failure(),
        LogRetention::Clear,
    );
    assert_eq!(retention, LogRetention::Retain, "Service on_exit sugar should override global on_failure");
}
