use chrono::{DateTime, TimeZone, Utc};
use kepler_protocol::protocol::LogEntry;
use parking_lot::RwLock;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Default maximum bytes to read when tailing logs (10MB)
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Default maximum lines to return
pub const DEFAULT_MAX_LINES: usize = 10_000;

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

impl std::fmt::Display for LogStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Default max log file size for rotation (10MB)
pub const DEFAULT_MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Default number of rotated files to keep
pub const DEFAULT_MAX_ROTATED_FILES: u32 = 5;

/// Disk-based log buffer for log entries per config
pub struct LogBuffer {
    logs_dir: PathBuf,
    /// Monotonically increasing sequence number for tracking position
    sequence: u64,
    /// Maximum log file size before rotation (bytes)
    max_log_size: u64,
    /// Maximum number of rotated files to keep
    max_rotated_files: u32,
    /// Pre-allocated buffer for formatting log entries (reduces allocations)
    format_buffer: String,
}

impl std::fmt::Debug for LogBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogBuffer")
            .field("logs_dir", &self.logs_dir)
            .field("sequence", &self.sequence)
            .field("max_log_size", &self.max_log_size)
            .field("max_rotated_files", &self.max_rotated_files)
            .finish()
    }
}

impl LogBuffer {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self::with_rotation(logs_dir, DEFAULT_MAX_LOG_SIZE, DEFAULT_MAX_ROTATED_FILES)
    }

    pub fn with_rotation(logs_dir: PathBuf, max_log_size: u64, max_rotated_files: u32) -> Self {
        // Create logs directory if it doesn't exist with secure permissions (0o700)
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            if let Err(e) = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&logs_dir)
            {
                tracing::warn!("Failed to create logs directory {:?}: {}", logs_dir, e);
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(e) = fs::create_dir_all(&logs_dir) {
                tracing::warn!("Failed to create logs directory {:?}: {}", logs_dir, e);
            }
        }

        // Load or estimate sequence number (optimized - no longer counts all lines)
        let sequence = Self::load_or_estimate_sequence(&logs_dir);

        Self {
            logs_dir,
            sequence,
            max_log_size,
            max_rotated_files,
            // Pre-allocate buffer for typical log line (256 bytes should cover most cases)
            format_buffer: String::with_capacity(256),
        }
    }

    /// Sequence metadata file path
    fn sequence_file_path(logs_dir: &Path) -> PathBuf {
        logs_dir.join(".sequence")
    }

    /// Load sequence from metadata file, or estimate from file sizes
    ///
    /// This is much faster than counting all lines on startup.
    /// Uses file size as an approximation when the metadata file doesn't exist.
    fn load_or_estimate_sequence(logs_dir: &Path) -> u64 {
        let seq_file = Self::sequence_file_path(logs_dir);

        // Try to load from metadata file first (O(1))
        if let Ok(contents) = fs::read_to_string(&seq_file) {
            if let Ok(seq) = contents.trim().parse::<u64>() {
                return seq;
            }
        }

        // Fall back to size-based estimation (O(number of files), not O(lines))
        // Assume average log line is ~100 bytes
        const AVG_LINE_SIZE: u64 = 100;
        let mut total_bytes = 0u64;

        if let Ok(entries) = fs::read_dir(logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    if let Ok(metadata) = fs::metadata(&path) {
                        total_bytes += metadata.len();
                    }
                }
            }
        }

        total_bytes / AVG_LINE_SIZE
    }

    /// Save sequence to metadata file
    fn save_sequence(&self) {
        let seq_file = Self::sequence_file_path(&self.logs_dir);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut file) = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&seq_file)
            {
                let _ = file.write_all(self.sequence.to_string().as_bytes());
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&seq_file, self.sequence.to_string());
        }
    }

    /// Count existing log entries across all log files (legacy, kept for compatibility)
    #[allow(dead_code)]
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

    /// Add a log entry, appending to disk with rotation if needed
    pub fn push(&mut self, service: String, line: String, stream: LogStream) {
        let log_file = self.log_file_path(&service);
        let timestamp = Utc::now().timestamp_millis();

        // Check if rotation is needed before writing
        self.maybe_rotate(&log_file);

        // Format: {timestamp_unix}\t{stream}\t{service}\t{line}
        // Use pre-allocated buffer to reduce allocations
        self.format_buffer.clear();
        let _ = writeln!(
            self.format_buffer,
            "{}\t{}\t{}\t{}",
            timestamp,
            stream.as_str(),
            service,
            line
        );
        let entry = &self.format_buffer;

        // Append to file with secure permissions (0o600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            match OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&log_file)
            {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(entry.as_bytes()) {
                        tracing::warn!("Failed to write log entry to {:?}: {}", log_file, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to open log file {:?}: {}", log_file, e);
                }
            }
        }
        #[cfg(not(unix))]
        {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
            {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(entry.as_bytes()) {
                        tracing::warn!("Failed to write log entry to {:?}: {}", log_file, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to open log file {:?}: {}", log_file, e);
                }
            }
        }

        self.sequence += 1;

        // Periodically save sequence to disk (every 100 entries)
        if self.sequence % 100 == 0 {
            self.save_sequence();
        }
    }

    /// Check if log file needs rotation and rotate if necessary
    fn maybe_rotate(&self, log_file: &PathBuf) {
        // Check current file size
        let file_size = match fs::metadata(log_file) {
            Ok(m) => m.len(),
            Err(_) => return, // File doesn't exist, no rotation needed
        };

        if file_size >= self.max_log_size {
            self.rotate_log_file(log_file);
        }
    }

    /// Rotate a log file: .log -> .log.1, .log.1 -> .log.2, etc.
    fn rotate_log_file(&self, log_file: &PathBuf) {
        tracing::debug!("Rotating log file {:?}", log_file);

        // Delete oldest rotated file if at max
        let oldest = format!("{}.{}", log_file.display(), self.max_rotated_files);
        let _ = fs::remove_file(&oldest);

        // Shift existing rotated files: .log.N -> .log.N+1
        for i in (1..self.max_rotated_files).rev() {
            let old_name = format!("{}.{}", log_file.display(), i);
            let new_name = format!("{}.{}", log_file.display(), i + 1);
            let _ = fs::rename(&old_name, &new_name);
        }

        // Rotate current file: .log -> .log.1
        let rotated = format!("{}.1", log_file.display());
        if let Err(e) = fs::rename(log_file, &rotated) {
            tracing::warn!("Failed to rotate log file {:?}: {}", log_file, e);
        }
    }

    /// Clean up old rotated log files beyond max_rotated_files
    pub fn cleanup_old_rotations(&self) {
        for file in self.list_log_files() {
            // Check for rotated files beyond max
            for i in (self.max_rotated_files + 1)..100 {
                let rotated = format!("{}.{}", file.display(), i);
                if PathBuf::from(&rotated).exists() {
                    let _ = fs::remove_file(&rotated);
                } else {
                    break; // No more rotated files
                }
            }
        }
    }

    /// Get the current sequence number (for follow mode)
    pub fn current_sequence(&self) -> u64 {
        self.sequence
    }

    /// List all log files in the directory (main files only, not rotated)
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

    /// List all log files for a service including rotated files
    /// Returns files in order: oldest rotated (.log.N) to newest (.log)
    fn list_service_log_files(&self, service: &str) -> Vec<PathBuf> {
        let main_file = self.log_file_path(service);
        let mut files = Vec::new();

        // Add rotated files in reverse order (oldest first)
        for i in (1..=self.max_rotated_files).rev() {
            let rotated = PathBuf::from(format!("{}.{}", main_file.display(), i));
            if rotated.exists() {
                files.push(rotated);
            }
        }

        // Add main file last (newest)
        if main_file.exists() {
            files.push(main_file);
        }

        files
    }

    /// Read log entries from a file
    fn read_log_file(&self, path: &PathBuf) -> Vec<LogLine> {
        self.read_log_file_bounded(path, None, None)
    }

    /// Read log entries from a file with optional bounds
    ///
    /// `max_bytes` - Maximum bytes to read from the file (reads from end)
    /// `max_lines` - Maximum number of lines to return
    fn read_log_file_bounded(
        &self,
        path: &PathBuf,
        max_bytes: Option<usize>,
        max_lines: Option<usize>,
    ) -> Vec<LogLine> {
        let mut entries = Vec::new();
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
            let mut file = file;
            let start_pos = file_size - max_bytes;

            if file.seek(SeekFrom::Start(start_pos as u64)).is_err() {
                return entries;
            }

            let mut reader = BufReader::new(file);

            // Skip the first partial line (we started in the middle)
            let mut partial_line = String::new();
            if reader.read_line(&mut partial_line).is_err() {
                return entries;
            }

            // Read remaining lines
            for line in reader.lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line) {
                    entries.push(entry);
                }
            }
        } else {
            // File is small enough, read everything
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line) {
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
            timestamp: Utc.timestamp_millis_opt(timestamp).single()?,
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
        self.tail_bounded(count, service, None)
    }

    /// Get the last N entries with bounded reading to prevent OOM
    ///
    /// `count` - Maximum number of lines to return
    /// `service` - Optional service filter
    /// `max_bytes` - Maximum bytes to read per file (prevents OOM with large logs)
    pub fn tail_bounded(
        &self,
        count: usize,
        service: Option<&str>,
        max_bytes: Option<usize>,
    ) -> Vec<LogLine> {
        // Apply sensible defaults to prevent OOM
        let count = count.min(DEFAULT_MAX_LINES);
        let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // Get list of log files to read (including rotated files)
        let files = match service {
            Some(svc) => self.list_service_log_files(svc),
            None => {
                // For all services, collect all files including rotated
                let mut all_files = Vec::new();
                for main_file in self.list_log_files() {
                    // Extract service name from file path
                    if let Some(stem) = main_file.file_stem().and_then(|s| s.to_str()) {
                        all_files.extend(self.list_service_log_files(stem));
                    }
                }
                all_files
            }
        };

        // Calculate per-file byte limit when reading multiple files
        let per_file_max_bytes = if files.is_empty() {
            max_bytes
        } else {
            // Distribute bytes across files, but ensure minimum per file
            (max_bytes / files.len()).max(1024 * 1024) // At least 1MB per file
        };

        // Read and parse entries from each file with bounds
        let mut entries = Vec::new();
        for file in files {
            let file_entries = self.read_log_file_bounded(&file, Some(per_file_max_bytes), None);
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

    /// Get logs with true pagination (reads from disk with offset/limit).
    ///
    /// This method efficiently reads only the needed portion of log files,
    /// avoiding loading all logs into memory.
    ///
    /// Returns (entries, total_count) where total_count is the total number
    /// of entries across all relevant log files.
    pub fn get_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        // Get list of log files to read (including rotated files)
        let files = match service {
            Some(svc) => self.list_service_log_files(svc),
            None => {
                // For all services, collect all files including rotated
                let mut all_files = Vec::new();
                for main_file in self.list_log_files() {
                    // Extract service name from file path
                    if let Some(stem) = main_file.file_stem().and_then(|s| s.to_str()) {
                        all_files.extend(self.list_service_log_files(stem));
                    }
                }
                all_files
            }
        };

        if files.is_empty() {
            return (Vec::new(), 0);
        }

        // Count lines in each file for accurate offset calculation
        let file_counts: Vec<(PathBuf, usize)> = files
            .iter()
            .map(|f| (f.clone(), self.count_lines_in_file(f)))
            .collect();

        let total: usize = file_counts.iter().map(|(_, c)| *c).sum();

        if offset >= total {
            return (Vec::new(), total);
        }

        // Read only needed files/ranges
        let entries = self.read_range_across_files(&file_counts, service, offset, limit);

        (entries, total)
    }

    /// Fast line counting for a file
    fn count_lines_in_file(&self, path: &PathBuf) -> usize {
        match File::open(path) {
            Ok(file) => BufReader::new(file).lines().count(),
            Err(_) => 0,
        }
    }

    /// Read a range of log entries across multiple files.
    ///
    /// This method skips files until reaching the offset, then reads
    /// only the needed portion of each file.
    fn read_range_across_files(
        &self,
        files: &[(PathBuf, usize)],
        service_filter: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Vec<LogLine> {
        // First, read all entries and sort them by timestamp
        // This is necessary because log files may have interleaved timestamps
        // especially when reading from multiple services
        let mut all_entries: Vec<LogLine> = Vec::new();

        for (file, _) in files {
            let entries = self.read_log_file(file);
            if let Some(svc) = service_filter {
                all_entries.extend(entries.into_iter().filter(|e| e.service == svc));
            } else {
                all_entries.extend(entries);
            }
        }

        // Sort by timestamp to ensure correct ordering
        all_entries.sort_by_key(|e| e.timestamp);

        // Apply offset and limit
        all_entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect()
    }

    /// Clear all entries (delete all log files)
    pub fn clear(&mut self) {
        for file in self.list_log_files() {
            // Remove rotated files first
            for i in 1..=self.max_rotated_files {
                let rotated = format!("{}.{}", file.display(), i);
                let _ = fs::remove_file(&rotated);
            }
            // Then remove the main file
            let _ = fs::remove_file(&file);
        }
        self.sequence = 0;
        self.save_sequence();
    }

    /// Clear entries for a specific service (delete the log file and all rotated files)
    pub fn clear_service(&mut self, service: &str) {
        let log_file = self.log_file_path(service);

        // Remove rotated files first (.log.1, .log.2, etc.)
        for i in 1..=self.max_rotated_files {
            let rotated = format!("{}.{}", log_file.display(), i);
            let rotated_path = PathBuf::from(&rotated);
            if rotated_path.exists() {
                // Count entries being removed to update sequence
                if let Ok(file) = File::open(&rotated_path) {
                    let count = BufReader::new(file).lines().count() as u64;
                    self.sequence = self.sequence.saturating_sub(count);
                }
                let _ = fs::remove_file(&rotated_path);
            }
        }

        // Then remove the main log file
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
    /// Also removes rotated log files for matching services
    pub fn clear_service_prefix(&mut self, prefix: &str) {
        // Sanitize prefix for matching filenames
        let safe_prefix = prefix.replace(['/', '\\', ':', '[', ']'], "_");

        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = path.file_name().map(|n| n.to_string_lossy().to_string());

                if let Some(name) = file_name {
                    // Check if this is a log file (main or rotated) for a matching service
                    // Main files: "service.log"
                    // Rotated files: "service.log.1", "service.log.2", etc.
                    let is_rotated = name.contains(".log.");
                    let base_name = if is_rotated {
                        // Extract base name from "service.log.N"
                        name.split(".log.").next().map(String::from)
                    } else if name.ends_with(".log") {
                        // Extract base name from "service.log"
                        name.strip_suffix(".log").map(String::from)
                    } else {
                        None
                    };

                    if let Some(base) = base_name {
                        if base.starts_with(&safe_prefix) {
                            // Count entries being removed
                            if let Ok(file) = File::open(&path) {
                                let count = BufReader::new(file).lines().count() as u64;
                                self.sequence = self.sequence.saturating_sub(count);
                            }
                            let _ = fs::remove_file(&path);
                        }
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

    pub fn with_rotation(logs_dir: PathBuf, max_log_size: u64, max_rotated_files: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LogBuffer::with_rotation(
                logs_dir,
                max_log_size,
                max_rotated_files,
            ))),
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

    pub fn tail_bounded(
        &self,
        count: usize,
        service: Option<&str>,
        max_bytes: Option<usize>,
    ) -> Vec<LogLine> {
        self.inner.read().tail_bounded(count, service, max_bytes)
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

    /// Get logs with true pagination
    pub fn get_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        self.inner.read().get_paginated(service, offset, limit)
    }
}

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
