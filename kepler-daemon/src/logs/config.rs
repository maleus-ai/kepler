//! Configuration for log writers

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Notify;

use super::DEFAULT_BUFFER_SIZE;

/// Configuration for creating BufferedLogWriter instances.
///
/// Uses truncation when max_log_size is specified: when the file exceeds max size,
/// it is truncated and writing starts fresh from the beginning.
/// If max_log_size is None, logs grow unbounded (no truncation).
#[derive(Debug, Clone)]
pub struct LogWriterConfig {
    pub logs_dir: PathBuf,
    /// Maximum log file size before truncation. None = unbounded (no truncation).
    pub max_log_size: Option<u64>,
    /// Buffer size in bytes. 0 = synchronous writes (no buffering).
    pub buffer_size: usize,
    /// Optional notifier to wake cursor handlers when log data is flushed.
    pub log_notify: Option<Arc<Notify>>,
}

impl LogWriterConfig {
    /// Create a new config with default settings (unbounded size, synchronous writes).
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            logs_dir,
            max_log_size: None, // Unbounded by default
            buffer_size: DEFAULT_BUFFER_SIZE,
            log_notify: None,
        }
    }

    /// Create with a specific max size (for truncation).
    pub fn with_max_size(logs_dir: PathBuf, max_log_size: u64) -> Self {
        Self {
            logs_dir,
            max_log_size: Some(max_log_size),
            buffer_size: DEFAULT_BUFFER_SIZE,
            log_notify: None,
        }
    }

    /// Create with specific max size and buffer size.
    pub fn with_options(
        logs_dir: PathBuf,
        max_log_size: Option<u64>,
        buffer_size: usize,
    ) -> Self {
        Self {
            logs_dir,
            max_log_size,
            buffer_size,
            log_notify: None,
        }
    }
}
