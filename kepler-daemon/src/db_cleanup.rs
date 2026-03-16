//! Shared SQLite cleanup utilities for log store and monitor.
//!
//! Both subsystems use the same batched-DELETE-with-time-budget pattern
//! for retention cleanup, and the same connection pragma setup.
//!
//! # Cleanup strategy
//!
//! Expired rows are deleted in fixed-size batches (default 5,000 for periodic,
//! 50,000 for startup). Between batches, the function checks whether a
//! cumulative time budget has been exceeded (default 25ms for periodic cleanup).
//! This prevents long cleanup runs from starving writes while still making
//! progress on large backlogs. If more rows remain after the budget is
//! exhausted, the caller is told via [`CleanupResult::TimeBudgetExceeded`]
//! and can resume on the next cleanup interval.
//!
//! Startup cleanup uses no time budget — it runs to completion since the
//! database may have accumulated a large backlog while the daemon was stopped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::config::StorageMode;

// ============================================================================
// Constants
// ============================================================================

/// Number of rows deleted per batch during periodic retention cleanup.
pub const PERIODIC_CLEANUP_BATCH: i64 = 5_000;
/// Number of rows deleted per batch during startup retention cleanup.
/// Larger than periodic because startup runs without a time budget.
pub const STARTUP_CLEANUP_BATCH: i64 = 50_000;
/// Default cumulative time budget for periodic cleanup passes (25ms).
/// Checked between batches, not mid-query — a single slow batch is allowed
/// to complete even if it exceeds this duration.
pub const DEFAULT_CLEANUP_TIME_BUDGET: Duration = Duration::from_millis(25);

// ============================================================================
// Cleanup result
// ============================================================================

/// Outcome of a time-budgeted cleanup pass.
pub enum CleanupResult {
    /// All expired rows deleted (last batch was smaller than batch_size).
    Complete,
    /// Time budget exceeded — more expired rows likely remain.
    TimeBudgetExceeded,
    /// Discard flag was set — caller should exit.
    Discarded,
}

// ============================================================================
// Batched cleanup
// ============================================================================

/// Run batched DELETEs until all expired rows are removed, the time budget is
/// exceeded, or the discard flag is set.
///
/// `delete_sql` must be a DELETE statement with two parameters: `?1` = cutoff
/// timestamp, `?2` = batch size limit.
///
/// The time budget is checked **between** batches, not mid-query. Each
/// individual DELETE runs to completion. If a single batch exceeds the budget
/// (e.g. on NFS), the function returns after that batch.
///
/// `time_budget = None` means no limit — run until `Complete` or `Discarded`.
pub fn run_cleanup_batches(
    conn: &Connection,
    delete_sql: &str,
    cutoff_ms: i64,
    batch_size: i64,
    time_budget: Option<Duration>,
    discard_flag: &AtomicBool,
) -> Result<CleanupResult, rusqlite::Error> {
    let start = Instant::now();

    loop {
        if discard_flag.load(Ordering::Relaxed) {
            return Ok(CleanupResult::Discarded);
        }

        let deleted = conn.execute(delete_sql, rusqlite::params![cutoff_ms, batch_size])?;

        if deleted > 0 {
            tracing::debug!("cleanup: deleted {} rows (cutoff={})", deleted, cutoff_ms);
        }

        if (deleted as i64) < batch_size {
            return Ok(CleanupResult::Complete);
        }

        // Check time budget *after* the batch completes.
        if let Some(budget) = time_budget {
            if start.elapsed() >= budget {
                return Ok(CleanupResult::TimeBudgetExceeded);
            }
        }
    }
}

// ============================================================================
// Writer connection setup
// ============================================================================

/// Configure a writer SQLite connection with pragmas appropriate for the
/// storage mode. Shared between the log store and monitor writer actors.
pub fn setup_writer_connection(
    conn: &Connection,
    mode: StorageMode,
) -> Result<(), rusqlite::Error> {
    // busy_timeout is harmless in WAL mode and needed for NFS DELETE-journal
    // mode where readers hold shared locks during queries.
    conn.busy_timeout(Duration::from_secs(2))?;

    // Shared pragmas
    conn.execute_batch(
        "PRAGMA mmap_size = 0;
         PRAGMA cache_size = -8000;
         PRAGMA temp_store = MEMORY;
         PRAGMA page_size = 4096;
         PRAGMA journal_size_limit = 6291456;",
    )?;

    match mode {
        StorageMode::Local => {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA wal_autocheckpoint = 1000;",
            )?;
        }
        StorageMode::Nfs => {
            conn.execute_batch(
                "PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;",
            )?;
        }
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn create_test_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE test_rows (
                 id        INTEGER PRIMARY KEY,
                 timestamp INTEGER NOT NULL
             );
             CREATE INDEX idx_test_ts ON test_rows(timestamp);",
        )
        .unwrap();
    }

    fn insert_rows(conn: &Connection, count: usize, start_ts: i64) {
        for i in 0..count {
            conn.execute(
                "INSERT INTO test_rows (timestamp) VALUES (?1)",
                rusqlite::params![start_ts + i as i64],
            )
            .unwrap();
        }
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM test_rows", [], |r| r.get(0))
            .unwrap()
    }

    const DELETE_SQL: &str =
        "DELETE FROM test_rows WHERE id IN (SELECT id FROM test_rows WHERE timestamp < ?1 ORDER BY timestamp LIMIT ?2)";

    #[test]
    fn cleanup_completes_when_all_deleted() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_table(&conn);
        insert_rows(&conn, 10, 100);

        let flag = AtomicBool::new(false);
        let result = run_cleanup_batches(&conn, DELETE_SQL, 200, 100, None, &flag).unwrap();
        assert!(matches!(result, CleanupResult::Complete));
        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn cleanup_respects_time_budget() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_table(&conn);
        // Insert enough rows that multiple batches are needed
        insert_rows(&conn, 100, 100);

        let flag = AtomicBool::new(false);
        // Use a zero-duration budget — should stop after the first batch
        let result =
            run_cleanup_batches(&conn, DELETE_SQL, 1000, 10, Some(Duration::ZERO), &flag).unwrap();
        assert!(matches!(result, CleanupResult::TimeBudgetExceeded));
        // First batch of 10 should be deleted, 90 remain
        assert_eq!(row_count(&conn), 90);
    }

    #[test]
    fn cleanup_respects_discard_flag() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_table(&conn);
        insert_rows(&conn, 10, 100);

        let flag = AtomicBool::new(true); // pre-set
        let result = run_cleanup_batches(&conn, DELETE_SQL, 200, 5, None, &flag).unwrap();
        assert!(matches!(result, CleanupResult::Discarded));
        // Nothing deleted because flag was checked before first batch
        assert_eq!(row_count(&conn), 10);
    }

    #[test]
    fn cleanup_no_budget_runs_to_completion() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_table(&conn);
        insert_rows(&conn, 100, 100);

        let flag = AtomicBool::new(false);
        // Small batch size, no budget — should loop until all deleted
        let result = run_cleanup_batches(&conn, DELETE_SQL, 1000, 10, None, &flag).unwrap();
        assert!(matches!(result, CleanupResult::Complete));
        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn cleanup_cutoff_boundary_is_exclusive() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_table(&conn);
        // Rows at timestamps 100, 101, 102, 103, 104
        insert_rows(&conn, 5, 100);

        let flag = AtomicBool::new(false);
        // cutoff = 102: should delete rows with timestamp < 102 (i.e. 100, 101)
        let result = run_cleanup_batches(&conn, DELETE_SQL, 102, 100, None, &flag).unwrap();
        assert!(matches!(result, CleanupResult::Complete));
        assert_eq!(row_count(&conn), 3);

        let min_ts: i64 = conn
            .query_row("SELECT MIN(timestamp) FROM test_rows", [], |r| r.get(0))
            .unwrap();
        assert_eq!(min_ts, 102);
    }

    #[test]
    fn setup_writer_connection_local_mode() {
        let conn = Connection::open_in_memory().unwrap();
        setup_writer_connection(&conn, StorageMode::Local).unwrap();

        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        // In-memory databases use "memory" journal mode, but WAL is requested.
        // WAL is not supported for in-memory DBs, so it stays as "memory".
        // Just verify the call doesn't error.
        assert!(!journal.is_empty());

        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        // NORMAL = 1
        assert_eq!(sync, 1);
    }

    #[test]
    fn setup_writer_connection_nfs_mode() {
        let conn = Connection::open_in_memory().unwrap();
        setup_writer_connection(&conn, StorageMode::Nfs).unwrap();

        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        // FULL = 2
        assert_eq!(sync, 2);
    }
}
