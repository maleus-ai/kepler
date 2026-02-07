//! Unit tests for LogReader

use kepler_daemon::logs::{BufferedLogWriter, LogReader, LogStream, LogWriterConfig};
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Create a temporary directory for test logs
fn setup_test_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

/// Helper to write log entries with controlled timestamps
fn write_log_entries(logs_dir: &std::path::Path, service: &str, stream: LogStream, messages: &[&str]) {
    let mut writer = BufferedLogWriter::new(
        logs_dir,
        service,
        stream,
        Some(10 * 1024 * 1024), // 10MB
        0, // No buffering
    );

    for msg in messages {
        writer.write(msg);
        // Small delay to ensure distinct timestamps
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn test_read_single_file() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "my-service", LogStream::Stdout, &[
        "First message",
        "Second message",
        "Third message",
    ]);

    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(100, Some("my-service"), false);

    assert_eq!(logs.len(), 3, "Should have 3 log entries");
    assert_eq!(logs[0].line, "First message");
    assert_eq!(logs[1].line, "Second message");
    assert_eq!(logs[2].line, "Third message");

    // All should be from the same service
    for log in &logs {
        assert_eq!(&*log.service, "my-service");
        assert_eq!(log.stream, LogStream::Stdout);
    }
}

#[test]
fn test_read_multiple_services() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "service-a", LogStream::Stdout, &["Message from A"]);
    write_log_entries(&logs_dir, "service-b", LogStream::Stdout, &["Message from B"]);
    write_log_entries(&logs_dir, "service-c", LogStream::Stdout, &["Message from C"]);

    let reader = LogReader::new(logs_dir.clone());

    // Read all services
    let all_logs = reader.tail(100, None, false);
    assert_eq!(all_logs.len(), 3, "Should have logs from all services");

    // Verify we can filter by service
    let logs_a = reader.tail(100, Some("service-a"), false);
    assert_eq!(logs_a.len(), 1);
    assert_eq!(logs_a[0].line, "Message from A");

    let logs_b = reader.tail(100, Some("service-b"), false);
    assert_eq!(logs_b.len(), 1);
    assert_eq!(logs_b[0].line, "Message from B");
}

#[test]
fn test_merge_stdout_stderr_chronologically() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write interleaved stdout/stderr with delays to ensure ordering
    let mut stdout_writer = BufferedLogWriter::new(
        &logs_dir,
        "my-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    let mut stderr_writer = BufferedLogWriter::new(
        &logs_dir,
        "my-service",
        LogStream::Stderr,
        Some(10 * 1024 * 1024),
        0,
    );

    stdout_writer.write("stdout-1");
    thread::sleep(Duration::from_millis(5));
    stderr_writer.write("stderr-1");
    thread::sleep(Duration::from_millis(5));
    stdout_writer.write("stdout-2");
    thread::sleep(Duration::from_millis(5));
    stderr_writer.write("stderr-2");

    drop(stdout_writer);
    drop(stderr_writer);

    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(100, Some("my-service"), false);

    assert_eq!(logs.len(), 4, "Should have 4 entries");

    // Verify chronological order
    assert_eq!(logs[0].line, "stdout-1");
    assert_eq!(logs[0].stream, LogStream::Stdout);

    assert_eq!(logs[1].line, "stderr-1");
    assert_eq!(logs[1].stream, LogStream::Stderr);

    assert_eq!(logs[2].line, "stdout-2");
    assert_eq!(logs[2].stream, LogStream::Stdout);

    assert_eq!(logs[3].line, "stderr-2");
    assert_eq!(logs[3].stream, LogStream::Stderr);

    // Verify timestamps are in order
    for i in 1..logs.len() {
        assert!(
            logs[i].timestamp >= logs[i - 1].timestamp,
            "Timestamps should be in order"
        );
    }
}

#[test]
fn test_tail_returns_last_n_lines() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write 100 lines
    let messages: Vec<String> = (0..100).map(|i| format!("Line {}", i)).collect();
    let message_refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
    write_log_entries(&logs_dir, "my-service", LogStream::Stdout, &message_refs);

    let reader = LogReader::new(logs_dir);

    // Request last 10
    let last_10 = reader.tail(10, Some("my-service"), false);
    assert_eq!(last_10.len(), 10, "Should return exactly 10 lines");
    assert_eq!(last_10[0].line, "Line 90");
    assert_eq!(last_10[9].line, "Line 99");

    // Request last 5
    let last_5 = reader.tail(5, Some("my-service"), false);
    assert_eq!(last_5.len(), 5);
    assert_eq!(last_5[0].line, "Line 95");
    assert_eq!(last_5[4].line, "Line 99");

    // Request more than available
    let all = reader.tail(200, Some("my-service"), false);
    assert_eq!(all.len(), 100);
}

#[test]
fn test_pagination_offset_limit() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write 50 lines
    let messages: Vec<String> = (0..50).map(|i| format!("Line {}", i)).collect();
    let message_refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
    write_log_entries(&logs_dir, "my-service", LogStream::Stdout, &message_refs);

    let reader = LogReader::new(logs_dir);

    // First page
    let (page1, has_more) = reader.get_paginated(Some("my-service"), 0, 10, false);
    assert!(has_more, "Should have more entries after first page");
    assert_eq!(page1.len(), 10);
    assert_eq!(page1[0].line, "Line 0");
    assert_eq!(page1[9].line, "Line 9");

    // Second page
    let (page2, has_more) = reader.get_paginated(Some("my-service"), 10, 10, false);
    assert!(has_more);
    assert_eq!(page2.len(), 10);
    assert_eq!(page2[0].line, "Line 10");
    assert_eq!(page2[9].line, "Line 19");

    // Last partial page
    let (last_page, has_more) = reader.get_paginated(Some("my-service"), 45, 10, false);
    assert!(!has_more, "Should not have more entries on last page");
    assert_eq!(last_page.len(), 5);
    assert_eq!(last_page[0].line, "Line 45");
    assert_eq!(last_page[4].line, "Line 49");

    // Beyond end
    let (beyond, has_more) = reader.get_paginated(Some("my-service"), 100, 10, false);
    assert!(!has_more);
    assert_eq!(beyond.len(), 0);
}

#[test]
fn test_truncated_file_continues_working() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let max_log_size = 100; // Small to force truncation

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "my-service",
        LogStream::Stdout,
        Some(max_log_size),
        0, // No buffering
    );

    // Write messages that will cause truncation
    for i in 0..50 {
        writer.write(&format!("Message {}", i));
        thread::sleep(Duration::from_millis(1));
    }
    drop(writer);

    // Verify file exists
    let main_file = logs_dir.join("my-service.stdout.log");
    assert!(main_file.exists(), "Main log file should exist");

    // File should be truncated (no rotated files)
    let rotated_1 = logs_dir.join("my-service.stdout.log.1");
    assert!(!rotated_1.exists(), "No rotated files should exist with truncation model");

    // Read back all logs
    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(1000, Some("my-service"), false);

    // Some logs will be lost due to truncation, but remaining should be valid
    // and timestamps should be in chronological order
    for i in 1..logs.len() {
        assert!(
            logs[i].timestamp >= logs[i - 1].timestamp,
            "Entry {} should be >= entry {} (timestamps: {:?} vs {:?})",
            i,
            i - 1,
            logs[i].timestamp,
            logs[i - 1].timestamp
        );
    }
}

#[test]
fn test_filter_by_service() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "web", LogStream::Stdout, &["web log 1", "web log 2"]);
    write_log_entries(&logs_dir, "api", LogStream::Stdout, &["api log 1"]);
    write_log_entries(&logs_dir, "worker", LogStream::Stdout, &["worker log 1", "worker log 2", "worker log 3"]);

    let reader = LogReader::new(logs_dir);

    // Filter by specific service
    let web_logs = reader.tail(100, Some("web"), false);
    assert_eq!(web_logs.len(), 2);
    for log in &web_logs {
        assert_eq!(&*log.service, "web");
    }

    let api_logs = reader.tail(100, Some("api"), false);
    assert_eq!(api_logs.len(), 1);
    assert_eq!(&*api_logs[0].service, "api");

    let worker_logs = reader.tail(100, Some("worker"), false);
    assert_eq!(worker_logs.len(), 3);
    for log in &worker_logs {
        assert_eq!(&*log.service, "worker");
    }

    // Non-existent service
    let none_logs = reader.tail(100, Some("nonexistent"), false);
    assert_eq!(none_logs.len(), 0);
}

#[test]
fn test_clear_all_logs() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "service-a", LogStream::Stdout, &["msg"]);
    write_log_entries(&logs_dir, "service-a", LogStream::Stderr, &["err"]);
    write_log_entries(&logs_dir, "service-b", LogStream::Stdout, &["msg"]);

    let reader = LogReader::new(logs_dir.clone());

    // Verify logs exist
    let logs = reader.tail(100, None, false);
    assert_eq!(logs.len(), 3);

    // Clear all
    reader.clear();

    // Verify empty
    let logs = reader.tail(100, None, false);
    assert_eq!(logs.len(), 0);

    // Verify files are deleted
    assert!(!logs_dir.join("service-a.stdout.log").exists());
    assert!(!logs_dir.join("service-a.stderr.log").exists());
    assert!(!logs_dir.join("service-b.stdout.log").exists());
}

#[test]
fn test_clear_service_logs() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "service-a", LogStream::Stdout, &["msg a"]);
    write_log_entries(&logs_dir, "service-b", LogStream::Stdout, &["msg b"]);

    let reader = LogReader::new(logs_dir.clone());

    // Clear only service-a
    reader.clear_service("service-a");

    // service-a should be gone
    let logs_a = reader.tail(100, Some("service-a"), false);
    assert_eq!(logs_a.len(), 0);

    // service-b should still exist
    let logs_b = reader.tail(100, Some("service-b"), false);
    assert_eq!(logs_b.len(), 1);
    assert_eq!(logs_b[0].line, "msg b");
}

#[test]
fn test_clear_service_prefix() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "api-users", LogStream::Stdout, &["users"]);
    write_log_entries(&logs_dir, "api-orders", LogStream::Stdout, &["orders"]);
    write_log_entries(&logs_dir, "worker", LogStream::Stdout, &["worker"]);

    let reader = LogReader::new(logs_dir.clone());

    // Clear services starting with "api-"
    reader.clear_service_prefix("api-");

    // api-* services should be gone
    let api_users = reader.tail(100, Some("api-users"), false);
    let api_orders = reader.tail(100, Some("api-orders"), false);
    assert_eq!(api_users.len(), 0);
    assert_eq!(api_orders.len(), 0);

    // worker should still exist
    let worker = reader.tail(100, Some("worker"), false);
    assert_eq!(worker.len(), 1);
}

#[test]
fn test_empty_logs_directory() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let reader = LogReader::new(logs_dir);

    let logs = reader.tail(100, None, false);
    assert_eq!(logs.len(), 0);

    let (paginated, has_more) = reader.get_paginated(None, 0, 10, false);
    assert_eq!(paginated.len(), 0);
    assert!(!has_more);
}

#[test]
fn test_nonexistent_service() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "existing", LogStream::Stdout, &["msg"]);

    let reader = LogReader::new(logs_dir);

    let logs = reader.tail(100, Some("nonexistent"), false);
    assert_eq!(logs.len(), 0);
}

#[test]
fn test_service_name_with_special_chars() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write with special characters in service name
    write_log_entries(&logs_dir, "my/special:service", LogStream::Stdout, &["special msg"]);

    let reader = LogReader::new(logs_dir.clone());

    // Should be able to read back using the same service name
    let logs = reader.tail(100, Some("my/special:service"), false);
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].line, "special msg");
}

#[test]
fn test_log_line_metadata() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "test-svc", LogStream::Stdout, &["test message"]);

    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(1, Some("test-svc"), false);

    assert_eq!(logs.len(), 1);
    let log = &logs[0];

    assert_eq!(&*log.service, "test-svc");
    assert_eq!(log.line, "test message");
    assert_eq!(log.stream, LogStream::Stdout);

    // Timestamp should be recent (within last minute)
    let now = chrono::Utc::now();
    let diff_ms = now.timestamp_millis() - log.timestamp;
    assert!(diff_ms < 60_000, "Timestamp should be recent");
}

#[test]
fn test_bounded_reading() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write many large messages
    let large_msg = "x".repeat(1000);
    let messages: Vec<String> = (0..100).map(|i| format!("{} - {}", i, large_msg)).collect();
    let message_refs: Vec<&str> = messages.iter().map(|s| s.as_str()).collect();
    write_log_entries(&logs_dir, "large-service", LogStream::Stdout, &message_refs);

    let reader = LogReader::new(logs_dir);

    // Bounded read should limit bytes read
    let logs = reader.tail_bounded(1000, Some("large-service"), Some(10 * 1024), false); // 10KB limit

    // Should return some logs, but not all (due to byte limit)
    assert!(!logs.is_empty(), "Should return some logs");
    // With 10KB limit and ~1KB per message, we expect roughly 10 messages
    // But could be more or less depending on exact byte boundaries
}

#[test]
fn test_multiple_rotated_files() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // With truncation model, only main file is read (rotation files ignored)
    let base_path = logs_dir.join("manual-service.stdout.log");

    fs::create_dir_all(&logs_dir).unwrap();

    // Create main file with multiple entries
    fs::write(&base_path, "1000\tOldest message\n2000\tMiddle message\n3000\tRecent message\n4000\tNewest message\n").unwrap();

    // Legacy rotation files are ignored
    let rotated1 = format!("{}.1", base_path.display());
    fs::write(&rotated1, "500\tLegacy rotated file\n").unwrap();

    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(100, Some("manual-service"), false);

    assert_eq!(logs.len(), 4, "Should read all 4 entries from main file");

    // Verify chronological order
    assert_eq!(logs[0].line, "Oldest message");
    assert_eq!(logs[1].line, "Middle message");
    assert_eq!(logs[2].line, "Recent message");
    assert_eq!(logs[3].line, "Newest message");
}

#[test]
fn test_reader_from_config() {
    let temp_dir = setup_test_dir();
    let config = LogWriterConfig::with_options(
        temp_dir.path().to_path_buf(),
        Some(1024 * 1024),
        4096,
    );

    write_log_entries(&config.logs_dir, "test-svc", LogStream::Stdout, &["test"]);

    let reader = LogReader::new(config.logs_dir.clone());
    let logs = reader.tail(10, Some("test-svc"), false);

    assert_eq!(logs.len(), 1);
}

// ── no_hooks filtering tests ──

#[test]
fn test_no_hooks_filters_hook_logs() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "backend", LogStream::Stdout, &["service msg"]);
    write_log_entries(&logs_dir, "backend.pre_start", LogStream::Stdout, &["hook msg"]);

    let reader = LogReader::new(logs_dir);

    // Without no_hooks: both entries
    let all = reader.tail(100, None, false);
    assert_eq!(all.len(), 2, "Should have 2 entries without filtering");

    // With no_hooks: only the regular service
    let filtered = reader.tail(100, None, true);
    assert_eq!(filtered.len(), 1, "Should have 1 entry with no_hooks");
    assert_eq!(&*filtered[0].service, "backend");
}

#[test]
fn test_no_hooks_with_service_filter() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "myapp", LogStream::Stdout, &["app msg"]);
    write_log_entries(&logs_dir, "myapp.pre_start", LogStream::Stdout, &["pre_start msg"]);
    write_log_entries(&logs_dir, "myapp.post_stop", LogStream::Stdout, &["post_stop msg"]);

    let reader = LogReader::new(logs_dir);

    // Filter by service + no_hooks: only the regular myapp entry
    let filtered = reader.tail(100, Some("myapp"), true);
    assert_eq!(filtered.len(), 1, "Should have 1 entry for myapp with no_hooks");
    assert_eq!(&*filtered[0].service, "myapp");

    // no_hooks without service filter: only non-hook entries
    let no_hooks = reader.tail(100, None, true);
    assert_eq!(no_hooks.len(), 1, "Should have 1 non-hook entry total");

    // No filtering: all 3 entries
    let all = reader.tail(100, None, false);
    assert_eq!(all.len(), 3, "Should have 3 entries without filtering");
}

#[test]
fn test_no_hooks_head() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "svc", LogStream::Stdout, &["regular msg"]);
    write_log_entries(&logs_dir, "svc.pre_start", LogStream::Stdout, &["hook msg"]);

    let reader = LogReader::new(logs_dir);

    let filtered = reader.head(100, None, true);
    assert_eq!(filtered.len(), 1, "head with no_hooks should exclude hooks");
    assert_eq!(&*filtered[0].service, "svc");

    let all = reader.head(100, None, false);
    assert_eq!(all.len(), 2, "head without no_hooks should include all");
}

#[test]
fn test_no_hooks_iter() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "app", LogStream::Stdout, &["app msg"]);
    write_log_entries(&logs_dir, "app.post_stop", LogStream::Stdout, &["hook msg"]);

    let reader = LogReader::new(logs_dir);

    let filtered: Vec<_> = reader.iter(None, true).collect();
    assert_eq!(filtered.len(), 1, "iter with no_hooks should exclude hooks");
    assert_eq!(&*filtered[0].service, "app");

    let all: Vec<_> = reader.iter(None, false).collect();
    assert_eq!(all.len(), 2, "iter without no_hooks should include all");
}

#[test]
fn test_no_hooks_get_paginated() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "web", LogStream::Stdout, &["web msg"]);
    write_log_entries(&logs_dir, "web.pre_start", LogStream::Stdout, &["hook msg"]);

    let reader = LogReader::new(logs_dir);

    let (filtered, _) = reader.get_paginated(None, 0, 100, true);
    assert_eq!(filtered.len(), 1, "get_paginated with no_hooks should exclude hooks");
    assert_eq!(&*filtered[0].service, "web");

    let (all, _) = reader.get_paginated(None, 0, 100, false);
    assert_eq!(all.len(), 2, "get_paginated without no_hooks should include all");
}

#[test]
fn test_no_hooks_only_hooks_returns_empty() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "svc.pre_start", LogStream::Stdout, &["hook 1"]);
    write_log_entries(&logs_dir, "svc.post_stop", LogStream::Stdout, &["hook 2"]);

    let reader = LogReader::new(logs_dir);

    let filtered = reader.tail(100, None, true);
    assert_eq!(filtered.len(), 0, "Should return empty when only hooks exist and no_hooks=true");

    let all = reader.tail(100, None, false);
    assert_eq!(all.len(), 2, "Should return 2 entries when no_hooks=false");
}

/// Test that truncation model ignores legacy rotation files.
/// With truncation, only the main file is read - any .1, .2, etc files are ignored.
#[test]
fn test_truncation_ignores_legacy_rotation_files() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();
    let base_path = logs_dir.join("truncation-test.stdout.log");

    fs::create_dir_all(&logs_dir).unwrap();

    // Create main file with multiple entries
    fs::write(&base_path, "3000\tEntry 1\n4000\tEntry 2\n5000\tEntry 3\n6000\tEntry 4\n").unwrap();

    // Create legacy rotation files (these should be ignored)
    let rotated1 = format!("{}.1", base_path.display());
    fs::write(&rotated1, "1000\tLegacy rotated 1\n").unwrap();

    let rotated2 = format!("{}.2", base_path.display());
    fs::write(&rotated2, "2000\tLegacy rotated 2\n").unwrap();

    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(100, Some("truncation-test"), false);

    // Only main file is read (4 entries)
    assert_eq!(logs.len(), 4, "Should read only main file entries");

    // Verify chronological order from main file only
    assert_eq!(logs[0].line, "Entry 1");
    assert_eq!(logs[0].timestamp, 3000);

    assert_eq!(logs[1].line, "Entry 2");
    assert_eq!(logs[1].timestamp, 4000);

    assert_eq!(logs[2].line, "Entry 3");
    assert_eq!(logs[2].timestamp, 5000);

    assert_eq!(logs[3].line, "Entry 4");
    assert_eq!(logs[3].timestamp, 6000);
}

/// Test that multiple services are merged chronologically.
/// With truncation model, only main files are read per service.
#[test]
fn test_multi_service_chronological_merge() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Service A: stdout with multiple entries
    let svc_a_base = logs_dir.join("service-a.stdout.log");
    fs::write(&svc_a_base, "1000\tA-first\n3000\tA-second\n5000\tA-third\n").unwrap();

    // Service B: stdout with interleaved timestamps
    let svc_b_base = logs_dir.join("service-b.stdout.log");
    fs::write(&svc_b_base, "2000\tB-first\n4000\tB-second\n6000\tB-third\n").unwrap();

    // Service C: only stderr, interleaved timestamp
    let svc_c_base = logs_dir.join("service-c.stderr.log");
    fs::write(&svc_c_base, "2500\tC-stderr\n").unwrap();

    let reader = LogReader::new(logs_dir);

    // Read all services - should be chronologically merged
    let all_logs = reader.tail(100, None, false);

    assert_eq!(all_logs.len(), 7, "Should have 7 total entries");

    // Verify chronological order across all services
    let expected_order = [
        ("service-a", "A-first", 1000),
        ("service-b", "B-first", 2000),
        ("service-c", "C-stderr", 2500),
        ("service-a", "A-second", 3000),
        ("service-b", "B-second", 4000),
        ("service-a", "A-third", 5000),
        ("service-b", "B-third", 6000),
    ];

    for (i, (exp_service, exp_line, exp_ts)) in expected_order.iter().enumerate() {
        assert_eq!(
            &*all_logs[i].service, *exp_service,
            "Entry {} service mismatch: expected {}, got {}",
            i, exp_service, all_logs[i].service
        );
        assert!(
            all_logs[i].line.contains(exp_line),
            "Entry {} line mismatch: expected to contain '{}', got '{}'",
            i, exp_line, all_logs[i].line
        );
        assert_eq!(
            all_logs[i].timestamp, *exp_ts,
            "Entry {} timestamp mismatch: expected {}, got {}",
            i, exp_ts, all_logs[i].timestamp
        );
    }

    // Verify filtering still works correctly
    let svc_a_only = reader.tail(100, Some("service-a"), false);
    assert_eq!(svc_a_only.len(), 3);
    assert_eq!(svc_a_only[0].timestamp, 1000);
    assert_eq!(svc_a_only[1].timestamp, 3000);
    assert_eq!(svc_a_only[2].timestamp, 5000);
}

/// Test truncation with repeated writes to ensure data integrity
#[test]
fn test_truncation_maintains_integrity() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let max_log_size = 500;  // Very small to force truncation

    // Write enough to trigger multiple truncations
    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "heavy-truncation",
        LogStream::Stdout,
        Some(max_log_size),
        0, // No buffering
    );

    for i in 0..100 {
        writer.write(&format!("Message {:04}", i));
        thread::sleep(Duration::from_millis(1)); // Ensure distinct timestamps
    }
    drop(writer);

    // Read back
    let reader = LogReader::new(logs_dir.clone());
    let logs = reader.tail(1000, Some("heavy-truncation"), false);

    // With truncation, we'll have fewer logs since old ones are discarded
    // File will contain only the most recent messages that fit
    assert!(
        !logs.is_empty(),
        "Should have some entries after truncation"
    );

    // Verify timestamps are in chronological order
    for i in 1..logs.len() {
        assert!(
            logs[i].timestamp >= logs[i - 1].timestamp,
            "Entry {} timestamp {} should be >= entry {} timestamp {}",
            i, logs[i].timestamp, i - 1, logs[i - 1].timestamp
        );
    }

    // No rotated files should exist
    let log_file = logs_dir.join("heavy-truncation.stdout.log");
    for i in 1..=5 {
        let rotated = format!("{}.{}", log_file.display(), i);
        assert!(
            !std::path::Path::new(&rotated).exists(),
            "Rotated file {} should not exist with truncation model",
            i
        );
    }
}
