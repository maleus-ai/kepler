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

struct LogStoreActor {
    conn: Connection,
    rx: std::sync::mpsc::Receiver<LogCommand>,
    batch: Vec<InsertEntry>,
    flush_interval: Duration,
    batch_size: usize,
    log_notify: Option<Arc<Notify>>,
    pending_count: Arc<AtomicU64>,
    discard_flag: Arc<AtomicBool>,
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

        setup_writer_connection(&conn, storage_mode)?;
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

            // When the batch is empty there is nothing to flush on a timer,
            // so block indefinitely until the next command arrives.
            // This avoids a tight loop when recv_timeout(0) returns instantly.
            let msg = if self.batch.is_empty() {
                self.rx.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected)
            } else {
                let remaining = self.flush_interval.saturating_sub(last_flush.elapsed());
                self.rx.recv_timeout(remaining)
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
                                self.handle_non_write(cmd, &mut last_flush);
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
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Flush interval elapsed
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
                if let Err(e) = self.conn.execute(
                    "DELETE FROM logs WHERE timestamp < ?1",
                    params![cutoff_ms],
                ) {
                    tracing::warn!("Failed to cleanup old logs: {}", e);
                }
                // VACUUM to reclaim disk space
                if let Err(e) = self.conn.execute_batch("VACUUM") {
                    tracing::warn!("Failed to VACUUM: {}", e);
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

fn setup_writer_connection(conn: &Connection, mode: StorageMode) -> Result<(), rusqlite::Error> {
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
    conn.execute_batch(
        "PRAGMA query_only = ON;
         PRAGMA mmap_size = 0;",
    )?;
    Ok(conn)
}
