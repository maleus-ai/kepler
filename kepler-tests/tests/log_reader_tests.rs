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
        10 * 1024 * 1024, // 10MB
        5,
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

    let reader = LogReader::new(logs_dir, 5);
    let logs = reader.tail(100, Some("my-service"));

    assert_eq!(logs.len(), 3, "Should have 3 log entries");
    assert_eq!(logs[0].line, "First message");
    assert_eq!(logs[1].line, "Second message");
    assert_eq!(logs[2].line, "Third message");

    // All should be from the same service
    for log in &logs {
        assert_eq!(log.service, "my-service");
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

    let reader = LogReader::new(logs_dir.clone(), 5);

    // Read all services
    let all_logs = reader.tail(100, None);
    assert_eq!(all_logs.len(), 3, "Should have logs from all services");

    // Verify we can filter by service
    let logs_a = reader.tail(100, Some("service-a"));
    assert_eq!(logs_a.len(), 1);
    assert_eq!(logs_a[0].line, "Message from A");

    let logs_b = reader.tail(100, Some("service-b"));
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
        10 * 1024 * 1024,
        5,
        0,
    );

    let mut stderr_writer = BufferedLogWriter::new(
        &logs_dir,
        "my-service",
        LogStream::Stderr,
        10 * 1024 * 1024,
        5,
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

    let reader = LogReader::new(logs_dir, 5);
    let logs = reader.tail(100, Some("my-service"));

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

    let reader = LogReader::new(logs_dir, 5);

    // Request last 10
    let last_10 = reader.tail(10, Some("my-service"));
    assert_eq!(last_10.len(), 10, "Should return exactly 10 lines");
    assert_eq!(last_10[0].line, "Line 90");
    assert_eq!(last_10[9].line, "Line 99");

    // Request last 5
    let last_5 = reader.tail(5, Some("my-service"));
    assert_eq!(last_5.len(), 5);
    assert_eq!(last_5[0].line, "Line 95");
    assert_eq!(last_5[4].line, "Line 99");

    // Request more than available
    let all = reader.tail(200, Some("my-service"));
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

    let reader = LogReader::new(logs_dir, 5);

    // First page
    let (page1, total) = reader.get_paginated(Some("my-service"), 0, 10);
    assert_eq!(total, 50, "Total should be 50");
    assert_eq!(page1.len(), 10);
    assert_eq!(page1[0].line, "Line 0");
    assert_eq!(page1[9].line, "Line 9");

    // Second page
    let (page2, total) = reader.get_paginated(Some("my-service"), 10, 10);
    assert_eq!(total, 50);
    assert_eq!(page2.len(), 10);
    assert_eq!(page2[0].line, "Line 10");
    assert_eq!(page2[9].line, "Line 19");

    // Last partial page
    let (last_page, total) = reader.get_paginated(Some("my-service"), 45, 10);
    assert_eq!(total, 50);
    assert_eq!(last_page.len(), 5);
    assert_eq!(last_page[0].line, "Line 45");
    assert_eq!(last_page[4].line, "Line 49");

    // Beyond end
    let (beyond, total) = reader.get_paginated(Some("my-service"), 100, 10);
    assert_eq!(total, 50);
    assert_eq!(beyond.len(), 0);
}

#[test]
fn test_rotated_files_read_in_order() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let max_log_size = 100; // Small to force rotation
    let max_rotated = 5;

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "my-service",
        LogStream::Stdout,
        max_log_size,
        max_rotated,
        0, // No buffering
    );

    // Write messages that will span multiple rotated files
    for i in 0..50 {
        writer.write(&format!("Message {}", i));
        thread::sleep(Duration::from_millis(1));
    }
    drop(writer);

    // Verify rotation happened
    let main_file = logs_dir.join("my-service.stdout.log");
    assert!(main_file.exists(), "Main log file should exist");

    // Read back all logs
    let reader = LogReader::new(logs_dir, max_rotated);
    let logs = reader.tail(1000, Some("my-service"));

    // Verify timestamps are in chronological order
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

    let reader = LogReader::new(logs_dir, 5);

    // Filter by specific service
    let web_logs = reader.tail(100, Some("web"));
    assert_eq!(web_logs.len(), 2);
    for log in &web_logs {
        assert_eq!(log.service, "web");
    }

    let api_logs = reader.tail(100, Some("api"));
    assert_eq!(api_logs.len(), 1);
    assert_eq!(api_logs[0].service, "api");

    let worker_logs = reader.tail(100, Some("worker"));
    assert_eq!(worker_logs.len(), 3);
    for log in &worker_logs {
        assert_eq!(log.service, "worker");
    }

    // Non-existent service
    let none_logs = reader.tail(100, Some("nonexistent"));
    assert_eq!(none_logs.len(), 0);
}

#[test]
fn test_clear_all_logs() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "service-a", LogStream::Stdout, &["msg"]);
    write_log_entries(&logs_dir, "service-a", LogStream::Stderr, &["err"]);
    write_log_entries(&logs_dir, "service-b", LogStream::Stdout, &["msg"]);

    let reader = LogReader::new(logs_dir.clone(), 5);

    // Verify logs exist
    let logs = reader.tail(100, None);
    assert_eq!(logs.len(), 3);

    // Clear all
    reader.clear();

    // Verify empty
    let logs = reader.tail(100, None);
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

    let reader = LogReader::new(logs_dir.clone(), 5);

    // Clear only service-a
    reader.clear_service("service-a");

    // service-a should be gone
    let logs_a = reader.tail(100, Some("service-a"));
    assert_eq!(logs_a.len(), 0);

    // service-b should still exist
    let logs_b = reader.tail(100, Some("service-b"));
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

    let reader = LogReader::new(logs_dir.clone(), 5);

    // Clear services starting with "api-"
    reader.clear_service_prefix("api-");

    // api-* services should be gone
    let api_users = reader.tail(100, Some("api-users"));
    let api_orders = reader.tail(100, Some("api-orders"));
    assert_eq!(api_users.len(), 0);
    assert_eq!(api_orders.len(), 0);

    // worker should still exist
    let worker = reader.tail(100, Some("worker"));
    assert_eq!(worker.len(), 1);
}

#[test]
fn test_empty_logs_directory() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let reader = LogReader::new(logs_dir, 5);

    let logs = reader.tail(100, None);
    assert_eq!(logs.len(), 0);

    let (paginated, total) = reader.get_paginated(None, 0, 10);
    assert_eq!(paginated.len(), 0);
    assert_eq!(total, 0);
}

#[test]
fn test_nonexistent_service() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "existing", LogStream::Stdout, &["msg"]);

    let reader = LogReader::new(logs_dir, 5);

    let logs = reader.tail(100, Some("nonexistent"));
    assert_eq!(logs.len(), 0);
}

#[test]
fn test_service_name_with_special_chars() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write with special characters in service name
    write_log_entries(&logs_dir, "my/special:service", LogStream::Stdout, &["special msg"]);

    let reader = LogReader::new(logs_dir.clone(), 5);

    // Should be able to read back using the same service name
    let logs = reader.tail(100, Some("my/special:service"));
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].line, "special msg");
}

#[test]
fn test_log_line_metadata() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    write_log_entries(&logs_dir, "test-svc", LogStream::Stdout, &["test message"]);

    let reader = LogReader::new(logs_dir, 5);
    let logs = reader.tail(1, Some("test-svc"));

    assert_eq!(logs.len(), 1);
    let log = &logs[0];

    assert_eq!(log.service, "test-svc");
    assert_eq!(log.line, "test message");
    assert_eq!(log.stream, LogStream::Stdout);

    // Timestamp should be recent (within last minute)
    let now = chrono::Utc::now();
    let diff = now.signed_duration_since(log.timestamp);
    assert!(diff.num_seconds() < 60, "Timestamp should be recent");
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

    let reader = LogReader::new(logs_dir, 5);

    // Bounded read should limit bytes read
    let logs = reader.tail_bounded(1000, Some("large-service"), Some(10 * 1024)); // 10KB limit

    // Should return some logs, but not all (due to byte limit)
    assert!(!logs.is_empty(), "Should return some logs");
    // With 10KB limit and ~1KB per message, we expect roughly 10 messages
    // But could be more or less depending on exact byte boundaries
}

#[test]
fn test_multiple_rotated_files() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Create multiple rotated files manually to test reading
    let base_path = logs_dir.join("manual-service.stdout.log");

    fs::create_dir_all(&logs_dir).unwrap();

    // Create rotated file .3 (oldest)
    let rotated3 = format!("{}.3", base_path.display());
    fs::write(&rotated3, "1000\tOldest message\n").unwrap();

    // Create rotated file .2
    let rotated2 = format!("{}.2", base_path.display());
    fs::write(&rotated2, "2000\tMiddle message\n").unwrap();

    // Create rotated file .1
    let rotated1 = format!("{}.1", base_path.display());
    fs::write(&rotated1, "3000\tRecent message\n").unwrap();

    // Create main file (newest)
    fs::write(&base_path, "4000\tNewest message\n").unwrap();

    let reader = LogReader::new(logs_dir, 5);
    let logs = reader.tail(100, Some("manual-service"));

    assert_eq!(logs.len(), 4, "Should read all 4 files");

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
        1024 * 1024,
        3,
        4096,
    );

    write_log_entries(&config.logs_dir, "test-svc", LogStream::Stdout, &["test"]);

    let reader = LogReader::new(config.logs_dir.clone(), config.max_rotated_files);
    let logs = reader.tail(10, Some("test-svc"));

    assert_eq!(logs.len(), 1);
}

/// Test rotation wrap-around: when rotation has cycled more than once,
/// file index order doesn't match timestamp order.
///
/// With max_rotated_files=3:
/// - Fill log -> rotate to .1 (ts=1000)
/// - Fill log -> rotate to .2 (ts=2000)
/// - Fill log -> rotate to .3 (ts=3000)
/// - Fill log -> rotate to .1 (overwrites, ts=4000) <- .1 is now NEWER than .3
/// - Fill log -> rotate to .2 (overwrites, ts=5000)
/// - Current log (ts=6000)
///
/// Reading order should be: .3 (3000) -> .1 (4000) -> .2 (5000) -> current (6000)
#[test]
fn test_rotation_wraparound_ordering() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();
    let base_path = logs_dir.join("wraparound-test.stdout.log");

    fs::create_dir_all(&logs_dir).unwrap();

    // Simulate wrap-around: .1 and .2 are NEWER than .3
    // This happens after rotation has cycled more than once

    // .3 - oldest (from first rotation cycle)
    let rotated3 = format!("{}.3", base_path.display());
    fs::write(&rotated3, "3000\tFile 3 - oldest from first cycle\n").unwrap();

    // .1 - newer (from second rotation cycle, overwrote old .1)
    let rotated1 = format!("{}.1", base_path.display());
    fs::write(&rotated1, "4000\tFile 1 - newer from second cycle\n").unwrap();

    // .2 - even newer (from second rotation cycle)
    let rotated2 = format!("{}.2", base_path.display());
    fs::write(&rotated2, "5000\tFile 2 - newer from second cycle\n").unwrap();

    // Current log - newest
    fs::write(&base_path, "6000\tCurrent file - newest\n").unwrap();

    let reader = LogReader::new(logs_dir, 3);
    let logs = reader.tail(100, Some("wraparound-test"));

    assert_eq!(logs.len(), 4, "Should read all 4 files");

    // Verify chronological order (NOT file index order!)
    assert_eq!(logs[0].line, "File 3 - oldest from first cycle");
    assert_eq!(logs[0].timestamp.timestamp_millis(), 3000);

    assert_eq!(logs[1].line, "File 1 - newer from second cycle");
    assert_eq!(logs[1].timestamp.timestamp_millis(), 4000);

    assert_eq!(logs[2].line, "File 2 - newer from second cycle");
    assert_eq!(logs[2].timestamp.timestamp_millis(), 5000);

    assert_eq!(logs[3].line, "Current file - newest");
    assert_eq!(logs[3].timestamp.timestamp_millis(), 6000);
}

/// Test that multiple services with rotation and wrap-around are reconciled correctly.
/// This simulates a realistic scenario where multiple services are logging concurrently
/// and their rotations happen at different times.
#[test]
fn test_multi_service_rotation_reconciliation() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Service A: has wrap-around (.1 is newer than .2)
    let svc_a_base = logs_dir.join("service-a.stdout.log");
    fs::write(
        format!("{}.2", svc_a_base.display()),
        "1000\tA-file2-oldest\n",
    ).unwrap();
    fs::write(
        format!("{}.1", svc_a_base.display()),
        "3000\tA-file1-wrapped\n",
    ).unwrap();
    fs::write(&svc_a_base, "5000\tA-current\n").unwrap();

    // Service B: normal order (no wrap-around)
    let svc_b_base = logs_dir.join("service-b.stdout.log");
    fs::write(
        format!("{}.2", svc_b_base.display()),
        "2000\tB-file2\n",
    ).unwrap();
    fs::write(
        format!("{}.1", svc_b_base.display()),
        "4000\tB-file1\n",
    ).unwrap();
    fs::write(&svc_b_base, "6000\tB-current\n").unwrap();

    // Service C: only stderr, interleaved timestamps
    let svc_c_base = logs_dir.join("service-c.stderr.log");
    fs::write(&svc_c_base, "2500\tC-stderr\n").unwrap();

    let reader = LogReader::new(logs_dir, 3);

    // Read all services - should be chronologically merged
    let all_logs = reader.tail(100, None);

    assert_eq!(all_logs.len(), 7, "Should have 7 total entries");

    // Verify chronological order across all services
    let expected_order = [
        ("service-a", "A-file2-oldest", 1000),
        ("service-b", "B-file2", 2000),
        ("service-c", "C-stderr", 2500),
        ("service-a", "A-file1-wrapped", 3000),
        ("service-b", "B-file1", 4000),
        ("service-a", "A-current", 5000),
        ("service-b", "B-current", 6000),
    ];

    for (i, (exp_service, exp_line, exp_ts)) in expected_order.iter().enumerate() {
        assert_eq!(
            all_logs[i].service, *exp_service,
            "Entry {} service mismatch: expected {}, got {}",
            i, exp_service, all_logs[i].service
        );
        assert!(
            all_logs[i].line.contains(exp_line),
            "Entry {} line mismatch: expected to contain '{}', got '{}'",
            i, exp_line, all_logs[i].line
        );
        assert_eq!(
            all_logs[i].timestamp.timestamp_millis(), *exp_ts,
            "Entry {} timestamp mismatch: expected {}, got {}",
            i, exp_ts, all_logs[i].timestamp.timestamp_millis()
        );
    }

    // Verify filtering still works correctly
    let svc_a_only = reader.tail(100, Some("service-a"));
    assert_eq!(svc_a_only.len(), 3);
    assert_eq!(svc_a_only[0].timestamp.timestamp_millis(), 1000);
    assert_eq!(svc_a_only[1].timestamp.timestamp_millis(), 3000);
    assert_eq!(svc_a_only[2].timestamp.timestamp_millis(), 5000);
}

/// Test heavy rotation with multiple cycles to ensure nothing is lost or corrupted
#[test]
fn test_heavy_multi_cycle_rotation() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let rotation_size = 500;  // Very small to force many rotations
    let max_rotated = 3;

    // Write enough to cycle through rotations multiple times
    // Each message is ~50 bytes, so 500 bytes = ~10 messages per file
    // Writing 100 messages = 10 files worth = ~3+ full cycles
    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "heavy-rotation",
        LogStream::Stdout,
        rotation_size,
        max_rotated,
        0, // No buffering
    );

    for i in 0..100 {
        writer.write(&format!("Message {:04}", i));
        thread::sleep(Duration::from_millis(1)); // Ensure distinct timestamps
    }
    drop(writer);

    // Read back
    let reader = LogReader::new(logs_dir.clone(), max_rotated);
    let logs = reader.tail(1000, Some("heavy-rotation"));

    // We should have logs from max_rotated files + current file
    // Each file holds ~10 messages, so we should have ~40 messages
    // (but could be less if rotation timing varies)
    assert!(
        logs.len() >= 20,
        "Should have at least 20 entries, got {}",
        logs.len()
    );

    // Verify timestamps are in chronological order
    for i in 1..logs.len() {
        assert!(
            logs[i].timestamp >= logs[i - 1].timestamp,
            "Entry {} timestamp {} should be >= entry {} timestamp {}",
            i, logs[i].timestamp, i - 1, logs[i - 1].timestamp
        );
    }

    // Verify the last entries are the most recent ones written
    let last_log = logs.last().unwrap();
    assert!(
        last_log.line.contains("Message 009") || last_log.line.contains("Message 0"),
        "Last entry should be a recent message, got: {}",
        last_log.line
    );
}
