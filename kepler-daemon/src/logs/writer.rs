//! Buffered log writer with optional truncation

use chrono::Utc;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Notify;

use super::{LogStream, LogWriterConfig};

#[cfg(unix)]
use super::validate_not_symlink;

/// Per-task buffered log writer with no shared state.
///
/// Each stdout/stderr task gets its own BufferedLogWriter instance,
/// eliminating all lock contention.
///
/// If max_log_size is specified, uses truncation: when the file exceeds max size,
/// it is truncated and writing starts fresh from the beginning.
/// If max_log_size is None, logs grow unbounded (no truncation).
pub struct BufferedLogWriter {
    log_file: PathBuf,
    file: Option<File>,
    buffer: Vec<u8>,
    buffer_size: usize,
    /// Maximum log file size before truncation. None = unbounded.
    max_log_size: Option<u64>,
    bytes_written: u64,
    format_buffer: String,
    service_name: String,
    stream: LogStream,
    /// Optional notifier to wake cursor handlers when log data is flushed.
    log_notify: Option<Arc<Notify>>,
}

impl std::fmt::Debug for BufferedLogWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferedLogWriter")
            .field("log_file", &self.log_file)
            .field("buffer_size", &self.buffer_size)
            .field("max_log_size", &self.max_log_size)
            .field("bytes_written", &self.bytes_written)
            .field("service_name", &self.service_name)
            .field("stream", &self.stream)
            .finish()
    }
}

impl BufferedLogWriter {
    /// Create a new BufferedLogWriter for a specific service and stream.
    ///
    /// The log file path is: `{logs_dir}/{service}.{stream}.log`
    ///
    /// If `max_log_size` is Some, the file is truncated when it exceeds this size.
    /// If `max_log_size` is None, logs grow unbounded (no truncation).
    pub fn new(
        logs_dir: &Path,
        service_name: &str,
        stream: LogStream,
        max_log_size: Option<u64>,
        buffer_size: usize,
    ) -> Self {
        // Sanitize service name for filename (replace special chars)
        let safe_name = service_name.replace(['/', '\\', ':', '[', ']'], "_");
        let log_file = logs_dir.join(format!("{}.{}", safe_name, stream.extension()));

        // Create logs directory if it doesn't exist with group-accessible permissions (0o770)
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o770)
                .create(logs_dir);
        }
        #[cfg(not(unix))]
        {
            let _ = fs::create_dir_all(logs_dir);
        }

        // Initialize bytes_written from existing file size
        let bytes_written = fs::metadata(&log_file)
            .map(|m| m.len())
            .unwrap_or(0);

        Self {
            log_file,
            file: None,
            buffer: Vec::with_capacity(buffer_size),
            buffer_size,
            max_log_size,
            bytes_written,
            format_buffer: String::with_capacity(256),
            service_name: service_name.to_string(),
            stream,
            log_notify: None,
        }
    }

    /// Create from a LogWriterConfig
    pub fn from_config(config: &LogWriterConfig, service_name: &str, stream: LogStream) -> Self {
        let mut writer = Self::new(
            &config.logs_dir,
            service_name,
            stream,
            config.max_log_size,
            config.buffer_size,
        );
        writer.log_notify = config.log_notify.clone();
        writer
    }

    /// Write a log line (adds timestamp automatically)
    pub fn write(&mut self, line: &str) {
        let timestamp = Utc::now().timestamp_millis();

        // Format: {timestamp_ms}\t{message}\n
        // No service name or stream in content - derived from filename
        self.format_buffer.clear();
        let _ = writeln!(self.format_buffer, "{}\t{}", timestamp, line);

        if self.buffer_size == 0 {
            // No buffering - write directly to disk
            self.write_format_buffer_to_disk();
            if let Some(ref notify) = self.log_notify {
                notify.notify_waiters();
            }
        } else {
            // Buffer the entry
            self.buffer.extend_from_slice(self.format_buffer.as_bytes());

            // Flush if buffer exceeds size
            if self.buffer.len() >= self.buffer_size {
                self.flush();
            }
        }
    }

    /// Flush buffer to disk
    pub fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let buffer = std::mem::take(&mut self.buffer);
        self.write_to_disk(&buffer);
        self.buffer = buffer;
        self.buffer.clear();
        if let Some(ref notify) = self.log_notify {
            notify.notify_waiters();
        }
    }

    /// Write the format buffer directly to disk without allocation.
    /// This avoids the borrow conflict of passing `self.format_buffer` to `write_to_disk`.
    fn write_format_buffer_to_disk(&mut self) {
        if let Some(max_size) = self.max_log_size
            && self.bytes_written >= max_size {
                self.truncate_log_file();
            }

        if self.file.is_none() {
            self.file = self.open_log_file();
        }

        if let Some(ref mut file) = self.file {
            let data = self.format_buffer.as_bytes();
            if let Err(e) = file.write_all(data) {
                tracing::warn!("Failed to write log entry: {}", e);
                self.file = None;
            } else {
                self.bytes_written += data.len() as u64;
            }
        }
    }

    /// Write data directly to disk
    fn write_to_disk(&mut self, data: &[u8]) {
        // Check if truncation is needed (only if max_log_size is specified)
        if let Some(max_size) = self.max_log_size
            && self.bytes_written >= max_size {
                self.truncate_log_file();
            }

        // Get or create the file handle
        if self.file.is_none() {
            self.file = self.open_log_file();
        }

        if let Some(ref mut file) = self.file {
            if let Err(e) = file.write_all(data) {
                tracing::warn!("Failed to write log entry: {}", e);
                self.file = None;
            } else {
                self.bytes_written += data.len() as u64;
            }
        }
    }

    /// Open the log file with appropriate flags
    fn open_log_file(&self) -> Option<File> {
        // Validate existing path is not a symlink
        #[cfg(unix)]
        if let Err(e) = validate_not_symlink(&self.log_file) {
            tracing::warn!("Refusing to open log file: {}", e);
            return File::open("/dev/null").ok();
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            match OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&self.log_file)
            {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!("Failed to open log file {:?}: {}", self.log_file, e);
                    File::open("/dev/null").ok()
                }
            }
        }
        #[cfg(not(unix))]
        {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_file)
            {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!("Failed to open log file {:?}: {}", self.log_file, e);
                    None
                }
            }
        }
    }

    /// Truncate the log file and start fresh.
    ///
    /// This is simpler than rotation and makes cursor-based streaming easier
    /// since there's only one file to track per service/stream.
    fn truncate_log_file(&mut self) {
        // Validate file is not a symlink
        #[cfg(unix)]
        if let Err(e) = validate_not_symlink(&self.log_file) {
            tracing::error!("Cannot truncate: {}", e);
            return;
        }

        tracing::debug!("Truncating log file {:?}", self.log_file);

        // Close current file handle
        self.file = None;

        // Truncate the file (O_NOFOLLOW prevents symlink race between validate and open)
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&self.log_file)
        };
        #[cfg(not(unix))]
        let result = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.log_file);

        if let Ok(file) = result
        {
            self.file = Some(file);
            self.bytes_written = 0;
        } else {
            tracing::warn!("Failed to truncate log file {:?}", self.log_file);
        }
    }

    /// Get the log file path
    pub fn log_file_path(&self) -> &Path {
        &self.log_file
    }

    /// Get service name
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Get stream type
    pub fn stream(&self) -> LogStream {
        self.stream
    }
}

impl Drop for BufferedLogWriter {
    fn drop(&mut self) {
        self.flush();
    }
}
