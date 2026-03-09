//! Log management for Kepler daemon
//!
//! SQLite-backed logging with single-writer architecture:
//! - `LogStoreHandle` - Cloneable handle to the writer actor (batched writes, flush interval)
//! - `LogWriter` - Per-service writer that sends entries through the store
//! - `SqliteLogReader` - Read-only reader (opens a new connection per query)

pub mod filter;
pub mod store;
mod log_reader;
mod log_writer;
mod json_parse;

pub use log_reader::LogReader as SqliteLogReader;
pub use log_writer::LogWriter;
pub use store::{DEFAULT_BATCH_SIZE, LogStoreHandle};

use std::sync::Arc;

/// A single log line with metadata
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Row ID (monotonically increasing, used as stream position)
    pub id: i64,
    /// Service name
    pub service: Arc<str>,
    /// Log line content (extracted message for JSON logs, raw line otherwise)
    pub line: String,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: i64,
    /// Log level string ("out", "err", "trace", "debug", "info", "warn", "error", "fatal")
    pub level: Arc<str>,
    /// Hook name (None for service process logs)
    pub hook: Option<Arc<str>>,
    /// Remaining JSON attributes (NULL for non-JSON lines)
    pub attributes: Option<String>,
}

/// Normalize a log level string to a canonical form.
///
/// Handles common aliases:
/// - "warning" → "warn"
/// - "informational" → "info"
/// - "critical", "emergency", "emerg", "alert" → "fatal"
///
/// For non-JSON lines, "out" (stdout) and "err" (stderr) are used as-is.
/// Unknown level strings are stored as-is (lowercase).
pub fn normalize_level(s: &str) -> &'static str {
    match s.to_ascii_lowercase().as_str() {
        "trace" => "trace",
        "debug" => "debug",
        "info" | "informational" => "info",
        "warn" | "warning" => "warn",
        "error" => "error",
        "fatal" | "critical" | "emergency" | "emerg" | "alert" => "fatal",
        "out" => "out",
        "err" => "err",
        _ => "info", // unknown → default to info
    }
}

/// Format a level string for CLI display (full uppercase).
pub fn format_level(level: &str) -> &'static str {
    match level {
        "trace" => "TRACE",
        "debug" => "DEBUG",
        "info" | "out" => "INFO",
        "warn" => "WARN",
        "error" | "err" => "ERROR",
        "fatal" => "FATAL",
        _ => "INFO",
    }
}

