//! Server-side cursor management for efficient log streaming.
//!
//! This module provides `CursorManager` which tracks file positions for
//! cursor-based log streaming. Each cursor tracks where the client left off
//! reading, allowing efficient incremental reads without re-scanning files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use dashmap::DashMap;
use tokio::sync::Notify;

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
    /// Map of file paths → inode numbers known to the iterator (to detect new/replaced files)
    known_files: HashMap<PathBuf, u64>,
    /// Connection that owns this cursor (for cleanup on disconnect)
    connection_id: u64,
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
    /// Per-config Notify: wakes waiting cursor handlers when log data is flushed
    log_notifiers: DashMap<PathBuf, Arc<Notify>>,
    /// Time-to-live for cursors (based on last poll time)
    ttl: Duration,
}

impl CursorManager {
    /// Create a new cursor manager with the specified TTL in seconds
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            cursors: DashMap::new(),
            log_notifiers: DashMap::new(),
            ttl: Duration::from_secs(ttl_seconds),
        }
    }

    /// Get or create the Notify for a given config path.
    /// Used by writers (via LogWriterConfig) and cursor handlers (for long polling).
    pub fn get_log_notify(&self, config_path: &Path) -> Arc<Notify> {
        self.log_notifiers
            .entry(config_path.to_path_buf())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
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
        connection_id: u64,
    ) -> String {
        let cursor_id = generate_cursor_id();

        let reader = LogReader::new(logs_dir.clone());
        let files = reader.collect_log_files(service.as_deref(), no_hooks);

        // Track known files with inodes for later new-file/replaced-file discovery
        let known_files: HashMap<PathBuf, u64> = files
            .iter()
            .filter_map(|(path, _, _)| {
                std::fs::metadata(path).ok().map(|m| {
                    #[cfg(unix)]
                    { (path.clone(), m.ino()) }
                    #[cfg(not(unix))]
                    { (path.clone(), 0u64) }
                })
            })
            .collect();

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
            connection_id,
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
            .filter(|(path, _, _)| {
                #[cfg(unix)]
                {
                    let current_ino = std::fs::metadata(path).ok().map(|m| m.ino());
                    match (cursor.known_files.get(path), current_ino) {
                        (None, _) => true,                         // brand new file
                        (Some(&known), Some(cur)) => known != cur, // replaced (different inode)
                        _ => false,
                    }
                }
                #[cfg(not(unix))]
                { !cursor.known_files.contains_key(path) }
            })
            .collect();

        if !new_files.is_empty() {
            for (path, _, _) in &new_files {
                if let Ok(meta) = std::fs::metadata(path) {
                    #[cfg(unix)]
                    cursor.known_files.insert(path.clone(), meta.ino());
                    #[cfg(not(unix))]
                    cursor.known_files.insert(path.clone(), 0);
                }
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

    /// Remove all cursors belonging to the given config path.
    /// Called when `stop --clean` deletes a config's log directory.
    pub fn invalidate_config_cursors(&self, config_path: &Path) {
        self.cursors.retain(|_id, arc| {
            let cursor = arc.lock().unwrap_or_else(|e| e.into_inner());
            cursor.config_path != config_path
        });
        self.log_notifiers.remove(config_path);
    }

    /// Remove all cursors belonging to the given connection.
    /// Called when a client disconnects.
    pub fn invalidate_connection_cursors(&self, connection_id: u64) {
        self.cursors.retain(|_id, arc| {
            let cursor = arc.lock().unwrap_or_else(|e| e.into_inner());
            cursor.connection_id != connection_id
        });
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
mod tests;
