//! SQLite-backed log store actor
//!
//! Single writer thread owns the SQLite write connection. All services push
//! `InsertEntry` through an unbounded channel; the actor batches inserts and
//! flushes on a configurable interval.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, params};
use tokio::sync::Notify;

use crate::config::StorageMode;

/// Commands sent to the LogStore actor via channel.
pub(crate) enum LogCommand {
    Write(InsertEntry),
    Clear,
    ClearService { service: String },
    ClearServiceHooks { service: String },
    CleanupBefore { cutoff_ms: i64 },
    /// Flush pending batch and reply when done (for tests / graceful shutdown).
    FlushSync { reply: std::sync::mpsc::SyncSender<()> },
    /// Update the log notify (for long-polling wakeups).
    SetLogNotify { notify: Arc<Notify> },
    Shutdown,
}

/// A single log entry to be inserted.
pub(crate) struct InsertEntry {
    pub timestamp: i64,
    pub service: String,
    pub hook: Option<String>,
    pub level: &'static str,
    pub line: String,
    pub attributes: Option<String>,
}

/// Cloneable handle to the LogStore actor.
#[derive(Clone)]
pub struct LogStoreHandle {
    tx: std::sync::mpsc::Sender<LogCommand>,
    db_path: PathBuf,
    storage_mode: StorageMode,
    /// Number of log entries sent but not yet flushed to SQLite.
    pending_count: Arc<AtomicU64>,
    /// Side-channel flag: when set, actor discards everything and exits immediately.
    discard_flag: Arc<AtomicBool>,
    /// SQLite interrupt handle — abort any running query from another thread.
    interrupt_handle: Option<Arc<std::sync::Mutex<rusqlite::InterruptHandle>>>,
}

impl std::fmt::Debug for LogStoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogStoreHandle")
            .field("db_path", &self.db_path)
            .field("storage_mode", &self.storage_mode)
            .finish()
    }
}

impl LogStoreHandle {
    /// Spawn the writer actor on a dedicated `std::thread`.
    /// Returns a cloneable handle for sending commands.
    pub fn spawn(
        db_path: PathBuf,
        flush_interval: Duration,
        batch_size: usize,
        storage_mode: StorageMode,
        log_notify: Option<Arc<Notify>>,
        retention_period: Option<Duration>,
        cleanup_interval: Duration,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let pending_count = Arc::new(AtomicU64::new(0));
        let discard_flag = Arc::new(AtomicBool::new(false));

        // Wait for the actor thread to finish creating the database schema
        // before returning the handle. This prevents readers from seeing an
        // empty database (no `logs` table) if they query before schema creation.
        // The actor sends back the SQLite interrupt handle so we can abort
        // long-running queries from another thread during shutdown_discard().
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<rusqlite::InterruptHandle>(1);

        let actor_db_path = db_path.clone();
        let actor_pending = Arc::clone(&pending_count);
        let actor_discard = Arc::clone(&discard_flag);
        std::thread::Builder::new()
            .name("log-store".into())
            .spawn(move || {
                let mut actor = match LogStoreActor::new(
                    &actor_db_path,
                    rx,
                    flush_interval,
                    batch_size,
                    storage_mode,
                    log_notify,
                    actor_pending,
                    actor_discard,
                    retention_period,
                    cleanup_interval,
                ) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!("Failed to open log database {:?}: {}", actor_db_path, e);
                        drop(ready_tx); // unblock caller even on error
                        return;
                    }
                };
                let _ = ready_tx.send(actor.conn.get_interrupt_handle());
                actor.run();
                drop(actor);
            })
            .expect("failed to spawn log-store thread");

        // Block until schema is created (or thread panics/errors).
        // If the actor failed to start, ready_tx is dropped and we get None.
        let interrupt_handle = ready_rx.recv().ok()
            .map(|h| Arc::new(std::sync::Mutex::new(h)));

        Self { tx, db_path, storage_mode, pending_count, discard_flag, interrupt_handle }
    }

    /// Send a write command (non-blocking, drops entry on disconnect).
    pub(crate) fn send(&self, cmd: LogCommand) {
        if matches!(cmd, LogCommand::Write(_)) {
            self.pending_count.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.tx.send(cmd);
    }

    /// Returns `true` if there are log entries that have been sent but not yet
    /// flushed to SQLite (still in the channel or batch buffer).
    pub fn has_pending(&self) -> bool {
        self.pending_count.load(Ordering::Relaxed) > 0
    }

    /// Path to the SQLite database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Directory containing the database file (legacy logs_dir equivalent).
    pub fn logs_dir(&self) -> &Path {
        self.db_path.parent().unwrap_or(&self.db_path)
    }

    /// Storage mode (Local or Nfs).
    pub fn storage_mode(&self) -> StorageMode {
        self.storage_mode
    }

    /// Clear all logs.
    pub fn clear(&self) {
        self.send(LogCommand::Clear);
    }

    /// Clear logs for a specific service (including its hook logs).
    pub fn clear_service(&self, service: &str) {
        self.send(LogCommand::ClearService { service: service.to_string() });
    }

    /// Clear hook logs for a service (not the service's own logs).
    pub fn clear_service_hooks(&self, service: &str) {
        self.send(LogCommand::ClearServiceHooks { service: service.to_string() });
    }

    /// Delete logs older than `cutoff_ms` (milliseconds since epoch).
    pub fn cleanup_before(&self, cutoff_ms: i64) {
        self.send(LogCommand::CleanupBefore { cutoff_ms });
    }

    /// Block until the current batch is flushed to disk.
    pub fn wait_flush_sync(&self) {
        let (tx, rx) = std::sync::mpsc::sync_channel(0);
        if self.tx.send(LogCommand::FlushSync { reply: tx }).is_ok() {
            let _ = rx.recv();
        }
    }

    /// Set the notification channel for long-polling wakeups.
    /// The actor fires this notify after flushing each batch.
    pub fn set_log_notify(&self, notify: Arc<Notify>) {
        let _ = self.tx.send(LogCommand::SetLogNotify { notify });
    }

    /// Request graceful shutdown. Flushes pending batch before exiting.
    pub fn shutdown(&self) {
        let _ = self.tx.send(LogCommand::Shutdown);
    }

    /// Discard all pending writes and exit immediately.
    /// Used for stop --clean / prune where the state directory will be deleted.
    /// Sets an atomic flag that the actor checks on every loop iteration,
    /// bypassing the potentially millions of queued Write commands.
    /// Also interrupts any ongoing SQLite operation (DELETE, INSERT batch, VACUUM)
    /// so the actor doesn't have to wait for it to complete before checking the flag.
    pub fn shutdown_discard(&self) {
        self.discard_flag.store(true, Ordering::Relaxed);
        if let Some(ref handle) = self.interrupt_handle {
            if let Ok(h) = handle.lock() {
                h.interrupt();
            }
        }
    }
}

// ============================================================================
// Actor
// ============================================================================

pub const DEFAULT_BATCH_SIZE: usize = 4096;

const LOG_DELETE_SQL: &str =
    "DELETE FROM logs WHERE id IN (SELECT id FROM logs WHERE timestamp < ?1 ORDER BY timestamp LIMIT ?2)";

struct LogStoreActor {
    conn: Connection,
    rx: std::sync::mpsc::Receiver<LogCommand>,
    batch: Vec<InsertEntry>,
    flush_interval: Duration,
    batch_size: usize,
    log_notify: Option<Arc<Notify>>,
    pending_count: Arc<AtomicU64>,
    discard_flag: Arc<AtomicBool>,
    retention_period: Option<Duration>,
    cleanup_interval: Duration,
    last_cleanup: Instant,
    cleanup_has_more: bool,
}

impl LogStoreActor {
    fn new(
        db_path: &Path,
        rx: std::sync::mpsc::Receiver<LogCommand>,
        flush_interval: Duration,
        batch_size: usize,
        storage_mode: StorageMode,
        log_notify: Option<Arc<Notify>>,
        pending_count: Arc<AtomicU64>,
        discard_flag: Arc<AtomicBool>,
        retention_period: Option<Duration>,
        cleanup_interval: Duration,
    ) -> Result<Self, rusqlite::Error> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let _ = std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o770)
                    .create(parent);
            }
            #[cfg(not(unix))]
            {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        crate::db_cleanup::setup_writer_connection(&conn, storage_mode)?;
        create_schema(&conn)?;

        Ok(Self {
            conn,
            rx,
            batch: Vec::with_capacity(batch_size),
            flush_interval,
            batch_size,
            log_notify,
            pending_count,
            discard_flag,
            retention_period,
            cleanup_interval,
            last_cleanup: Instant::now(),
            cleanup_has_more: false,
        })
    }

    fn run(&mut self) {
        let mut last_flush = Instant::now();

        loop {
            // Check discard flag before any processing
            if self.discard_flag.load(Ordering::Relaxed) {
                self.discard();
                return;
            }

            let timeout = if !self.batch.is_empty() {
                let flush_remaining = self.flush_interval.saturating_sub(last_flush.elapsed());
                if self.should_cleanup() {
                    // Catch-up mode: 1ms min to avoid spin while still being responsive
                    Some(flush_remaining.min(Duration::from_millis(1)))
                } else if self.retention_period.is_some() {
                    Some(flush_remaining.min(self.cleanup_remaining()))
                } else {
                    Some(flush_remaining)
                }
            } else if self.should_cleanup() {
                // Catch-up with empty batch: 1ms to avoid spin
                Some(Duration::from_millis(1))
            } else if self.retention_period.is_some() {
                // Sleep until next cleanup check
                Some(self.cleanup_remaining())
            } else {
                // No retention, no pending writes: block indefinitely
                None
            };

            let msg = match timeout {
                None => self.rx.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                Some(t) => self.rx.recv_timeout(t),
            };

            // Re-check discard flag after waking from recv. This handles the
            // case where shutdown_discard() + shutdown() are called while the
            // actor was blocked on recv(): the Shutdown unblocks us, but we
            // should discard rather than process the message normally.
            if self.discard_flag.load(Ordering::Relaxed) {
                self.discard();
                return;
            }

            match msg {
                Ok(LogCommand::Write(entry)) => {
                    self.batch.push(entry);
                    // Drain any additional pending entries without blocking
                    while self.batch.len() < self.batch_size {
                        if self.discard_flag.load(Ordering::Relaxed) {
                            self.discard();
                            return;
                        }
                        match self.rx.try_recv() {
                            Ok(LogCommand::Write(e)) => self.batch.push(e),
                            Ok(cmd) => {
                                if self.handle_non_write(cmd, &mut last_flush) {
                                    return; // shutdown
                                }
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
                Ok(cmd) => {
                    if self.handle_non_write(cmd, &mut last_flush) {
                        return; // shutdown
                    }
                    // Fall through to flush+cleanup checks (no continue)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Flush or cleanup interval elapsed
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if self.discard_flag.load(Ordering::Relaxed) {
                        self.discard();
                    } else {
                        self.flush_batch();
                    }
                    return;
                }
            }

            // Flush if batch is non-empty and (batch full OR interval elapsed)
            if !self.batch.is_empty()
                && (self.batch.len() >= self.batch_size || last_flush.elapsed() >= self.flush_interval)
            {
                self.flush_batch();
                last_flush = Instant::now();
            }

            // Periodic retention cleanup
            if self.should_cleanup() {
                self.run_cleanup_pass();
            }
        }
    }

    /// Handle a non-Write command. Returns `true` if the actor should shut down.
    fn handle_non_write(&mut self, cmd: LogCommand, last_flush: &mut Instant) -> bool {
        match cmd {
            LogCommand::Write(_) => unreachable!(),
            LogCommand::Clear => {
                self.flush_batch();
                *last_flush = Instant::now();
                if let Err(e) = self.conn.execute("DELETE FROM logs", []) {
                    tracing::warn!("Failed to clear logs: {}", e);
                }
            }
            LogCommand::ClearService { service } => {
                self.flush_batch();
                *last_flush = Instant::now();
                if let Err(e) = self.conn.execute("DELETE FROM logs WHERE service = ?1", params![service]) {
                    tracing::warn!("Failed to clear service logs: {}", e);
                }
            }
            LogCommand::ClearServiceHooks { service } => {
                self.flush_batch();
                *last_flush = Instant::now();
                if let Err(e) = self.conn.execute(
                    "DELETE FROM logs WHERE service = ?1 AND hook IS NOT NULL",
                    params![service],
                ) {
                    tracing::warn!("Failed to clear service hook logs: {}", e);
                }
            }
            LogCommand::CleanupBefore { cutoff_ms } => {
                self.flush_batch();
                *last_flush = Instant::now();
                if self.run_batched_delete(cutoff_ms) {
                    return true; // Shutdown happened during batched delete
                }
            }
            LogCommand::FlushSync { reply } => {
                self.flush_batch();
                *last_flush = Instant::now();
                let _ = reply.send(());
            }
            LogCommand::SetLogNotify { notify } => {
                self.log_notify = Some(notify);
            }
            LogCommand::Shutdown => {
                self.flush_batch();
                // Run PRAGMA optimize before closing
                let _ = self.conn.execute_batch("PRAGMA optimize");
                return true;
            }
        }
        false
    }

    // ========================================================================
    // Retention cleanup
    // ========================================================================

    fn should_cleanup(&self) -> bool {
        self.retention_period.is_some()
            && (self.cleanup_has_more || self.last_cleanup.elapsed() >= self.cleanup_interval)
    }

    fn cleanup_remaining(&self) -> Duration {
        self.cleanup_interval.saturating_sub(self.last_cleanup.elapsed())
    }

    /// Delete expired rows within the time budget (periodic cleanup).
    fn run_cleanup_pass(&mut self) {
        use crate::db_cleanup::{self, CleanupResult};

        let Some(retention) = self.retention_period else { return };
        let cutoff = chrono::Utc::now().timestamp_millis() - retention.as_millis() as i64;

        self.flush_batch();

        match db_cleanup::run_cleanup_batches(
            &self.conn,
            LOG_DELETE_SQL,
            cutoff,
            db_cleanup::PERIODIC_CLEANUP_BATCH,
            Some(db_cleanup::DEFAULT_CLEANUP_TIME_BUDGET),
            &self.discard_flag,
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
                if !self.discard_flag.load(Ordering::Relaxed) {
                    tracing::warn!("log cleanup failed: {}", e);
                }
                self.cleanup_has_more = false;
            }
        }

        self.last_cleanup = Instant::now();
    }

    /// Batched delete loop for startup catch-up. Uses larger batches and
    /// drains pending writes between time-budget windows to avoid channel
    /// backpressure. Returns `true` if a Shutdown command was processed.
    fn run_batched_delete(&mut self, cutoff_ms: i64) -> bool {
        use crate::db_cleanup::{self, CleanupResult};

        loop {
            match db_cleanup::run_cleanup_batches(
                &self.conn,
                LOG_DELETE_SQL,
                cutoff_ms,
                db_cleanup::STARTUP_CLEANUP_BATCH,
                Some(db_cleanup::DEFAULT_CLEANUP_TIME_BUDGET),
                &self.discard_flag,
            ) {
                Ok(CleanupResult::Complete) => return false,
                Ok(CleanupResult::Discarded) => return false,
                Ok(CleanupResult::TimeBudgetExceeded) => {
                    // Drain and flush pending writes between budget windows
                    if self.drain_and_flush_writes() {
                        return true; // Shutdown requested
                    }
                }
                Err(e) => {
                    if !self.discard_flag.load(Ordering::Relaxed) {
                        tracing::warn!("log startup cleanup failed: {}", e);
                    }
                    return false;
                }
            }
        }
    }

    /// Drain pending Write commands from the channel and flush them.
    /// Handles non-Write commands inline. Returns `true` if shutdown was requested.
    fn drain_and_flush_writes(&mut self) -> bool {
        let mut last_flush = Instant::now();
        loop {
            match self.rx.try_recv() {
                Ok(LogCommand::Write(entry)) => {
                    self.batch.push(entry);
                    if self.batch.len() >= self.batch_size {
                        self.flush_batch();
                        last_flush = Instant::now();
                    }
                }
                Ok(cmd) => {
                    if self.handle_non_write(cmd, &mut last_flush) {
                        return true; // Shutdown
                    }
                    break;
                }
                Err(_) => break, // Channel empty
            }
        }
        if !self.batch.is_empty() {
            self.flush_batch();
        }
        false
    }

    /// Discard all pending writes and drain the channel without processing.
    fn discard(&mut self) {
        let batch_len = self.batch.len();
        self.batch.clear();
        // Drain remaining channel entries to free memory
        let mut count = 0u64;
        while self.rx.try_recv().is_ok() {
            count += 1;
        }
        tracing::info!("log-store discard: dropped {} buffered + {} queued entries", batch_len, count);
        self.pending_count.store(0, Ordering::Relaxed);
    }

    fn flush_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }

        let count = self.batch.len() as u64;

        let result = (|| -> Result<(), rusqlite::Error> {
            let tx = self.conn.transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO logs (timestamp, service, hook, level, line, attributes) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;

                for entry in self.batch.drain(..) {
                    stmt.execute(params![
                        entry.timestamp,
                        entry.service,
                        entry.hook,
                        entry.level,
                        entry.line,
                        entry.attributes,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })();

        if let Err(e) = result {
            tracing::warn!("Failed to flush log batch: {}", e);
            self.batch.clear();
        }

        self.pending_count.fetch_sub(count, Ordering::Relaxed);

        if let Some(ref log_notify) = self.log_notify {
            log_notify.notify_waiters();
        }
    }
}

// ============================================================================
// SQLite setup
// ============================================================================

// setup_writer_connection is shared — see crate::db_cleanup::setup_writer_connection

fn create_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS logs (
            id         INTEGER PRIMARY KEY,
            timestamp  INTEGER NOT NULL,
            service    TEXT    NOT NULL,
            hook       TEXT,
            level      TEXT    NOT NULL,
            line       TEXT    NOT NULL,
            attributes TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp);
        CREATE INDEX IF NOT EXISTS idx_logs_service_id ON logs(service, id);
        CREATE INDEX IF NOT EXISTS idx_logs_service_hook ON logs(service, hook, id);",
    )?;
    Ok(())
}

/// Open a read-only connection to the log database.
pub fn open_readonly(db_path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // busy_timeout is harmless in WAL mode and critical for NFS DELETE-journal
    // mode where readers can get SQLITE_BUSY during writer commits.
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.execute_batch(
        "PRAGMA query_only = ON;
         PRAGMA mmap_size = 0;",
    )?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    const BATCHED_DELETE_SQL: &str =
        "DELETE FROM logs WHERE id IN (SELECT id FROM logs WHERE timestamp < ?1 ORDER BY timestamp LIMIT ?2)";

    /// Verify that the batched DELETE subquery is accepted by bundled SQLite.
    #[test]
    fn batched_delete_syntax_accepted() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id        INTEGER PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                line      TEXT NOT NULL
            )",
        )
        .unwrap();

        conn.execute(BATCHED_DELETE_SQL, params![1000, 100]).unwrap();
    }

    /// Batched DELETE deletes at most N rows and leaves the rest intact.
    #[test]
    fn batched_delete_caps_deleted_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id        INTEGER PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                line      TEXT NOT NULL
            );
            CREATE INDEX idx_logs_ts ON logs(timestamp);",
        )
        .unwrap();

        for i in 1..=20 {
            conn.execute(
                "INSERT INTO logs (timestamp, line) VALUES (?1, ?2)",
                params![i, format!("line {}", i)],
            )
            .unwrap();
        }

        let deleted = conn.execute(BATCHED_DELETE_SQL, params![15, 5]).unwrap();
        assert_eq!(deleted, 5, "should delete exactly 5 rows");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 15);
    }

    /// ORDER BY in the subquery ensures the oldest rows are deleted first.
    #[test]
    fn batched_delete_removes_oldest_first() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id        INTEGER PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                line      TEXT NOT NULL
            );
            CREATE INDEX idx_logs_ts ON logs(timestamp);",
        )
        .unwrap();

        for ts in [100, 200, 300, 400, 500] {
            conn.execute(
                "INSERT INTO logs (timestamp, line) VALUES (?1, ?2)",
                params![ts, format!("ts={}", ts)],
            )
            .unwrap();
        }

        let deleted = conn.execute(BATCHED_DELETE_SQL, params![600, 3]).unwrap();
        assert_eq!(deleted, 3);

        let mut stmt = conn
            .prepare("SELECT timestamp FROM logs ORDER BY timestamp")
            .unwrap();
        let timestamps: Vec<i64> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(timestamps, vec![400, 500]);
    }

    /// When fewer rows match than the limit, all matching rows are deleted.
    #[test]
    fn batched_delete_fewer_rows_than_limit() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id        INTEGER PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                line      TEXT NOT NULL
            )",
        )
        .unwrap();

        for i in 1..=3 {
            conn.execute(
                "INSERT INTO logs (timestamp, line) VALUES (?1, ?2)",
                params![i, format!("line {}", i)],
            )
            .unwrap();
        }

        let deleted = conn.execute(BATCHED_DELETE_SQL, params![100, 1000]).unwrap();
        assert_eq!(deleted, 3);

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    /// LIMIT 0 deletes nothing.
    #[test]
    fn batched_delete_limit_zero() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id        INTEGER PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                line      TEXT NOT NULL
            )",
        )
        .unwrap();

        conn.execute("INSERT INTO logs (timestamp, line) VALUES (1, 'hello')", []).unwrap();

        let deleted = conn.execute(BATCHED_DELETE_SQL, params![100, 0]).unwrap();
        assert_eq!(deleted, 0);

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
    }
}
