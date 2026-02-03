//! Log management for Kepler daemon
//!
//! This module provides:
//! - `BufferedLogWriter` - Per-task buffered writer (no shared state)
//! - `LogReader` - Stateless reader for querying logs
//! - `MergedLogIterator` - Forward iteration (oldest first)
//! - `ReverseMergedLogIterator` - Reverse iteration (newest first)

mod config;
mod iterator;
mod reader;
mod reverse;
mod writer;

pub use config::LogWriterConfig;
pub use iterator::MergedLogIterator;
pub use reader::LogReader;
pub use reverse::{ReverseLineReader, ReverseMergedLogIterator};
pub use writer::BufferedLogWriter;

use chrono::{DateTime, Utc};
use kepler_protocol::protocol::LogEntry;
use std::sync::Arc;

/// Default maximum bytes to read when tailing logs (10MB)
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Default maximum lines to return
pub const DEFAULT_MAX_LINES: usize = 10_000;

/// Default max log file size before truncation (10MB)
pub const DEFAULT_MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Default buffer size (8KB)
pub const DEFAULT_BUFFER_SIZE: usize = 8 * 1024;

/// Validate that a path is not a symlink (security measure to prevent symlink attacks)
#[cfg(unix)]
pub(crate) fn validate_not_symlink(path: &std::path::Path) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Log path is a symlink: {:?}", path),
            ));
        }
    }
    Ok(())
}

/// A single log line with metadata
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Service name (Arc for cheap cloning when reading many lines from same service)
    pub service: Arc<str>,
    pub line: String,
    pub timestamp: DateTime<Utc>,
    pub stream: LogStream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl LogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogStream::Stdout => "stdout",
            LogStream::Stderr => "stderr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stdout" => Some(LogStream::Stdout),
            "stderr" => Some(LogStream::Stderr),
            _ => None,
        }
    }

    /// Get file extension for log files
    pub fn extension(&self) -> &'static str {
        match self {
            LogStream::Stdout => "stdout.log",
            LogStream::Stderr => "stderr.log",
        }
    }
}

impl std::fmt::Display for LogStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Conversions
// ============================================================================

/// Convert LogLine to protocol LogEntry
impl From<LogLine> for LogEntry {
    fn from(line: LogLine) -> Self {
        LogEntry {
            service: line.service.to_string(),
            line: line.line,
            timestamp: Some(line.timestamp.timestamp_millis()),
            stream: line.stream.as_str().to_string(),
        }
    }
}

impl From<&LogLine> for LogEntry {
    fn from(line: &LogLine) -> Self {
        LogEntry {
            service: line.service.to_string(),
            line: line.line.clone(),
            timestamp: Some(line.timestamp.timestamp_millis()),
            stream: line.stream.as_str().to_string(),
        }
    }
}
