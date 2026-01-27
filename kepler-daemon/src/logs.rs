use chrono::{DateTime, TimeZone, Utc};
use kepler_protocol::protocol::LogEntry;
use parking_lot::RwLock;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;

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
}

/// Disk-based log buffer for log entries per config
#[derive(Debug)]
pub struct LogBuffer {
    logs_dir: PathBuf,
    /// Monotonically increasing sequence number for tracking position
    sequence: u64,
}

impl LogBuffer {
    pub fn new(logs_dir: PathBuf) -> Self {
        // Create logs directory if it doesn't exist with secure permissions (0o700)
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&logs_dir);
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(&logs_dir).ok();
        }

        // Count existing log entries to initialize sequence
        let sequence = Self::count_existing_entries(&logs_dir);

        Self { logs_dir, sequence }
    }

    /// Count existing log entries across all log files
    fn count_existing_entries(logs_dir: &PathBuf) -> u64 {
        let mut count = 0u64;
        if let Ok(entries) = fs::read_dir(logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    if let Ok(file) = File::open(&path) {
                        count += BufReader::new(file).lines().count() as u64;
                    }
                }
            }
        }
        count
    }

    /// Get the log file path for a service
    fn log_file_path(&self, service: &str) -> PathBuf {
        // Sanitize service name for filename (replace special chars)
        let safe_name = service.replace(['/', '\\', ':', '[', ']'], "_");
        self.logs_dir.join(format!("{}.log", safe_name))
    }

    /// Add a log entry, appending to disk
    pub fn push(&mut self, service: String, line: String, stream: LogStream) {
        let log_file = self.log_file_path(&service);
        let timestamp = Utc::now().timestamp();

        // Format: {timestamp_unix}\t{stream}\t{service}\t{line}
        let entry = format!("{}\t{}\t{}\t{}\n", timestamp, stream.as_str(), service, line);

        // Append to file with secure permissions (0o600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&log_file)
            {
                let _ = file.write_all(entry.as_bytes());
            }
        }
        #[cfg(not(unix))]
        {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
            {
                let _ = file.write_all(entry.as_bytes());
            }
        }

        self.sequence += 1;
    }

    /// Get the current sequence number (for follow mode)
    pub fn current_sequence(&self) -> u64 {
        self.sequence
    }

    /// List all log files in the directory
    fn list_log_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    files.push(path);
                }
            }
        }
        files
    }

    /// Read log entries from a file
    fn read_log_file(&self, path: &PathBuf) -> Vec<LogLine> {
        let mut entries = Vec::new();
        if let Ok(file) = File::open(path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line) {
                    entries.push(entry);
                }
            }
        }
        entries
    }

    /// Parse a log line from the file format
    fn parse_log_line(line: &str) -> Option<LogLine> {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() < 4 {
            return None;
        }

        let timestamp = parts[0].parse::<i64>().ok()?;
        let stream = LogStream::parse(parts[1])?;
        let service = parts[2].to_string();
        let content = parts[3].to_string();

        Some(LogLine {
            service,
            line: content,
            timestamp: Utc.timestamp_opt(timestamp, 0).single()?,
            stream,
        })
    }

    /// Get entries since a given sequence number
    pub fn entries_since(&self, since_seq: u64) -> Vec<LogLine> {
        if since_seq >= self.sequence {
            return Vec::new();
        }

        // Read all entries and sort by timestamp
        let mut all_entries = Vec::new();
        for file in self.list_log_files() {
            all_entries.extend(self.read_log_file(&file));
        }
        all_entries.sort_by_key(|e| e.timestamp);

        // Skip entries up to since_seq
        let skip = since_seq as usize;
        all_entries.into_iter().skip(skip).collect()
    }

    /// Get the last N entries, optionally filtered by service
    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        // Get list of log files to read
        let files = match service {
            Some(svc) => {
                let path = self.log_file_path(svc);
                if path.exists() {
                    vec![path]
                } else {
                    vec![]
                }
            }
            None => self.list_log_files(),
        };

        // Read and parse entries from each file
        let mut entries = Vec::new();
        for file in files {
            let file_entries = self.read_log_file(&file);
            // Filter by service if specified (in case filename doesn't exactly match)
            if let Some(svc) = service {
                entries.extend(file_entries.into_iter().filter(|e| e.service == svc));
            } else {
                entries.extend(file_entries);
            }
        }

        // Sort by timestamp and return last N
        entries.sort_by_key(|e| e.timestamp);
        let start = entries.len().saturating_sub(count);
        entries.into_iter().skip(start).collect()
    }

    /// Get all entries, optionally filtered by service
    pub fn all(&self, service: Option<&str>) -> Vec<LogLine> {
        let files = match service {
            Some(svc) => {
                let path = self.log_file_path(svc);
                if path.exists() {
                    vec![path]
                } else {
                    vec![]
                }
            }
            None => self.list_log_files(),
        };

        let mut entries = Vec::new();
        for file in files {
            let file_entries = self.read_log_file(&file);
            if let Some(svc) = service {
                entries.extend(file_entries.into_iter().filter(|e| e.service == svc));
            } else {
                entries.extend(file_entries);
            }
        }

        entries.sort_by_key(|e| e.timestamp);
        entries
    }

    /// Clear all entries (delete all log files)
    pub fn clear(&mut self) {
        for file in self.list_log_files() {
            let _ = fs::remove_file(file);
        }
        self.sequence = 0;
    }

    /// Clear entries for a specific service (delete the log file)
    pub fn clear_service(&mut self, service: &str) {
        let log_file = self.log_file_path(service);
        if log_file.exists() {
            // Count entries being removed to update sequence
            if let Ok(file) = File::open(&log_file) {
                let count = BufReader::new(file).lines().count() as u64;
                self.sequence = self.sequence.saturating_sub(count);
            }
            let _ = fs::remove_file(log_file);
        }
    }

    /// Clear entries for services matching a prefix (e.g., "[global" clears all global hooks)
    pub fn clear_service_prefix(&mut self, prefix: &str) {
        // Sanitize prefix for matching filenames
        let safe_prefix = prefix.replace(['/', '\\', ':', '[', ']'], "_");

        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_stem() {
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with(&safe_prefix) {
                        // Count entries being removed
                        if let Ok(file) = File::open(&path) {
                            let count = BufReader::new(file).lines().count() as u64;
                            self.sequence = self.sequence.saturating_sub(count);
                        }
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }
}

/// Thread-safe log buffer wrapper
#[derive(Debug, Clone)]
pub struct SharedLogBuffer {
    inner: Arc<RwLock<LogBuffer>>,
}

impl SharedLogBuffer {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LogBuffer::new(logs_dir))),
        }
    }

    pub fn push(&self, service: String, line: String, stream: LogStream) {
        self.inner.write().push(service, line, stream);
    }

    pub fn current_sequence(&self) -> u64 {
        self.inner.read().current_sequence()
    }

    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        self.inner.read().tail(count, service)
    }

    pub fn entries_since(&self, since_seq: u64, service: Option<&str>) -> Vec<LogLine> {
        let buffer = self.inner.read();
        let entries = buffer.entries_since(since_seq);
        if let Some(svc) = service {
            entries.into_iter().filter(|e| e.service == svc).collect()
        } else {
            entries
        }
    }

    pub fn clear(&self) {
        self.inner.write().clear();
    }

    pub fn clear_service(&self, service: &str) {
        self.inner.write().clear_service(service);
    }

    pub fn clear_service_prefix(&self, prefix: &str) {
        self.inner.write().clear_service_prefix(prefix);
    }
}

/// Convert LogLine to protocol LogEntry
impl From<LogLine> for LogEntry {
    fn from(line: LogLine) -> Self {
        LogEntry {
            service: line.service,
            line: line.line,
            timestamp: Some(line.timestamp.timestamp()),
            stream: line.stream.as_str().to_string(),
        }
    }
}

impl From<&LogLine> for LogEntry {
    fn from(line: &LogLine) -> Self {
        LogEntry {
            service: line.service.clone(),
            line: line.line.clone(),
            timestamp: Some(line.timestamp.timestamp()),
            stream: line.stream.as_str().to_string(),
        }
    }
}
