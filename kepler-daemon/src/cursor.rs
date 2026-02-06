//! Server-side cursor management for efficient log streaming.
//!
//! This module provides `CursorManager` which tracks file positions for
//! cursor-based log streaming. Each cursor tracks where the client left off
//! reading, allowing efficient incremental reads without re-scanning files.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::logs::{LogLine, LogReader};
use kepler_protocol::protocol::MAX_CURSOR_BATCH_SIZE;

/// Tracks the position within a single log file
#[derive(Debug, Clone)]
struct FilePosition {
    /// Current read offset in bytes
    offset: u64,
    /// File size at last read (used to detect truncation)
    size: u64,
}

impl FilePosition {
    /// Create a position at the end of a file
    fn at_end(file: &File) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            offset: metadata.len(),
            size: metadata.len(),
        })
    }

    /// Create a position at the start of a file
    fn at_start(file: &File) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            offset: 0,
            size: metadata.len(),
        })
    }
}

/// State for a single cursor
#[derive(Debug, Clone)]
pub struct CursorState {
    /// File path -> position info
    positions: HashMap<PathBuf, FilePosition>,
    /// Service filter (None = all services)
    service_filter: Option<String>,
    /// Whether to exclude hook log files
    no_hooks: bool,
    /// Last time this cursor was polled (for TTL)
    last_polled: Instant,
    /// Config this cursor belongs to (for validation)
    config_path: PathBuf,
    /// Logs directory for this cursor
    logs_dir: PathBuf,
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
/// Each cursor tracks file positions for efficient incremental reads.
pub struct CursorManager {
    /// Map of cursor ID -> cursor state
    cursors: DashMap<String, CursorState>,
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

        // Collect current log files
        let reader = LogReader::new(logs_dir.clone());
        let files = reader.collect_log_files(service.as_deref(), no_hooks);

        let mut positions = HashMap::new();
        for (path, _service, _stream) in files {
            if let Ok(file) = File::open(&path) {
                let pos = if from_start {
                    FilePosition::at_start(&file)
                } else {
                    FilePosition::at_end(&file)
                };
                if let Ok(pos) = pos {
                    positions.insert(path, pos);
                }
            }
        }

        let state = CursorState {
            positions,
            service_filter: service,
            no_hooks,
            last_polled: Instant::now(),
            config_path,
            logs_dir,
        };

        self.cursors.insert(cursor_id.clone(), state);
        cursor_id
    }

    /// Read entries using a cursor, updating positions.
    ///
    /// Returns (entries, has_more) where has_more indicates if there's more data to read.
    pub fn read_entries(
        &self,
        cursor_id: &str,
        config_path: &Path,
    ) -> Result<(Vec<LogLine>, bool), CursorError> {
        // Phase 1: Lock, validate, clone state, drop lock
        let (mut positions, service_filter, no_hooks, logs_dir) = {
            let mut cursor = self
                .cursors
                .get_mut(cursor_id)
                .ok_or_else(|| CursorError::CursorExpired(cursor_id.to_string()))?;

            // Validate config path
            if cursor.config_path != config_path {
                return Err(CursorError::CursorConfigMismatch {
                    cursor_config: cursor.config_path.clone(),
                    requested_config: config_path.to_path_buf(),
                });
            }

            // Update last polled time
            cursor.last_polled = Instant::now();

            // Clone what we need and drop the lock
            (
                cursor.positions.clone(),
                cursor.service_filter.clone(),
                cursor.no_hooks,
                cursor.logs_dir.clone(),
            )
        }; // RefMut dropped here

        // Phase 2: File I/O without any lock
        let reader = LogReader::new(logs_dir);
        let current_files = reader.collect_log_files(service_filter.as_deref(), no_hooks);

        // Add any new files we haven't seen before
        for (path, _service, _stream) in &current_files {
            if !positions.contains_key(path)
                && let Ok(file) = File::open(path) {
                    // New file - start from beginning
                    if let Ok(pos) = FilePosition::at_start(&file) {
                        positions.insert(path.clone(), pos);
                    }
                }
        }

        // Remove positions for files that no longer exist
        let current_paths: std::collections::HashSet<_> =
            current_files.iter().map(|(p, _, _)| p.clone()).collect();
        positions.retain(|path, _| current_paths.contains(path));

        // Read entries from each file
        let mut all_entries: Vec<(LogLine, usize)> = Vec::new();
        let mut source_idx = 0;

        for (path, service, stream) in &current_files {
            if let Some(position) = positions.get_mut(path) {
                let file = match File::open(path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };

                let metadata = match file.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                // Detect truncation: if current size < our offset, file was truncated
                if metadata.len() < position.offset {
                    position.offset = 0; // Reset to beginning
                }

                // Only read if there's new content
                if metadata.len() > position.offset {
                    let mut buf_reader = BufReader::new(file);
                    if buf_reader.seek(SeekFrom::Start(position.offset)).is_err() {
                        continue;
                    }

                    let service_arc: Arc<str> = Arc::from(service.as_str());
                    let mut line_buf = String::with_capacity(256);

                    loop {
                        match buf_reader.read_line(&mut line_buf) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("I/O error reading log file {:?}: {}", path, e);
                                break;
                            }
                        }
                        if let Some(log_line) =
                            LogReader::parse_log_line_arc(&line_buf, &service_arc, *stream)
                        {
                            all_entries.push((log_line, source_idx));
                        }
                        line_buf.clear();
                    }

                    // Update position
                    if let Ok(new_pos) = buf_reader.stream_position() {
                        position.offset = new_pos;
                    }
                    position.size = metadata.len();
                }
            }
            source_idx += 1;
        }

        // Sort entries chronologically
        all_entries.sort_by_key(|(entry, _)| entry.timestamp);

        // Apply batch limit
        let has_more = all_entries.len() > MAX_CURSOR_BATCH_SIZE;
        let entries: Vec<LogLine> = all_entries
            .into_iter()
            .take(MAX_CURSOR_BATCH_SIZE)
            .map(|(entry, _)| entry)
            .collect();

        // Phase 3: Lock again, write positions back
        // If cursor was removed between phases (by cleanup_stale), silently discard
        if let Some(mut cursor) = self.cursors.get_mut(cursor_id) {
            cursor.positions = positions;
            cursor.last_polled = Instant::now();
        }

        Ok((entries, has_more))
    }

    /// Remove stale cursors (those not polled within TTL).
    /// Called periodically by a background task.
    pub fn cleanup_stale(&self) {
        let now = Instant::now();
        self.cursors
            .retain(|_id, state| now.duration_since(state.last_polled) < self.ttl);
    }

}
