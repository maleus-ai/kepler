//! Dedicated writer thread for the monitor database.
//!
//! Mirrors the `LogStoreActor` pattern: a single-threaded actor owns the
//! SQLite write connection and processes commands from an mpsc channel.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, params};

use crate::config::StorageMode;
use crate::db_cleanup::{self, CleanupResult};

use super::ServiceMetrics;

// ============================================================================
// Commands
// ============================================================================

/// Commands processed by the monitor writer actor.
pub(crate) enum MonitorCommand {
    /// Store a batch of metric samples at the given timestamp.
    InsertMetrics {
        timestamp: i64,
        metrics: Vec<ServiceMetrics>,
    },
    /// Run startup retention cleanup, deleting all rows before `cutoff_ms`.
    /// Uses no time budget — runs until complete or discarded.
    CleanupBefore {
        cutoff_ms: i64,
    },
    /// Flush pending metrics (unless discard flag is set) and exit.
    Shutdown,
}

// ============================================================================
// Shutdown token
// ============================================================================

/// Pre-created token for triggering monitor discard without having the
/// `MonitorHandle` in hand.
///
/// Created at `ConfigActorHandle` construction time and passed to
/// `MonitorHandle::spawn()`, which fills in the `interrupt_handle` slot.
/// This allows `shutdown_discard_monitor()` to work even before the
/// monitor is spawned — the flag is set immediately, and when the monitor
/// later starts, it sees the flag and exits.
pub struct MonitorShutdownToken {
    pub(crate) discard_flag: AtomicBool,
    pub(crate) interrupt_handle: parking_lot::Mutex<Option<rusqlite::InterruptHandle>>,
}

impl MonitorShutdownToken {
    pub fn new() -> Self {
        Self {
            discard_flag: AtomicBool::new(false),
            interrupt_handle: parking_lot::Mutex::new(None),
        }
    }

    /// Set the discard flag and interrupt any running SQLite query.
    pub fn discard(&self) {
        self.discard_flag.store(true, Ordering::Relaxed);
        if let Some(ref handle) = *self.interrupt_handle.lock() {
            handle.interrupt();
        }
    }
}

// ============================================================================
// Handle
// ============================================================================

/// Cloneable handle to the monitor writer actor.
///
/// Provides `shutdown()` for graceful exit (flush pending metrics) and
/// `shutdown_discard()` for immediate exit (drop everything). Created by
/// [`MonitorHandle::spawn`], which starts both the writer thread and the
/// collector task.
#[derive(Clone)]
pub struct MonitorHandle {
    tx: std::sync::mpsc::Sender<MonitorCommand>,
    shutdown_token: Arc<MonitorShutdownToken>,
    collector_abort: Arc<tokio::task::AbortHandle>,
}

impl MonitorHandle {
    /// Spawn the monitor writer actor + collector task.
    ///
    /// The writer runs on a dedicated `std::thread`; the collector runs as an
    /// async `tokio::spawn` task that sends `InsertMetrics` through the channel.
    pub fn spawn(
        config: crate::config::MonitorConfig,
        shutdown_token: Arc<MonitorShutdownToken>,
        config_handle: crate::config_actor::ConfigActorHandle,
        state_dir: std::path::PathBuf,
        containment: crate::containment::ContainmentManager,
        storage_mode: StorageMode,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let actor_token = Arc::clone(&shutdown_token);

        let retention_period = config.retention_period;
        let cleanup_interval = config
            .cleanup_interval
            .unwrap_or(DEFAULT_CLEANUP_INTERVAL);

        // Wait for the actor thread to create the schema before proceeding.
        let (ready_tx, ready_rx) =
            std::sync::mpsc::sync_channel::<rusqlite::InterruptHandle>(1);

        let actor_state_dir = state_dir.clone();
        std::thread::Builder::new()
            .name("monitor-writer".into())
            .spawn(move || {
                let mut actor = match MonitorWriterActor::new(
                    &actor_state_dir,
                    rx,
                    storage_mode,
                    actor_token,
                    retention_period,
                    cleanup_interval,
                ) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(
                            "Failed to open monitor database {:?}: {}",
                            actor_state_dir,
                            e
                        );
                        drop(ready_tx);
                        return;
                    }
                };
                let _ = ready_tx.send(actor.conn.get_interrupt_handle());
                actor.run();
                drop(actor);
            })
            .expect("failed to spawn monitor-writer thread");

        // Block until schema is created (or thread panics/errors).
        if let Ok(ih) = ready_rx.recv() {
            *shutdown_token.interrupt_handle.lock() = Some(ih);
        }

        // Startup retention cleanup
        if let Some(retention) = config.retention_period {
            let cutoff =
                chrono::Utc::now().timestamp_millis() - retention.as_millis() as i64;
            let _ = tx.send(MonitorCommand::CleanupBefore { cutoff_ms: cutoff });
        }

        // Spawn collector task
        let collector_tx = tx.clone();
        let collector_task = super::collector::spawn_collector(
            config,
            config_handle,
            collector_tx,
            containment,
        );
        let collector_abort = Arc::new(collector_task.abort_handle());

        // If the discard flag was already set before we spawned (edge case:
        // shutdown_discard called before monitor started), send Shutdown.
        if shutdown_token.discard_flag.load(Ordering::Relaxed) {
            let _ = tx.send(MonitorCommand::Shutdown);
        }

        Self {
            tx,
            shutdown_token,
            collector_abort,
        }
    }

    /// Request graceful shutdown. Flushes pending metrics before exiting.
    /// The collector exits naturally when its tx.send() fails.
    pub fn shutdown(&self) {
        self.collector_abort.abort();
        let _ = self.tx.send(MonitorCommand::Shutdown);
    }

    /// Discard all pending metrics and exit immediately.
    ///
    /// Sets the atomic discard flag, interrupts any running SQLite query,
    /// aborts the collector task, and sends `Shutdown` to unblock the writer
    /// from `recv()`/`recv_timeout()`.
    pub fn shutdown_discard(&self) {
        self.shutdown_token.discard();
        self.collector_abort.abort();
        let _ = self.tx.send(MonitorCommand::Shutdown);
    }
}

const DEFAULT_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

const METRICS_DELETE_SQL: &str =
    "DELETE FROM metrics WHERE rowid IN (SELECT rowid FROM metrics WHERE timestamp < ?1 ORDER BY timestamp LIMIT ?2)";

// ============================================================================
// Actor
// ============================================================================

/// Single-threaded actor that owns the SQLite write connection for the
/// monitor database. Processes commands from the collector via an mpsc
/// channel, handles retention cleanup with time-budgeted batched deletes.
struct MonitorWriterActor {
    conn: Connection,
    rx: std::sync::mpsc::Receiver<MonitorCommand>,
    batch: Vec<(i64, Vec<ServiceMetrics>)>,
    shutdown_token: Arc<MonitorShutdownToken>,
    retention_period: Option<Duration>,
    cleanup_interval: Duration,
    last_cleanup: Instant,
    cleanup_has_more: bool,
}

impl MonitorWriterActor {
    fn new(
        state_dir: &Path,
        rx: std::sync::mpsc::Receiver<MonitorCommand>,
        storage_mode: StorageMode,
        shutdown_token: Arc<MonitorShutdownToken>,
        retention_period: Option<Duration>,
        cleanup_interval: Duration,
    ) -> Result<Self, rusqlite::Error> {
        let db_path = state_dir.join("monitor.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        db_cleanup::setup_writer_connection(&conn, storage_mode)?;
        create_schema(&conn)?;

        Ok(Self {
            conn,
            rx,
            batch: Vec::new(),
            shutdown_token,
            retention_period,
            cleanup_interval,
            last_cleanup: Instant::now(),
            cleanup_has_more: false,
        })
    }

    fn run(&mut self) {
        let mut last_flush = Instant::now();

        loop {
            if self.discard_flag() {
                self.discard();
                return;
            }

            let timeout = if !self.batch.is_empty() {
                let flush_remaining = FLUSH_INTERVAL.saturating_sub(last_flush.elapsed());
                if self.should_cleanup() {
                    Some(flush_remaining.min(Duration::from_millis(1)))
                } else if self.retention_period.is_some() {
                    Some(flush_remaining.min(self.cleanup_remaining()))
                } else {
                    Some(flush_remaining)
                }
            } else if self.should_cleanup() {
                Some(Duration::from_millis(1))
            } else if self.retention_period.is_some() {
                Some(self.cleanup_remaining())
            } else {
                None
            };

            let msg = match timeout {
                None => self
                    .rx
                    .recv()
                    .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                Some(t) => self.rx.recv_timeout(t),
            };

            if self.discard_flag() {
                self.discard();
                return;
            }

            match msg {
                Ok(MonitorCommand::InsertMetrics { timestamp, metrics }) => {
                    self.batch.push((timestamp, metrics));
                    // Flush immediately — monitor write rate is low (~1/5s)
                    // so batching provides no benefit, and immediate flush
                    // ensures metrics are visible to readers right away.
                    self.flush_batch();
                    last_flush = Instant::now();
                }
                Ok(MonitorCommand::CleanupBefore { cutoff_ms }) => {
                    self.flush_batch();
                    last_flush = Instant::now();
                    self.run_startup_cleanup(cutoff_ms);
                }
                Ok(MonitorCommand::Shutdown) => {
                    if !self.discard_flag() {
                        self.flush_batch();
                        let _ = self.conn.execute_batch("PRAGMA optimize");
                    }
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Flush or cleanup interval elapsed
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if self.discard_flag() {
                        self.discard();
                    } else {
                        self.flush_batch();
                    }
                    return;
                }
            }

            // Flush if batch is non-empty and interval elapsed
            if !self.batch.is_empty() && last_flush.elapsed() >= FLUSH_INTERVAL {
                self.flush_batch();
                last_flush = Instant::now();
            }

            // Periodic retention cleanup
            if self.should_cleanup() {
                self.run_cleanup_pass();
            }
        }
    }

    // ========================================================================
    // Retention cleanup
    // ========================================================================

    fn should_cleanup(&self) -> bool {
        self.retention_period.is_some()
            && (self.cleanup_has_more
                || self.last_cleanup.elapsed() >= self.cleanup_interval)
    }

    fn cleanup_remaining(&self) -> Duration {
        self.cleanup_interval
            .saturating_sub(self.last_cleanup.elapsed())
    }

    fn run_cleanup_pass(&mut self) {
        let Some(retention) = self.retention_period else {
            return;
        };
        let cutoff =
            chrono::Utc::now().timestamp_millis() - retention.as_millis() as i64;

        self.flush_batch();

        match db_cleanup::run_cleanup_batches(
            &self.conn,
            METRICS_DELETE_SQL,
            cutoff,
            db_cleanup::PERIODIC_CLEANUP_BATCH,
            Some(db_cleanup::DEFAULT_CLEANUP_TIME_BUDGET),
            &self.shutdown_token.discard_flag,
        ) {
            Ok(CleanupResult::Complete) => {
                self.cleanup_has_more = false;
            }
            Ok(CleanupResult::TimeBudgetExceeded) => {
                self.cleanup_has_more = true;
            }
            Ok(CleanupResult::Discarded) => {
                self.cleanup_has_more = false;
            }
            Err(e) => {
                if !self.discard_flag() {
                    tracing::warn!("monitor cleanup failed: {}", e);
                }
                self.cleanup_has_more = false;
            }
        }

        self.last_cleanup = Instant::now();
    }

    fn run_startup_cleanup(&mut self, cutoff_ms: i64) {
        // Startup cleanup uses no time budget — run until complete.
        // At monitor write rates (~1 msg/5s), channel backlog is negligible.
        match db_cleanup::run_cleanup_batches(
            &self.conn,
            METRICS_DELETE_SQL,
            cutoff_ms,
            db_cleanup::STARTUP_CLEANUP_BATCH,
            None, // no time budget
            &self.shutdown_token.discard_flag,
        ) {
            Ok(CleanupResult::Complete) => {}
            Ok(CleanupResult::TimeBudgetExceeded) => unreachable!(),
            Ok(CleanupResult::Discarded) => {}
            Err(e) => {
                if !self.discard_flag() {
                    tracing::warn!("monitor startup cleanup failed: {}", e);
                }
            }
        }
    }

    // ========================================================================
    // Write batching
    // ========================================================================

    fn flush_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }

        let result = (|| -> Result<(), rusqlite::Error> {
            let tx = self.conn.transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO metrics (timestamp, service, cpu_percent, memory_rss, memory_vss, pids)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;

                for (timestamp, metrics) in self.batch.drain(..) {
                    for m in metrics {
                        let pids_json =
                            serde_json::to_string(&m.pids).unwrap_or_else(|_| "[]".to_string());
                        stmt.execute(params![
                            timestamp,
                            m.service,
                            m.cpu_percent,
                            m.memory_rss,
                            m.memory_vss,
                            pids_json,
                        ])?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })();

        if let Err(e) = result {
            tracing::warn!("Failed to flush monitor batch: {}", e);
            self.batch.clear();
        }
    }

    fn discard(&mut self) {
        let batch_len = self.batch.len();
        self.batch.clear();
        let mut count = 0u64;
        while self.rx.try_recv().is_ok() {
            count += 1;
        }
        tracing::info!(
            "monitor-writer discard: dropped {} buffered + {} queued entries",
            batch_len,
            count
        );
    }

    fn discard_flag(&self) -> bool {
        self.shutdown_token.discard_flag.load(Ordering::Relaxed)
    }
}

// ============================================================================
// Schema
// ============================================================================

fn create_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics (
             timestamp INTEGER NOT NULL,
             service TEXT NOT NULL,
             cpu_percent REAL NOT NULL,
             memory_rss INTEGER NOT NULL,
             memory_vss INTEGER NOT NULL,
             pids TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics(timestamp);
         CREATE INDEX IF NOT EXISTS idx_metrics_svc_ts ON metrics(service, timestamp);",
    )?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    const BATCHED_DELETE_SQL: &str =
        "DELETE FROM metrics WHERE rowid IN (SELECT rowid FROM metrics WHERE timestamp < ?1 ORDER BY timestamp LIMIT ?2)";

    #[test]
    fn metrics_batched_delete_syntax_accepted() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE metrics (
                 timestamp INTEGER NOT NULL,
                 service TEXT NOT NULL,
                 cpu_percent REAL NOT NULL,
                 memory_rss INTEGER NOT NULL,
                 memory_vss INTEGER NOT NULL,
                 pids TEXT NOT NULL
             );
             CREATE INDEX idx_metrics_ts ON metrics(timestamp);",
        )
        .unwrap();

        conn.execute(BATCHED_DELETE_SQL, params![1000, 100]).unwrap();
    }

    #[test]
    fn metrics_batched_delete_caps_and_orders() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE metrics (
                 timestamp INTEGER NOT NULL,
                 service TEXT NOT NULL,
                 cpu_percent REAL NOT NULL,
                 memory_rss INTEGER NOT NULL,
                 memory_vss INTEGER NOT NULL,
                 pids TEXT NOT NULL
             );
             CREATE INDEX idx_metrics_ts ON metrics(timestamp);",
        )
        .unwrap();

        for i in 1..=10 {
            conn.execute(
                "INSERT INTO metrics (timestamp, service, cpu_percent, memory_rss, memory_vss, pids)
                 VALUES (?1, 'svc', 0.0, 0, 0, '[]')",
                params![i * 100],
            )
            .unwrap();
        }

        let deleted = conn.execute(BATCHED_DELETE_SQL, params![800, 4]).unwrap();
        assert_eq!(deleted, 4, "should delete exactly 4 rows");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 6);

        let min_ts: i64 = conn
            .query_row("SELECT MIN(timestamp) FROM metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(min_ts, 500);
    }

    #[test]
    fn schema_creation_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        super::create_schema(&conn).unwrap();
        // Second call should succeed (IF NOT EXISTS)
        super::create_schema(&conn).unwrap();

        // Verify table and indexes exist
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='metrics'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);

        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_metrics%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 2);
    }

    #[test]
    fn flush_batch_inserts_metrics() {
        use super::{MonitorWriterActor, ServiceMetrics, MonitorShutdownToken};
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};

        let conn = Connection::open_in_memory().unwrap();
        super::create_schema(&conn).unwrap();

        let (_tx, rx) = std::sync::mpsc::channel();
        let token = Arc::new(MonitorShutdownToken::new());

        let mut actor = MonitorWriterActor {
            conn,
            rx,
            batch: Vec::new(),
            shutdown_token: token,
            retention_period: None,
            cleanup_interval: Duration::from_secs(60),
            last_cleanup: Instant::now(),
            cleanup_has_more: false,
        };

        // Push metrics into the batch
        actor.batch.push((1000, vec![
            ServiceMetrics {
                service: "web".to_string(),
                cpu_percent: 25.5,
                memory_rss: 1024,
                memory_vss: 2048,
                pids: vec![100, 101],
            },
            ServiceMetrics {
                service: "db".to_string(),
                cpu_percent: 10.0,
                memory_rss: 512,
                memory_vss: 1024,
                pids: vec![200],
            },
        ]));

        actor.flush_batch();
        assert!(actor.batch.is_empty());

        // Verify rows were inserted
        let count: i64 = actor.conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Verify pids JSON serialization
        let pids_json: String = actor.conn
            .query_row(
                "SELECT pids FROM metrics WHERE service = 'web'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let pids: Vec<u32> = serde_json::from_str(&pids_json).unwrap();
        assert_eq!(pids, vec![100, 101]);
    }

    #[test]
    fn no_retention_means_no_cleanup() {
        use super::{MonitorWriterActor, MonitorShutdownToken};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let conn = Connection::open_in_memory().unwrap();
        super::create_schema(&conn).unwrap();

        let (_tx, rx) = std::sync::mpsc::channel();
        let token = Arc::new(MonitorShutdownToken::new());

        let actor = MonitorWriterActor {
            conn,
            rx,
            batch: Vec::new(),
            shutdown_token: token,
            retention_period: None,
            cleanup_interval: Duration::from_secs(60),
            last_cleanup: Instant::now() - Duration::from_secs(120), // well past interval
            cleanup_has_more: false,
        };

        // With retention_period = None, should_cleanup must return false
        // even if the cleanup interval has elapsed
        assert!(!actor.should_cleanup());
    }
}
