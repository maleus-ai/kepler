//! Server-side cursor management for efficient log streaming.
//!
//! This module provides `CursorManager` which tracks file positions for
//! cursor-based log streaming. Each cursor tracks where the client left off
//! reading, allowing efficient incremental reads without re-scanning files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::logs::{LogReader, MergedLogIterator};
use kepler_protocol::protocol::{MAX_CURSOR_BATCH_SIZE, MAX_MESSAGE_SIZE};

/// Budget for log entry payload within a cursor response.
/// Leaves headroom for the Response envelope JSON overhead.
const CURSOR_RESPONSE_BUDGET: usize = MAX_MESSAGE_SIZE * 8 / 10;

/// State for a single cursor — holds the iterator with open file handles.
pub struct CursorState {
    /// Persistent iterator that keeps file handles open across batches
    iterator: MergedLogIterator,
    /// Last time this cursor was polled (for TTL)
    last_polled: Instant,
    /// Config this cursor belongs to (for validation)
    config_path: PathBuf,
    /// Logs directory for discovering new files
    logs_dir: PathBuf,
    /// Service filter (None = all services)
    service_filter: Option<String>,
    /// Whether to exclude hook log files
    no_hooks: bool,
    /// Set of file paths known to the iterator (to detect new files)
    known_files: HashSet<PathBuf>,
}

/// Error types for cursor operations
#[derive(Debug)]
pub enum CursorError {
    /// Cursor was not found (expired or invalid)
    CursorExpired(String),
    /// Cursor belongs to a different config
    CursorConfigMismatch {
        cursor_config: PathBuf,
        requested_config: PathBuf,
    },
    /// IO error during reading
    IoError(std::io::Error),
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CursorError::CursorExpired(id) => write!(f, "Cursor expired or invalid: {}", id),
            CursorError::CursorConfigMismatch {
                cursor_config,
                requested_config,
            } => write!(
                f,
                "Cursor belongs to config {:?}, not {:?}",
                cursor_config, requested_config
            ),
            CursorError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for CursorError {}

impl From<std::io::Error> for CursorError {
    fn from(e: std::io::Error) -> Self {
        CursorError::IoError(e)
    }
}

static CURSOR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique cursor ID
fn generate_cursor_id() -> String {
    let id = CURSOR_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("cursor_{:x}", id)
}

/// Manages server-side cursors for log streaming.
///
/// Cursors are stored at the daemon level (not per-config) to centralize TTL cleanup.
/// Each cursor holds a `MergedLogIterator` with open file handles, wrapped in
/// `Arc<Mutex<>>` so the DashMap shard lock is held only for an Arc clone (~ns),
/// and all file I/O happens under the per-cursor Mutex.
pub struct CursorManager {
    /// Map of cursor ID -> cursor state (Arc<Mutex<>> for per-cursor locking)
    cursors: DashMap<String, Arc<Mutex<CursorState>>>,
    /// Time-to-live for cursors (based on last poll time)
    ttl: Duration,
}

impl CursorManager {
    /// Create a new cursor manager with the specified TTL in seconds
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            cursors: DashMap::new(),
            ttl: Duration::from_secs(ttl_seconds),
        }
    }

    /// Create a new cursor at start or end of files.
    ///
    /// - `from_start=true`: Start at beginning of files (for 'all' mode)
    /// - `from_start=false`: Start at end of files (for 'follow' mode)
    ///
    /// Returns the cursor ID.
    pub fn create_cursor(
        &self,
        config_path: PathBuf,
        logs_dir: PathBuf,
        service: Option<String>,
        from_start: bool,
        no_hooks: bool,
    ) -> String {
        let cursor_id = generate_cursor_id();

        let reader = LogReader::new(logs_dir.clone());
        let files = reader.collect_log_files(service.as_deref(), no_hooks);

        // Track known files for later new-file discovery
        let known_files: HashSet<PathBuf> = files.iter().map(|(path, _, _)| path.clone()).collect();

        let iterator = if from_start {
            MergedLogIterator::new(files)
        } else {
            // For follow mode, start at end of existing files
            let mut positions = HashMap::new();
            for (path, _, _) in &files {
                if let Ok(metadata) = std::fs::metadata(path) {
                    positions.insert(path.clone(), metadata.len());
                }
            }
            MergedLogIterator::with_positions(files, &positions)
        };

        let state = Arc::new(Mutex::new(CursorState {
            iterator,
            last_polled: Instant::now(),
            config_path,
            logs_dir,
            service_filter: service,
            no_hooks,
            known_files,
        }));

        self.cursors.insert(cursor_id.clone(), state);
        cursor_id
    }

    /// Read entries using a cursor, iterating in-place.
    ///
    /// Locking protocol:
    /// 1. DashMap read lock on shard (shared, brief) → clone Arc
    /// 2. Drop DashMap Ref → shard unlocked
    /// 3. Per-cursor Mutex lock → file I/O
    /// 4. Drop MutexGuard → cursor unlocked
    ///
    /// Returns (entries, has_more) where has_more indicates if there's more data to read.
    pub fn read_entries(
        &self,
        cursor_id: &str,
        config_path: &Path,
    ) -> Result<(Vec<crate::logs::LogLine>, bool), CursorError> {
        // Clone Arc, then DashMap Ref is dropped → shard unlocked
        let arc = self
            .cursors
            .get(cursor_id)
            .ok_or_else(|| CursorError::CursorExpired(cursor_id.to_string()))?
            .clone();

        let mut cursor = arc.lock().unwrap_or_else(|e| e.into_inner());

        // Validate config path
        if cursor.config_path != config_path {
            return Err(CursorError::CursorConfigMismatch {
                cursor_config: cursor.config_path.clone(),
                requested_config: config_path.to_path_buf(),
            });
        }

        cursor.last_polled = Instant::now();

        // Discover new log files created since the cursor was created
        // (e.g., services that started after the cursor was opened)
        let reader = LogReader::new(cursor.logs_dir.clone());
        let current_files = reader.collect_log_files(cursor.service_filter.as_deref(), cursor.no_hooks);
        let new_files: Vec<_> = current_files
            .into_iter()
            .filter(|(path, _, _)| !cursor.known_files.contains(path))
            .collect();

        if !new_files.is_empty() {
            for (path, _, _) in &new_files {
                cursor.known_files.insert(path.clone());
            }
            cursor.iterator.add_sources(new_files);
        }

        // Retry sources that previously hit EOF (picks up appended data in follow mode)
        cursor.iterator.retry_eof_sources();

        let mut entries = Vec::with_capacity(MAX_CURSOR_BATCH_SIZE.min(8_000));
        let mut estimated_size: usize = 0;

        for log_line in cursor.iterator.by_ref() {
            // Estimate bincode size: ~20 bytes overhead + service name + line content
            let entry_size = 20 + log_line.service.len() + log_line.line.len();
            if !entries.is_empty()
                && (estimated_size + entry_size >= CURSOR_RESPONSE_BUDGET
                    || entries.len() >= MAX_CURSOR_BATCH_SIZE)
            {
                // Push back: put this line into the iterator's pending slot
                cursor.iterator.push_back(log_line);
                break;
            }
            estimated_size += entry_size;
            entries.push(log_line);
        }

        let has_more = cursor.iterator.has_more();
        Ok((entries, has_more))
    }

    /// Remove stale cursors (those not polled within TTL).
    /// Called periodically by a background task.
    pub fn cleanup_stale(&self) {
        let now = Instant::now();
        self.cursors.retain(|_id, arc| {
            let cursor = arc.lock().unwrap_or_else(|e| e.into_inner());
            now.duration_since(cursor.last_polled) < self.ttl
        });
    }
}

#[cfg(test)]
mod tests {
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
        );

        // First read: nothing (cursor starts at EOF)
        let (entries, _) = manager.read_entries(&cursor_id, &config_path).unwrap();
        assert_eq!(entries.len(), 0);

        // Append new data
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(logs_dir.join("svc.stdout.log"))
            .unwrap();
        write!(file, "2000\tnew-line\n").unwrap();
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
                        mgr.create_cursor(cfg.clone(), logs, None, true, false);

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
                                mgr.create_cursor(cfg.clone(), logs.clone(), None, true, false);
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
            manager.create_cursor(config_a, logs_dir, None, true, false);

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
}
