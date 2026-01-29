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
use std::path::PathBuf;
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

/// Test that rotated logs are properly merged with correct timestamp ordering
#[test]
fn test_rotated_logs_merged_correctly() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create a log buffer with very small rotation (2KB) to guarantee multiple rotations
    let rotation_size = 2 * 1024; // 2KB
    let max_rotated_files = 5;
    let mut buffer = LogBuffer::with_rotation(
        logs_dir.clone(),
        rotation_size,
        max_rotated_files,
    );

    // Calculate approximate lines per file (each line ~120 bytes with padding)
    let line_size = 120;
    let lines_per_file = rotation_size as usize / line_size;

    // Write enough lines to fill at least 4 rotated files
    let total_lines = lines_per_file * 6;
    for i in 0..total_lines {
        buffer.push(
            "test-service".to_string(),
            format!(
                "Log line {:06} with padding to fill up the file quickly: {}",
                i,
                "x".repeat(80)
            ),
            LogStream::Stdout,
        );
    }

    // Verify main log file exists
    let log_file = logs_dir.join("test-service.log");
    assert!(log_file.exists(), "Main log file should exist");

    // Count how many rotated files actually exist
    let mut rotated_count = 0;
    for i in 1..=max_rotated_files {
        let rotated = format!("{}.{}", log_file.display(), i);
        if std::path::Path::new(&rotated).exists() {
            rotated_count += 1;
        }
    }

    // We must have at least 3 rotated files to properly test reconciliation
    assert!(
        rotated_count >= 3,
        "Test requires at least 3 rotated files to verify reconciliation, but only found {}. \
         This means the rotation didn't create enough files.",
        rotated_count
    );

    // Read content from each rotated file to verify they contain different data
    let mut first_line_per_file: Vec<(String, i32)> = Vec::new();

    // Check rotated files (oldest to newest: .log.3, .log.2, .log.1)
    for i in (1..=rotated_count).rev() {
        let rotated_path = format!("{}.{}", log_file.display(), i);
        if let Ok(content) = std::fs::read_to_string(&rotated_path) {
            if let Some(first_line) = content.lines().next() {
                // Extract line number from "Log line NNNNNN"
                if let Some(num_str) = first_line.split_whitespace().nth(2) {
                    if let Ok(num) = num_str.parse::<i32>() {
                        first_line_per_file.push((format!(".log.{}", i), num));
                    }
                }
            }
        }
    }

    // Check main log file
    if let Ok(content) = std::fs::read_to_string(&log_file) {
        if let Some(first_line) = content.lines().next() {
            if let Some(num_str) = first_line.split_whitespace().nth(2) {
                if let Ok(num) = num_str.parse::<i32>() {
                    first_line_per_file.push((".log".to_string(), num));
                }
            }
        }
    }

    // Verify that rotated files contain progressively newer data
    // .log.3 should have oldest entries, .log should have newest
    for i in 1..first_line_per_file.len() {
        assert!(
            first_line_per_file[i].1 > first_line_per_file[i - 1].1,
            "Rotated files should contain progressively newer data. \
             {} starts at line {}, but {} starts at line {}",
            first_line_per_file[i - 1].0,
            first_line_per_file[i - 1].1,
            first_line_per_file[i].0,
            first_line_per_file[i].1
        );
    }

    // Now test the actual reconciliation: tail() should merge all files correctly
    let all_entries = buffer.tail(total_lines * 2, Some("test-service"));

    // Verify we got entries (accounting for rotation cleanup - oldest files are deleted)
    assert!(
        all_entries.len() > lines_per_file * 2,
        "Should have entries from multiple files, got {} (expected > {})",
        all_entries.len(),
        lines_per_file * 2
    );

    // Extract line numbers from merged output
    let line_numbers: Vec<i32> = all_entries
        .iter()
        .filter_map(|e| {
            e.line
                .split_whitespace()
                .nth(2)
                .and_then(|s| s.parse::<i32>().ok())
        })
        .collect();

    // Verify the merged output contains entries from multiple rotated files
    // The first entry should come from the oldest available rotated file
    let first_merged_line = line_numbers.first().copied().unwrap_or(0);
    let last_merged_line = line_numbers.last().copied().unwrap_or(0);

    // The range of line numbers should span multiple files worth of data
    let line_range = last_merged_line - first_merged_line;
    assert!(
        line_range >= (lines_per_file * 2) as i32,
        "Merged logs should span multiple files. First line: {}, last line: {}, range: {} \
         (expected range >= {})",
        first_merged_line,
        last_merged_line,
        line_range,
        lines_per_file * 2
    );

    // Verify entries are sorted by timestamp (oldest to newest)
    for i in 1..all_entries.len() {
        assert!(
            all_entries[i - 1].timestamp <= all_entries[i].timestamp,
            "Entries should be sorted by timestamp: {} vs {}",
            all_entries[i - 1].timestamp,
            all_entries[i].timestamp
        );
    }

    // Verify line numbers are sequential (no gaps or duplicates in the merged output)
    for i in 1..line_numbers.len() {
        assert!(
            line_numbers[i] > line_numbers[i - 1],
            "Line numbers should be strictly increasing: {} followed by {} at index {}",
            line_numbers[i - 1],
            line_numbers[i],
            i
        );
    }

    // Verify no duplicates
    let unique_count = line_numbers.len();
    let mut sorted_numbers = line_numbers.clone();
    sorted_numbers.sort();
    sorted_numbers.dedup();
    assert_eq!(
        sorted_numbers.len(),
        unique_count,
        "Should have no duplicate log entries"
    );
}

/// Test that rotated logs maintain correct timestamp ordering (.log.2 oldest, then .log.1, then .log)
#[test]
fn test_rotated_logs_timestamp_ordering() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create a log buffer with very small rotation (1KB) to force frequent rotations
    let rotation_size = 1024; // 1KB max size
    let max_rotated_files = 3;
    let mut buffer = LogBuffer::with_rotation(
        logs_dir.clone(),
        rotation_size,
        max_rotated_files,
    );

    // Each line is ~30 bytes, so 1KB holds ~30 lines
    // We need enough lines to create at least 3 rotated files
    let lines_per_file = 30;
    let total_batches = 15; // Should create ~15 files worth, keeping last 3 rotated + main
    let lines_per_batch = lines_per_file;

    // Write logs in batches with timestamps that increase
    for batch in 0..total_batches {
        for i in 0..lines_per_batch {
            buffer.push(
                "timestamp-test".to_string(),
                format!("B{:02}L{:03}", batch, i),
                LogStream::Stdout,
            );
        }
        // Small delay between batches to ensure timestamp separation
        std::thread::sleep(Duration::from_millis(5));
    }

    // Verify we have multiple rotated files
    let log_file = logs_dir.join("timestamp-test.log");
    let mut rotated_count = 0;
    for i in 1..=max_rotated_files {
        let rotated = format!("{}.{}", log_file.display(), i);
        if std::path::Path::new(&rotated).exists() {
            rotated_count += 1;
        }
    }

    assert!(
        rotated_count >= 2,
        "Test requires at least 2 rotated files, but only found {}",
        rotated_count
    );

    // Read timestamps from each file directly to verify file ordering
    let mut file_timestamps: Vec<(String, u64)> = Vec::new();

    // Check rotated files (oldest to newest: .log.3, .log.2, .log.1)
    for i in (1..=rotated_count).rev() {
        let rotated_path = format!("{}.{}", log_file.display(), i);
        if let Ok(content) = std::fs::read_to_string(&rotated_path) {
            if let Some(first_line) = content.lines().next() {
                // Parse timestamp from log format: "TIMESTAMP SERVICE STREAM MESSAGE"
                if let Some(ts_str) = first_line.split_whitespace().next() {
                    if let Ok(ts) = ts_str.parse::<u64>() {
                        file_timestamps.push((format!(".log.{}", i), ts));
                    }
                }
            }
        }
    }

    // Check main log file
    if let Ok(content) = std::fs::read_to_string(&log_file) {
        if let Some(first_line) = content.lines().next() {
            if let Some(ts_str) = first_line.split_whitespace().next() {
                if let Ok(ts) = ts_str.parse::<u64>() {
                    file_timestamps.push((".log".to_string(), ts));
                }
            }
        }
    }

    // Verify files are ordered by timestamp (older files have older timestamps)
    for i in 1..file_timestamps.len() {
        assert!(
            file_timestamps[i].1 >= file_timestamps[i - 1].1,
            "File timestamps should increase: {} has ts {}, but {} has ts {}",
            file_timestamps[i - 1].0,
            file_timestamps[i - 1].1,
            file_timestamps[i].0,
            file_timestamps[i].1
        );
    }

    // Now verify that tail() merges entries in correct timestamp order
    let entries = buffer.tail(1000, Some("timestamp-test"));

    assert!(
        entries.len() > lines_per_file * 2,
        "Should have entries from multiple files, got {}",
        entries.len()
    );

    // Verify timestamp ordering in merged output
    for i in 1..entries.len() {
        assert!(
            entries[i].timestamp >= entries[i - 1].timestamp,
            "Merged entries should be sorted by timestamp: index {} has ts {}, but index {} has ts {}",
            i - 1,
            entries[i - 1].timestamp,
            i,
            entries[i].timestamp
        );
    }

    // Verify that entries span across batches (which were in different files)
    let batches_seen: std::collections::HashSet<_> = entries
        .iter()
        .filter_map(|e| {
            // Extract batch number from "B##L###" format
            e.line.get(1..3).and_then(|s| s.parse::<u32>().ok())
        })
        .collect();

    assert!(
        batches_seen.len() >= 3,
        "Merged output should contain entries from at least 3 different batches, found {}",
        batches_seen.len()
    );
}

/// Test log pagination with offset and limit
#[test]
fn test_log_pagination_offset_limit() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let buffer = SharedLogBuffer::new(logs_dir);

    // Write 100 log lines
    for i in 0..100 {
        buffer.push(
            "pagination-test".to_string(),
            format!("Line {:03}", i),
            LogStream::Stdout,
        );
    }

    // Test first page
    let (page1, total) = buffer.get_paginated(Some("pagination-test"), 0, 10);
    assert_eq!(total, 100, "Total should be 100");
    assert_eq!(page1.len(), 10, "First page should have 10 entries");
    assert!(page1[0].line.contains("000"), "First entry should be line 000");

    // Test second page
    let (page2, _) = buffer.get_paginated(Some("pagination-test"), 10, 10);
    assert_eq!(page2.len(), 10, "Second page should have 10 entries");
    assert!(page2[0].line.contains("010"), "Second page should start at line 010");

    // Test last page (partial)
    let (last_page, _) = buffer.get_paginated(Some("pagination-test"), 95, 10);
    assert_eq!(last_page.len(), 5, "Last page should have only 5 entries");

    // Test offset beyond total
    let (empty, _) = buffer.get_paginated(Some("pagination-test"), 200, 10);
    assert_eq!(empty.len(), 0, "Should return empty for offset beyond total");
}

/// Test log pagination across rotated files
#[test]
fn test_log_pagination_across_rotated_files() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create buffer with small rotation to ensure multiple files
    let rotation_size = 2 * 1024; // 2KB
    let max_rotated_files = 4;
    let buffer = SharedLogBuffer::with_rotation(
        logs_dir.clone(),
        rotation_size,
        max_rotated_files,
    );

    // Each line is ~70 bytes, so 2KB holds ~28 lines
    // Write enough to create at least 3 rotated files
    let line_size = 70;
    let lines_per_file = rotation_size as usize / line_size;
    let total_lines = lines_per_file * 6; // Should create ~6 files

    for i in 0..total_lines {
        buffer.push(
            "rotated-pagination".to_string(),
            format!("Line {:04} with padding: {}", i, "y".repeat(40)),
            LogStream::Stdout,
        );
    }

    // Verify multiple rotated files exist
    let log_file = logs_dir.join("rotated-pagination.log");
    let mut rotated_count = 0;
    for i in 1..=max_rotated_files {
        let rotated = format!("{}.{}", log_file.display(), i);
        if std::path::Path::new(&rotated).exists() {
            rotated_count += 1;
        }
    }

    assert!(
        rotated_count >= 2,
        "Test requires at least 2 rotated files to test pagination across files, but only found {}",
        rotated_count
    );

    // Get total count first
    let (_, total) = buffer.get_paginated(Some("rotated-pagination"), 0, 1);

    // Verify total spans multiple files worth of data
    assert!(
        total >= lines_per_file * 2,
        "Total ({}) should span at least 2 files worth of entries ({})",
        total,
        lines_per_file * 2
    );

    // Test pagination: get first page
    let page_size = lines_per_file / 2; // Half a file per page
    let (page1, total1) = buffer.get_paginated(Some("rotated-pagination"), 0, page_size);
    assert_eq!(page1.len(), page_size, "First page should have {} entries", page_size);
    assert_eq!(total1, total, "Total should be consistent");

    // Get second page
    let (page2, total2) = buffer.get_paginated(Some("rotated-pagination"), page_size, page_size);
    assert_eq!(page2.len(), page_size, "Second page should have {} entries", page_size);
    assert_eq!(total2, total, "Total should be consistent across pages");

    // Verify pages don't overlap (page2 first entry should come after page1 last entry)
    if let (Some(last_p1), Some(first_p2)) = (page1.last(), page2.first()) {
        assert!(
            first_p2.timestamp >= last_p1.timestamp,
            "Second page should start after first page ends"
        );
    }

    // Get a page that spans across file boundaries
    // This should test reading from multiple rotated files in one pagination call
    let (mid_page, _) = buffer.get_paginated(
        Some("rotated-pagination"),
        lines_per_file - 5,  // Start near end of one file
        lines_per_file + 10, // Span into next file
    );

    assert!(
        mid_page.len() > lines_per_file / 2,
        "Mid page should have entries spanning file boundaries, got {}",
        mid_page.len()
    );

    // Verify all entries across all pages are in timestamp order
    for i in 1..mid_page.len() {
        assert!(
            mid_page[i - 1].timestamp <= mid_page[i].timestamp,
            "Paginated entries should be sorted by timestamp even across file boundaries"
        );
    }

    // Extract line numbers and verify they're sequential (no gaps from file transitions)
    let line_numbers: Vec<i32> = mid_page
        .iter()
        .filter_map(|e| {
            e.line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<i32>().ok())
        })
        .collect();

    for i in 1..line_numbers.len() {
        assert_eq!(
            line_numbers[i],
            line_numbers[i - 1] + 1,
            "Line numbers should be sequential across file boundaries: {} followed by {}",
            line_numbers[i - 1],
            line_numbers[i]
        );
    }
}

/// Test that pagination total count is accurate
#[test]
fn test_log_pagination_total_count_accurate() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let buffer = SharedLogBuffer::new(logs_dir);

    // Write exactly 123 log lines
    let expected_count = 123;
    for i in 0..expected_count {
        buffer.push(
            "count-test".to_string(),
            format!("Entry {}", i),
            LogStream::Stdout,
        );
    }

    // Get paginated result
    let (_, total) = buffer.get_paginated(Some("count-test"), 0, 10);
    assert_eq!(total, expected_count, "Total count should be exact");

    // Get with different offset/limit - total should be consistent
    let (_, total2) = buffer.get_paginated(Some("count-test"), 50, 20);
    assert_eq!(total2, expected_count, "Total should be consistent across pagination calls");

    // Filter by non-existent service
    let (entries, total_none) = buffer.get_paginated(Some("non-existent"), 0, 10);
    assert_eq!(total_none, 0, "Non-existent service should have 0 total");
    assert_eq!(entries.len(), 0, "Non-existent service should return empty");
}

/// Test that clear_service removes both main log file and all rotated files
#[test]
fn test_clear_service_removes_rotated_logs() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create buffer with small rotation to create multiple files
    let rotation_size = 1024; // 1KB
    let max_rotated_files = 4;
    let mut buffer = LogBuffer::with_rotation(
        logs_dir.clone(),
        rotation_size,
        max_rotated_files,
    );

    // Write enough data to create multiple rotated files
    // Each line is ~50 bytes, so 1KB holds ~20 lines
    let lines_per_file = 20;
    let total_lines = lines_per_file * 6; // Should create multiple rotations

    for i in 0..total_lines {
        buffer.push(
            "retention-test".to_string(),
            format!("Line {:04} padding: {}", i, "z".repeat(20)),
            LogStream::Stdout,
        );
    }

    // Verify we have rotated files
    let log_file = logs_dir.join("retention-test.log");
    assert!(log_file.exists(), "Main log file should exist");

    let mut rotated_files_before = Vec::new();
    for i in 1..=max_rotated_files {
        let rotated = PathBuf::from(format!("{}.{}", log_file.display(), i));
        if rotated.exists() {
            rotated_files_before.push(rotated);
        }
    }

    assert!(
        rotated_files_before.len() >= 2,
        "Test requires at least 2 rotated files, found {}",
        rotated_files_before.len()
    );

    // Now clear the service logs (simulating log retention policy)
    buffer.clear_service("retention-test");

    // Verify main log file is gone
    assert!(
        !log_file.exists(),
        "Main log file should be removed after clear_service"
    );

    // Verify ALL rotated files are gone
    for rotated in &rotated_files_before {
        assert!(
            !rotated.exists(),
            "Rotated file {:?} should be removed after clear_service",
            rotated
        );
    }

    // Double-check by scanning the directory
    let remaining_files: Vec<_> = std::fs::read_dir(&logs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("retention-test")
        })
        .collect();

    assert!(
        remaining_files.is_empty(),
        "No files for retention-test should remain, found: {:?}",
        remaining_files.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

/// Test that clear_service_prefix removes rotated logs for all matching services
#[test]
fn test_clear_service_prefix_removes_rotated_logs() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create buffer with small rotation
    let rotation_size = 1024; // 1KB
    let max_rotated_files = 3;
    let mut buffer = LogBuffer::with_rotation(
        logs_dir.clone(),
        rotation_size,
        max_rotated_files,
    );

    // Write logs for multiple services with a common prefix
    let lines_per_file = 20;
    let total_lines = lines_per_file * 4;

    for i in 0..total_lines {
        buffer.push(
            "_prefix_.hook1".to_string(),
            format!("Hook1 line {:04} pad: {}", i, "a".repeat(20)),
            LogStream::Stdout,
        );
        buffer.push(
            "_prefix_.hook2".to_string(),
            format!("Hook2 line {:04} pad: {}", i, "b".repeat(20)),
            LogStream::Stdout,
        );
        // Also write to a service that should NOT be cleared
        buffer.push(
            "other-service".to_string(),
            format!("Other line {:04} pad: {}", i, "c".repeat(20)),
            LogStream::Stdout,
        );
    }

    // Verify files exist
    let hook1_log = logs_dir.join("_prefix_.hook1.log");
    let hook2_log = logs_dir.join("_prefix_.hook2.log");
    let other_log = logs_dir.join("other-service.log");

    assert!(hook1_log.exists(), "hook1 main log should exist");
    assert!(hook2_log.exists(), "hook2 main log should exist");
    assert!(other_log.exists(), "other-service main log should exist");

    // We should have at least some files for the hooks
    // (they might share rotation or have their own)

    // Clear all services with the prefix
    buffer.clear_service_prefix("_prefix_");

    // Verify hook1 and hook2 files are ALL gone (main + rotated)
    assert!(
        !hook1_log.exists(),
        "hook1 main log should be removed"
    );
    assert!(
        !hook2_log.exists(),
        "hook2 main log should be removed"
    );

    // Check no rotated files for hook1 remain
    for i in 1..=max_rotated_files {
        let rotated = format!("{}.{}", hook1_log.display(), i);
        assert!(
            !std::path::Path::new(&rotated).exists(),
            "hook1 rotated file .{} should be removed",
            i
        );
    }

    // Check no rotated files for hook2 remain
    for i in 1..=max_rotated_files {
        let rotated = format!("{}.{}", hook2_log.display(), i);
        assert!(
            !std::path::Path::new(&rotated).exists(),
            "hook2 rotated file .{} should be removed",
            i
        );
    }

    // Verify other-service is STILL there (should not be affected)
    assert!(
        other_log.exists(),
        "other-service main log should NOT be removed"
    );

    // Verify we can still read logs from other-service
    let other_entries = buffer.tail(100, Some("other-service"));
    assert!(
        !other_entries.is_empty(),
        "other-service should still have logs"
    );
}

/// Test that clear (clear all) removes all rotated logs for all services
#[test]
fn test_clear_all_removes_all_rotated_logs() {
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create buffer with small rotation
    let rotation_size = 1024; // 1KB
    let max_rotated_files = 3;
    let mut buffer = LogBuffer::with_rotation(
        logs_dir.clone(),
        rotation_size,
        max_rotated_files,
    );

    // Write logs for multiple services
    let lines_per_file = 20;
    let total_lines = lines_per_file * 5;

    for i in 0..total_lines {
        buffer.push(
            "service-a".to_string(),
            format!("A line {:04} padding: {}", i, "x".repeat(25)),
            LogStream::Stdout,
        );
        buffer.push(
            "service-b".to_string(),
            format!("B line {:04} padding: {}", i, "y".repeat(25)),
            LogStream::Stdout,
        );
    }

    // Verify files exist before clear
    let svc_a_log = logs_dir.join("service-a.log");
    let svc_b_log = logs_dir.join("service-b.log");

    assert!(svc_a_log.exists(), "service-a main log should exist");
    assert!(svc_b_log.exists(), "service-b main log should exist");

    // Count total files in logs dir before clear
    let files_before: Vec<_> = std::fs::read_dir(&logs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();

    assert!(
        files_before.len() >= 2,
        "Should have at least 2 log files before clear"
    );

    // Clear all logs
    buffer.clear();

    // Verify logs directory is empty (or only contains non-log files like metadata)
    let remaining_log_files: Vec<_> = std::fs::read_dir(&logs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".log") ||
                    e.file_name().to_string_lossy().contains(".log."))
        .collect();

    assert!(
        remaining_log_files.is_empty(),
        "All log files should be removed after clear(), found: {:?}",
        remaining_log_files.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    // Verify tail returns empty
    let all_entries = buffer.tail(1000, None);
    assert!(
        all_entries.is_empty(),
        "Buffer should be empty after clear()"
    );
}
