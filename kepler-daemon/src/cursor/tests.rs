use super::*;
use std::fs;
use std::io::Write;
use std::sync::Arc;

/// Write log lines in the expected format: TIMESTAMP\tCONTENT\n
fn write_log_file(dir: &Path, service: &str, stream: &str, lines: &[(i64, &str)]) {
    let filename = format!("{}.{}.log", service, stream);
    let content: String = lines
        .iter()
        .map(|(ts, msg)| format!("{}\t{}\n", ts, msg))
        .collect();
    fs::write(dir.join(filename), content).unwrap();
}

// ========================================================================
// Byte-budget batching
// ========================================================================

/// Large lines trigger the byte budget well before the entry count limit.
#[test]
fn test_byte_budget_limits_large_lines() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    // Each line ~10KB → budget (8MB) reached after ~800 entries
    let large_payload = "X".repeat(10_000);
    let total = 2000;
    let mut content = String::new();
    for i in 0..total {
        content.push_str(&format!("{}\t{}\n", 1000 + i, large_payload));
    }
    fs::write(dir.path().join("svc.stdout.log"), &content).unwrap();

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();

    assert!(
        entries.len() < total,
        "Byte budget should limit batch: got {} of {} entries",
        entries.len(),
        total
    );
    assert!(has_more, "Should have more entries to read");

    // Budget overshoot is at most one entry
    let estimated: usize = entries
        .iter()
        .map(|e| 20 + e.service.len() + e.line.len())
        .sum();
    let max_entry = entries
        .iter()
        .map(|e| 20 + e.service.len() + e.line.len())
        .max()
        .unwrap();
    assert!(
        estimated <= CURSOR_RESPONSE_BUDGET + max_entry,
        "Estimated {} should be near budget {} (max_entry={})",
        estimated,
        CURSOR_RESPONSE_BUDGET,
        max_entry
    );
}

/// Small lines reach the entry count cap before the byte budget.
#[test]
fn test_small_lines_hit_count_limit() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    let total = MAX_CURSOR_BATCH_SIZE + 10_000;
    let mut content = String::new();
    for i in 0..total {
        content.push_str(&format!("{}\tline-{}\n", 1000 + i, i));
    }
    fs::write(dir.path().join("svc.stdout.log"), &content).unwrap();

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();

    assert_eq!(entries.len(), MAX_CURSOR_BATCH_SIZE);
    assert!(has_more);
}

/// Mixed small and large lines: every batch stays within budget + one entry.
#[test]
fn test_mixed_line_sizes_respect_budget() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    let mut content = String::new();
    let total = 10_000;
    for i in 0..total {
        let payload = if i % 10 == 0 {
            "H".repeat(50_000) // ~50KB every 10th line
        } else {
            format!("small-{}", i)
        };
        content.push_str(&format!("{}\t{}\n", 1000 + i, payload));
    }
    fs::write(dir.path().join("svc.stdout.log"), &content).unwrap();

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let mut all_count = 0;
    let mut batches = 0;
    loop {
        let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();

        if entries.len() > 1 {
            let batch_size: usize = entries
                .iter()
                .map(|e| 20 + e.service.len() + e.line.len())
                .sum();
            let max_entry = entries
                .iter()
                .map(|e| 20 + e.service.len() + e.line.len())
                .max()
                .unwrap();
            assert!(
                batch_size <= CURSOR_RESPONSE_BUDGET + max_entry,
                "Batch {} overshot budget: {} vs {} (max_entry={})",
                batches,
                batch_size,
                CURSOR_RESPONSE_BUDGET,
                max_entry
            );
        }

        all_count += entries.len();
        batches += 1;
        if !has_more {
            break;
        }
        assert!(batches < 500, "Too many batches — possible infinite loop");
    }

    assert_eq!(all_count, total, "All entries should be read");
    assert!(batches > 1, "Should require multiple batches due to large lines");
}

// ========================================================================
// Complete reading — no data loss, no duplicates
// ========================================================================

/// Multiple read_entries() calls cover every line exactly once.
#[test]
fn test_cursor_reads_all_entries_no_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    let payload = "Y".repeat(5_000); // ~5KB → budget hit after ~1,600 entries
    let total: usize = 5_000;
    let mut content = String::new();
    for i in 0..total {
        content.push_str(&format!("{}\t{}-{:05}\n", 1000 + i, payload, i));
    }
    fs::write(dir.path().join("svc.stdout.log"), &content).unwrap();

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let mut all_entries = Vec::new();
    let mut iterations = 0;
    loop {
        let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();
        all_entries.extend(entries);
        iterations += 1;
        if !has_more {
            break;
        }
        assert!(iterations < 100, "Too many iterations — possible infinite loop");
    }

    assert_eq!(all_entries.len(), total, "All entries should be read");
    assert!(iterations > 1, "Should require multiple batches");

    // Chronological order
    for i in 1..all_entries.len() {
        assert!(all_entries[i - 1].timestamp <= all_entries[i].timestamp);
    }

    // No duplicates / no gaps — every sequential index appears exactly once
    for (i, entry) in all_entries.iter().enumerate() {
        let expected_suffix = format!("-{:05}", i);
        assert!(
            entry.line.ends_with(&expected_suffix),
            "Entry {} should end with '{}', got '…{}'",
            i,
            expected_suffix,
            &entry.line[entry.line.len().saturating_sub(10)..],
        );
    }
}

/// Position tracking across batches: sequential indices, no gaps, no repeats.
#[test]
fn test_cursor_position_tracking_no_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    let payload = "Z".repeat(8_000);
    let total: usize = 3_000;
    let mut content = String::new();
    for i in 0..total {
        content.push_str(&format!("{}\t{}-{:04}\n", 1000 + i, payload, i));
    }
    fs::write(dir.path().join("svc.stdout.log"), &content).unwrap();

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let mut seen_indices = Vec::new();
    loop {
        let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();
        for entry in &entries {
            let idx: usize = entry.line.rsplit('-').next().unwrap().parse().unwrap();
            seen_indices.push(idx);
        }
        if !has_more {
            break;
        }
    }

    assert_eq!(seen_indices.len(), total);
    for (i, &idx) in seen_indices.iter().enumerate() {
        assert_eq!(idx, i, "Expected index {} at position {}, got {}", i, i, idx);
    }
}

// ========================================================================
// Multi-service and filtering
// ========================================================================

/// Interleaved timestamps from two services merge chronologically.
#[test]
fn test_multi_service_cursor_reads_all() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "alpha", "stdout", &[
        (1000, "a1"),
        (3000, "a2"),
        (5000, "a3"),
    ]);
    write_log_file(dir.path(), "beta", "stdout", &[
        (2000, "b1"),
        (4000, "b2"),
        (6000, "b3"),
    ]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert!(!has_more);
    assert_eq!(entries.len(), 6);

    let lines: Vec<&str> = entries.iter().map(|e| e.line.as_str()).collect();
    assert_eq!(lines, vec!["a1", "b1", "a2", "b2", "a3", "b3"]);
}

/// Service filter limits cursor to a single service.
#[test]
fn test_service_filter_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "alpha", "stdout", &[(1000, "a1"), (3000, "a2")]);
    write_log_file(dir.path(), "beta", "stdout", &[(2000, "b1"), (4000, "b2")]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        Some("alpha".to_string()),
        true,
        false,
        0,
    );

    let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert!(!has_more);
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| &*e.service == "alpha"));
}

// ========================================================================
// Cursor create modes (from_start / follow)
// ========================================================================

/// from_start=true reads all existing entries.
#[test]
fn test_cursor_from_start_reads_everything() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[
        (1000, "first"),
        (2000, "second"),
        (3000, "third"),
    ]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert!(!has_more);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].line, "first");
    assert_eq!(entries[1].line, "second");
    assert_eq!(entries[2].line, "third");
}

/// from_start=false (follow mode) skips existing data.
#[test]
fn test_cursor_from_end_reads_nothing_existing() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[
        (1000, "first"),
        (2000, "second"),
        (3000, "third"),
    ]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        false,
        false,
        0,
    );

    let (entries, has_more) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 0);
    assert!(!has_more);
}

/// Follow cursor picks up lines appended after creation.
#[test]
fn test_cursor_follow_picks_up_new_data() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "old-line")]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir.clone(),
        None,
        false,
        false,
        0,
    );

    // First read: nothing (cursor starts at EOF)
    let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 0);

    // Append new data
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(logs_dir.join("svc.stdout.log"))
        .unwrap();
    writeln!(file, "2000\tnew-line").unwrap();
    drop(file);

    // Second read: picks up the appended line
    let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].line, "new-line");
}

/// Cursor discovers new log files created after cursor creation.
/// Simulates services that start producing logs after the cursor is opened.
#[test]
fn test_cursor_discovers_new_files() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    // Only "migration" log exists at cursor creation
    write_log_file(dir.path(), "migration", "stdout", &[
        (1000, "migrating"),
        (2000, "done"),
    ]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir.clone(),
        None,
        true,
        false,
        0,
    );

    // First read: only migration logs
    let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| &*e.service == "migration"));

    // New service starts producing logs
    write_log_file(dir.path(), "web", "stdout", &[
        (3000, "web-started"),
        (4000, "web-running"),
    ]);

    // Second read: discovers and returns new web logs
    let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 2, "Should discover new web log file: {:?}", entries);
    assert!(entries.iter().all(|e| &*e.service == "web"));
}

/// Cursor detects a replaced log file (deleted + recreated with new inode)
/// and picks up the new content.
#[cfg(unix)]
#[test]
fn test_cursor_detects_replaced_file() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    // Create initial log file
    write_log_file(dir.path(), "worker", "stdout", &[
        (1000, "old-line-1"),
        (2000, "old-line-2"),
    ]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir.clone(),
        None,
        true,
        false,
        0,
    );

    // First read: get the initial lines
    let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 2);

    // Delete and recreate the file (new inode, simulates stop + restart)
    let log_path = logs_dir.join("worker.stdout.log");
    fs::remove_file(&log_path).unwrap();
    write_log_file(dir.path(), "worker", "stdout", &[
        (3000, "new-line-1"),
        (4000, "new-line-2"),
        (5000, "new-line-3"),
    ]);

    // Second read: cursor should detect the inode change and pick up new content
    let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
    assert_eq!(entries.len(), 3, "Should detect replaced file and read new content: {:?}", entries);
    assert_eq!(entries[0].line, "new-line-1");
    assert_eq!(entries[1].line, "new-line-2");
    assert_eq!(entries[2].line, "new-line-3");
}

// ========================================================================
// Concurrency — detect deadlocks
// ========================================================================

/// Multiple threads reading different cursors on the same CursorManager.
#[test]
fn test_concurrent_cursors_no_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    for svc_id in 0..5 {
        let mut content = String::new();
        for i in 0..1_000 {
            content.push_str(&format!("{}\tsvc{}-line-{}\n", 1000 + i, svc_id, i));
        }
        fs::write(
            dir.path().join(format!("svc{}.stdout.log", svc_id)),
            &content,
        )
        .unwrap();
    }

    let manager = Arc::new(CursorManager::new(300));

    let handles: Vec<_> = (0..10)
        .map(|thread_id| {
            let mgr = manager.clone();
            let logs = logs_dir.clone();
            let cfg = config_path.clone();
            std::thread::spawn(move || {
                let cursor_id =
                    mgr.create_cursor(cfg.clone(), logs, None, true, false, 0);

                let mut total = 0;
                let mut iters = 0;
                loop {
                    let (entries, has_more) =
                        mgr.read_entries(&cursor_id, &cfg).unwrap();
                    total += entries.len();
                    iters += 1;
                    if !has_more {
                        break;
                    }
                    assert!(iters < 1000, "Thread {} stuck in loop", thread_id);
                }

                // 5 services × 1,000 lines = 5,000
                assert_eq!(total, 5_000, "Thread {} read {} entries", thread_id, total);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("Thread panicked — possible deadlock");
    }
}

/// Concurrent create + read + cleanup doesn't deadlock.
#[test]
fn test_concurrent_create_read_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[
        (1000, "line-1"),
        (2000, "line-2"),
        (3000, "line-3"),
    ]);

    let manager = Arc::new(CursorManager::new(300));

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let mgr = manager.clone();
            let logs = logs_dir.clone();
            let cfg = config_path.clone();
            std::thread::spawn(move || {
                if i % 3 == 0 {
                    // Cleanup thread
                    for _ in 0..50 {
                        mgr.cleanup_stale();
                        std::thread::yield_now();
                    }
                } else {
                    // Reader thread
                    for _ in 0..20 {
                        let cid =
                            mgr.create_cursor(cfg.clone(), logs.clone(), None, true, false, 0);
                        // Cursor may be cleaned up between create and read, that's OK
                        let _ = mgr.read_entries(&cid, &cfg);
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("Thread panicked — possible deadlock");
    }
}

// ========================================================================
// Error cases
// ========================================================================

/// Reading with wrong config path returns CursorConfigMismatch.
#[test]
fn test_cursor_config_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_a = PathBuf::from("/fake/config-a.yaml");
    let config_b = PathBuf::from("/fake/config-b.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    let manager = CursorManager::new(300);
    let cursor_id =
        manager.create_cursor(config_a, logs_dir, None, true, false, 0);

    let result = manager.read_entries(&cursor_id, &config_b);
    assert!(matches!(result, Err(CursorError::CursorConfigMismatch { .. })));
}

/// Expired cursors are removed by cleanup_stale.
#[test]
fn test_cursor_expired_after_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    // TTL = 0 → immediately stale
    let manager = CursorManager::new(0);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    std::thread::sleep(Duration::from_millis(10));
    manager.cleanup_stale();

    let result = manager.read_entries(&cursor_id, &config_path);
    assert!(matches!(result, Err(CursorError::CursorExpired(_))));
}

/// Reading a nonexistent cursor returns CursorExpired.
#[test]
fn test_cursor_nonexistent_id() {
    let config_path = PathBuf::from("/fake/config.yaml");
    let manager = CursorManager::new(300);

    let result = manager.read_entries("cursor_does_not_exist", &config_path);
    assert!(matches!(result, Err(CursorError::CursorExpired(_))));
}

// ========================================================================
// Cursor invalidation
// ========================================================================

/// invalidate_config_cursors removes only cursors belonging to the target config.
#[test]
fn test_invalidate_config_cursors_removes_matching() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_a = PathBuf::from("/fake/config-a.yaml");
    let config_b = PathBuf::from("/fake/config-b.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    let manager = CursorManager::new(300);
    let cursor_a = manager.create_cursor(
        config_a.clone(),
        logs_dir.clone(),
        None,
        true,
        false,
        1,
    );
    let cursor_b = manager.create_cursor(
        config_b.clone(),
        logs_dir,
        None,
        true,
        false,
        2,
    );

    // Both cursors are alive
    assert!(manager.read_entries(&cursor_a, &config_a).is_ok());
    assert!(manager.read_entries(&cursor_b, &config_b).is_ok());

    // Invalidate config_a cursors
    manager.invalidate_config_cursors(&config_a);

    // cursor_a should be gone, cursor_b should survive
    assert!(matches!(
        manager.read_entries(&cursor_a, &config_a),
        Err(CursorError::CursorExpired(_))
    ));
    assert!(manager.read_entries(&cursor_b, &config_b).is_ok());
}

/// invalidate_config_cursors removes multiple cursors for the same config.
#[test]
fn test_invalidate_config_cursors_removes_all_for_config() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    let manager = CursorManager::new(300);
    let cursor_1 = manager.create_cursor(
        config_path.clone(),
        logs_dir.clone(),
        None,
        true,
        false,
        1,
    );
    let cursor_2 = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        2,
    );

    manager.invalidate_config_cursors(&config_path);

    assert!(matches!(
        manager.read_entries(&cursor_1, &config_path),
        Err(CursorError::CursorExpired(_))
    ));
    assert!(matches!(
        manager.read_entries(&cursor_2, &config_path),
        Err(CursorError::CursorExpired(_))
    ));
}

/// invalidate_connection_cursors removes only cursors belonging to the target connection.
#[test]
fn test_invalidate_connection_cursors_removes_matching() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    let manager = CursorManager::new(300);
    let cursor_conn1 = manager.create_cursor(
        config_path.clone(),
        logs_dir.clone(),
        None,
        true,
        false,
        100,
    );
    let cursor_conn2 = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        200,
    );

    // Both cursors are alive
    assert!(manager.read_entries(&cursor_conn1, &config_path).is_ok());
    assert!(manager.read_entries(&cursor_conn2, &config_path).is_ok());

    // Invalidate connection 100
    manager.invalidate_connection_cursors(100);

    // cursor_conn1 should be gone, cursor_conn2 should survive
    assert!(matches!(
        manager.read_entries(&cursor_conn1, &config_path),
        Err(CursorError::CursorExpired(_))
    ));
    assert!(manager.read_entries(&cursor_conn2, &config_path).is_ok());
}

/// invalidate_connection_cursors removes multiple cursors for the same connection.
#[test]
fn test_invalidate_connection_cursors_removes_all_for_connection() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let config_a = PathBuf::from("/fake/config-a.yaml");
    let config_b = PathBuf::from("/fake/config-b.yaml");

    write_log_file(dir_a.path(), "svc", "stdout", &[(1000, "a")]);
    write_log_file(dir_b.path(), "svc", "stdout", &[(1000, "b")]);

    let manager = CursorManager::new(300);
    // Same connection_id (42) opens cursors on two different configs
    let cursor_1 = manager.create_cursor(
        config_a.clone(),
        dir_a.path().to_path_buf(),
        None,
        true,
        false,
        42,
    );
    let cursor_2 = manager.create_cursor(
        config_b.clone(),
        dir_b.path().to_path_buf(),
        None,
        true,
        false,
        42,
    );

    manager.invalidate_connection_cursors(42);

    assert!(matches!(
        manager.read_entries(&cursor_1, &config_a),
        Err(CursorError::CursorExpired(_))
    ));
    assert!(matches!(
        manager.read_entries(&cursor_2, &config_b),
        Err(CursorError::CursorExpired(_))
    ));
}

/// Invalidating a config with no matching cursors is a no-op.
#[test]
fn test_invalidate_config_cursors_noop_when_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");
    let other_config = PathBuf::from("/fake/other.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        0,
    );

    // Invalidate a different config — cursor should survive
    manager.invalidate_config_cursors(&other_config);

    assert!(manager.read_entries(&cursor_id, &config_path).is_ok());
}

/// Invalidating a connection with no matching cursors is a no-op.
#[test]
fn test_invalidate_connection_cursors_noop_when_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[(1000, "hello")]);

    let manager = CursorManager::new(300);
    let cursor_id = manager.create_cursor(
        config_path.clone(),
        logs_dir,
        None,
        true,
        false,
        10,
    );

    // Invalidate a different connection — cursor should survive
    manager.invalidate_connection_cursors(999);

    assert!(manager.read_entries(&cursor_id, &config_path).is_ok());
}

/// Concurrent invalidation + read doesn't deadlock.
#[test]
fn test_concurrent_invalidation_no_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let logs_dir = dir.path().to_path_buf();
    let config_path = PathBuf::from("/fake/config.yaml");

    write_log_file(dir.path(), "svc", "stdout", &[
        (1000, "line-1"),
        (2000, "line-2"),
        (3000, "line-3"),
    ]);

    let manager = Arc::new(CursorManager::new(300));

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let mgr = manager.clone();
            let logs = logs_dir.clone();
            let cfg = config_path.clone();
            std::thread::spawn(move || {
                match i % 4 {
                    0 => {
                        // Config invalidation thread
                        for _ in 0..50 {
                            mgr.invalidate_config_cursors(&cfg);
                            std::thread::yield_now();
                        }
                    }
                    1 => {
                        // Connection invalidation thread
                        for conn in 0..50u64 {
                            mgr.invalidate_connection_cursors(conn);
                            std::thread::yield_now();
                        }
                    }
                    _ => {
                        // Reader thread
                        for j in 0..20 {
                            let cid = mgr.create_cursor(
                                cfg.clone(),
                                logs.clone(),
                                None,
                                true,
                                false,
                                i as u64 * 1000 + j,
                            );
                            // Cursor may be invalidated between create and read
                            let _ = mgr.read_entries(&cid, &cfg);
                        }
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("Thread panicked — possible deadlock");
    }
}
