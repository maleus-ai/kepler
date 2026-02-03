use chrono::{DateTime, TimeZone, Utc};
use kepler_protocol::protocol::LogEntry;
use std::collections::BinaryHeap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Validate that a path is not a symlink (security measure to prevent symlink attacks)
#[cfg(unix)]
fn validate_not_symlink(path: &std::path::Path) -> std::io::Result<()> {
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

/// Default maximum bytes to read when tailing logs (10MB)
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Default maximum lines to return
pub const DEFAULT_MAX_LINES: usize = 10_000;

/// Default max log file size for rotation (10MB)
pub const DEFAULT_MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Default number of rotated files to keep
pub const DEFAULT_MAX_ROTATED_FILES: u32 = 5;

/// Default buffer size (64KB)
pub const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

/// A single log line with metadata
#[derive(Debug, Clone)]
pub struct LogLine {
    pub service: String,
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

/// Configuration for creating BufferedLogWriter instances
#[derive(Debug, Clone)]
pub struct LogWriterConfig {
    pub logs_dir: PathBuf,
    pub max_log_size: u64,
    pub max_rotated_files: u32,
    pub buffer_size: usize,
}

impl LogWriterConfig {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            logs_dir,
            max_log_size: DEFAULT_MAX_LOG_SIZE,
            max_rotated_files: DEFAULT_MAX_ROTATED_FILES,
            buffer_size: DEFAULT_BUFFER_SIZE,
        }
    }

    pub fn with_rotation(logs_dir: PathBuf, max_log_size: u64, max_rotated_files: u32) -> Self {
        Self {
            logs_dir,
            max_log_size,
            max_rotated_files,
            buffer_size: DEFAULT_BUFFER_SIZE,
        }
    }

    pub fn with_options(
        logs_dir: PathBuf,
        max_log_size: u64,
        max_rotated_files: u32,
        buffer_size: usize,
    ) -> Self {
        Self {
            logs_dir,
            max_log_size,
            max_rotated_files,
            buffer_size,
        }
    }
}

/// Per-task buffered log writer with no shared state.
///
/// Each stdout/stderr task gets its own BufferedLogWriter instance,
/// eliminating all lock contention.
pub struct BufferedLogWriter {
    log_file: PathBuf,
    file: Option<File>,
    buffer: Vec<u8>,
    buffer_size: usize,
    max_log_size: u64,
    max_rotated_files: u32,
    bytes_written: u64,
    rotation_index: u32,
    format_buffer: String,
    service_name: String,
    stream: LogStream,
}

impl std::fmt::Debug for BufferedLogWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferedLogWriter")
            .field("log_file", &self.log_file)
            .field("buffer_size", &self.buffer_size)
            .field("max_log_size", &self.max_log_size)
            .field("max_rotated_files", &self.max_rotated_files)
            .field("bytes_written", &self.bytes_written)
            .field("rotation_index", &self.rotation_index)
            .field("service_name", &self.service_name)
            .field("stream", &self.stream)
            .finish()
    }
}

impl BufferedLogWriter {
    /// Create a new BufferedLogWriter for a specific service and stream.
    ///
    /// The log file path is: `{logs_dir}/{service}.{stream}.log`
    pub fn new(
        logs_dir: &Path,
        service_name: &str,
        stream: LogStream,
        max_log_size: u64,
        max_rotated_files: u32,
        buffer_size: usize,
    ) -> Self {
        // Sanitize service name for filename (replace special chars)
        let safe_name = service_name.replace(['/', '\\', ':', '[', ']'], "_");
        let log_file = logs_dir.join(format!("{}.{}", safe_name, stream.extension()));

        // Create logs directory if it doesn't exist with secure permissions (0o700)
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
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
            max_rotated_files,
            bytes_written,
            rotation_index: 0,
            format_buffer: String::with_capacity(256),
            service_name: service_name.to_string(),
            stream,
        }
    }

    /// Create from a LogWriterConfig
    pub fn from_config(config: &LogWriterConfig, service_name: &str, stream: LogStream) -> Self {
        Self::new(
            &config.logs_dir,
            service_name,
            stream,
            config.max_log_size,
            config.max_rotated_files,
            config.buffer_size,
        )
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
            // Copy bytes to avoid borrow conflict
            let bytes = self.format_buffer.as_bytes().to_vec();
            self.write_to_disk(&bytes);
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
    }

    /// Write data directly to disk
    fn write_to_disk(&mut self, data: &[u8]) {
        // Check if rotation is needed
        if self.bytes_written >= self.max_log_size {
            self.file = None;
            self.rotate_log_file();
            self.bytes_written = 0;
        }

        // Get or create the file handle
        if self.file.is_none() {
            self.file = Some(self.open_log_file());
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
    fn open_log_file(&self) -> File {
        // Validate existing path is not a symlink
        #[cfg(unix)]
        if let Err(e) = validate_not_symlink(&self.log_file) {
            tracing::warn!("Refusing to open log file: {}", e);
            return File::open("/dev/null").expect("Failed to open /dev/null");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&self.log_file)
                .unwrap_or_else(|e| {
                    tracing::warn!("Failed to open log file {:?}: {}", self.log_file, e);
                    File::open("/dev/null").expect("Failed to open /dev/null")
                })
        }
        #[cfg(not(unix))]
        {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_file)
                .expect("Failed to open log file")
        }
    }

    /// Rotate the log file using cycling index (single rename, no cascade)
    fn rotate_log_file(&mut self) {
        // Validate source before rotation
        #[cfg(unix)]
        if let Err(e) = validate_not_symlink(&self.log_file) {
            tracing::error!("Cannot rotate: {}", e);
            return;
        }

        // Get next rotation index (1 to max_rotated_files, cycling)
        self.rotation_index = (self.rotation_index % self.max_rotated_files) + 1;

        let rotated = format!("{}.{}", self.log_file.display(), self.rotation_index);
        tracing::debug!("Rotating log file {:?} -> {}", self.log_file, rotated);

        // Single rename (overwrites if target exists)
        if let Err(e) = fs::rename(&self.log_file, &rotated) {
            tracing::warn!("Failed to rotate log file {:?}: {}", self.log_file, e);
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

// ============================================================================
// LogReader - Stateless reader for querying logs from disk
// ============================================================================

/// Stateless log reader for querying logs from disk.
///
/// Created on-demand for log queries. Uses merge-sort strategy to combine
/// multiple log files chronologically.
pub struct LogReader {
    logs_dir: PathBuf,
    max_rotated_files: u32,
}

impl LogReader {
    pub fn new(logs_dir: PathBuf, max_rotated_files: u32) -> Self {
        Self {
            logs_dir,
            max_rotated_files,
        }
    }

    /// Get the last N entries, optionally filtered by service
    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        self.tail_bounded(count, service, None)
    }

    /// Get the last N entries with bounded reading to prevent OOM
    pub fn tail_bounded(
        &self,
        count: usize,
        service: Option<&str>,
        max_bytes: Option<usize>,
    ) -> Vec<LogLine> {
        // Apply sensible defaults to prevent OOM
        let count = count.min(DEFAULT_MAX_LINES);
        let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // Collect all relevant log files
        let files = self.collect_log_files(service);

        if files.is_empty() {
            return Vec::new();
        }

        // Calculate per-file byte limit
        let per_file_max_bytes = if files.is_empty() {
            max_bytes
        } else {
            (max_bytes / files.len()).max(1024 * 1024)
        };

        // Read entries from all files
        let mut entries = Vec::with_capacity(count);
        for (path, svc, stream) in files {
            let file_entries = self.read_log_file_bounded(&path, &svc, stream, Some(per_file_max_bytes), None);
            entries.extend(file_entries);
        }

        // Sort by timestamp
        entries.sort_by_key(|e| e.timestamp);

        // Return last N
        let start = entries.len().saturating_sub(count);
        entries.into_iter().skip(start).collect()
    }

    /// Get logs with true pagination
    pub fn get_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        let files = self.collect_log_files(service);

        if files.is_empty() {
            return (Vec::new(), 0);
        }

        // Read all entries and count
        let mut all_entries: Vec<LogLine> = Vec::new();
        for (path, svc, stream) in files {
            let entries = self.read_log_file(&path, &svc, stream);
            all_entries.extend(entries);
        }

        // Sort by timestamp
        all_entries.sort_by_key(|e| e.timestamp);

        let total = all_entries.len();

        if offset >= total {
            return (Vec::new(), total);
        }

        // Apply offset and limit
        let entries: Vec<LogLine> = all_entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();

        (entries, total)
    }

    /// Collect all log files, optionally filtered by service
    /// Returns (path, service_name, stream)
    fn collect_log_files(&self, service: Option<&str>) -> Vec<(PathBuf, String, LogStream)> {
        let mut files = Vec::new();

        if let Some(svc) = service {
            // Collect files for specific service
            self.collect_service_files(&mut files, svc);
        } else {
            // Collect files for all services
            if let Ok(entries) = fs::read_dir(&self.logs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some((svc, _stream)) = self.parse_log_filename(&path) {
                        // Only collect main files, we'll enumerate rotated files separately
                        if path.extension().is_some_and(|ext| ext == "log") {
                            self.collect_service_files(&mut files, &svc);
                        }
                    }
                }
            }
        }

        // Remove duplicates (same file collected multiple times)
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files.dedup_by(|a, b| a.0 == b.0);

        files
    }

    /// Collect all log files for a specific service (both streams, including rotated)
    fn collect_service_files(&self, files: &mut Vec<(PathBuf, String, LogStream)>, service: &str) {
        let safe_name = service.replace(['/', '\\', ':', '[', ']'], "_");

        for stream in [LogStream::Stdout, LogStream::Stderr] {
            let main_file = self.logs_dir.join(format!("{}.{}", safe_name, stream.extension()));

            // Collect rotated files first (oldest to newest based on first entry timestamp)
            let mut rotated: Vec<(PathBuf, i64)> = Vec::new();
            for i in 1..=self.max_rotated_files {
                let rotated_path = PathBuf::from(format!("{}.{}", main_file.display(), i));
                if rotated_path.exists() {
                    let ts = Self::first_entry_timestamp(&rotated_path);
                    rotated.push((rotated_path, ts));
                }
            }

            // Sort rotated files by first entry timestamp
            rotated.sort_by_key(|(_, ts)| *ts);

            for (path, _) in rotated {
                files.push((path, service.to_string(), stream));
            }

            // Add main file if exists
            if main_file.exists() {
                files.push((main_file, service.to_string(), stream));
            }
        }
    }

    /// Parse log filename to extract service name and stream
    /// Returns (service_name, stream) if valid
    fn parse_log_filename(&self, path: &Path) -> Option<(String, LogStream)> {
        let filename = path.file_name()?.to_str()?;

        // Check for rotated file pattern: service.stream.log.N
        let filename = if let Some(base) = filename.strip_suffix(|c: char| c.is_ascii_digit()) {
            base.strip_suffix('.')?
        } else {
            filename
        };

        // Parse: service.stdout.log or service.stderr.log
        if let Some(service) = filename.strip_suffix(".stdout.log") {
            Some((service.to_string(), LogStream::Stdout))
        } else if let Some(service) = filename.strip_suffix(".stderr.log") {
            Some((service.to_string(), LogStream::Stderr))
        } else {
            None
        }
    }

    /// Read the timestamp from the first log entry in a file
    fn first_entry_timestamp(path: &PathBuf) -> i64 {
        if let Ok(file) = File::open(path) {
            let mut reader = BufReader::new(file);
            let mut first_line = String::new();
            if reader.read_line(&mut first_line).is_ok() {
                // New format: "TIMESTAMP\tMESSAGE"
                if let Some(ts_str) = first_line.split('\t').next() {
                    if let Ok(ts) = ts_str.parse::<i64>() {
                        return ts;
                    }
                }
            }
        }
        0
    }

    /// Read all entries from a log file
    fn read_log_file(&self, path: &PathBuf, service: &str, stream: LogStream) -> Vec<LogLine> {
        self.read_log_file_bounded(path, service, stream, None, None)
    }

    /// Read log entries from a file with optional bounds
    fn read_log_file_bounded(
        &self,
        path: &PathBuf,
        service: &str,
        stream: LogStream,
        max_bytes: Option<usize>,
        max_lines: Option<usize>,
    ) -> Vec<LogLine> {
        let mut entries = Vec::with_capacity(256);
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return entries,
        };

        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return entries,
        };

        let file_size = metadata.len() as usize;
        let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // If file is larger than max_bytes, read only the last max_bytes
        if file_size > max_bytes {
            use std::io::{Seek, SeekFrom};
            let mut file = file;
            let start_pos = file_size - max_bytes;

            if file.seek(SeekFrom::Start(start_pos as u64)).is_err() {
                return entries;
            }

            let mut reader = BufReader::new(file);

            // Skip the first partial line
            let mut partial_line = String::new();
            if reader.read_line(&mut partial_line).is_err() {
                return entries;
            }

            // Read remaining lines
            for line in reader.lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line, service, stream) {
                    entries.push(entry);
                }
            }
        } else {
            // File is small enough, read everything
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line, service, stream) {
                    entries.push(entry);
                }
            }
        }

        // Apply max_lines limit if specified
        if let Some(max) = max_lines {
            let len = entries.len();
            if len > max {
                entries = entries.into_iter().skip(len - max).collect();
            }
        }

        entries
    }

    /// Parse a log line from the new file format
    /// New format: "TIMESTAMP\tMESSAGE"
    fn parse_log_line(line: &str, service: &str, stream: LogStream) -> Option<LogLine> {
        let mut parts = line.splitn(2, '\t');

        let timestamp_str = parts.next()?;
        let content = parts.next().unwrap_or("");

        let timestamp = timestamp_str.parse::<i64>().ok()?;

        Some(LogLine {
            service: service.to_string(),
            line: content.to_string(),
            timestamp: Utc.timestamp_millis_opt(timestamp).single()?,
            stream,
        })
    }

    /// Clear all log files
    pub fn clear(&self) {
        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                // Delete .stdout.log and .stderr.log files and their rotated versions
                if filename.contains(".stdout.log") || filename.contains(".stderr.log") {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    /// Clear logs for a specific service
    pub fn clear_service(&self, service: &str) {
        let safe_name = service.replace(['/', '\\', ':', '[', ']'], "_");

        for stream in [LogStream::Stdout, LogStream::Stderr] {
            let main_file = self.logs_dir.join(format!("{}.{}", safe_name, stream.extension()));

            // Remove rotated files
            for i in 1..=self.max_rotated_files {
                let rotated = PathBuf::from(format!("{}.{}", main_file.display(), i));
                let _ = fs::remove_file(&rotated);
            }

            // Remove main file
            let _ = fs::remove_file(&main_file);
        }
    }

    /// Clear logs for services matching a prefix
    pub fn clear_service_prefix(&self, prefix: &str) {
        let safe_prefix = prefix.replace(['/', '\\', ':', '[', ']'], "_");

        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                if filename.starts_with(&safe_prefix) {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    /// Get logs directory
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }
}

// ============================================================================
// Merged Log Iterator for efficient chronological reading
// ============================================================================

/// Entry in the priority queue for merge-sort iteration
#[derive(Debug)]
struct HeapEntry {
    timestamp: i64,
    log_line: LogLine,
    source_idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering for min-heap (smallest timestamp first)
        other.timestamp.cmp(&self.timestamp)
    }
}

/// Iterator that merges multiple log sources chronologically
pub struct MergedLogIterator {
    readers: Vec<BufReader<File>>,
    services: Vec<String>,
    streams: Vec<LogStream>,
    heap: BinaryHeap<HeapEntry>,
}

impl MergedLogIterator {
    /// Create a new merged iterator from a list of log files
    pub fn new(files: Vec<(PathBuf, String, LogStream)>) -> Self {
        let mut readers = Vec::with_capacity(files.len());
        let mut services = Vec::with_capacity(files.len());
        let mut streams = Vec::with_capacity(files.len());
        let heap = BinaryHeap::new();

        for (path, service, stream) in files.into_iter() {
            if let Ok(file) = File::open(&path) {
                let reader = BufReader::new(file);
                readers.push(reader);
                services.push(service);
                streams.push(stream);
            } else {
                continue;
            }
        }

        // Initialize heap with first entry from each reader
        let mut iter = Self {
            readers,
            services,
            streams,
            heap,
        };

        for i in 0..iter.readers.len() {
            iter.read_next_into_heap(i);
        }

        iter
    }

    fn read_next_into_heap(&mut self, source_idx: usize) {
        let mut line = String::new();
        if self.readers[source_idx].read_line(&mut line).is_ok() && !line.is_empty() {
            let line = line.trim_end();
            if let Some(log_line) = LogReader::parse_log_line(
                line,
                &self.services[source_idx],
                self.streams[source_idx],
            ) {
                self.heap.push(HeapEntry {
                    timestamp: log_line.timestamp.timestamp_millis(),
                    log_line,
                    source_idx,
                });
            }
        }
    }
}

impl Iterator for MergedLogIterator {
    type Item = LogLine;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.heap.pop()?;
        let source_idx = entry.source_idx;

        // Read next line from this source
        self.read_next_into_heap(source_idx);

        Some(entry.log_line)
    }
}

// ============================================================================
// Conversions
// ============================================================================

/// Convert LogLine to protocol LogEntry
impl From<LogLine> for LogEntry {
    fn from(line: LogLine) -> Self {
        LogEntry {
            service: line.service,
            line: line.line,
            timestamp: Some(line.timestamp.timestamp_millis()),
            stream: line.stream.as_str().to_string(),
        }
    }
}

impl From<&LogLine> for LogEntry {
    fn from(line: &LogLine) -> Self {
        LogEntry {
            service: line.service.clone(),
            line: line.line.clone(),
            timestamp: Some(line.timestamp.timestamp_millis()),
            stream: line.stream.as_str().to_string(),
        }
    }
}
