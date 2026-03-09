//! Thin per-service log writer that sends entries to the LogStore actor.

use chrono::Utc;

use super::json_parse::try_parse_json_log;
use super::store::{InsertEntry, LogCommand, LogStoreHandle};

/// Per-service log writer. Sends entries to the LogStore via channel.
///
/// Cheap to create and clone — just holds a channel sender and metadata.
/// No flush or Drop logic needed; the store actor handles batching.
pub struct LogWriter {
    handle: LogStoreHandle,
    service: String,
    hook: Option<String>,
    level: &'static str,
}

impl std::fmt::Debug for LogWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogWriter")
            .field("service", &self.service)
            .field("hook", &self.hook)
            .field("level", &self.level)
            .finish()
    }
}

impl LogWriter {
    /// Create a writer for a service's stdout or stderr.
    pub fn new(handle: &LogStoreHandle, service: &str, level: &'static str) -> Self {
        Self {
            handle: handle.clone(),
            service: service.to_string(),
            hook: None,
            level,
        }
    }

    /// Create a writer for a hook's output.
    pub fn with_hook(handle: &LogStoreHandle, service: &str, hook: &str, level: &'static str) -> Self {
        Self {
            handle: handle.clone(),
            service: service.to_string(),
            hook: Some(hook.to_string()),
            level,
        }
    }

    /// Write a log line. Timestamp is added automatically.
    /// If the line is valid JSON, message/level/timestamp are extracted and
    /// remaining fields are stored as attributes.
    pub fn write(&self, line: &str) {
        let now = Utc::now().timestamp_millis();
        self.write_inner(line, now);
    }

    /// Write a log line with an explicit timestamp (for testing).
    pub fn write_with_timestamp(&self, line: &str, timestamp: i64) {
        self.write_inner(line, timestamp);
    }

    fn write_inner(&self, line: &str, default_timestamp: i64) {
        if line.starts_with('{') {
            if let Some(parsed) = try_parse_json_log(line) {
                self.handle.send(LogCommand::Write(InsertEntry {
                    timestamp: parsed.timestamp.unwrap_or(default_timestamp),
                    service: self.service.clone(),
                    hook: self.hook.clone(),
                    level: parsed.level.unwrap_or(self.level),
                    line: parsed.message,
                    attributes: parsed.attributes,
                }));
                return;
            }
        }

        // Non-JSON or failed to parse — store as raw log
        self.handle.send(LogCommand::Write(InsertEntry {
            timestamp: default_timestamp,
            service: self.service.clone(),
            hook: self.hook.clone(),
            level: self.level,
            line: line.to_string(),
            attributes: None,
        }));
    }

    /// Get the service name this writer belongs to.
    pub fn service_name(&self) -> &str {
        &self.service
    }

    /// Get the level (e.g. "out" or "err").
    pub fn level(&self) -> &'static str {
        self.level
    }
}
